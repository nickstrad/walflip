//! T21: sandboxed, structure-aware fuzz target for the statement -> commit -> capture path.
//!
//! The input is a small structured program, never raw schema text: a schema of 1-3 tables from a
//! fixed grammar (names from a small pool, including quoted names), then a list of `exec` calls,
//! each a few statements built from DML/DDL/VACUUM/pragma pieces, statements that must be rejected
//! (ATTACH, transaction control, `PRAGMA journal_mode`, ...), statements that must fail, and
//! raw-ish token soup so the authorizer denials are exercised on odd text.
//!
//! Each input runs in its own directory under a per-process sandbox root, through the real
//! `db::create` and `db::exec_with` (library, no process spawn). After EVERY exec call, success or
//! failure:
//! - the WAL is gone (exec's connection closed and checkpointed), `live.db` passes
//!   `integrity_check`, and `status().uncaptured == 0`;
//! - history is consistent (INV-22: contiguous idx from 1, no orphan or empty commits, contiguous
//!   seq, page-sized frames);
//! - accounting: no `(recovered)` commits (there is no foreign writer), commits added == the
//!   report's (or `ExecFailed`'s) count, at most one per statement run;
//! - `clone(total)` is byte-identical to `live.db`, and clones at a few small k pass
//!   `integrity_check` (every k at the end of the program);
//! - a call that captured nothing left `live.db` byte-identical; a call containing a statement
//!   that must be rejected or must fail returns an error;
//! - nothing appears outside the case directory: two ATTACH canary paths (one absolute outside the
//!   sandbox, one relative to the cwd) never exist, the cwd listing is unchanged, and the sandbox
//!   root holds only the current case and the SQLite temp dir.
//!
//! The sandbox (installed once via `db::sandbox`, `testing` feature only) adds to walflip's own
//! denials: dangerous pragmas and functions, SQLite length/depth limits, `SQLITE_LIMIT_ATTACHED=0`,
//! `DBCONFIG_DEFENSIVE`, and a deterministic progress-handler op budget per `create`/`exec` call.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rusqlite::config::DbConfig;
use rusqlite::hooks::AuthAction;
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use walflip::db::{self, sandbox, Db, ExecFailed};

// ---------- sandbox ----------

const DENY_PRAGMAS: &[&str] = &[
    "temp_store_directory",
    "data_store_directory",
    "mmap_size",
    "cache_size",
    "max_page_count",
    "locking_mode",
    "writable_schema",
    "schema_version",
    "hard_heap_limit",
    "soft_heap_limit",
];
const DENY_FUNCS: &[&str] = &["fts3_tokenizer", "load_extension"];

/// Progress-handler calls (every `PROGRESS_OPS` VM ops) allowed per create/exec call.
const PROGRESS_OPS: i32 = 1000;
const PROGRESS_BUDGET: u32 = 1000;
static PROGRESS_USED: AtomicU32 = AtomicU32::new(0);

fn fuzz_deny(a: &AuthAction<'_>) -> Option<String> {
    match a {
        AuthAction::Pragma { pragma_name, .. } if DENY_PRAGMAS.iter().any(|p| pragma_name.eq_ignore_ascii_case(p)) => {
            Some(format!("PRAGMA {pragma_name} is not allowed (fuzz sandbox)"))
        }
        AuthAction::Function { function_name } if DENY_FUNCS.iter().any(|f| function_name.eq_ignore_ascii_case(f)) => {
            Some(format!("{function_name}() is not allowed (fuzz sandbox)"))
        }
        _ => None,
    }
}

fn fuzz_setup(c: &Connection) -> rusqlite::Result<()> {
    c.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1 << 20);
    c.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 << 10);
    c.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 100);
    c.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 10);
    c.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 10);
    c.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    c.progress_handler(PROGRESS_OPS, Some(|| PROGRESS_USED.fetch_add(1, Ordering::Relaxed) >= PROGRESS_BUDGET));
    Ok(())
}

struct Env {
    root: PathBuf,
    sqlite_tmp: PathBuf,
    /// Absolute ATTACH target outside the sandbox root.
    canary_abs: PathBuf,
    /// Relative ATTACH target (would land in the cwd).
    canary_rel: &'static str,
    cwd: PathBuf,
    cwd_listing: BTreeSet<String>,
}

fn listing(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

fn env() -> &'static Env {
    static E: OnceLock<Env> = OnceLock::new();
    E.get_or_init(|| {
        let pid = std::process::id();
        let tmp = std::env::temp_dir();
        let root = tmp.join(format!("walflip-fuzz-exec-{pid}"));
        let _ = fs::remove_dir_all(&root);
        let sqlite_tmp = root.join("sqlite-tmp");
        fs::create_dir_all(&sqlite_tmp).unwrap();
        // SQLite reads this once, before its first temp file: keep its temp files in the sandbox.
        std::env::set_var("SQLITE_TMPDIR", &sqlite_tmp);
        assert!(sandbox::install(sandbox::Sandbox { deny: fuzz_deny, setup: fuzz_setup }));
        let cwd = std::env::current_dir().unwrap();
        Env {
            canary_abs: tmp.join(format!("walflip-fuzz-canary-{pid}.db")),
            canary_rel: "walflip-fuzz-canary.db",
            cwd_listing: listing(&cwd),
            cwd,
            sqlite_tmp,
            root,
        }
    })
}

// ---------- input grammar ----------

const TABLES: &[&str] = &["t1", "\"Tab 2\"", "\"q\"\"t\"", "t4"];
const COLS: &[(&str, &str)] = &[("a", "INTEGER"), ("b", "BLOB"), ("\"c d\"", "TEXT"), ("\"x\"\"y\"", "")];
const NEW_COLS: &[&str] = &["e", "\"f g\"", "a", "b"];
const SIZES: &[usize] = &[0, 1, 100, 1000, 4000, 70_000];
/// Above SQLITE_LIMIT_LENGTH: the statement must fail.
const HUGE: usize = 2_000_000;

fn table(i: u8) -> &'static str {
    TABLES[i as usize % TABLES.len()]
}
fn col(i: u8) -> &'static str {
    COLS[i as usize % COLS.len()].0
}
fn size(i: u8) -> usize {
    if i >= 252 {
        HUGE
    } else {
        SIZES[i as usize % SIZES.len()]
    }
}

#[derive(Arbitrary, Debug)]
struct TableDef {
    name: u8,
    /// Bitmask over `COLS` (at least one column is always kept).
    cols: u8,
    without_rowid: bool,
}

#[derive(Arbitrary, Debug)]
struct Schema {
    tables: Vec<TableDef>,
    page_size: u8,
    auto_vacuum: u8,
    seed_rows: u8,
    index: bool,
}

#[derive(Arbitrary, Debug)]
enum Val {
    Null,
    Int(i32),
    Text(u8),
    Zero(u8),
    Hex(u32),
}

#[derive(Arbitrary, Debug)]
enum Filter {
    All,
    IdEq(i8),
    IdLt(i8),
    IsNull(u8),
}

#[derive(Arbitrary, Debug)]
enum Op {
    Insert { t: u8, rows: u8, id: i8, or_replace: bool, vals: [Val; 2], cols: (u8, u8) },
    Update { t: u8, col: u8, val: Val, filter: Filter },
    Delete { t: u8, filter: Filter },
    Select { t: u8 },
    AddColumn { t: u8, c: u8, default: bool },
    DropColumn { t: u8, c: u8 },
    RenameColumn { t: u8, from: u8, to: u8 },
    RenameTable { t: u8, to: u8 },
    CreateTable { t: u8, cols: u8, without_rowid: bool },
    DropTable { t: u8 },
    CreateIndex { t: u8, c: u8, unique: bool, name: u8 },
    DropIndex { name: u8 },
    CreateTrigger { t: u8, other: u8, name: u8, abort: bool },
    DropTrigger { name: u8 },
    CreateView { t: u8, name: u8 },
    Savepoint { inner: Box<Op>, rollback: bool },
    Vacuum(u8),
    Pragma(u8, u8),
    Reject(u8),
    Fail(u8),
    Tokens(Vec<Tok>),
}

#[derive(Arbitrary, Debug)]
enum Tok {
    Kw(u8),
    Table(u8),
    Col(u8),
    Int(i8),
    Str(u8),
    Punct(u8),
    Func(u8),
}

const KWS: &[&str] = &[
    "SELECT", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "FROM", "WHERE", "CREATE", "TABLE",
    "INDEX", "ON", "DROP", "ALTER", "ADD", "COLUMN", "RENAME", "TO", "ATTACH", "DETACH", "DATABASE", "AS",
    "BEGIN", "COMMIT", "ROLLBACK", "END", "SAVEPOINT", "RELEASE", "PRAGMA", "journal_mode", "wal_checkpoint",
    "VACUUM", "WITH", "RECURSIVE", "UNION", "ALL", "NOT", "NULL", "PRIMARY", "KEY", "UNIQUE", "TRIGGER",
    "AFTER", "FOR", "EACH", "ROW", "DEFAULT", "OR", "REPLACE", "IGNORE", "LIMIT", "temp_store_directory",
    "writable_schema", "schema_version", "mmap_size", "cache_size", "locking_mode", "IMMEDIATE",
    "EXCLUSIVE", "TEMP", "VIEW", "WAL", "DELETE", "main", "temp", "IF", "EXISTS", "WITHOUT", "ROWID",
];
const PUNCT: &[&str] = &["(", ")", ",", ";", "=", "*", ".", "+", "||"];
const FUNCS: &[&str] = &["zeroblob", "load_extension", "fts3_tokenizer", "printf", "length", "count", "abs", "hex"];

struct Ctx<'a> {
    env: &'a Env,
    /// Pool indices of the schema's tables: most table references pick one of these, so most
    /// statements reach SQLite's write path instead of failing on a missing table.
    known: Vec<u8>,
}

impl Ctx<'_> {
    fn t(&self, i: u8) -> &'static str {
        if i < 200 && !self.known.is_empty() {
            table(self.known[i as usize % self.known.len()])
        } else {
            table(i)
        }
    }

    fn val(&self, v: &Val) -> String {
        match v {
            Val::Null => "NULL".into(),
            Val::Int(i) => i.to_string(),
            Val::Text(s) => format!("printf('%.*c', {}, 'x')", size(*s)),
            Val::Zero(s) => format!("zeroblob({})", size(*s)),
            Val::Hex(h) => format!("x'{h:08x}{:08x}'", h.rotate_left(7)),
        }
    }

    fn filter(&self, f: &Filter) -> String {
        match f {
            Filter::All => String::new(),
            Filter::IdEq(i) => format!(" WHERE id = {i}"),
            Filter::IdLt(i) => format!(" WHERE id < {i}"),
            Filter::IsNull(c) => format!(" WHERE {} IS NULL", col(*c)),
        }
    }

    fn create_table(&self, t: u8, cols: u8, without_rowid: bool) -> String {
        let mask = if cols & 0xf == 0 { 1 } else { cols & 0xf };
        let mut defs = vec!["id INTEGER PRIMARY KEY".to_string()];
        for (i, (n, ty)) in COLS.iter().enumerate() {
            if mask & (1 << i) != 0 {
                defs.push(format!("{n} {ty}").trim_end().to_string());
            }
        }
        let wr = if without_rowid { " WITHOUT ROWID" } else { "" };
        format!("CREATE TABLE IF NOT EXISTS {}({}){wr}", table(t), defs.join(", "))
    }

    /// The SQL for `op` and whether it must make its statement fail.
    fn op(&self, op: &Op) -> (String, bool) {
        let ok = |s: String| (s, false);
        match op {
            Op::Insert { t, rows, id, or_replace, vals, cols } => {
                let (c1, c2) = (col(cols.0), col(cols.1));
                let or = if *or_replace { "OR REPLACE " } else { "" };
                let rows: Vec<String> = (0..(rows % 4 + 1) as i32)
                    .map(|r| format!("({}, {}, {})", *id as i32 * 4 + r, self.val(&vals[0]), self.val(&vals[1])))
                    .collect();
                // zeroblob() above SQLITE_LIMIT_LENGTH is SQLITE_TOOBIG; printf() silently truncates instead.
                // With a duplicate column (`a, a`) SQLite evaluates only one of the two values.
                let must_fail = c1 != c2 && vals.iter().any(|v| matches!(v, Val::Zero(s) if size(*s) == HUGE));
                (format!("INSERT {or}INTO {}(id, {c1}, {c2}) VALUES {}", self.t(*t), rows.join(", ")), must_fail)
            }
            Op::Update { t, col: c, val, filter } => {
                ok(format!("UPDATE {} SET {} = {}{}", self.t(*t), col(*c), self.val(val), self.filter(filter)))
            }
            Op::Delete { t, filter } => ok(format!("DELETE FROM {}{}", self.t(*t), self.filter(filter))),
            Op::Select { t } => ok(format!("SELECT count(*), sum(length(id)) FROM {}", self.t(*t))),
            Op::AddColumn { t, c, default } => {
                let d = if *default { " DEFAULT 7" } else { "" };
                ok(format!("ALTER TABLE {} ADD COLUMN {}{d}", self.t(*t), NEW_COLS[*c as usize % NEW_COLS.len()]))
            }
            Op::DropColumn { t, c } => ok(format!("ALTER TABLE {} DROP COLUMN {}", self.t(*t), col(*c))),
            Op::RenameColumn { t, from, to } => ok(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                self.t(*t),
                col(*from),
                NEW_COLS[*to as usize % NEW_COLS.len()]
            )),
            Op::RenameTable { t, to } => ok(format!("ALTER TABLE {} RENAME TO {}", self.t(*t), table(*to))),
            Op::CreateTable { t, cols, without_rowid } => ok(self.create_table(*t, *cols, *without_rowid)),
            Op::DropTable { t } => ok(format!("DROP TABLE IF EXISTS {}", self.t(*t))),
            Op::CreateIndex { t, c, unique, name } => {
                let u = if *unique { "UNIQUE " } else { "" };
                ok(format!("CREATE {u}INDEX IF NOT EXISTS \"i {}\" ON {}({})", name % 4, self.t(*t), col(*c)))
            }
            Op::DropIndex { name } => ok(format!("DROP INDEX IF EXISTS \"i {}\"", name % 4)),
            Op::CreateTrigger { t, other, name, abort } => {
                let body = if *abort {
                    "SELECT RAISE(ABORT, 'trigger says no') WHERE new.id % 3 = 0;".to_string()
                } else {
                    format!("INSERT OR REPLACE INTO {}(id) VALUES (new.id + 1000);", self.t(*other))
                };
                ok(format!("CREATE TRIGGER IF NOT EXISTS tr{} AFTER INSERT ON {} BEGIN {body} END", name % 3, self.t(*t)))
            }
            Op::DropTrigger { name } => ok(format!("DROP TRIGGER IF EXISTS tr{}", name % 3)),
            Op::CreateView { t, name } => {
                ok(format!("CREATE VIEW IF NOT EXISTS v{} AS SELECT * FROM {}", name % 2, self.t(*t)))
            }
            Op::Savepoint { inner, rollback } => {
                let (s, fail) = self.op(inner);
                let end = if *rollback { "ROLLBACK TO sp; RELEASE sp" } else { "RELEASE sp" };
                // A failing inner statement aborts the whole batch before the savepoint ends.
                (format!("SAVEPOINT sp; {s}; {end}"), fail)
            }
            Op::Vacuum(k) => ok(["VACUUM", "vacuum;", "VACUUM main", "VACUUM \"main\""][*k as usize % 4].into()),
            Op::Pragma(k, v) => ok(match k % 12 {
                0 => format!("PRAGMA wal_checkpoint({})", ["PASSIVE", "FULL", "RESTART", "TRUNCATE"][*v as usize % 4]),
                1 => format!("PRAGMA incremental_vacuum({})", v % 8),
                2 => format!("PRAGMA user_version = {v}"),
                3 => format!("PRAGMA auto_vacuum = {}", v % 3),
                4 => format!("PRAGMA secure_delete = {}", v % 2),
                5 => format!("PRAGMA foreign_keys = {}", v % 2),
                6 => format!("PRAGMA recursive_triggers = {}", v % 2),
                7 => format!("PRAGMA wal_autocheckpoint = {}", v % 3),
                8 => format!("PRAGMA temp_store = {}", v % 3),
                9 => "PRAGMA integrity_check".into(),
                10 => "PRAGMA optimize".into(),
                _ => format!("PRAGMA page_size = {}", 512 << (v % 4)),
            }),
            Op::Reject(k) => {
                let abs = self.env.canary_abs.display().to_string().replace('\'', "''");
                let s = match k % 14 {
                    0 => format!("ATTACH '{abs}' AS x"),
                    1 => format!("ATTACH DATABASE '{}' AS x", self.env.canary_rel),
                    2 => "DETACH x".into(),
                    3 => "BEGIN".into(),
                    4 => "COMMIT".into(),
                    5 => "ROLLBACK".into(),
                    6 => "END".into(),
                    7 => "PRAGMA journal_mode = DELETE".into(),
                    8 => format!("VACUUM INTO '{abs}'"),
                    9 => format!("PRAGMA {} = 1", DENY_PRAGMAS[*k as usize / 14 % DENY_PRAGMAS.len()]),
                    10 => format!("SELECT load_extension('{abs}')"),
                    11 => "SELECT fts3_tokenizer('simple')".into(),
                    12 => "BEGIN IMMEDIATE".into(),
                    _ => "PRAGMA journal_mode".into(),
                };
                (s, true)
            }
            Op::Fail(k) => {
                let s = match k % 6 {
                    0 => "INSERT INTO nope VALUES (1)".to_string(),
                    1 => format!("INSERT INTO {}(id) VALUES (1), (1)", table(*k / 6)),
                    2 => "SELECT * FROM".into(),
                    3 => "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c".into(),
                    4 => format!("SELECT zeroblob({HUGE})"),
                    _ => "SELECT RAISE(ABORT, 'no')".into(),
                };
                (s, true)
            }
            Op::Tokens(toks) => {
                let abs = self.env.canary_abs.display().to_string().replace('\'', "''");
                let strs = ["'x'".to_string(), format!("'{abs}'"), format!("'{}'", self.env.canary_rel), "''".into()];
                let words: Vec<String> = toks
                    .iter()
                    .take(24)
                    .map(|t| match t {
                        Tok::Kw(i) => KWS[*i as usize % KWS.len()].to_string(),
                        Tok::Table(i) => table(*i).to_string(),
                        Tok::Col(i) => col(*i).to_string(),
                        Tok::Int(i) => i.to_string(),
                        Tok::Str(i) => strs[*i as usize % strs.len()].clone(),
                        Tok::Punct(i) => PUNCT[*i as usize % PUNCT.len()].to_string(),
                        Tok::Func(i) => FUNCS[*i as usize % FUNCS.len()].to_string(),
                    })
                    .collect();
                ok(words.join(" "))
            }
        }
    }
}

#[derive(Arbitrary, Debug)]
struct Stmt {
    parts: Vec<Op>,
}

#[derive(Arbitrary, Debug)]
struct Program {
    schema: Schema,
    calls: Vec<Vec<Stmt>>,
}

impl Ctx<'_> {
    fn schema(&self, s: &Schema) -> String {
        let mut out = Vec::new();
        match s.page_size % 5 {
            0 => {}
            k => out.push(format!("PRAGMA page_size = {}", 256 << k)),
        }
        match s.auto_vacuum % 3 {
            0 => {}
            k => out.push(format!("PRAGMA auto_vacuum = {k}")),
        }
        for t in s.tables.iter().take(3) {
            out.push(self.create_table(t.name, t.cols, t.without_rowid));
        }
        if s.tables.is_empty() {
            out.push(self.create_table(0, 1, false));
        }
        let first = s.tables.first().map_or(0, |t| t.name);
        for i in 0..s.seed_rows % 4 {
            out.push(format!("INSERT INTO {}(id) VALUES ({i})", table(first)));
        }
        if s.index {
            out.push(format!("CREATE INDEX \"i 0\" ON {}(id)", table(first)));
        }
        out.join(";\n") + ";"
    }

    /// Statements of one call and whether the call must fail.
    fn call(&self, stmts: &[Stmt]) -> (Vec<String>, bool) {
        let mut must_fail = false;
        let mut out = Vec::new();
        for s in stmts.iter().take(4) {
            let parts: Vec<(String, bool)> = s.parts.iter().take(3).map(|p| self.op(p)).collect();
            if parts.is_empty() {
                continue;
            }
            must_fail |= parts.iter().any(|p| p.1);
            out.push(parts.into_iter().map(|p| p.0).collect::<Vec<_>>().join("; "));
        }
        (out, must_fail)
    }
}

// ---------- oracle ----------

fn meta_i64(m: &Connection, sql: &str) -> i64 {
    m.query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn open_meta(d: &Db) -> Connection {
    Connection::open_with_flags(&d.meta, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// INV-22 over meta.db (same checks as `tests/common/invariants.rs::check_meta_structure`).
fn check_history(d: &Db) -> u64 {
    let m = open_meta(d);
    let q = |sql: &str| meta_i64(&m, sql);
    let (count, min, max) = (
        q("SELECT count(*) FROM commits"),
        q("SELECT coalesce(min(idx), 1) FROM commits"),
        q("SELECT coalesce(max(idx), 0) FROM commits"),
    );
    assert!(min == 1 && max == count, "INV-22: idx not contiguous 1..={count} (min {min}, max {max})");
    assert_eq!(q("SELECT count(*) FROM frames WHERE commit_idx NOT IN (SELECT idx FROM commits)"), 0, "orphan frames");
    assert_eq!(q("SELECT count(*) FROM commits WHERE idx NOT IN (SELECT commit_idx FROM frames)"), 0, "empty commits");
    assert_eq!(
        q("SELECT count(*) FROM (SELECT commit_idx FROM frames GROUP BY commit_idx
           HAVING min(seq) != 0 OR max(seq) != count(*) - 1)"),
        0,
        "non-contiguous frame seq"
    );
    let ps = q("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'page_size'");
    assert_eq!(q(&format!("SELECT count(*) FROM frames WHERE length(page) != {ps} OR pgno < 1")), 0, "bad frame");
    count as u64
}

fn assert_sandbox(env: &Env, case: &Path) {
    assert!(!env.canary_abs.exists(), "file written outside the sandbox: {}", env.canary_abs.display());
    assert!(!env.cwd.join(env.canary_rel).exists(), "file written in the cwd: {}", env.canary_rel);
    // Only SQLite-looking names count: other tools (a concurrent `cargo test` creating `target/`)
    // may legitimately add entries to the cwd while the fuzzer runs.
    let sqlite_like = |n: &&String| [".db", "-wal", "-shm", "-journal", "canary"].iter().any(|s| n.contains(s));
    let new: Vec<String> = listing(&env.cwd).difference(&env.cwd_listing).filter(sqlite_like).cloned().collect();
    assert!(new.is_empty(), "new SQLite file in the cwd: {new:?}");
    let case_name = case.file_name().unwrap().to_string_lossy().into_owned();
    for name in listing(&env.root) {
        assert!(name == "sqlite-tmp" || name == case_name, "unexpected entry in sandbox root: {name}");
    }
    let tmp = listing(&env.sqlite_tmp);
    assert!(tmp.is_empty(), "SQLite temp files left behind: {tmp:?}");
}

fn clone_bytes(d: &Db, case: &Path, k: u64) -> Vec<u8> {
    let out = case.join(format!("clone-{k}.db"));
    db::clone_at(d, k, &out).unwrap_or_else(|e| panic!("clone -n {k}: {e:#}"));
    let s = db::summarize(&out).unwrap_or_else(|e| panic!("summarize clone {k}: {e:#}"));
    assert_eq!(s.integrity, "ok", "clone -n {k} fails integrity_check");
    let b = fs::read(&out).unwrap();
    fs::remove_file(&out).unwrap();
    b
}

/// Everything that must hold after any exec call. Returns the commit count.
fn check_state(d: &Db, case: &Path, all_clones: bool) -> u64 {
    let wal = d.wal_path();
    assert!(fs::metadata(&wal).map_or(true, |m| m.len() == 0), "WAL left behind after exec");
    let total = check_history(d);
    assert_eq!(db::status(d).unwrap().uncaptured, 0, "uncaptured commits after exec");
    let live = fs::read(&d.live).unwrap();
    let s = db::summarize(&d.live).unwrap();
    assert_eq!(s.integrity, "ok", "live.db fails integrity_check");
    assert!(clone_bytes(d, case, total) == live, "clone -n {total} != live.db");
    let ks: BTreeSet<u64> = if all_clones { (0..total).collect() } else { [0, 1, total.saturating_sub(1)].into() };
    for k in ks.into_iter().filter(|&k| k < total) {
        clone_bytes(d, case, k);
    }
    total
}

/// `EXEC_SQL_TRACE=1` prints every exec call and its outcome (triage, corpus inspection).
fn trace() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var_os("EXEC_SQL_TRACE").is_some())
}

static CASE: AtomicU64 = AtomicU64::new(0);

fn run(p: &Program) {
    let env = env();
    let mut known: Vec<u8> = p.schema.tables.iter().take(3).map(|t| t.name % TABLES.len() as u8).collect();
    if known.is_empty() {
        known.push(0);
    }
    let ctx = Ctx { env, known };
    let case = env.root.join(format!("case-{}", CASE.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&case).unwrap();
    let d = Db::new(&case, "d").unwrap();

    PROGRESS_USED.store(0, Ordering::Relaxed);
    let schema = ctx.schema(&p.schema);
    db::create(&d, Some(&schema)).unwrap_or_else(|e| panic!("create failed for a grammar schema: {e:#}\n{schema}"));
    let mut total = check_state(&d, &case, false);
    assert_eq!(total, 0);

    for call in p.calls.iter().take(8) {
        let (stmts, must_fail) = ctx.call(call);
        if stmts.is_empty() {
            continue;
        }
        let live_before = fs::read(&d.live).unwrap();
        PROGRESS_USED.store(0, Ordering::Relaxed);
        let r = db::exec_with(&d, &stmts, Duration::from_secs(5));
        if trace() {
            let outcome = match &r {
                Ok(rep) => format!("ok {rep:?}"),
                Err(e) => format!("err {e:#}"),
            };
            eprintln!("exec {stmts:?}\n  -> {outcome}");
        }
        let (captured, ran) = match &r {
            Ok(rep) => {
                assert_eq!(rep.recovered, 0, "recovered commits without a foreign writer");
                (rep.captured, stmts.len())
            }
            Err(e) => match e.downcast_ref::<ExecFailed>() {
                Some(f) => {
                    assert_eq!(f.recovered, 0, "recovered commits without a foreign writer: {e:#}");
                    (f.captured, f.statement)
                }
                None => (0, stmts.len()),
            },
        };
        if must_fail {
            assert!(r.is_err(), "call must fail but succeeded: {stmts:?}");
        }
        assert!(captured <= ran, "{captured} commits from {ran} statement(s): {stmts:?}");
        let now = check_state(&d, &case, false);
        assert_eq!(now, total + captured as u64, "history grew by {} but exec reported {captured}: {stmts:?}", now as i64 - total as i64);
        if captured == 0 {
            assert!(fs::read(&d.live).unwrap() == live_before, "live.db changed with no commit captured: {stmts:?} -> {r:?}");
        }
        total = now;
        assert_sandbox(env, &case);
    }
    check_state(&d, &case, true);
    assert_sandbox(env, &case);
    fs::remove_dir_all(&case).unwrap();
}

fuzz_target!(|p: Program| {
    run(&p);
});
