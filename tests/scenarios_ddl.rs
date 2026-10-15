//! DDL scenarios from docs/TESTING_PLAN.md section 3.2: S6 (DDL-heavy, quoted names) and
//! S7 (schema evolution). Every step runs `assert_invariants`, which compares
//! `sqlite_master` and all table rows of every `clone -n k` against the shadow model.
mod common;

use common::scenario::{assert_same_outcome, Driver, Scenario, Step};

fn ex(s: &str) -> Step {
    Step::Exec(s.into())
}

/// S6a: DDL of every kind with plain names.
fn s6_plain() -> Scenario {
    Scenario::new(
        "S6 ddl plain",
        "CREATE TABLE base(id INTEGER PRIMARY KEY, v TEXT);",
        "ddl",
        vec![
            ex("CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b INTEGER)"),
            ex("INSERT INTO t(a, b) VALUES('x', 1)"),
            ex("CREATE INDEX t_a ON t(a)"),
            ex("CREATE TABLE audit(id INTEGER PRIMARY KEY, msg TEXT)"),
            ex("CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO audit(msg) VALUES('ins ' || new.a); END"),
            ex("INSERT INTO t(a, b) VALUES('y', 2)"),
            ex("CREATE VIEW t_big AS SELECT a FROM t WHERE b > 1"),
            ex("CREATE UNIQUE INDEX t_b ON t(b)"),
            Step::ExecFails("INSERT INTO t(a, b) VALUES('dup', 2)".into()),
            ex("DROP TRIGGER t_ai"),
            ex("DROP VIEW t_big"),
            ex("DROP INDEX t_a"),
            ex("DROP TABLE audit"),
            Step::Clone(5),
            ex("CREATE TABLE after_drop(id INTEGER PRIMARY KEY)"),
        ],
    )
}

/// S6b: unusual table names (INV-47; B1 in `summarize`).
fn s6_quoted() -> Scenario {
    Scenario::new(
        "S6 ddl quoted names",
        "CREATE TABLE base(id INTEGER PRIMARY KEY, v TEXT);",
        "ddlq",
        vec![
            ex(r#"CREATE TABLE "a""b"(id INTEGER PRIMARY KEY, v TEXT)"#),
            ex(r#"INSERT INTO "a""b"(v) VALUES('quote')"#),
            ex(r#"CREATE TABLE "with space"(id INTEGER PRIMARY KEY, v TEXT)"#),
            ex(r#"INSERT INTO "with space"(v) VALUES('space')"#),
            ex(r#"CREATE TABLE "select"(id INTEGER PRIMARY KEY, v TEXT)"#),
            ex(r#"INSERT INTO "select"(v) VALUES('kw')"#),
            ex(r#"UPDATE "a""b" SET v = 'changed'"#),
            ex(r#"DROP TABLE "with space""#),
            Step::Clone(5),
        ],
    )
}

/// S7: schema evolution with data changes in between.
fn s7() -> Scenario {
    Scenario::new(
        "S7 schema evolution",
        "CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT);",
        "evo",
        vec![
            ex("INSERT INTO people(name) VALUES('ann')"),
            ex("ALTER TABLE people ADD COLUMN age INTEGER NOT NULL DEFAULT 7"),
            ex("INSERT INTO people(name) VALUES('bob')"),
            ex("INSERT INTO people(name, age) VALUES('cy', 40)"),
            ex("ALTER TABLE people RENAME COLUMN name TO full_name"),
            ex("UPDATE people SET full_name = upper(full_name)"),
            ex("ALTER TABLE people RENAME TO persons"),
            ex("INSERT INTO persons(full_name, age) VALUES('dee', 5)"),
            ex("ALTER TABLE persons ADD COLUMN note TEXT"),
            ex("UPDATE persons SET note = 'n' || id WHERE age > 6"),
            ex("ALTER TABLE persons DROP COLUMN note"),
            Step::Clone(2),
            Step::Clone(7),
        ],
    )
}

#[test]
fn s6_ddl_plain_cli() {
    let o = s6_plain().run(Driver::Cli);
    assert_eq!(o.log().len(), 13);
}

#[test]
fn s6_ddl_plain_lib() {
    s6_plain().run(Driver::Lib);
}

#[test]
fn s6_ddl_plain_cli_and_lib_agree() {
    assert_same_outcome(&s6_plain().run(Driver::Cli), &s6_plain().run(Driver::Lib));
}

#[test]
#[ignore = "KNOWN FAILING: B1, fixed by T13"]
fn s6_ddl_quoted_names_cli() {
    // INV-47: `clone` summarize must work for any table name SQLite accepts. Today it fails
    // with `unrecognized token: """ in SELECT count(*) FROM "a"b"` (unquoted identifier, B1).
    s6_quoted().run(Driver::Cli);
}

/// The library path is correct; only the CLI's row-count summary is unquoted (B1).
#[test]
fn s6_ddl_quoted_names_lib() {
    s6_quoted().run(Driver::Lib);
}

#[test]
fn s7_schema_evolution_cli() {
    let o = s7().run(Driver::Cli);
    assert_eq!(o.log().len(), 11);
    assert_eq!(o.clones.len(), 2);
}

#[test]
fn s7_schema_evolution_lib() {
    s7().run(Driver::Lib);
}

#[test]
fn s7_cli_and_lib_agree() {
    assert_same_outcome(&s7().run(Driver::Cli), &s7().run(Driver::Lib));
}
