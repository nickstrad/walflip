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
    assert_eq!(
        o.log().len(),
        4,
        "INV-38: only the four good statements committed"
    );
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
        assert!(
            err.contains(TX_ERR),
            "error should name transaction control: {err}"
        );
        assert_eq!(
            Observed::of(&d),
            obs,
            "INV-37: status/log changed after: {sql}"
        );
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
        Driver::Lib => db::exec(
            &Db::existing(&root, name).unwrap(),
            std::slice::from_ref(&seed),
        )
        .unwrap() as u64,
    };
    m.exec(&seed, n);

    let err = match driver {
        Driver::Cli => {
            let o = h.with_stdin(&["--db", name, "exec", "-"], text);
            assert!(
                !o.status.success(),
                "batch unexpectedly succeeded: {text:?}"
            );
            assert!(
                !String::from_utf8_lossy(&o.stdout).contains("captured"),
                "a failed batch prints no total"
            );
            String::from_utf8(o.stderr).unwrap()
        }
        Driver::Lib => {
            let d = Db::existing(&root, name).unwrap();
            format!(
                "{:#}",
                db::exec(&d, &db::parse_statements(text)).expect_err("batch should fail")
            )
        }
    };
    assert!(
        err.contains(want_err),
        "[{driver:?}] error should contain {want_err:?}: {err}"
    );

    // Shadow is fed only the lines before the failure, one commit each.
    for line in db::parse_statements(text).iter().take(ok) {
        m.exec(line, 1);
    }
    let o = assert_invariants(&root, name, Some(&m), Checks::full());
    assert_eq!(
        o.commits,
        1 + ok as u64,
        "[{driver:?}] commits after a failed batch"
    );

    // The db is still usable and history stays consistent.
    let after = ins("after");
    let n = match driver {
        Driver::Cli => parse_captured(&h.ok(&["--db", name, "exec", &after])),
        Driver::Lib => db::exec(
            &Db::existing(&root, name).unwrap(),
            std::slice::from_ref(&after),
        )
        .unwrap() as u64,
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
    both(
        "INSERT INTO t(v) VALUES('1')\nBEGIN;\nINSERT INTO t(v) VALUES('2')\n",
        1,
        TX_ERR,
    );
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
    assert_eq!(
        r,
        db::ExecReport {
            captured: 2,
            recovered: 1
        }
    );
    drop(ext);

    let labels: Vec<String> = db::log(&d, 10, false)
        .unwrap()
        .into_iter()
        .map(|e| e.sql)
        .collect();
    assert_eq!(
        labels,
        [ins("a"), db::RECOVERED_LABEL.to_string(), ins("b")]
    );

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
    let err = db::exec(
        &d,
        &[
            ins("1"),
            ins("2"),
            "INSERT INTO nope VALUES(1)".into(),
            ins("4"),
        ],
    )
    .expect_err("line 3 fails");
    let f = err
        .downcast_ref::<db::ExecFailed>()
        .expect("error carries ExecFailed");
    assert_eq!(
        *f,
        db::ExecFailed {
            captured: 2,
            recovered: 0,
            statement: 3
        }
    );
    assert!(
        format!("{err:#}").contains("no such table: nope"),
        "{err:#}"
    );
    // Nothing captured yet: no ExecFailed context, the error is the statement's own.
    let err = db::exec(&d, &["INSERT INTO nope VALUES(1)".into()]).unwrap_err();
    assert!(err.downcast_ref::<db::ExecFailed>().is_none());
}

/// Multi-line DDL batch for the Q5 `exec --script` tests: a multi-line CREATE TABLE, a trigger
/// whose body has `;`, a multi-line INSERT and a `;` inside a string, with comments around them.
const SCRIPT: &str = "-- schema change; then data\n\
CREATE TABLE u(\n  a INTEGER\n);\n\
CREATE TRIGGER tu AFTER INSERT ON t BEGIN\n  INSERT INTO u(a) VALUES(new.id);\nEND;\n\
/* two rows; one statement */\n\
INSERT INTO t(v)\nVALUES\n  ('x;y'),\n  ('z');\n\
INSERT INTO t(v) VALUES('w'); -- trailing comment;\n";

/// Create `name` through the CLI with one seed commit; returns the model so far.
fn seeded(h: &Harness, name: &str) -> Model {
    let mut m = Model::new(Some(SCHEMA));
    h.create(name, SCHEMA);
    m.exec(&ins("seed"), h.exec(name, &ins("seed")));
    m
}

/// INV-50 / Q5: the same multi-line batch fails line by line (`exec -`) but runs with
/// `exec --script` (stdin or a file): one commit per `;`-terminated statement, in order, each
/// labelled with its statement text.
#[test]
fn s10_script_mode_runs_multi_line_ddl_batch() {
    let stmts = db::split_script(SCRIPT).unwrap();
    assert_eq!(stmts.len(), 4);
    let h = Harness::new();

    let lines_model = seeded(&h, "lines");
    let o = h.with_stdin(&["--db", "lines", "exec", "-"], SCRIPT);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("incomplete input"),
        "line mode should fail"
    );
    assert_invariants(&h.root(), "lines", Some(&lines_model), Checks::full());

    let file = h.path("batch.sql");
    std::fs::write(&file, SCRIPT).unwrap();
    for (name, source) in [("stdin", "-"), ("file", file.to_str().unwrap())] {
        let mut m = seeded(&h, name);
        let o = h.with_stdin(&["--db", name, "exec", "--script", source], SCRIPT);
        assert!(
            o.status.success(),
            "[{name}] {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            parse_captured(&String::from_utf8(o.stdout).unwrap()),
            4,
            "[{name}]"
        );
        for s in &stmts {
            m.exec(s, 1);
        }
        let obs = assert_invariants(&h.root(), name, Some(&m), Checks::full());
        let labels: Vec<&String> = obs.log.iter().skip(1).map(|r| &r.sql).collect();
        assert_eq!(
            labels,
            stmts.iter().collect::<Vec<_>>(),
            "[{name}] labels are the statements"
        );
    }

    assert!(h
        .err(&["--db", "stdin", "exec", "--script", "-", "SELECT 1"])
        .contains("cannot be used"));
    assert!(h.err(&["--db", "stdin", "exec"]).contains("required"));
}

/// Q5: a script whose last statement has no `;` is rejected before anything runs.
#[test]
fn s10_script_unterminated_tail_runs_nothing() {
    let h = Harness::new();
    let m = seeded(&h, "tail");
    let o = h.with_stdin(
        &["--db", "tail", "exec", "--script", "-"],
        &format!("{};\n{}\n", ins("1"), ins("2")),
    );
    assert!(!o.status.success());
    let err = String::from_utf8(o.stderr).unwrap();
    assert!(
        err.contains("incomplete input") && err.contains("line 2"),
        "{err}"
    );
    let obs = assert_invariants(&h.root(), "tail", Some(&m), Checks::full());
    assert_eq!(obs.commits, 1, "nothing from the script ran");
}

/// Q5: per-statement atomicity as in line mode: a failing statement stops the script, earlier
/// statements stay committed and captured, later ones never run.
#[test]
fn s10_script_failing_statement_stops_batch() {
    let h = Harness::new();
    let mut m = seeded(&h, "stop");
    let text = format!(
        "{};\nINSERT INTO nope\n  VALUES(1);\n{};\n",
        ins("1"),
        ins("2")
    );
    let o = h.with_stdin(&["--db", "stop", "exec", "--script", "-"], &text);
    assert!(!o.status.success());
    let err = String::from_utf8(o.stderr).unwrap();
    assert!(
        err.contains("no such table: nope") && err.contains("statement 2 failed"),
        "{err}"
    );
    m.exec(&format!("{};", ins("1")), 1);
    let obs = assert_invariants(&h.root(), "stop", Some(&m), Checks::full());
    assert_eq!(obs.commits, 2);
}

/// `log` shows a multi-line label on one row, line breaks as `\n`; the stored label is unchanged.
#[test]
fn log_shows_multi_line_labels_on_one_row() {
    let h = Harness::new();
    seeded(&h, "lg");
    let o = h.with_stdin(
        &["--db", "lg", "exec", "--script", "-"],
        "CREATE TABLE u(\n  a INTEGER\n);\n",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    h.exec("lg", "INSERT INTO t(v)\r\nVALUES('crlf')");
    let out = h.ok(&["--db", "lg", "log"]);
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 4, "header + 3 commits, one line each:\n{out}");
    assert!(
        rows[2].ends_with("  CREATE TABLE u(\\n  a INTEGER\\n);"),
        "{out}"
    );
    assert!(
        rows[3].ends_with("  INSERT INTO t(v)\\nVALUES('crlf')"),
        "{out}"
    );
    let stored: Vec<String> = db::log(&Db::existing(&h.root(), "lg").unwrap(), 10, false)
        .unwrap()
        .into_iter()
        .map(|e| e.sql)
        .collect();
    assert_eq!(stored[1], "CREATE TABLE u(\n  a INTEGER\n);");
    assert_eq!(stored[2], "INSERT INTO t(v)\r\nVALUES('crlf')");
}
