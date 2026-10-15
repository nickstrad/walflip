//! Independent oracle for table contents: a shadow in-memory SQLite database fed the same
//! statements as the managed db, snapshotted after every statement that produced a commit.
//!
//! `snapshots[k]` is the expected content of `clone -n k`; `snapshots[0]` is schema + seed rows.
//! A snapshot is `None` when the model cannot know the state at that commit; later steps
//! (e.g. external writers) may push `None`, and `assert_invariants` then skips INV-30 for it.

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// Quote an SQL identifier (`"a""b"` style), so any table name SQLite accepts round-trips.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Render one SQLite value as a type-tagged string, so `1`, `1.0`, `'1'` and `x'01'` differ.
fn render(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => "null".into(),
        ValueRef::Integer(i) => format!("i:{i}"),
        ValueRef::Real(r) => format!("r:{r:?}"),
        ValueRef::Text(t) => format!("t:{}", String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => {
            let mut s = String::with_capacity(2 + b.len() * 2);
            s.push_str("b:");
            for byte in b {
                let _ = write!(s, "{byte:02x}");
            }
            s
        }
    }
}

/// All rows of `table`, every column rendered, sorted (deterministic for rowid and
/// `WITHOUT ROWID` tables alike).
pub fn table_rows(conn: &Connection, table: &str) -> Vec<Vec<String>> {
    let sql = format!("SELECT * FROM {}", quote_ident(table));
    let mut st = conn.prepare(&sql).unwrap_or_else(|e| panic!("dump {table}: {e}"));
    let ncol = st.column_count();
    let mut rows: Vec<Vec<String>> = st
        .query_map([], |r| (0..ncol).map(|i| r.get_ref(i).map(render)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows.sort();
    rows
}

/// Contents of every user table, plus the schema itself under the key `sqlite_master`
/// (type, name, tbl_name, sql), so DDL is compared too.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TableDump(pub BTreeMap<String, Vec<Vec<String>>>);

impl TableDump {
    pub const SCHEMA_KEY: &'static str = "sqlite_master";

    pub fn of_conn(conn: &Connection) -> Self {
        let mut map = BTreeMap::new();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for t in names {
            let rows = table_rows(conn, &t);
            map.insert(t, rows);
        }
        let schema = {
            let mut st = conn
                .prepare(
                    "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master
                     WHERE name NOT LIKE 'sqlite_%'",
                )
                .unwrap();
            let mut rows: Vec<Vec<String>> = st
                .query_map([], |r| (0..4).map(|i| r.get::<_, String>(i)).collect())
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows.sort();
            rows
        };
        map.insert(Self::SCHEMA_KEY.into(), schema);
        Self(map)
    }

    /// Dump a db file without creating `-wal`/`-shm` beside it. The file must not have a live WAL.
    pub fn of_file(path: &Path) -> Self {
        Self::of_conn(&super::open_ro(path))
    }

    /// Human-readable first difference, for assertion messages.
    pub fn diff(&self, other: &Self) -> String {
        let keys: std::collections::BTreeSet<&String> = self.0.keys().chain(other.0.keys()).collect();
        for k in keys {
            match (self.0.get(k), other.0.get(k)) {
                (Some(a), Some(b)) if a == b => continue,
                (a, b) => return format!("table {k:?}: got {a:?}, expected {b:?}"),
            }
        }
        "no difference".into()
    }
}

pub struct Model {
    pub shadow: Connection,
    pub snapshots: Vec<Option<TableDump>>,
    /// Sum of commits the system under test reported (`captured N`).
    pub exec_sum: u64,
}

impl Model {
    /// Shadow db initialised with the same schema text the managed db was created with.
    pub fn new(schema: Option<&str>) -> Self {
        let shadow = Connection::open_in_memory().unwrap();
        if let Some(sql) = schema {
            shadow.execute_batch(sql).expect("model: schema failed in the shadow db");
        }
        let snap = TableDump::of_conn(&shadow);
        Self { shadow, snapshots: vec![Some(snap)], exec_sum: 0 }
    }

    pub fn current(&self) -> TableDump {
        TableDump::of_conn(&self.shadow)
    }

    /// Commits the model expects to exist (`snapshots.len() - 1`).
    pub fn commits(&self) -> u64 {
        self.snapshots.len() as u64 - 1
    }

    fn run(&self, stmt: &str) {
        let tx = self.shadow.unchecked_transaction().unwrap();
        tx.execute_batch(stmt)
            .unwrap_or_else(|e| panic!("model: statement succeeded on walflip but failed in shadow: {stmt}: {e}"));
        tx.commit().unwrap();
    }

    /// A statement that walflip ran successfully and for which it reported `captured` commits.
    /// One statement must produce at most one commit (INV-38).
    pub fn exec(&mut self, stmt: &str, captured: u64) {
        let before = self.current();
        self.run(stmt);
        let after = self.current();
        assert!(captured <= 1, "INV-38: one statement produced {captured} commits: {stmt}");
        if captured == 0 {
            assert_eq!(
                before, after,
                "INV-38: statement changed table contents but walflip captured 0 commits: {stmt}"
            );
        }
        self.exec_sum += captured;
        if captured == 1 {
            self.snapshots.push(Some(after));
        }
    }

    /// A batch of statements walflip ran successfully, reporting `captured` commits in total.
    /// Which statements committed is inferred from whether the shadow's contents changed, so
    /// only use this for batches with no content-preserving writes (e.g. `UPDATE t SET v=v`).
    pub fn exec_batch(&mut self, stmts: &[String], captured: u64) {
        let mut inferred = 0;
        for s in stmts {
            let before = self.current();
            self.run(s);
            let after = self.current();
            if before != after {
                inferred += 1;
                self.snapshots.push(Some(after));
            }
        }
        self.exec_sum += captured;
        assert_eq!(
            inferred, captured,
            "INV-38: batch of {} statements: model saw {inferred} content changes, walflip captured {captured}",
            stmts.len()
        );
    }

    /// A statement walflip rejected: nothing is applied to the model (INV-37).
    pub fn failed(&mut self, _stmt: &str) {}
}
