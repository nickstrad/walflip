//! The metadata SQLite file: base-snapshot facts, a commit log, and the captured WAL frames.

use crate::wal;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use std::{fs, io, path::Path, time::Duration};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS commits(
    idx          INTEGER PRIMARY KEY,   -- 1-based position in history
    label        TEXT NOT NULL,         -- the SQL that produced it
    db_pages     INTEGER NOT NULL,      -- database size after this commit
    committed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS frames(
    commit_idx INTEGER NOT NULL REFERENCES commits(idx),
    seq        INTEGER NOT NULL,        -- order inside the commit
    pgno       INTEGER NOT NULL,
    page       BLOB NOT NULL,
    PRIMARY KEY(commit_idx, seq)
);";

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
        [key, value],
    )?;
    Ok(())
}

/// How many commits of the WAL generation identified by `salt` are already stored.
///
/// The cursor is (salt1:salt2, commits seen). A different salt means SQLite restarted the WAL
/// after a checkpoint, so nothing in it has been seen yet.
pub fn commits_seen(
    stored_salt: Option<&str>,
    stored_commits: Option<&str>,
    salt: (u32, u32),
) -> usize {
    let current = format!("{}:{}", salt.0, salt.1);
    match stored_salt {
        Some(s) if s == current => stored_commits.and_then(|v| v.parse().ok()).unwrap_or(0),
        _ => 0,
    }
}

/// How long a connection waits for a lock held by another process before giving up.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// What a capture saw of the WAL file: its length and first 32 bytes (the header, whose salt
/// changes on every WAL restart). `None` when there is no WAL. Two equal marks mean no commit was
/// added in between, so a caller holding the write lock can skip re-reading an unchanged WAL.
pub type WalMark = Option<(u64, Vec<u8>)>;

/// The current [`WalMark`] of the file at `wal`.
pub fn wal_mark(wal: &Path) -> WalMark {
    use std::io::Read;
    let f = fs::File::open(wal).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = Vec::with_capacity(32);
    f.take(32).read_to_end(&mut head).ok()?;
    Some((len, head))
}

/// Meta key of the resume state ([`wal::Resume::to_meta`]) that lets a capture parse only the part
/// of the WAL added since the last one. It is an addition to `cursor_salt`/`cursor_commits`, which
/// stay authoritative: the state is used only while it agrees with them (an older walflip that
/// advanced them without it, or a missing key in an older `meta.db`, means a full parse).
const CURSOR_RESUME: &str = "cursor_resume";

/// The stored cursor.
struct Cursor {
    salt: Option<String>,
    commits: Option<String>,
    resume: Option<String>,
}

impl Cursor {
    fn load(conn: &Connection) -> Result<Self> {
        Ok(Self {
            salt: get_meta(conn, "cursor_salt")?,
            commits: get_meta(conn, "cursor_commits")?,
            resume: get_meta(conn, CURSOR_RESUME)?,
        })
    }

    /// The resume state, if present, well formed and in agreement with the salt/count cursor.
    fn resume(&self) -> Option<wal::Resume> {
        let r = wal::Resume::from_meta(self.resume.as_deref()?)?;
        let salt = r.salt();
        let agrees = self.salt.as_deref() == Some(format!("{}:{}", salt.0, salt.1).as_str())
            && self.commits.as_deref() == Some(r.commits.to_string().as_str());
        agrees.then_some(r)
    }

    /// True when storing `salt`, `total` and `end` would change nothing.
    fn is(&self, salt: &str, total: u64, end: &wal::Resume) -> bool {
        self.salt.as_deref() == Some(salt)
            && self.commits == Some(total.to_string())
            && self.resume == Some(end.to_meta())
    }
}

/// The bytes of the WAL file that a parse needs: the header, plus either the whole file
/// (`base == 0`) or only the part from a resume state's [`wal::Resume::tail_start`] on.
struct WalView {
    header: Vec<u8>,
    /// File bytes from offset `base` to the end of the file as read.
    body: Vec<u8>,
    base: u64,
}

impl WalView {
    /// Read the WAL at `path` (`None` when there is none). With `from`, read only the tail it
    /// needs when the file still has the same header and reaches the stored offset; otherwise
    /// (or without `from`) read the whole file.
    fn read(path: &Path, from: Option<&wal::Resume>) -> Result<Option<Self>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len();
        let mut header = Vec::with_capacity(32);
        (&mut f).take(32).read_to_end(&mut header)?;
        let base = match from {
            Some(r) if header[..] == r.header[..] && len >= r.offset => r.tail_start(),
            _ => 0,
        };
        f.seek(SeekFrom::Start(base))?;
        let mut body = Vec::with_capacity(len.saturating_sub(base) as usize);
        f.read_to_end(&mut body)?;
        Ok(Some(Self { header, body, base }))
    }

    /// What `wal_mark` would have returned for the file as read.
    fn mark(&self) -> WalMark {
        Some((self.base + self.body.len() as u64, self.header.clone()))
    }
}

/// Read the WAL at `path` and parse what is not stored yet: from the stored resume state when it
/// fits the file, else the whole file with the salt/count cursor. Calls `f` with the parse (or
/// `None` when there is no WAL or no valid header), the number of leading commits in it that are
/// already stored, and the WAL mark.
fn with_unseen<T>(
    path: &Path,
    cursor: &Cursor,
    f: impl FnOnce(Option<(&wal::Wal<'_>, usize)>, WalMark) -> Result<T>,
) -> Result<T> {
    let resume = cursor.resume();
    let Some(mut view) = WalView::read(path, resume.as_ref())? else {
        return f(None, None);
    };
    if let Some(r) = resume.as_ref().filter(|_| view.base > 0) {
        if let Some(w) = wal::parse_from(&view.header, &view.body, r) {
            return f(Some((&w, 0)), view.mark());
        }
        // The state does not fit (rewritten or damaged WAL): fall back to a full parse.
        match WalView::read(path, None)? {
            Some(v) => view = v,
            None => return f(None, None),
        }
    }
    let mark = view.mark();
    match wal::parse(&view.body)? {
        Some(w) => {
            let seen = commits_seen(cursor.salt.as_deref(), cursor.commits.as_deref(), w.salt);
            f(Some((&w, seen)), mark)
        }
        None => f(None, mark),
    }
}

/// True when `e` is SQLite reporting that another connection holds the lock it needed.
pub fn is_busy(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(f, _))
                if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        )
    })
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_timeout(path, DEFAULT_BUSY_TIMEOUT)
    }

    /// Open with a custom busy timeout: how long any statement waits for another process's lock.
    pub fn open_with_timeout(path: &Path, busy: Duration) -> Result<Self> {
        let conn = Connection::open(path)?;
        // Before the schema: even `CREATE TABLE IF NOT EXISTS` needs a shared lock.
        conn.busy_timeout(busy)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open an existing `meta.db` read-write (so a hot rollback journal left by a crashed writer
    /// is rolled back, which `SQLITE_OPEN_READ_ONLY` cannot do) without creating the file and
    /// without running the schema. Fails with a clear error when `meta.db` is absent. For the
    /// read paths (`list`, `status`, `log`, `clone`), which must not write into the folder.
    pub fn open_existing(path: &Path) -> Result<Self> {
        if !path.is_file() {
            anyhow::bail!(
                "{} does not exist (no captured history here)",
                path.display()
            );
        }
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        get_meta(&self.conn, key)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn, key, value)
    }

    pub fn page_size(&self) -> Result<Option<u64>> {
        Ok(self.get("page_size")?.map(|v| v.parse()).transpose()?)
    }

    pub fn commit_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM commits", [], |r| r.get(0))?)
    }

    /// Commits in the WAL at `wal_path` that are not stored yet (0 when there is no WAL or it does
    /// not parse). Read-only and unlocked, so only a hint while writers are running.
    pub fn uncaptured(&self, wal_path: &Path) -> Result<u64> {
        let cursor = Cursor::load(&self.conn)?;
        let r = with_unseen(wal_path, &cursor, |w, _| {
            Ok(w.map_or(0, |(w, seen)| w.commits.len().saturating_sub(seen)))
        });
        // A WAL that cannot be read or parsed counts as nothing uncaptured (a hint only).
        Ok(r.unwrap_or(0) as u64)
    }

    /// Take the history write lock: `BEGIN IMMEDIATE` on `meta.db`. Only one connection holds it
    /// at a time; others wait up to the busy timeout, then fail with SQLITE_BUSY (see [`is_busy`]).
    /// It is an ordinary SQLite file lock, so it vanishes when the holding process dies, and an
    /// unfinished transaction is rolled back from its journal by the next opener.
    pub fn lock(&mut self) -> Result<WriteLock<'_>> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(WriteLock {
            conn: &self.conn,
            done: false,
        })
    }

    /// Copy any commits in the live WAL we have not stored yet. Idempotent: re-reading the
    /// same WAL adds nothing. Returns the number of commits added. Takes the write lock for the
    /// duration unless this connection is already inside a transaction.
    pub fn capture(&mut self, wal_path: &Path, label: &str) -> Result<usize> {
        if !self.conn.is_autocommit() {
            return Ok(capture_in_savepoint(&self.conn, wal_path, label)?.added);
        }
        let lock = self.lock()?;
        let c = lock.capture(wal_path, label)?;
        lock.commit()?;
        Ok(c.added)
    }
}

/// The result of one capture.
#[derive(Debug)]
pub struct Captured {
    /// Commits added to history.
    pub added: usize,
    /// The WAL as this capture read it.
    pub mark: WalMark,
}

/// The history write lock (an open `BEGIN IMMEDIATE` on `meta.db`). Dropping it without
/// [`WriteLock::commit`] rolls back everything captured under it.
pub struct WriteLock<'a> {
    conn: &'a Connection,
    done: bool,
}

impl WriteLock<'_> {
    /// Like [`Store::capture`], but inside this lock. The WAL and the cursor are read only now,
    /// with the lock held, so no other walflip process can add or capture commits in between. A
    /// failed capture is rolled back to a savepoint: history and the cursor stay exactly as before
    /// (INV-26) and the lock stays held.
    pub fn capture(&self, wal_path: &Path, label: &str) -> Result<Captured> {
        capture_in_savepoint(self.conn, wal_path, label)
    }

    /// Make everything captured under the lock durable and release it.
    pub fn commit(mut self) -> Result<()> {
        self.done = true;
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
}

impl Drop for WriteLock<'_> {
    fn drop(&mut self) {
        if !self.done && !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}

fn capture_in_savepoint(conn: &Connection, wal_path: &Path, label: &str) -> Result<Captured> {
    conn.execute_batch("SAVEPOINT walflip_capture")?;
    match capture_rows(conn, wal_path, label) {
        Ok(c) => {
            conn.execute_batch("RELEASE walflip_capture")?;
            Ok(c)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO walflip_capture; RELEASE walflip_capture");
            Err(e)
        }
    }
}

/// The body of a capture; the caller provides the transaction.
fn capture_rows(conn: &Connection, wal_path: &Path, label: &str) -> Result<Captured> {
    let cursor = Cursor::load(conn)?;
    with_unseen(wal_path, &cursor, |parsed, mark| {
        let Some((wal, seen)) = parsed else {
            return Ok(Captured { added: 0, mark });
        };
        let added = store_commits(conn, &cursor, wal, seen, label)?;
        Ok(Captured { added, mark })
    })
}

/// Store `wal.commits[seen..]` and advance the cursor to `wal.end`. Returns the number added.
fn store_commits(
    conn: &Connection,
    cursor: &Cursor,
    wal: &wal::Wal<'_>,
    seen: usize,
    label: &str,
) -> Result<usize> {
    // Clone writes pages at (pgno-1)*page_size using the size saved at create; a different
    // size in the WAL would make replayed frames land at the wrong offsets.
    let stored_ps: Option<u64> = get_meta(conn, "page_size")?
        .map(|v| v.parse())
        .transpose()?;
    if let Some(stored) = stored_ps {
        if stored != u64::from(wal.page_size) {
            bail!(
                "WAL page size {} does not match base snapshot page size {stored}",
                wal.page_size
            );
        }
    }

    let salt = format!("{}:{}", wal.salt.0, wal.salt.1);
    let total = wal.end.commits;
    let added = wal.commits.len().saturating_sub(seen);
    // Nothing new and the cursor already says so: write nothing, so meta.db stays byte-identical.
    if added == 0 && cursor.is(&salt, total, &wal.end) {
        return Ok(0);
    }
    // History idx is contiguous from 1, so max+1 is the next one (and, unlike count(*), costs
    // O(log n) instead of a scan of the table).
    let first_idx = conn.query_row("SELECT coalesce(max(idx), 0) FROM commits", [], |r| {
        r.get::<_, u64>(0)
    })? + 1;
    for (idx, commit) in (first_idx..).zip(wal.commits.iter().skip(seen)) {
        conn.execute(
            "INSERT INTO commits(idx,label,db_pages) VALUES(?1,?2,?3)",
            params![idx, label, commit.db_pages],
        )?;
        for (seq, f) in commit.frames.iter().enumerate() {
            conn.execute(
                "INSERT INTO frames(commit_idx,seq,pgno,page) VALUES(?1,?2,?3,?4)",
                params![idx, seq, f.pgno, f.page],
            )?;
        }
    }
    set_meta(conn, "cursor_salt", &salt)?;
    set_meta(conn, "cursor_commits", &total.to_string())?;
    set_meta(conn, CURSOR_RESUME, &wal.end.to_meta())?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn cursor_cases() {
        assert_eq!(commits_seen(None, None, (1, 2)), 0);
        assert_eq!(commits_seen(Some("1:2"), Some("5"), (1, 2)), 5);
        assert_eq!(
            commits_seen(Some("1:2"), Some("5"), (1, 3)),
            0,
            "new salt restarts the count"
        );
        assert_eq!(commits_seen(Some("1:2"), Some("junk"), (1, 2)), 0);
        assert_eq!(commits_seen(Some("1:2"), None, (1, 2)), 0);
    }

    use crate::wal::encode::{encode, Commit as EC};
    use tempfile::TempDir;

    const PS: usize = 512;

    fn page(b: u8) -> Vec<u8> {
        vec![b; PS]
    }

    fn setup() -> (TempDir, Store, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("meta.db")).unwrap();
        let wal = dir.path().join("db-wal");
        (dir, store, wal)
    }

    fn write_wal(path: &Path, salt: (u32, u32), ps: u32, commits: &[EC]) {
        fs::write(path, encode(false, salt, ps, commits)).unwrap();
    }

    /// (commits rows, frames rows) as stored, ordered.
    type Rows = (Vec<(i64, i64)>, Vec<(i64, i64, i64, Vec<u8>)>);

    fn rows(s: &Store) -> Rows {
        let c = s
            .conn()
            .prepare("SELECT idx, db_pages FROM commits ORDER BY idx")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let f = s
            .conn()
            .prepare("SELECT commit_idx, seq, pgno, page FROM frames ORDER BY commit_idx, seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (c, f)
    }

    /// What `rows` should return for commits stored in order.
    fn expected(commits: &[EC]) -> Rows {
        let mut c = Vec::new();
        let mut f = Vec::new();
        for (i, (frames, db_pages)) in commits.iter().enumerate() {
            let idx = i as i64 + 1;
            c.push((idx, i64::from(*db_pages)));
            for (seq, (pgno, p)) in frames.iter().enumerate() {
                f.push((idx, seq as i64, i64::from(*pgno), p.clone()));
            }
        }
        (c, f)
    }

    fn sample(n: usize) -> Vec<EC> {
        (0..n)
            .map(|i| {
                let b = i as u8 + 1;
                (
                    vec![(1, page(b)), (2 + i as u32, page(b.wrapping_add(100)))],
                    2 + i as u32,
                )
            })
            .collect()
    }

    #[test]
    fn capture_is_idempotent() {
        let (_d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (7, 8), PS as u32, &c);
        assert_eq!(s.capture(&wal, "a").unwrap(), 3);
        let before = rows(&s);
        assert_eq!(s.capture(&wal, "a").unwrap(), 0);
        assert_eq!(s.capture(&wal, "a").unwrap(), 0);
        assert_eq!(rows(&s), before);
        assert_eq!(before, expected(&c));
    }

    #[test]
    fn inv19_growth_adds_only_new_commits() {
        let (_d, mut s, wal) = setup();
        let c = sample(5);
        write_wal(&wal, (1, 2), PS as u32, &c[..2]);
        assert_eq!(s.capture(&wal, "x").unwrap(), 2);
        write_wal(&wal, (1, 2), PS as u32, &c);
        assert_eq!(s.capture(&wal, "y").unwrap(), 3);
        assert_eq!(rows(&s), expected(&c));
        let labels: Vec<String> = s
            .conn()
            .prepare("SELECT label FROM commits ORDER BY idx")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(labels, ["x", "x", "y", "y", "y"]);
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("5"));
    }

    #[test]
    fn inv20_new_salt_restarts_cursor_keeps_history() {
        let (_d, mut s, wal) = setup();
        let g1 = sample(4);
        write_wal(&wal, (1, 2), PS as u32, &g1);
        assert_eq!(s.capture(&wal, "g1").unwrap(), 4);
        // New generation: fewer frames than before.
        let g2: Vec<EC> = vec![(vec![(1, page(0xAA))], 1), (vec![(3, page(0xBB))], 3)];
        write_wal(&wal, (9, 10), PS as u32, &g2);
        assert_eq!(s.capture(&wal, "g2").unwrap(), 2);
        assert_eq!(s.capture(&wal, "g2").unwrap(), 0);
        let all: Vec<EC> = g1.iter().chain(g2.iter()).cloned().collect();
        assert_eq!(rows(&s), expected(&all));
        assert_eq!(s.get("cursor_salt").unwrap().as_deref(), Some("9:10"));
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn inv21_page_size_mismatch_rejected_without_side_effects() {
        let (_d, mut s, wal) = setup();
        s.set("page_size", "4096").unwrap();
        s.set("cursor_salt", "5:6").unwrap();
        s.set("cursor_commits", "1").unwrap();
        write_wal(&wal, (5, 6), 512, &sample(2));
        assert!(s.capture(&wal, "bad").is_err());
        assert_eq!(rows(&s), (vec![], vec![]));
        assert_eq!(s.commit_count().unwrap(), 0);
        assert_eq!(s.get("cursor_salt").unwrap().as_deref(), Some("5:6"));
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn inv26_failed_capture_leaves_store_unchanged() {
        let (_d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (1, 2), PS as u32, &c[..1]);
        assert_eq!(s.capture(&wal, "a").unwrap(), 1);
        let before = rows(&s);
        let meta_before = (
            s.get("cursor_salt").unwrap(),
            s.get("cursor_commits").unwrap(),
        );
        // Make the final cursor write fail, after commits/frames were inserted in the tx.
        s.conn()
            .execute_batch(
                "CREATE TRIGGER boom BEFORE UPDATE ON meta WHEN NEW.key='cursor_commits' \
                 BEGIN SELECT RAISE(ABORT,'boom'); END;
                 CREATE TRIGGER boom2 BEFORE INSERT ON meta WHEN NEW.key='cursor_commits' \
                 BEGIN SELECT RAISE(ABORT,'boom'); END;",
            )
            .unwrap();
        write_wal(&wal, (1, 2), PS as u32, &c);
        assert!(s.capture(&wal, "b").is_err());
        assert_eq!(rows(&s), before);
        assert_eq!(
            (
                s.get("cursor_salt").unwrap(),
                s.get("cursor_commits").unwrap()
            ),
            meta_before
        );
        // Once the fault is gone the same capture succeeds and adds the missing commits.
        s.conn()
            .execute_batch("DROP TRIGGER boom; DROP TRIGGER boom2;")
            .unwrap();
        assert_eq!(s.capture(&wal, "b").unwrap(), 2);
        assert_eq!(rows(&s), expected(&c));
    }

    /// INV-26 under the write lock: a failed capture rolls back to its savepoint, so commits,
    /// frames and cursor are as before, the lock is still held, and a later capture in the same
    /// lock (and its commit) works.
    #[test]
    fn inv26_failed_capture_inside_lock_rolls_back_only_itself() {
        let (d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (1, 2), PS as u32, &c[..1]);
        assert_eq!(s.capture(&wal, "a").unwrap(), 1);
        let before = rows(&s);
        let bad = d.path().join("bad-wal");
        write_wal(&bad, (1, 2), 1024, &sample(1));
        s.set("page_size", &PS.to_string()).unwrap();
        {
            let lock = s.lock().unwrap();
            assert!(lock.capture(&bad, "bad").is_err());
            write_wal(&wal, (1, 2), PS as u32, &c);
            assert_eq!(lock.capture(&wal, "b").unwrap().added, 2);
            lock.commit().unwrap();
        }
        assert_eq!(rows(&s), expected(&c));
        assert!(s.conn().is_autocommit());
        // Dropping a lock without commit rolls back what it captured.
        write_wal(&wal, (1, 2), PS as u32, &[c.clone(), sample(1)].concat());
        {
            let lock = s.lock().unwrap();
            assert_eq!(lock.capture(&wal, "dropped").unwrap().added, 1);
        }
        assert_eq!(rows(&s), expected(&c));
        assert_ne!(before, rows(&s));
    }

    /// INV-26 with a lock held elsewhere: capture waits, then fails with SQLITE_BUSY
    /// (`is_busy`) and the store is unchanged.
    #[test]
    fn inv26_capture_fails_busy_without_side_effects() {
        let (d, _s, wal) = setup();
        let mut s =
            Store::open_with_timeout(&d.path().join("meta.db"), Duration::from_millis(50)).unwrap();
        write_wal(&wal, (1, 2), PS as u32, &sample(2));
        let other = Connection::open(d.path().join("meta.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let err = s.capture(&wal, "x").unwrap_err();
        assert!(is_busy(&err), "{err:#}");
        other.execute_batch("ROLLBACK").unwrap();
        assert_eq!(rows(&s), (vec![], vec![]));
        assert_eq!(s.get("cursor_commits").unwrap(), None);
        assert_eq!(s.capture(&wal, "x").unwrap(), 2);
    }

    /// An idempotent capture writes nothing, so a no-op leaves meta.db byte-identical (INV-37).
    #[test]
    fn capture_with_nothing_new_does_not_touch_the_file() {
        let (d, mut s, wal) = setup();
        write_wal(&wal, (1, 2), PS as u32, &sample(2));
        s.capture(&wal, "x").unwrap();
        let meta = d.path().join("meta.db");
        let before = fs::read(&meta).unwrap();
        assert_eq!(s.capture(&wal, "x").unwrap(), 0);
        assert_eq!(fs::read(&meta).unwrap(), before);
        assert_eq!(s.uncaptured(&wal).unwrap(), 0);
        write_wal(&wal, (1, 2), PS as u32, &sample(3));
        assert_eq!(s.uncaptured(&wal).unwrap(), 1);
    }

    // ---- T24: incremental capture (INV-51) ----

    fn resume_of(s: &Store) -> wal::Resume {
        wal::Resume::from_meta(&s.get(CURSOR_RESUME).unwrap().expect("resume state stored"))
            .unwrap()
    }

    fn cursor_of(s: &Store) -> (Option<String>, Option<String>) {
        (
            s.get("cursor_salt").unwrap(),
            s.get("cursor_commits").unwrap(),
        )
    }

    /// After a capture only the tail is read: damage inside the already captured frames (which a
    /// full parse would stop at) is not seen, and appended commits are still added.
    #[test]
    fn capture_reads_only_the_tail_after_the_stored_state() {
        let (_d, mut s, wal) = setup();
        let c = sample(4);
        let full = encode(false, (1, 2), PS as u32, &c);
        let two = encode(false, (1, 2), PS as u32, &c[..2]);
        fs::write(&wal, &two).unwrap();
        assert_eq!(s.capture(&wal, "a").unwrap(), 2);
        let r = resume_of(&s);
        assert_eq!((r.offset, r.commits), (two.len() as u64, 2));

        let mut damaged = full.clone();
        damaged[32 + 24 + 5] ^= 0xff; // first frame's page
        assert!(wal::parse(&damaged).unwrap().unwrap().commits.is_empty());
        fs::write(&wal, &damaged).unwrap();
        assert_eq!(s.uncaptured(&wal).unwrap(), 2);
        assert_eq!(s.capture(&wal, "b").unwrap(), 2);
        assert_eq!(rows(&s), expected(&c));
        assert_eq!(resume_of(&s), wal::parse(&full).unwrap().unwrap().end);
        assert_eq!(cursor_of(&s), (Some("1:2".into()), Some("4".into())));
    }

    /// A meta.db written before the resume state existed (only `cursor_salt`/`cursor_commits`)
    /// keeps working: the first capture parses in full, then stores the state.
    #[test]
    fn old_cursor_format_is_migrated() {
        let (d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (4, 5), PS as u32, &c);
        // As an older walflip left it: 2 commits captured, no resume state.
        {
            let lock = s.lock().unwrap();
            lock.capture(&wal, "old").unwrap();
            lock.commit().unwrap();
        }
        s.conn()
            .execute_batch(
                "DELETE FROM frames WHERE commit_idx=3; DELETE FROM commits WHERE idx=3;",
            )
            .unwrap();
        s.set("cursor_commits", "2").unwrap();
        s.conn()
            .execute("DELETE FROM meta WHERE key=?1", [CURSOR_RESUME])
            .unwrap();

        assert_eq!(s.uncaptured(&wal).unwrap(), 1);
        assert_eq!(s.capture(&wal, "new").unwrap(), 1);
        assert_eq!(rows(&s), expected(&c));
        assert_eq!(resume_of(&s).commits, 3);
        let meta = d.path().join("meta.db");
        let before = fs::read(&meta).unwrap();
        assert_eq!(s.capture(&wal, "new").unwrap(), 0);
        assert_eq!(
            fs::read(&meta).unwrap(),
            before,
            "migrated cursor: a no-op capture writes nothing"
        );
    }

    /// A resume state that disagrees with the count cursor (an older walflip advanced the count
    /// without it) or that is not readable is ignored; the count cursor decides.
    #[test]
    fn stale_or_damaged_resume_state_falls_back_to_the_count_cursor() {
        for damage in ["stale", "junk", "other-salt"] {
            let (_d, mut s, wal) = setup();
            let c = sample(5);
            write_wal(&wal, (1, 2), PS as u32, &c[..2]);
            assert_eq!(s.capture(&wal, "a").unwrap(), 2);
            write_wal(&wal, (1, 2), PS as u32, &c);
            match damage {
                // An older binary captured commit 3 and moved only the count.
                "stale" => {
                    let lock = s.lock().unwrap();
                    lock.conn
                        .execute(
                            "INSERT INTO commits(idx,label,db_pages) VALUES(3,'old',?1)",
                            [c[2].1],
                        )
                        .unwrap();
                    for (seq, (pgno, p)) in c[2].0.iter().enumerate() {
                        lock.conn
                            .execute(
                                "INSERT INTO frames VALUES(3,?1,?2,?3)",
                                params![seq, pgno, p],
                            )
                            .unwrap();
                    }
                    lock.conn
                        .execute("UPDATE meta SET value='3' WHERE key='cursor_commits'", [])
                        .unwrap();
                    lock.commit().unwrap();
                }
                "junk" => s.set(CURSOR_RESUME, "v1 not a state").unwrap(),
                _ => s.set("cursor_salt", "9:9").unwrap(),
            }
            let want = match damage {
                "stale" => 2,
                "junk" => 3,
                _ => 5,
            };
            assert_eq!(s.uncaptured(&wal).unwrap(), want as u64, "{damage}");
            assert_eq!(s.capture(&wal, "b").unwrap(), want, "{damage}");
            assert_eq!(
                resume_of(&s),
                wal::parse(&encode(false, (1, 2), PS as u32, &c))
                    .unwrap()
                    .unwrap()
                    .end
            );
            if damage != "other-salt" {
                assert_eq!(rows(&s), expected(&c), "{damage}");
            }
        }
    }

    /// Same salt but the file no longer matches the state (shorter than the stored offset, or a
    /// different last commit frame): full parse with the count cursor, as before T24.
    #[test]
    fn rewritten_wal_with_the_same_salt_falls_back_to_a_full_parse() {
        let (_d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (1, 2), PS as u32, &c);
        assert_eq!(s.capture(&wal, "a").unwrap(), 3);
        // Shorter than the stored offset: nothing new, cursor follows the file.
        write_wal(&wal, (1, 2), PS as u32, &c[..1]);
        assert_eq!(s.uncaptured(&wal).unwrap(), 0);
        assert_eq!(s.capture(&wal, "b").unwrap(), 0);
        assert_eq!(cursor_of(&s).1.as_deref(), Some("1"));
        assert_eq!(resume_of(&s).commits, 1);
        // Same length at the state, other content at its last commit frame: the checksum there
        // differs, so the full parse runs and the count (1) decides.
        let other: Vec<EC> = vec![
            c[0].clone(),
            (vec![(7, page(0x77))], 7),
            (vec![(8, page(0x88))], 8),
        ];
        write_wal(&wal, (1, 2), PS as u32, &other);
        assert_eq!(s.capture(&wal, "c").unwrap(), 2);
        assert_eq!(
            resume_of(&s),
            wal::parse(&encode(false, (1, 2), PS as u32, &other))
                .unwrap()
                .unwrap()
                .end
        );
    }

    /// One step of the differential below.
    #[derive(Clone, Debug)]
    enum Op {
        /// Append commits to the current generation.
        Grow(Vec<EC>),
        /// WAL restart: a new salt and new commits written over the head of the file.
        Restart(Vec<EC>),
        /// Bytes of an unfinished frame appended.
        PartialFrame(usize),
        /// Checkpoint TRUNCATE: an empty file (or deleted).
        Empty(bool),
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => prop::collection::vec(commit_strategy(), 0..4).prop_map(Op::Grow),
            2 => prop::collection::vec(commit_strategy(), 0..4).prop_map(Op::Restart),
            1 => (1usize..(24 + PS)).prop_map(Op::PartialFrame),
            1 => any::<bool>().prop_map(Op::Empty),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        /// Incremental capture stores exactly what the pre-T24 algorithm (full parse plus the
        /// salt/count cursor; simulated by deleting the resume state before every capture) stores,
        /// through growth, WAL restarts, partial frames and truncation. `uncaptured` agrees too.
        #[test]
        fn incremental_capture_equals_full_parse_capture(
            ops in prop::collection::vec(op_strategy(), 1..10), be in any::<bool>(),
        ) {
            let (_d, mut inc, wal) = setup();
            let (_d2, mut full, _) = setup();
            let mut salt = (1u32, 1u32);
            let mut gen: Vec<EC> = Vec::new();
            for op in ops {
                let mut bytes = encode(be, salt, PS as u32, &gen);
                match op {
                    Op::Grow(c) => {
                        gen.extend(c);
                        bytes = encode(be, salt, PS as u32, &gen);
                    }
                    Op::Restart(c) => {
                        salt = (salt.0 + 1, salt.1.wrapping_mul(31).wrapping_add(7));
                        gen = c;
                        let new = encode(be, salt, PS as u32, &gen);
                        bytes = fs::read(&wal).unwrap_or_default();
                        if bytes.len() < new.len() {
                            bytes.resize(new.len(), 0);
                        }
                        bytes[..new.len()].copy_from_slice(&new);
                    }
                    Op::PartialFrame(n) => {
                        bytes = fs::read(&wal).unwrap_or(bytes);
                        bytes.extend(std::iter::repeat_n(0xab, n));
                    }
                    Op::Empty(delete) => {
                        gen.clear();
                        salt = (salt.0 + 1, salt.1 ^ 0x5555);
                        if delete {
                            let _ = fs::remove_file(&wal);
                            prop_assert_eq!(inc.capture(&wal, "x").unwrap(), 0);
                            prop_assert_eq!(full.capture(&wal, "x").unwrap(), 0);
                            continue;
                        }
                        bytes.clear();
                    }
                }
                fs::write(&wal, &bytes).unwrap();
                full.conn().execute("DELETE FROM meta WHERE key=?1", [CURSOR_RESUME]).unwrap();
                prop_assert_eq!(inc.uncaptured(&wal).unwrap(), full.uncaptured(&wal).unwrap());
                // Both fail alike on a garbage header (bad magic): compare outcomes, not unwraps.
                let a = inc.capture(&wal, "x").ok();
                full.conn().execute("DELETE FROM meta WHERE key=?1", [CURSOR_RESUME]).unwrap();
                let b = full.capture(&wal, "x").ok();
                prop_assert_eq!(a, b);
                prop_assert_eq!(rows(&inc), rows(&full));
                prop_assert_eq!(cursor_of(&inc), cursor_of(&full));
                prop_assert_eq!(inc.uncaptured(&wal).unwrap(), 0);
            }
        }
    }

    #[test]
    fn frame_order_within_commit_is_preserved() {
        let (_d, mut s, wal) = setup();
        let c: Vec<EC> = vec![(
            vec![
                (5, page(1)),
                (2, page(2)),
                (5, page(3)),
                (1, page(4)),
                (9, page(5)),
            ],
            9,
        )];
        write_wal(&wal, (1, 1), PS as u32, &c);
        s.capture(&wal, "o").unwrap();
        let (_, f) = rows(&s);
        let got: Vec<(i64, Vec<u8>)> = f.into_iter().map(|(_, _, pg, p)| (pg, p)).collect();
        let want: Vec<(i64, Vec<u8>)> = c[0]
            .0
            .iter()
            .map(|(pg, p)| (i64::from(*pg), p.clone()))
            .collect();
        assert_eq!(got, want);
    }

    fn commit_strategy() -> impl Strategy<Value = EC> {
        (
            prop::collection::vec((1u32..8, any::<u8>()), 1..5),
            1u32..16,
        )
            .prop_map(|(fr, n)| (fr.into_iter().map(|(p, b)| (p, vec![b; PS])).collect(), n))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn prefix_captures_equal_encoder_input(
            commits in prop::collection::vec(commit_strategy(), 0..12),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..5),
        ) {
            let (_d, mut s, wal) = setup();
            let mut points: Vec<usize> =
                cuts.iter().map(|i| i.index(commits.len() + 1)).collect();
            points.push(commits.len());
            points.sort_unstable();
            let mut prev = 0;
            for p in points {
                write_wal(&wal, (3, 4), PS as u32, &commits[..p]);
                let added = s.capture(&wal, "p").unwrap();
                prop_assert_eq!(added, p - prev);
                prop_assert_eq!(rows(&s), expected(&commits[..p]));
                prev = p;
            }
        }
    }

    proptest! {
        #[test]
        fn matching_salt_returns_stored_count(s in any::<(u32, u32)>(), n in any::<u32>()) {
            let stored = format!("{}:{}", s.0, s.1);
            prop_assert_eq!(commits_seen(Some(&stored), Some(&n.to_string()), s), n as usize);
        }

        #[test]
        fn different_salt_always_restarts(a in any::<(u32, u32)>(), b in any::<(u32, u32)>(), n in any::<u32>()) {
            prop_assume!(a != b);
            let stored = format!("{}:{}", a.0, a.1);
            prop_assert_eq!(commits_seen(Some(&stored), Some(&n.to_string()), b), 0);
        }
    }
}
