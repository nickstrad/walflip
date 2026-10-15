//! Core scenarios from docs/TESTING_PLAN.md section 3.2, run through both drivers.
mod common;

use common::scenario::{assert_same_outcome, Driver, Scenario, Step};

/// S1: full lifecycle (create, exec, no-op, failure, batch, clone, rename, drop).
fn s1() -> Scenario {
    let ins = |s: &str| Step::Exec(format!("INSERT INTO notes(body) VALUES('{s}')"));
    Scenario::new(
        "S1 lifecycle",
        "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT);",
        "notes",
        vec![
            ins("first"),
            ins("second"),
            ins("third"),
            Step::Exec("UPDATE notes SET body = 'x' WHERE id = 404".into()),
            Step::ExecFails("INSERT INTO nope VALUES(1)".into()),
            Step::Clone(2),
            Step::Batch(vec![
                "INSERT INTO notes(body) VALUES('b1')".into(),
                "UPDATE notes SET body = 'b2' WHERE id = 1".into(),
                "DELETE FROM notes WHERE id = 2".into(),
            ]),
            Step::Clone(0),
            Step::Clone(6),
            Step::Rename("journal".into()),
            Step::Clone(6),
            Step::Drop,
        ],
    )
}

#[test]
fn s1_lifecycle_cli() {
    let o = s1().run(Driver::Cli);
    assert_eq!(o.clones.len(), 4);
    assert_eq!(o.log().len(), 6, "INV-27: final observed log");
}

#[test]
fn s1_lifecycle_lib() {
    s1().run(Driver::Lib);
}

#[test]
fn s1_cli_and_lib_agree() {
    assert_same_outcome(&s1().run(Driver::Cli), &s1().run(Driver::Lib));
}
