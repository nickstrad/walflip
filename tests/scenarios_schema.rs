//! S18: schema-file matrix (docs/TESTING_PLAN.md section 3.2).
mod common;

use common::invariants::{assert_invariants, Checks};
use common::scenario::{Driver, Scenario, Step};
use common::{ints, Harness};

const MULTI: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT NOT NULL);
CREATE INDEX t_v ON t(v);
INSERT INTO t(v) VALUES('a'),('b');
INSERT INTO t(v) VALUES('c');
";

/// Create from a schema file; returns the raw output.
fn create_raw(h: &Harness, name: &str, schema: &str) -> std::process::Output {
    let f = h.path(&format!("{name}.sql"));
    std::fs::write(&f, schema).unwrap();
    h.run(&["create", name, "--schema", f.to_str().unwrap()])
}

/// A failed create leaves no folder, no listing, and the name stays usable.
fn assert_failed_clean(h: &Harness, name: &str, out: &std::process::Output) {
    assert!(!out.status.success(), "create {name} unexpectedly succeeded");
    assert!(!h.root().join(name).exists(), "failed create left folder {name}");
    assert!(!h.ok(&["list"]).contains(name), "failed create {name} is listed");
}

/// base.db must equal live.db (checkpointed, WAL-free) right after create.
fn assert_base_is_live(h: &Harness, name: &str) {
    let dir = h.root().join(name);
    let base = std::fs::read(dir.join("base.db")).unwrap();
    let live = std::fs::read(dir.join("live.db")).unwrap();
    assert!(base == live, "base.db != live.db after create ({} vs {} bytes)", base.len(), live.len());
}

#[test]
fn s18_multi_statement_with_seed_rows() {
    let h = Harness::new();
    h.create("m", MULTI);
    assert_base_is_live(&h, "m");
    assert_eq!(ints(&h.root().join("m/base.db"), "SELECT id FROM t ORDER BY id"), vec![1, 2, 3]);
    assert_eq!(h.commits("m"), 0, "seed rows are base state, not commits");
    assert_eq!(h.exec("m", "INSERT INTO t(v) VALUES('d')"), 1);
    assert_invariants(&h.root(), "m", None, Checks::full());
}

#[test]
fn s18_seeded_scenario_both_drivers() {
    for d in [Driver::Cli, Driver::Lib] {
        let s = Scenario::new(
            "S18 seeded",
            MULTI,
            "seeded",
            vec![Step::Exec("INSERT INTO t(v) VALUES('d')".into()), Step::Clone(0), Step::Clone(1)],
        );
        let o = s.run(d);
        assert_eq!(o.log().len(), 1);
    }
}

#[test]
fn s18_stdin_and_file_give_identical_base() {
    let h = Harness::new();
    h.create("viafile", MULTI);
    let o = h.with_stdin(&["create", "viastdin", "--schema", "-"], MULTI);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let a = std::fs::read(h.root().join("viafile/base.db")).unwrap();
    let b = std::fs::read(h.root().join("viastdin/base.db")).unwrap();
    assert!(a == b, "stdin and file schema produced different base.db");
    assert_base_is_live(&h, "viastdin");
    assert_eq!(h.exec("viastdin", "INSERT INTO t(v) VALUES('x')"), 1);
    assert_invariants(&h.root(), "viastdin", None, Checks::full());
}

#[test]
fn s18_empty_file() {
    let h = Harness::new();
    let o = create_raw(&h, "e", "");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_base_is_live(&h, "e");
    assert_eq!(h.commits("e"), 0);
    // An empty schema yields an empty db; the first exec creates a table and is captured.
    assert_eq!(h.exec("e", "CREATE TABLE z(a)"), 1);
    assert_invariants(&h.root(), "e", None, Checks::full());
}

#[test]
fn s18_syntax_error_leaves_nothing() {
    let h = Harness::new();
    let o = create_raw(&h, "bad", "CREATE TABLE t(id INTEGER);\nCREATE TABEL oops(;");
    assert_failed_clean(&h, "bad", &o);
    // The name is reusable afterwards.
    h.create("bad", "CREATE TABLE t(id INTEGER);");
}

#[test]
fn s18_constraint_violation_in_seed_leaves_nothing() {
    let h = Harness::new();
    let o = create_raw(
        &h,
        "dup",
        "CREATE TABLE t(id INTEGER PRIMARY KEY);\nINSERT INTO t VALUES(1);\nINSERT INTO t VALUES(1);",
    );
    assert_failed_clean(&h, "dup", &o);
}

#[test]
fn s18_begin_commit_in_schema() {
    let h = Harness::new();
    let o = create_raw(
        &h,
        "tx",
        "BEGIN;\nCREATE TABLE t(id INTEGER PRIMARY KEY);\nINSERT INTO t VALUES(7);\nCOMMIT;",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_base_is_live(&h, "tx");
    assert_eq!(ints(&h.root().join("tx/base.db"), "SELECT id FROM t"), vec![7]);
    assert_eq!(h.exec("tx", "INSERT INTO t VALUES(8)"), 1);
    assert_invariants(&h.root(), "tx", None, Checks::full());
}

#[test]
fn s18_unterminated_begin() {
    let h = Harness::new();
    let o = create_raw(&h, "ub", "BEGIN;\nCREATE TABLE t(id INTEGER PRIMARY KEY);\nINSERT INTO t VALUES(1);");
    assert_failed_clean(&h, "ub", &o);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("applying schema"), "error should say `applying schema`: {err}");
}

#[test]
fn s18_journal_mode_delete_is_rejected() {
    let h = Harness::new();
    let o = create_raw(&h, "jm", "PRAGMA journal_mode=DELETE;\nCREATE TABLE t(id INTEGER);");
    // `create` must fail mentioning journal_mode and leave no folder.
    assert_failed_clean(&h, "jm", &o);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("journal_mode"), "error should mention journal_mode: {err}");
}

#[test]
fn s18_page_size_pragma() {
    let h = Harness::new();
    let o = create_raw(&h, "ps", "PRAGMA page_size=8192;\nCREATE TABLE t(id INTEGER);");
    // Q7: the schema runs before WAL is enabled, so page_size takes effect.
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = h.ok(&["--db", "ps", "status"]);
    assert!(out.contains("8192"), "expected page size 8192 in status: {out}");
    assert_eq!(h.exec("ps", "CREATE TABLE u(a)"), 1);
    assert_invariants(&h.root(), "ps", None, Checks::full());
    let c = h.path("ps_c1.db");
    h.ok(&["--db", "ps", "clone", "--n", "1", "--out", c.to_str().unwrap()]);
    assert_eq!(ints(&c, "PRAGMA page_size"), vec![8192]);
}

#[test]
fn s18_attach_in_schema() {
    let h = Harness::new();
    let other = h.path("other.db");
    let schema = format!(
        "ATTACH DATABASE '{}' AS o;\nCREATE TABLE o.x(a);\nCREATE TABLE t(id INTEGER);",
        other.display()
    );
    let out = create_raw(&h, "zzq", &schema);
    // Q8: ATTACH is denied by the authorizer; nothing is written outside the db folder.
    assert_failed_clean(&h, "zzq", &out);
    assert!(!other.exists(), "ATTACH created a file outside the db folder");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ATTACH"), "error should mention ATTACH: {err}");
}

#[test]
fn s18_exec_attach_and_journal_mode_are_denied() {
    let h = Harness::new();
    h.create("ex", "CREATE TABLE t(id INTEGER);");
    let other = h.path("other_exec.db");
    let sql = format!("ATTACH DATABASE '{}' AS o", other.display());
    let out = h.run(&["--db", "ex", "exec", &sql]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ATTACH"));
    assert!(!other.exists());
    let out = h.run(&["--db", "ex", "exec", "PRAGMA journal_mode=DELETE"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("journal_mode"));
    assert_eq!(h.commits("ex"), 0);
    assert_eq!(h.exec("ex", "INSERT INTO t VALUES(1)"), 1);
    assert_invariants(&h.root(), "ex", None, Checks::full());
}
