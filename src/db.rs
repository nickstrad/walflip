//! Database-level operations. Everything here returns plain data; printing belongs to the CLI.

use crate::replay;
use crate::store::{wal_mark, Store, WalMark, WriteLock};
use anyhow::{bail, Context, Result};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

const MAX_NAME: usize = 64;

/// Database names become folder names, so keep them boring.
fn validate_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= MAX_NAME
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        bail!("invalid database name '{s}': use 1-{MAX_NAME} letters, digits, '_' or '-'");
    }
    Ok(())
}

/// One managed database: a folder holding `live.db`, `base.db` and `meta.db`.
pub struct Db {
    pub dir: PathBuf,
    pub live: PathBuf,
    pub base: PathBuf,
    pub meta: PathBuf,
}

impl Db {
    pub fn new(root: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let dir = root.join(name);
        Ok(Self {
            live: dir.join("live.db"),
            base: dir.join("base.db"),
            meta: dir.join("meta.db"),
            dir,
        })
    }

    /// Like `new`, but the database must already exist.
    pub fn existing(root: &Path, name: &str) -> Result<Self> {
        let db = Self::new(root, name)?;
        if !db.live.exists() {
            bail!("no database named '{name}' under {} (see `walflip list`)", root.display());
        }
        Ok(db)
    }

    pub fn wal_path(&self) -> PathBuf {
        let mut s = self.live.clone().into_os_string();
        s.push("-wal");
        s.into()
    }
}

/// Split SQL text into statements: one per non-blank line.
pub fn parse_statements(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

pub struct Created {
    pub page_size: u32,
}

/// Create the folder, a WAL-mode live db, and the base snapshot. `schema` is arbitrary SQL
/// (typically DDL, optionally seed rows) run before the snapshot is taken, so it becomes the
/// state that `clone -n 0` returns. If it fails, nothing is left on disk.
pub fn create(db: &Db, schema: Option<&str>) -> Result<Created> {
    if db.dir.exists() {
        bail!("{} already exists", db.dir.display());
    }
    fs::create_dir_all(&db.dir)?;
    let result = init_files(db, schema);
    if result.is_err() {
        let _ = fs::remove_dir_all(&db.dir);
    }
    Ok(Created { page_size: result? })
}

/// What the authorizer refused, so errors can name it (SQLite only says "not authorized").
type Denied = Arc<Mutex<Option<String>>>;

/// What the `exec` authorizer is guarding right now (stored in an [`AtomicU8`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum AuthMode {
    /// walflip's own SQL (`BEGIN IMMEDIATE`, `COMMIT`, ...), and every schema in `create`.
    Own = 0,
    /// A user statement inside exec's transaction: transaction control is denied too.
    Statement = 1,
    /// walflip's own literal `VACUUM` (plan Q4): its internal `ATTACH ''` of a temp db is allowed.
    Vacuum = 2,
}

impl AuthMode {
    fn load(m: &AtomicU8) -> Self {
        match m.load(Ordering::SeqCst) {
            1 => Self::Statement,
            2 => Self::Vacuum,
            _ => Self::Own,
        }
    }
}

/// Install an authorizer that denies `ATTACH`, `DETACH` and `PRAGMA journal_mode`. Other pragmas
/// (`page_size`, `auto_vacuum`, ...) stay allowed. Transaction control (`BEGIN`/`COMMIT`/`END`/
/// `ROLLBACK`) is denied only in [`AuthMode::Statement`]: `exec` sets it around user statements
/// (its own `BEGIN`/`COMMIT` must still pass), `create` passes `None` so schemas may use
/// transactions. In [`AuthMode::Vacuum`] the one `ATTACH` that SQLite's `VACUUM` issues itself
/// (an empty file name: a private temp db) passes; any other `ATTACH` stays denied.
fn install_authorizer(conn: &Connection, mode: Option<Arc<AtomicU8>>) -> Denied {
    let denied: Denied = Arc::new(Mutex::new(None));
    let slot = denied.clone();
    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        let m = mode.as_deref().map_or(AuthMode::Own, AuthMode::load);
        match deny_reason(&ctx.action, m) {
            Some(reason) => {
                *slot.lock().unwrap() = Some(reason);
                Authorization::Deny
            }
            None => Authorization::Allow,
        }
    }));
    denied
}

fn deny_reason(action: &AuthAction<'_>, mode: AuthMode) -> Option<String> {
    match action {
        AuthAction::Attach { filename } if mode == AuthMode::Vacuum && filename.is_empty() => None,
        AuthAction::Attach { .. } => Some("ATTACH is not allowed".into()),
        AuthAction::Detach { .. } => Some("DETACH is not allowed".into()),
        AuthAction::Pragma { pragma_name, .. } if pragma_name.eq_ignore_ascii_case("journal_mode") => {
            Some("PRAGMA journal_mode is not allowed (walflip manages WAL mode)".into())
        }
        AuthAction::Transaction { .. } if mode == AuthMode::Statement => Some(
            "transaction control (BEGIN/COMMIT/END/ROLLBACK) is not allowed in exec: \
             each statement already runs in its own transaction"
                .into(),
        ),
        _ => None,
    }
}

/// Turn a failure into an error that names what the authorizer denied, if anything.
fn with_denial<T>(r: rusqlite::Result<T>, denied: &Denied, what: String) -> Result<T> {
    r.map_err(|e| match denied.lock().unwrap().take() {
        Some(reason) => anyhow::Error::new(e).context(format!("{what}: {reason}")),
        None => anyhow::Error::new(e).context(what),
    })
}

fn assert_wal(conn: &Connection) -> Result<()> {
    let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if mode != "wal" {
        bail!("live.db is not in WAL mode (journal_mode is {mode})");
    }
    Ok(())
}

fn init_files(db: &Db, schema: Option<&str>) -> Result<u32> {
    let conn = Connection::open(&db.live)?;
    // Schema first, in rollback mode, so page_size/auto_vacuum pragmas take effect.
    if let Some(sql) = schema {
        let denied = install_authorizer(&conn, None);
        with_denial(conn.execute_batch(sql), &denied, "applying schema".into())?;
        if !conn.is_autocommit() {
            bail!("applying schema: schema left a transaction open (unterminated BEGIN)");
        }
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    }
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if mode != "wal" {
        bail!("could not enable WAL (got {mode})");
    }
    // Fold the schema into the main file so base.db is a complete, WAL-free starting point.
    conn.pragma_update_and_check(None, "wal_checkpoint", "TRUNCATE", |_| Ok(()))?;
    let page_size: u32 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    drop(conn);

    fs::copy(&db.live, &db.base)?;
    Store::open(&db.meta)?.set("page_size", &page_size.to_string())?;
    Ok(page_size)
}

pub struct DbSummary {
    pub name: String,
    pub commits: u64,
}

pub fn list(root: &Path) -> Result<Vec<DbSummary>> {
    let entries = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if !path.join("live.db").exists() {
            continue;
        }
        let Ok(name) = e.file_name().into_string() else { continue };
        // A folder with a `live.db` but no `meta.db` is not a walflip database: skipped, and
        // nothing is created in it.
        let meta = path.join("meta.db");
        if !meta.is_file() {
            continue;
        }
        let commits = Store::open_existing(&meta)?.commit_count()?;
        out.push(DbSummary { name, commits });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn rename(root: &Path, old: &str, new: &str) -> Result<()> {
    let from = Db::existing(root, old)?;
    let to = Db::new(root, new)?;
    if to.dir.exists() {
        bail!("{} already exists", to.dir.display());
    }
    fs::rename(&from.dir, &to.dir)?;
    Ok(())
}

pub fn drop_db(db: &Db) -> Result<()> {
    fs::remove_dir_all(&db.dir)?;
    Ok(())
}

/// Label of commits found in the WAL that no walflip `exec` recorded (a writer that crashed before
/// capture, or a process writing `live.db` directly). Plan Q3 / INV-25.
pub const RECOVERED_LABEL: &str = "(recovered)";

/// What one `exec` call captured.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExecReport {
    /// All commits added to history, including `recovered`.
    pub captured: usize,
    /// Commits that were already in the WAL before a statement ran, stored as [`RECOVERED_LABEL`].
    pub recovered: usize,
}

/// Attached to the error of an `exec` that failed after it had already captured commits, so the
/// count is not lost (`err.downcast_ref::<ExecFailed>()`). Earlier statements stay committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecFailed {
    /// Commits captured by this `exec` before (and including) the failing statement.
    pub captured: usize,
    pub recovered: usize,
    /// 1-based position of the failing statement.
    pub statement: usize,
}

impl std::fmt::Display for ExecFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "statement {} failed; {} commit(s) were captured before it stopped", self.statement, self.captured)?;
        if self.recovered > 0 {
            write!(f, " ({} of them {RECOVERED_LABEL})", self.recovered)?;
        }
        Ok(())
    }
}

/// Run each statement as its own transaction and capture the commits it produced.
/// Returns the total number of commits captured. A statement that changes nothing
/// (e.g. an UPDATE that matches no rows) writes no WAL frames and so adds no commit.
/// See [`exec_with`] for locking, recovery and error behaviour.
pub fn exec(db: &Db, statements: &[String]) -> Result<usize> {
    Ok(exec_with(db, statements, crate::store::DEFAULT_BUSY_TIMEOUT)?.captured)
}

/// [`exec`] with a busy timeout and a report that separates recovered commits.
///
/// Concurrency (plan Q1, INV-24): for each statement, `exec` first takes the history write lock
/// (`BEGIN IMMEDIATE` on `meta.db`), then the `live.db` write lock (`BEGIN IMMEDIATE`), runs the
/// statement, commits it, captures it inside the still-open `meta.db` transaction, and only then
/// releases the history lock. So the WAL and the cursor are read only while no other walflip
/// process can write or capture, and each statement's commit is captured before anyone else can
/// add one. Every walflip process takes the two locks in the same order, so they cannot deadlock.
/// Both locks are SQLite file locks: they vanish with the process. A lock not obtained within
/// `busy` is a clean `locked` error with nothing changed.
///
/// Recovery (plan Q3, INV-25): with both locks held and before the statement runs, any commit
/// already in the WAL that history does not have yet was written by someone else (a crashed
/// writer, an external tool). It is captured first under [`RECOVERED_LABEL`], never attributed to
/// the statement.
///
/// Statements may not control the transaction (`BEGIN`/`COMMIT`/`ROLLBACK`, plan Q2): the
/// authorizer rejects them, so a statement like `INSERT ...; COMMIT; INSERT ...` fails and
/// nothing it did is committed. As a backstop (INV-39), a failed statement whose run changed the
/// WAL is still captured, so `live.db` and history cannot diverge; and a statement that yields
/// more than one commit is an error (INV-38) after its commits are captured. An error after some
/// commits were captured carries [`ExecFailed`] with the count.
///
/// `VACUUM` (plan Q4) cannot run inside a transaction, so a statement that is exactly `VACUUM`
/// or `VACUUM main` (see [`vacuum_kind`]) takes a separate path, [`ExecState::vacuum`]: same
/// locks and recovery, then the live transaction is released and walflip runs its own literal
/// `VACUUM` in autocommit, still under the history lock, and captures its one commit. Any other
/// statement starting with `VACUUM` (`VACUUM INTO`, another schema, more SQL after it) is
/// rejected before anything is locked or written.
pub fn exec_with(db: &Db, statements: &[String], busy: Duration) -> Result<ExecReport> {
    exec_inner(db, statements, busy, None)
}

/// Test hook run between a statement's commit and its capture (simulates a foreign writer
/// slipping in there, the one window the locks cannot close). Always `None` outside tests.
type AfterCommit = Option<Box<dyn Fn()>>;

fn exec_inner(db: &Db, statements: &[String], busy: Duration, after_commit: AfterCommit) -> Result<ExecReport> {
    let mut store = Store::open_with_timeout(&db.meta, busy)?;
    let conn = Connection::open(&db.live)?;
    conn.busy_timeout(busy)?;
    assert_wal(&conn)?;
    let mode = Arc::new(AtomicU8::new(AuthMode::Own as u8));
    let denied = install_authorizer(&conn, Some(mode.clone()));
    // Stop SQLite from folding the WAL back into the db while we are still reading it.
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;

    let mut s = ExecState {
        conn: &conn,
        mode: &mode,
        denied: &denied,
        wal: db.wal_path(),
        last_mark: None,
        report: ExecReport::default(),
        after_commit,
    };
    for (i, stmt) in statements.iter().enumerate() {
        if let Err(e) = s.statement(&mut store, stmt) {
            let r = s.report;
            if r.captured == 0 {
                return Err(e);
            }
            return Err(e.context(ExecFailed { captured: r.captured, recovered: r.recovered, statement: i + 1 }));
        }
    }
    Ok(s.report)
}

struct ExecState<'a> {
    conn: &'a Connection,
    mode: &'a AtomicU8,
    denied: &'a Denied,
    wal: PathBuf,
    /// The WAL as our last capture read it (outer `None` = no capture yet). While it is unchanged
    /// nobody has committed since, so the recovery capture can skip re-reading the WAL.
    last_mark: Option<WalMark>,
    report: ExecReport,
    after_commit: AfterCommit,
}

impl<'a> ExecState<'a> {
    fn statement(&mut self, store: &mut Store, stmt: &str) -> Result<()> {
        match vacuum_kind(stmt) {
            VacuumKind::Bare => return self.vacuum(store, stmt),
            VacuumKind::Other => bail!(
                "only a bare `VACUUM` (or `VACUUM main`) is supported in exec; VACUUM INTO, \
                 other schemas and more SQL in the same statement are not allowed: {stmt}"
            ),
            VacuumKind::No => {}
        }
        let lock = lock_history(store)?;
        let tx = self.begin_live()?;
        let before = self.recover(&lock)?;
        let r = run_statement(tx, self.mode, self.denied, stmt);
        let added = self.finish(lock, before, stmt, r)?;
        if added > 1 {
            bail!("statement produced {added} commits (at most 1 expected); all were captured: {stmt}");
        }
        Ok(())
    }

    /// Plan Q4: run `VACUUM` outside a transaction, captured as one commit labelled `stmt`.
    ///
    /// Lock order as for any statement: the history lock, then `BEGIN IMMEDIATE` on `live.db` so
    /// the `(recovered)` capture sees a WAL nobody else can add to. Then the live transaction is
    /// rolled back (it wrote nothing) and walflip's own literal `VACUUM` runs in autocommit with
    /// [`AuthMode::Vacuum`], and its commit is captured under the still-held history lock. So no
    /// other walflip process can write in between; a non-walflip writer can (the gap between the
    /// rollback and VACUUM taking the write lock), and its commit then lands in the same capture,
    /// which trips the more-than-one-commit backstop: everything is captured and `exec` errors.
    ///
    /// One commit is expected: VACUUM rebuilds the db in a temp file, then copies it back into
    /// `main` inside a single write transaction, so the WAL gets one commit frame however many
    /// pages are written (cache spills add non-commit frames only; checked with `cache_size=5`).
    fn vacuum(&mut self, store: &mut Store, stmt: &str) -> Result<()> {
        let lock = lock_history(store)?;
        let tx = self.begin_live()?;
        self.recover(&lock)?;
        tx.rollback().context("releasing live.db before VACUUM")?;
        let before = wal_mark(&self.wal);
        let r = run_vacuum(self.conn, self.mode, self.denied, stmt);
        let added = self.finish(lock, before, stmt, r)?;
        if added > 1 {
            bail!(
                "VACUUM produced {added} commits (1 expected); all were captured as `{stmt}`. Another \
                 process wrote live.db between walflip releasing it and VACUUM starting"
            );
        }
        Ok(())
    }

    fn begin_live(&self) -> Result<Transaction<'a>> {
        Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)
            .map_err(|e| locked(e.into(), "live.db: another process is writing it"))
    }

    /// B3: both locks are held, so whatever is in the WAL and not in history was committed by
    /// someone outside walflip. Capture it before the statement can be blamed for it. Returns the
    /// WAL mark as it stands before the statement.
    fn recover(&mut self, lock: &WriteLock<'_>) -> Result<WalMark> {
        let before = wal_mark(&self.wal);
        if self.last_mark.as_ref() != Some(&before) {
            let n = lock.capture(&self.wal, RECOVERED_LABEL).context("capturing commits found in the WAL")?.added;
            self.report.recovered += n;
            self.report.captured += n;
        }
        Ok(before)
    }

    /// Capture what the statement (result `r`) committed, labelled `stmt`, and release the history
    /// lock. Returns the number of commits added; the caller decides whether more than one is an
    /// error.
    fn finish(&mut self, lock: WriteLock<'_>, before: WalMark, stmt: &str, r: Result<()>) -> Result<usize> {
        if let Err(e) = r {
            // Backstop: anything that reached live.db despite the error must reach history too.
            // Skip capture when the WAL is untouched so a clean failure leaves meta.db as is.
            let n = if wal_mark(&self.wal) == before {
                0
            } else {
                let c = lock.capture(&self.wal, stmt).with_context(|| format!("{e:#}"))?;
                self.last_mark = Some(c.mark);
                c.added
            };
            lock.commit()?;
            self.report.captured += n;
            if n == 0 {
                return Err(e);
            }
            return Err(e.context(format!("{n} commit(s) from this statement reached live.db and were captured")));
        }
        if let Some(hook) = &self.after_commit {
            hook();
        }
        let c = lock.capture(&self.wal, stmt)?;
        lock.commit()?;
        self.last_mark = Some(c.mark);
        self.report.captured += c.added;
        Ok(c.added)
    }
}

fn lock_history(store: &mut Store) -> Result<WriteLock<'_>> {
    store.lock().map_err(|e| locked(e, "history (meta.db): another walflip exec is running"))
}

/// How [`exec`] treats a statement with respect to `VACUUM` (plan Q4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VacuumKind {
    /// Does not start with the keyword `VACUUM`: the ordinary path.
    No,
    /// Exactly `^\s*VACUUM(\s+(main|"main"))?\s*;?\s*$`, case-insensitive: the VACUUM path.
    Bare,
    /// Starts with `VACUUM` but is anything else (`VACUUM INTO`, `VACUUM temp`, `VACUUM; ...`,
    /// a trailing comment): rejected.
    Other,
}

fn vacuum_kind(stmt: &str) -> VacuumKind {
    let s = stmt.trim_start();
    let word_end = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len());
    if !s[..word_end].eq_ignore_ascii_case("vacuum") {
        return VacuumKind::No;
    }
    let rest = s[word_end..].trim_end();
    let rest = rest.strip_suffix(';').unwrap_or(rest).trim_end();
    let bare = rest.is_empty()
        || (rest.starts_with(char::is_whitespace) && {
            let schema = rest.trim_start();
            schema.eq_ignore_ascii_case("main") || schema.eq_ignore_ascii_case("\"main\"")
        });
    if bare {
        VacuumKind::Bare
    } else {
        VacuumKind::Other
    }
}

/// Run walflip's own literal `VACUUM` in autocommit (the user's text, already matched by
/// [`vacuum_kind`], is only the label). Transaction control is allowed (VACUUM issues its own
/// `BEGIN`) and so is its `ATTACH ''`; nothing else changes in the authorizer.
fn run_vacuum(conn: &Connection, mode: &AtomicU8, denied: &Denied, stmt: &str) -> Result<()> {
    debug_assert!(conn.is_autocommit());
    mode.store(AuthMode::Vacuum as u8, Ordering::SeqCst);
    let r = conn.execute_batch("VACUUM");
    mode.store(AuthMode::Own as u8, Ordering::SeqCst);
    with_denial(r, denied, format!("failed: {stmt}")).map_err(|e| locked(e, "live.db: another process is writing it"))
}

/// Name the lock in a busy error; pass other errors through.
fn locked(e: anyhow::Error, what: &str) -> anyhow::Error {
    if crate::store::is_busy(&e) {
        e.context(format!("locked: gave up waiting for the write lock on {what}"))
    } else {
        e
    }
}

/// Run one user statement in `tx`, with transaction control denied inside it, and commit.
fn run_statement(tx: Transaction<'_>, mode: &AtomicU8, denied: &Denied, stmt: &str) -> Result<()> {
    mode.store(AuthMode::Statement as u8, Ordering::SeqCst);
    let r = tx.execute_batch(stmt);
    mode.store(AuthMode::Own as u8, Ordering::SeqCst);
    with_denial(r, denied, format!("failed: {stmt}"))?;
    tx.commit().with_context(|| format!("failed to commit: {stmt}"))?;
    Ok(())
}

pub struct LogEntry {
    pub n: u64,
    pub frames: u64,
    pub db_pages: u64,
    pub sql: String,
}

/// Commits in ascending order. `from_end` selects the last `limit` commits instead of the first.
pub fn log(db: &Db, limit: u64, from_end: bool) -> Result<Vec<LogEntry>> {
    let store = Store::open_existing(&db.meta)?;
    let dir = if from_end { "DESC" } else { "ASC" };
    let mut st = store.conn().prepare(&format!(
        "SELECT idx, frames, db_pages, label FROM (
           SELECT c.idx AS idx, count(f.seq) AS frames, c.db_pages AS db_pages, c.label AS label
           FROM commits c LEFT JOIN frames f ON f.commit_idx = c.idx
           GROUP BY c.idx ORDER BY c.idx {dir} LIMIT ?1
         ) ORDER BY idx"
    ))?;
    let rows = st.query_map([limit], |r| {
        Ok(LogEntry { n: r.get(0)?, frames: r.get(1)?, db_pages: r.get(2)?, sql: r.get(3)? })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub struct Status {
    pub page_size: Option<u64>,
    pub commits: u64,
    pub frames: u64,
    /// Size of the live WAL file, if one exists.
    pub wal_bytes: Option<u64>,
    /// Commits in the live WAL not captured yet (a crashed or external writer); the next `exec`
    /// stores them as [`RECOVERED_LABEL`]. A hint only: read without the write lock.
    pub uncaptured: u64,
}

pub fn status(db: &Db) -> Result<Status> {
    let store = Store::open_existing(&db.meta)?;
    Ok(Status {
        page_size: store.page_size()?,
        commits: store.commit_count()?,
        frames: store.conn().query_row("SELECT count(*) FROM frames", [], |r| r.get(0))?,
        wal_bytes: fs::metadata(db.wal_path()).ok().map(|m| m.len()),
        uncaptured: store.uncaptured(&db.wal_path())?,
    })
}

/// Rebuild the database as of commit `n` into `out`. Returns the number of frames replayed.
pub fn clone_at(db: &Db, n: u64, out: &Path) -> Result<usize> {
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let store = Store::open_existing(&db.meta)?;
    let total = store.commit_count()?;
    if n > total {
        bail!("only {total} commits captured, cannot clone -n {n}");
    }
    let page_size = store.page_size()?.context("database has no recorded page size")? as usize;

    let mut st = store
        .conn()
        .prepare("SELECT pgno, page FROM frames WHERE commit_idx <= ?1 ORDER BY commit_idx, seq")?;
    let frames: Vec<(u32, Vec<u8>)> = st
        .query_map([n], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    // The commit frame records the db size; applying it shrinks the image if later commits freed pages.
    let db_pages: Option<u32> = if n > 0 {
        Some(store.conn().query_row("SELECT db_pages FROM commits WHERE idx=?1", [n], |r| r.get(0))?)
    } else {
        None
    };

    let mut image = fs::read(&db.base)?;
    // Allocation bound: earlier commits may reference pages above a later, shrunken db_pages, so
    // use the max of the base size and db_pages over commits 1..=n (not just commit n's).
    let base_pages = (image.len() / page_size) as u64;
    let max_recorded: Option<u32> = store.conn().query_row(
        "SELECT max(db_pages) FROM commits WHERE idx <= ?1",
        [n],
        |r| r.get(0),
    )?;
    let max_pages = base_pages.max(max_recorded.unwrap_or(0) as u64).min(u32::MAX as u64) as u32;
    replay::apply(&mut image, page_size, frames.iter().map(|(p, d)| (*p, d.as_slice())), db_pages, max_pages)?;
    // create_new closes the check-then-act gap: a racer that loses gets AlreadyExists and must not
    // touch the winner's file.
    let mut f = match OpenOptions::new().write(true).create_new(true).open(out) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => bail!("{} already exists", out.display()),
        Err(e) => return Err(e.into()),
    };
    if let Err(e) = f.write_all(&image) {
        drop(f);
        let _ = fs::remove_file(out);
        return Err(e.into());
    }
    Ok(frames.len())
}

pub struct Summary {
    pub integrity: String,
    pub tables: Vec<(String, u64)>,
}

/// Percent-encode a path for a SQLite `file:` URI. Only unreserved characters and `/` stay
/// literal, so no byte of the user path is interpreted as URI syntax. A relative path stays
/// relative; a leading `//` is collapsed to one `/` so it is not read as an authority.
fn uri_encode_path(path: &Path) -> String {
    let mut out = String::new();
    for &b in path.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    if out.starts_with("//") {
        // `//` would start a URI authority; on POSIX it names the same file as one `/`.
        out = format!("/{}", out.trim_start_matches('/'));
    }
    out
}

/// Open a db file read-only and report integrity plus per-table row counts.
pub fn summarize(path: &Path) -> Result<Summary> {
    // immutable=1 so opening does not create -wal/-shm side files.
    // The path is percent-encoded so `?`, `#` and `%` are literal file name bytes.
    let uri = format!("file:{}?immutable=1", uri_encode_path(path));
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let integrity: String = conn.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut tables = Vec::new();
    for t in names {
        let quoted = format!("\"{}\"", t.replace('"', "\"\""));
        let c: u64 = conn.query_row(&format!("SELECT count(*) FROM {quoted}"), [], |r| r.get(0))?;
        tables.push((t, c));
    }
    Ok(Summary { integrity, tables })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn accepted_names_are_safe_folder_names(s in any::<String>()) {
            if validate_name(&s).is_ok() {
                prop_assert!(!s.is_empty() && s.len() <= MAX_NAME);
                prop_assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
            }
        }
    }

    #[test]
    fn clone_works_after_a_later_commit_shrinks_the_db() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Db::new(tmp.path(), "shrink").unwrap();
        create(&d, Some("PRAGMA auto_vacuum=FULL; CREATE TABLE a(x BLOB); CREATE TABLE b(x BLOB);")).unwrap();
        let big = "INSERT INTO a VALUES (zeroblob(100000));".to_string();
        exec(&d, &[big]).unwrap();
        exec(&d, &["DROP TABLE a;".to_string()]).unwrap();
        let total = Store::open(&d.meta).unwrap().commit_count().unwrap();
        let pages: Vec<u64> = log(&d, 10, false).unwrap().iter().map(|e| e.db_pages).collect();
        assert!(pages.windows(2).any(|w| w[1] < w[0]), "db never shrank: {pages:?}");
        for n in 0..=total {
            let out = tmp.path().join(format!("c{n}.db"));
            clone_at(&d, n, &out).unwrap();
            assert_eq!(summarize(&out).unwrap().integrity, "ok", "commit {n}");
        }
    }

    #[test]
    fn summarize_counts_tables_with_doubled_quotes_in_names() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Db::new(tmp.path(), "quoted").unwrap();
        create(&d, Some(r#"CREATE TABLE "a""b"(x INTEGER); INSERT INTO "a""b" VALUES (1), (2);"#)).unwrap();
        let out = tmp.path().join("c.db");
        clone_at(&d, 0, &out).unwrap();
        let s = summarize(&out).unwrap();
        assert_eq!(s.integrity, "ok");
        assert!(s.tables.contains(&(r#"a"b"#.to_string(), 2)), "{:?}", s.tables);
    }

    #[test]
    fn summarize_and_clone_handle_uri_special_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Db::new(tmp.path(), "uri").unwrap();
        create(&d, Some("CREATE TABLE t(x); INSERT INTO t VALUES (1);")).unwrap();
        // A decoy that a `%41` in a name would resolve to if the path were URI-decoded.
        let decoy = tmp.path().join("eA.db");
        let other = Db::new(tmp.path(), "other").unwrap();
        create(&other, Some("CREATE TABLE decoy(x);")).unwrap();
        clone_at(&other, 0, &decoy).unwrap();
        for name in ["a?b.db", "c#d.db", "e%41.db", "sp ace.db", "u\u{e9}\u{4e2d}.db", "q?immutable=0#.db", "100%.db"] {
            let out = tmp.path().join(name);
            clone_at(&d, 0, &out).unwrap();
            let s = summarize(&out).unwrap();
            assert_eq!(s.integrity, "ok", "{name}");
            assert_eq!(s.tables, vec![("t".to_string(), 1)], "{name}");
        }
    }

    #[test]
    fn clone_refuses_existing_output_and_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Db::new(tmp.path(), "exists").unwrap();
        create(&d, Some("CREATE TABLE a(x);")).unwrap();
        let out = tmp.path().join("c.db");
        fs::write(&out, b"keep me").unwrap();
        let err = clone_at(&d, 0, &out).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(fs::read(&out).unwrap(), b"keep me");
    }

    fn kv_db(tmp: &Path, name: &str) -> Db {
        let d = Db::new(tmp, name).unwrap();
        create(&d, Some("CREATE TABLE t(v TEXT);")).unwrap();
        d
    }

    fn ins(v: &str) -> String {
        format!("INSERT INTO t VALUES('{v}')")
    }

    fn labels(d: &Db) -> Vec<String> {
        log(d, 100, false).unwrap().into_iter().map(|e| e.sql).collect()
    }

    /// INV-38 backstop, kept covered now that orphans are recovered up front: a foreign writer
    /// committing between a statement's commit and its capture makes that statement yield 2
    /// commits. Both are captured (history == live.db) and `exec` reports the error.
    #[test]
    fn backstop_more_than_one_commit_is_captured_then_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let d = kv_db(tmp.path(), "bs");
        let live = d.live.clone();
        let ext = Connection::open(&live).unwrap();
        ext.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        let hook: AfterCommit = Some(Box::new(move || ext.execute_batch(&ins("sneaky")).unwrap()));
        let err = exec_inner(&d, &[ins("a")], Duration::from_secs(5), hook).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("2 commits"), "{msg}");
        let f = err.downcast_ref::<ExecFailed>().expect("count kept");
        assert_eq!((f.captured, f.recovered, f.statement), (2, 0, 1));
        assert_eq!(labels(&d), [ins("a"), ins("a")]);
        let out = tmp.path().join("all.db");
        clone_at(&d, 2, &out).unwrap();
        let rows: i64 = Connection::open(&out).unwrap().query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2);
    }

    /// Q1: while another process holds the history lock, `exec` waits `busy`, then fails with a
    /// `locked` error and changes nothing; once the lock is gone it works.
    #[test]
    fn exec_fails_cleanly_when_history_lock_is_held() {
        let tmp = tempfile::tempdir().unwrap();
        let d = kv_db(tmp.path(), "lk");
        exec(&d, &[ins("a")]).unwrap();
        let holder = Connection::open(&d.meta).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let meta_before = fs::read(&d.meta).unwrap();
        let t = std::time::Instant::now();
        let err = format!("{:#}", exec_with(&d, &[ins("b")], Duration::from_millis(200)).unwrap_err());
        assert!(t.elapsed() >= Duration::from_millis(150), "did not wait for the lock");
        assert!(err.contains("locked") && err.contains("meta.db"), "{err}");
        holder.execute_batch("ROLLBACK").unwrap();
        assert_eq!(fs::read(&d.meta).unwrap(), meta_before);
        assert_eq!(labels(&d), [ins("a")]);
        drop(holder);
        assert_eq!(exec(&d, &[ins("b")]).unwrap(), 1);
        assert_eq!(labels(&d), [ins("a"), ins("b")]);
    }

    /// Q1: a foreign writer holding live.db's write lock also gives a clean `locked` error, and
    /// the history lock is released (the next exec does not wait on it).
    #[test]
    fn exec_fails_cleanly_when_live_db_is_write_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let d = kv_db(tmp.path(), "lw");
        let holder = Connection::open(&d.live).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let err = format!("{:#}", exec_with(&d, &[ins("b")], Duration::from_millis(100)).unwrap_err());
        assert!(err.contains("locked") && err.contains("live.db"), "{err}");
        let mut s = Store::open_with_timeout(&d.meta, Duration::from_millis(1)).unwrap();
        s.lock().unwrap().commit().unwrap();
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        assert_eq!(exec(&d, &[ins("b")]).unwrap(), 1);
    }

    /// B3: orphans are recovered before the first statement of a batch and between statements;
    /// a clean failure after a recovery keeps the recovered commits.
    #[test]
    fn orphans_are_recovered_under_their_own_label() {
        let tmp = tempfile::tempdir().unwrap();
        let d = kv_db(tmp.path(), "or");
        let ext = Connection::open(&d.live).unwrap();
        ext.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        ext.execute_batch(&format!("{}; {}", ins("x1"), ins("x1b"))).unwrap(); // 2 autocommits
        let r = exec_with(&d, &[ins("a"), ins("b")], Duration::from_secs(5)).unwrap();
        assert_eq!(r, ExecReport { captured: 4, recovered: 2 });
        assert_eq!(labels(&d), [RECOVERED_LABEL, RECOVERED_LABEL, &ins("a"), &ins("b")]);

        ext.execute_batch(&ins("x2")).unwrap();
        assert_eq!(status(&d).unwrap().uncaptured, 1);
        let err = exec(&d, &["INSERT INTO nope VALUES(1)".into()]).unwrap_err();
        let f = err.downcast_ref::<ExecFailed>().expect("recovered count kept on failure");
        assert_eq!((f.captured, f.recovered), (1, 1));
        assert_eq!(labels(&d).last().map(String::as_str), Some(RECOVERED_LABEL));
        assert_eq!(status(&d).unwrap().uncaptured, 0);
    }

    /// Q4: only `^\s*VACUUM(\s+(main|"main"))?\s*;?\s*$` (any case) takes the VACUUM path.
    #[test]
    fn vacuum_kind_matches_strictly() {
        for s in ["VACUUM", "vacuum;", "  Vacuum  ;  ", "VACUUM main", "vacuum MAIN;", "VACUUM \"main\"", "VACUUM\n\tmain\n;\n"] {
            assert_eq!(vacuum_kind(s), VacuumKind::Bare, "{s:?}");
        }
        for s in [
            "VACUUM INTO 'x.db'",
            "VACUUM main INTO 'x.db'",
            "VACUUM temp",
            "VACUUM; DROP TABLE t",
            "VACUUM;;",
            "VACUUM -- comment",
            "VACUUM \"main\"x",
            "VACUUM\"main\"",
            "VACUUM 'main'",
            "VACUUM main main",
        ] {
            assert_eq!(vacuum_kind(s), VacuumKind::Other, "{s:?}");
        }
        for s in ["", ";", "vacuumx", "VACUUMS", "SELECT 1; VACUUM", "/* c */ VACUUM", "INSERT INTO vacuum VALUES(1)"] {
            assert_eq!(vacuum_kind(s), VacuumKind::No, "{s:?}");
        }
    }

    #[test]
    fn vacuum_mode_allows_only_the_temp_attach() {
        let attach = |f| AuthAction::Attach { filename: f };
        assert_eq!(deny_reason(&attach(""), AuthMode::Vacuum), None);
        for m in [AuthMode::Own, AuthMode::Statement] {
            assert!(deny_reason(&attach(""), m).is_some(), "{m:?}");
        }
        for m in [AuthMode::Own, AuthMode::Statement, AuthMode::Vacuum] {
            assert!(deny_reason(&attach("x.db"), m).is_some(), "{m:?}");
            assert!(deny_reason(&AuthAction::Detach { database_name: "x" }, m).is_some(), "{m:?}");
        }
        let begin = AuthAction::Transaction { operation: rusqlite::hooks::TransactionOperation::Begin };
        assert!(deny_reason(&begin, AuthMode::Statement).is_some());
        assert_eq!(deny_reason(&begin, AuthMode::Vacuum), None);
    }

    /// Q4 known gap: a foreign commit landing between the live rollback and the VACUUM (simulated
    /// here right after VACUUM, before capture: the capture sees the same two commits) is captured
    /// with the VACUUM's label and `exec` errors; history still equals live.db.
    #[test]
    fn vacuum_backstop_captures_a_foreign_commit_and_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let d = kv_db(tmp.path(), "vb");
        exec(&d, &[ins("a")]).unwrap();
        let ext = Connection::open(&d.live).unwrap();
        ext.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        let hook: AfterCommit = Some(Box::new(move || ext.execute_batch(&ins("sneaky")).unwrap()));
        let err = exec_inner(&d, &["VACUUM".into()], Duration::from_secs(5), hook).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("VACUUM produced 2 commits"), "{msg}");
        let f = err.downcast_ref::<ExecFailed>().expect("count kept");
        assert_eq!((f.captured, f.recovered, f.statement), (2, 0, 1));
        assert_eq!(labels(&d), [ins("a"), "VACUUM".into(), "VACUUM".into()]);
        let out = tmp.path().join("all.db");
        clone_at(&d, 3, &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), fs::read(&d.live).unwrap());
    }

    #[test]
    fn rejects_path_like_names() {
        for bad in ["", ".", "..", "a/b", "a b", "名前", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_name("orders-2024_v1").is_ok());
    }

    #[test]
    fn statements_are_one_per_nonblank_line() {
        assert_eq!(
            parse_statements("  INSERT 1;\n\n\t\nUPDATE 2;  \n"),
            vec!["INSERT 1;".to_string(), "UPDATE 2;".to_string()]
        );
        assert!(parse_statements("").is_empty());
    }
}
