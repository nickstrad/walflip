//! S10 from docs/TESTING_PLAN.md section 3.2: interrupted / failed batches.
//!
//! Plan Q2: `exec` rejects statements that control the transaction (B2). A rejected statement
//! commits nothing (INV-37), and after every failure `clone -n total` == `live.db` (INV-39). A
//! stdin batch stops at its first failing line; earlier lines stay committed and captured, and a
//! multi-line statement fails with `incomplete input` (INV-50).
mod common;

use common::invariants::{assert_invariants, Checks, FileHashes, Observed};
use common::model::Model;
use common::scenario::{assert_same_outcome, Driver, Scenario, Step};
use common::{parse_captured, Harness};
use rusqlite::Connection;
use walflip::db::{self, Db};

const SCHEMA: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";
const TX_ERR: &str = "transaction control";

fn ins(v: &str) -> String {
    format!("INSERT INTO t(v) VALUES('{v}')")
}

fn fails(s: &str) -> Step {
    Step::ExecFails(s.into())
}

/// S10a: statements that end, open or roll back the transaction are all rejected, interleaved
/// with good statements; invariants (incl. INV-31/INV-39 live oracle) run after every step.
fn s10() -> Scenario {
    Scenario::new(
        "S10 transaction control",
        SCHEMA,
        "fail",
        vec![
            Step::Exec(ins("a")),
            fails("INSERT INTO t(v) VALUES('x1'); COMMIT; BEGIN; INSERT INTO t(v) VALUES('x2')"),
            fails("INSERT INTO t(v) VALUES('y'); COMMIT; INSERT INTO nope VALUES(1)"),
            fails("INSERT INTO t(v) VALUES('z1'); ROLLBACK; INSERT INTO t(v) VALUES('z2')"),
            fails("INSERT INTO t(v) VALUES('e'); END"),
            Step::Exec(ins("b")),
            fails("BEGIN; INSERT INTO t(v) VALUES('open')"),
            fails("BEGIN"),
            fails("COMMIT"),
            fails("ROLLBACK"),
            fails("ATTACH DATABASE ':memory:' AS m"),
            fails("INSERT INTO t(v) VALUES('n'); INSERT INTO nope VALUES(1)"),
            // Savepoints nest inside exec's transaction and cannot end it, so they stay allowed.
            Step::Exec("SAVEPOINT s; INSERT INTO t(v) VALUES('sp'); RELEASE s".into()),
            Step::Exec(ins("c")),
            Step::Clone(2),
        ],
    )
}

#[test]
fn s10_transaction_control_cli() {
    let o = s10().run(Driver::Cli);
    assert_eq!(o.log().len(), 4, "INV-38: only the four good statements committed");
}

#[test]
fn s10_transaction_control_lib() {
    s10().run(Driver::Lib);
}

#[test]
fn s10_cli_and_lib_agree() {
    assert_same_outcome(&s10().run(Driver::Cli), &s10().run(Driver::Lib));
}

/// B2 reproduction: the rejected statement exits non-zero, names transaction control, and leaves
/// every file of the db folder byte-identical (INV-37), so history cannot lose rows.
#[test]
fn s10_commit_inside_statement_changes_nothing() {
    let h = Harness::new();
    h.create("b2", SCHEMA);
    assert_eq!(h.exec("b2", &ins("a")), 1);
    let d = Db::existing(&h.root(), "b2").unwrap();
    for sql in [
        "INSERT INTO t(v) VALUES(1); COMMIT; BEGIN; INSERT INTO t(v) VALUES(2)",
        "INSERT INTO t(v) VALUES(1); COMMIT; INSERT INTO nope VALUES(1)",
        "INSERT INTO t(v) VALUES(1); ROLLBACK; INSERT INTO t(v) VALUES(2)",
        "BEGIN",
    ] {
        let (obs, hashes) = (Observed::of(&d), FileHashes::of(&d.dir));
        let err = h.err(&["--db", "b2", "exec", sql]);
        assert!(err.contains(TX_ERR), "error should name transaction control: {err}");
        assert_eq!(Observed::of(&d), obs, "INV-37: status/log changed after: {sql}");
        let after = FileHashes::of(&d.dir);
        for f in ["live.db", "base.db", "meta.db"] {
            assert_eq!(hashes.0[f], after.0[f], "INV-37: {f} changed after: {sql}");
        }
    }
    // The next good statement is commit 2 and holds exactly two rows (B2 made it hold three).
    assert_eq!(h.exec("b2", &ins("b")), 1);
    let mut m = Model::new(Some(SCHEMA));
    m.exec(&ins("a"), 1);
    m.exec(&ins("b"), 1);
    assert_invariants(&h.root(), "b2", Some(&m), Checks::full());
}

/// Run `text` as one stdin batch (CLI) or `parse_statements` + `db::exec` (Lib); it must fail
/// with `want_err`, after the first `ok` lines were committed one commit each.
fn batch_fails(driver: Driver, text: &str, ok: usize, want_err: &str) {
    let h = Harness::new();
    let root = h.root();
    let name = "batch";
    let mut m = Model::new(Some(SCHEMA));
    match driver {
        Driver::Cli => {
            h.create(name, SCHEMA);
        }
        Driver::Lib => {
            db::create(&Db::new(&root, name).unwrap(), Some(SCHEMA)).unwrap();
        }
    }
    let seed = ins("seed");
    let n = match driver {
        Driver::Cli => h.exec(name, &seed),
        Driver::Lib => db::exec(&Db::existing(&root, name).unwrap(), &[seed.clone()]).unwrap() as u64,
    };
    m.exec(&seed, n);

    let err = match driver {
        Driver::Cli => {
            let o = h.with_stdin(&["--db", name, "exec", "-"], text);
            assert!(!o.status.success(), "batch unexpectedly succeeded: {text:?}");
            assert!(!String::from_utf8_lossy(&o.stdout).contains("captured"), "a failed batch prints no total");
            String::from_utf8(o.stderr).unwrap()
        }
        Driver::Lib => {
            let d = Db::existing(&root, name).unwrap();
            format!("{:#}", db::exec(&d, &db::parse_statements(text)).expect_err("batch should fail"))
        }
    };
    assert!(err.contains(want_err), "[{driver:?}] error should contain {want_err:?}: {err}");

    // Shadow is fed only the lines before the failure, one commit each.
    for line in db::parse_statements(text).iter().take(ok) {
        m.exec(line, 1);
    }
    let o = assert_invariants(&root, name, Some(&m), Checks::full());
    assert_eq!(o.commits, 1 + ok as u64, "[{driver:?}] commits after a failed batch");

    // The db is still usable and history stays consistent.
    let after = ins("after");
    let n = match driver {
        Driver::Cli => parse_captured(&h.ok(&["--db", name, "exec", &after])),
        Driver::Lib => db::exec(&Db::existing(&root, name).unwrap(), &[after.clone()]).unwrap() as u64,
    };
    m.exec(&after, n);
    assert_invariants(&root, name, Some(&m), Checks::full());
}

fn both(text: &str, ok: usize, want_err: &str) {
    batch_fails(Driver::Cli, text, ok, want_err);
    batch_fails(Driver::Lib, text, ok, want_err);
}

#[test]
fn s10_batch_with_a_bad_line_in_the_middle() {
    both(
        "INSERT INTO t(v) VALUES('1')\nINSERT INTO t(v) VALUES('2')\nINSERT INTO nope VALUES(3)\nINSERT INTO t(v) VALUES('4')\n",
        2,
        "no such table: nope",
    );
}

#[test]
fn s10_batch_with_commit_inside_a_line() {
    both(
        "INSERT INTO t(v) VALUES('1')\nINSERT INTO t(v) VALUES('2'); COMMIT; INSERT INTO t(v) VALUES('3');\nINSERT INTO t(v) VALUES('4')\n",
        1,
        TX_ERR,
    );
}

#[test]
fn s10_batch_with_begin_without_commit() {
    both("INSERT INTO t(v) VALUES('1')\nBEGIN;\nINSERT INTO t(v) VALUES('2')\n", 1, TX_ERR);
}

/// INV-50: `exec -` splits on lines, so a statement spread over lines fails on its first line
/// with `incomplete input`; earlier lines stay committed, later ones never run.
#[test]
fn s10_multi_line_statement_fails_with_incomplete_input() {
    both(
        "INSERT INTO t(v) VALUES('1')\nCREATE TABLE u(\n  a INTEGER\n);\nINSERT INTO t(v) VALUES('2')\n",
        1,
        "incomplete input",
    );
}

/// INV-25 (B3, T16): a commit already in the WAL when `exec` starts (a writer that has not
/// checkpointed) is captured first under `(recovered)`, then the statement gets its own commit;
/// `exec` succeeds and counts both. Before T16 this was the >1-commit backstop case (an error);
/// that backstop is now covered by `db::tests::backstop_more_than_one_commit_is_captured_then_reported`.
#[test]
fn orphan_commit_is_recovered_before_the_statement() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Db::new(tmp.path(), "bs").unwrap();
    db::create(&d, Some(SCHEMA)).unwrap();
    db::exec(&d, &[ins("a")]).unwrap();

    // Keep the foreign connection open so its commit stays in the WAL.
    let ext = Connection::open(&d.live).unwrap();
    ext.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    ext.execute_batch(&ins("external")).unwrap();

    let r = db::exec_with(&d, &[ins("b")], walflip::store::DEFAULT_BUSY_TIMEOUT).unwrap();
    assert_eq!(r, db::ExecReport { captured: 2, recovered: 1 });
    drop(ext);

    let labels: Vec<String> = db::log(&d, 10).unwrap().into_iter().map(|e| e.sql).collect();
    assert_eq!(labels, [ins("a"), db::RECOVERED_LABEL.to_string(), ins("b")]);

    let mut m = Model::new(Some(SCHEMA));
    m.exec(&ins("a"), 1);
    m.exec(&ins("external"), 1);
    m.exec(&ins("b"), 1);
    assert_invariants(tmp.path(), "bs", Some(&m), Checks::full());
}

/// T14 finding: a batch that fails on a later line reports how many commits it had captured.
#[test]
fn failed_batch_reports_commits_captured_before_it() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Db::new(tmp.path(), "fb").unwrap();
    db::create(&d, Some(SCHEMA)).unwrap();
    let err = db::exec(&d, &[ins("1"), ins("2"), "INSERT INTO nope VALUES(1)".into(), ins("4")])
        .expect_err("line 3 fails");
    let f = err.downcast_ref::<db::ExecFailed>().expect("error carries ExecFailed");
    assert_eq!(*f, db::ExecFailed { captured: 2, recovered: 0, statement: 3 });
    assert!(format!("{err:#}").contains("no such table: nope"), "{err:#}");
    // Nothing captured yet: no ExecFailed context, the error is the statement's own.
    let err = db::exec(&d, &["INSERT INTO nope VALUES(1)".into()]).unwrap_err();
    assert!(err.downcast_ref::<db::ExecFailed>().is_none());
}
