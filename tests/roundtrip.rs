//! Property test against real SQLite: for any sequence of writes, cloning at the commit that
//! followed step k must reproduce exactly the table contents the live db had after step k.

use proptest::prelude::*;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use walflip::db::{self, Db};

#[derive(Debug, Clone)]
enum Op {
    Put(i64, i64),
    Set(i64, i64),
    Del(i64),
}

impl Op {
    fn sql(&self) -> String {
        match self {
            Op::Put(id, v) => format!("INSERT OR REPLACE INTO kv(id,val) VALUES({id},{v})"),
            Op::Set(id, v) => format!("UPDATE kv SET val={v} WHERE id={id}"),
            Op::Del(id) => format!("DELETE FROM kv WHERE id={id}"),
        }
    }
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0i64..24, -1000i64..1000).prop_map(|(i, v)| Op::Put(i, v)),
        (0i64..24, -1000i64..1000).prop_map(|(i, v)| Op::Set(i, v)),
        (0i64..24).prop_map(Op::Del),
    ]
}

fn dump(path: &Path) -> Vec<(i64, i64)> {
    let conn = Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let mut st = conn.prepare("SELECT id, val FROM kv ORDER BY id").unwrap();
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows
}

fn fresh(root: &Path) -> Db {
    let d = Db::new(root, "kv").unwrap();
    db::create(&d, Some("CREATE TABLE kv(id INTEGER PRIMARY KEY, val INTEGER);")).unwrap();
    d
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// One process per statement: every exec starts a fresh WAL generation.
    #[test]
    fn every_commit_is_reproducible(ops in prop::collection::vec(op(), 1..40)) {
        let tmp = tempfile::tempdir().unwrap();
        let d = fresh(tmp.path());

        let mut total = 0u64;
        let mut checkpoints = vec![(0u64, dump(&d.live))];
        for o in &ops {
            let added = db::exec(&d, &[o.sql()]).unwrap() as u64;
            total += added;
            if added > 0 {
                checkpoints.push((total, dump(&d.live)));
            }
        }
        prop_assert_eq!(db::status(&d).unwrap().commits, total);

        for (n, expected) in checkpoints {
            let out = tmp.path().join(format!("at{n}.db"));
            db::clone_at(&d, n, &out).unwrap();
            prop_assert_eq!(db::summarize(&out).unwrap().integrity, "ok");
            prop_assert_eq!(dump(&out), expected, "clone -n {}", n);
        }
    }

    /// All statements through one connection, so the whole history lives in a single WAL.
    #[test]
    fn single_wal_batch_matches_final_state(ops in prop::collection::vec(op(), 1..40)) {
        let tmp = tempfile::tempdir().unwrap();
        let d = fresh(tmp.path());

        let stmts: Vec<String> = ops.iter().map(Op::sql).collect();
        let total = db::exec(&d, &stmts).unwrap() as u64;

        let out = tmp.path().join("final.db");
        db::clone_at(&d, total, &out).unwrap();
        prop_assert_eq!(dump(&out), dump(&d.live));
    }

    /// Re-running with no new statements must not duplicate history.
    #[test]
    fn capture_is_idempotent(ops in prop::collection::vec(op(), 1..15)) {
        let tmp = tempfile::tempdir().unwrap();
        let d = fresh(tmp.path());
        let stmts: Vec<String> = ops.iter().map(Op::sql).collect();
        let total = db::exec(&d, &stmts).unwrap() as u64;
        prop_assert_eq!(db::exec(&d, &[]).unwrap(), 0);
        prop_assert_eq!(db::status(&d).unwrap().commits, total);
    }
}
