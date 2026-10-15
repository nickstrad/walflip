//! End-to-end scenarios driving the compiled `walflip` binary, each showing a different way
//! the tool is used.
//!
//! Scenarios that the step language can express run through `Scenario::run`
//! (docs/TESTING_PLAN.md section 3), which calls `assert_invariants` after every step. Behaviour
//! the step language cannot express (exact CLI output, env selection, schema on stdin, multiple
//! dbs, error paths, seed script) is asserted in companion tests that drive their own `Harness`
//! and finish with `assert_invariants`.
mod common;
use common::invariants::{assert_invariants, hash_file, Checks};
use common::model::Model;
use common::scenario::{Driver, Outcome, Scenario, Step};
use common::{ints, texts, Harness};
use std::fs;
use std::path::Path;

const NOTES: &str = "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT);";

/// Write clone bytes to a scratch file so the `ints`/`texts` helpers can query them.
fn query_clone<T>(bytes: &[u8], f: impl FnOnce(&Path) -> T) -> T {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clone.db");
    fs::write(&path, bytes).unwrap();
    f(&path)
}

/// Commit count observed after step `step` (0 is the create).
fn commits_at(o: &Outcome, step: usize) -> u64 {
    o.observed[step].as_ref().expect("db observed at this step").commits
}

/// Scenario 1: the whole database lifecycle through the CLI, with the full invariant set
/// checked after every step.
#[test]
fn lifecycle_create_write_inspect_clone_rename_drop() {
    let ins = |body: &str| Step::Exec(format!("INSERT INTO notes(body) VALUES('{body}')"));
    let o = Scenario::new(
        "lifecycle",
        NOTES,
        "notes",
        vec![
            ins("first"),
            ins("second"),
            ins("third"),
            // A no-op and a failing statement leave history and contents unchanged.
            Step::Exec("UPDATE notes SET body = 'x' WHERE id = 404".into()),
            Step::ExecFails("INSERT INTO nope VALUES(1)".into()),
            Step::Clone(2),
            Step::Rename("journal".into()),
            Step::Clone(3),
            Step::Drop,
        ],
    )
    .run(Driver::Cli);

    assert_eq!(commits_at(&o, 5), 3);
    assert_eq!(query_clone(&o.clones[0].1, |p| texts(p, "SELECT body FROM notes ORDER BY id")), ["first", "second"]);
    // History survives a rename: observed state is identical before and after it.
    assert_eq!(commits_at(&o, 7), 3, "history survives a rename");
    assert_eq!(o.observed[6], o.observed[7], "INV-43: rename changed observed history");
    assert_eq!(query_clone(&o.clones[1].1, |p| texts(p, "SELECT body FROM notes ORDER BY id")), ["first", "second", "third"]);
    assert_eq!(o.log().iter().filter(|r| r.sql.contains("INSERT INTO notes")).count(), 3);
}

/// CLI output, error messages and the meta.db identity across rename, which the step language
/// does not see.
#[test]
fn lifecycle_cli_output_and_errors() {
    let h = Harness::new();
    assert!(h.ok(&["list"]).contains("no databases"));

    let out = h.create("notes", NOTES);
    assert!(out.contains("created") && out.contains("page size 4096"));
    assert!(h.root().join("notes").join("live.db").exists());
    assert!(h.root().join("notes").join("base.db").exists());
    assert!(h.root().join("notes").join("meta.db").exists());

    for body in ["first", "second", "third"] {
        h.exec("notes", &format!("INSERT INTO notes(body) VALUES('{body}')"));
    }
    assert!(h.err(&["--db", "notes", "exec", "INSERT INTO nope VALUES(1)"]).contains("failed:"));
    assert_eq!(h.commits("notes"), 3);
    assert!(h.ok(&["list"]).contains("notes"));

    let two = h.path("two.db");
    let out = h.ok(&["--db", "notes", "clone", "-n", "2", "-o", two.to_str().unwrap()]);
    assert!(out.contains("integrity_check: ok") && out.contains("notes: 2 rows"));

    let meta_hash = hash_file(&h.root().join("notes").join("meta.db"));
    h.ok(&["rename", "notes", "journal"]);
    assert_eq!(h.commits("journal"), 3, "history survives a rename");
    h.err(&["--db", "notes", "status"]);
    assert_eq!(hash_file(&h.root().join("journal").join("meta.db")), meta_hash, "INV-43: rename changed meta.db");

    h.ok(&["drop", "journal"]);
    assert!(!h.root().join("journal").exists(), "INV-43: drop left the folder");
    assert!(h.ok(&["list"]).contains("no databases"));
}

/// A db with a few commits plus its model, for the harness self-checks below.
fn small_db() -> (Harness, Model) {
    let h = Harness::new();
    let schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";
    h.create("d", schema);
    let mut model = Model::new(Some(schema));
    for i in 1..=4 {
        let sql = format!("INSERT INTO t VALUES({i}, '{}')", "x".repeat(50 * i));
        model.exec(&sql, h.exec("d", &sql));
    }
    assert_invariants(&h.root(), "d", Some(&model), Checks::full());
    (h, model)
}

/// Run `assert_invariants` expecting it to panic; returns the panic message.
fn invariants_panic(h: &Harness, model: Option<&Model>) -> String {
    let root = h.root();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_invariants(&root, "d", model, Checks::full());
    }))
    .expect_err("assert_invariants accepted a corrupted meta.db");
    err.downcast_ref::<String>().cloned().or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string())).unwrap()
}

fn corrupt_meta(h: &Harness, sql: &str, params: impl rusqlite::Params) {
    let conn = rusqlite::Connection::open(h.root().join("d").join("meta.db")).unwrap();
    assert_eq!(conn.execute(sql, params).unwrap(), 1, "corruption touched exactly one row");
}

/// Harness self-check: a deleted frames row is caught by the meta structure check.
#[test]
fn invariants_catch_a_deleted_frame() {
    let (h, model) = small_db();
    corrupt_meta(&h, "DELETE FROM frames WHERE commit_idx = 2 AND seq = 0", []);
    let msg = invariants_panic(&h, Some(&model));
    assert!(msg.contains("INV-22"), "unexpected message: {msg}");
}

/// Harness self-check: one flipped byte in a stored page is caught by the clone oracles.
#[test]
fn invariants_catch_a_flipped_page_byte() {
    let (h, model) = small_db();
    let meta = h.root().join("d").join("meta.db");
    let (rowid, mut page): (i64, Vec<u8>) = rusqlite::Connection::open(&meta)
        .unwrap()
        .query_row("SELECT rowid, page FROM frames ORDER BY commit_idx DESC, seq DESC LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let at = page.len() - 100; // inside the cell content area at the end of the page
    page[at] ^= 0x01;
    corrupt_meta(&h, "UPDATE frames SET page = ?1 WHERE rowid = ?2", rusqlite::params![page, rowid]);
    let msg = invariants_panic(&h, Some(&model));
    assert!(msg.contains("INV-30"), "unexpected message: {msg}");
    // Without a model the byte-for-byte oracle still catches it (integrity_check does not).
    let msg = invariants_panic(&h, None);
    assert!(msg.contains("INV-31"), "unexpected message: {msg}");
}

/// Scenario 2: bulk loading from stdin, then time travel to every point along the way.
/// The blank line and the padded statement are kept so the stdin path is exercised as written.
#[test]
fn stdin_batch_then_clone_at_every_commit() {
    let schema = "CREATE TABLE entries(id INTEGER PRIMARY KEY, amount INTEGER);";
    let o = Scenario::new(
        "ledger",
        schema,
        "ledger",
        vec![
            Step::Batch(vec![
                "INSERT INTO entries VALUES(1, 100)".into(),
                "".into(),
                "INSERT INTO entries VALUES(2, 250)".into(),
                "  UPDATE entries SET amount = 175 WHERE id = 2  ".into(),
                "DELETE FROM entries WHERE id = 1".into(),
                "".into(),
            ]),
            Step::Clone(0),
            Step::Clone(1),
            Step::Clone(2),
            Step::Clone(3),
            Step::Clone(4),
        ],
    )
    .run(Driver::Cli);

    assert_eq!(commits_at(&o, 1), 4, "captured 4 commit(s)");
    let expect: [&[i64]; 5] = [&[], &[100], &[100, 250], &[100, 175], &[175]];
    for (n, want) in expect.iter().enumerate() {
        assert_eq!(query_clone(&o.clones[n].1, |p| ints(p, "SELECT amount FROM entries ORDER BY id")), *want, "clone -n {n}");
    }
}

/// Scenario 3: configured entirely through the environment, as in a shell session or CI job.
/// The step language always passes `--db`, so this stays a bespoke test.
#[test]
fn environment_selects_root_and_database() {
    let schema = "CREATE TABLE t(x INTEGER);";
    let h = Harness::new();
    let run = |args: &[&str]| {
        let o = h
            .bare()
            .env("WALFLIP_ROOT", h.root())
            .env("WALFLIP_DB", "envdb")
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let schema_file = h.path("env.sql");
    std::fs::write(&schema_file, schema).unwrap();
    run(&["create", "envdb", "--schema", schema_file.to_str().unwrap()]);
    run(&["exec", "INSERT INTO t VALUES(42)"]);
    assert!(run(&["status"]).contains("commits   : 1"));
    assert!(run(&["list"]).contains("envdb"));

    let mut model = Model::new(Some(schema));
    model.exec("INSERT INTO t VALUES(42)", 1);
    assert_invariants(&h.root(), "envdb", Some(&model), Checks::full());
}

/// Scenario 4: several databases side by side stay independent. The step language is single-db
/// and cannot create without a schema, so this stays a bespoke test over one shared root.
#[test]
fn multiple_databases_are_isolated() {
    let users_schema = "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);";
    let metrics_schema = "CREATE TABLE samples(ts INTEGER, v REAL);";
    let h = Harness::new();
    h.create("users", users_schema);
    h.create("metrics", metrics_schema);
    h.ok(&["create", "scratch"]);

    let mut users = Model::new(Some(users_schema));
    let mut metrics = Model::new(Some(metrics_schema));
    let mut scratch = Model::new(None);
    for sql in ["INSERT INTO users VALUES(1,'ada')", "INSERT INTO users VALUES(2,'grace')"] {
        users.exec(sql, h.exec("users", sql));
    }
    let sql = "INSERT INTO samples VALUES(1, 0.5)";
    metrics.exec(sql, h.exec("metrics", sql));
    let sql = "CREATE TABLE tmp(a)";
    scratch.exec(sql, h.exec("scratch", sql));

    assert_eq!((h.commits("users"), h.commits("metrics"), h.commits("scratch")), (2, 1, 1));
    let list = h.ok(&["list"]);
    for n in ["metrics", "scratch", "users"] {
        assert!(list.contains(n));
    }
    assert_invariants(&h.root(), "users", Some(&users), Checks::full());
    assert_invariants(&h.root(), "metrics", Some(&metrics), Checks::full());
    assert_invariants(&h.root(), "scratch", Some(&scratch), Checks::full());

    let u = h.path("users.db");
    h.ok(&["--db", "users", "clone", "-n", "2", "-o", u.to_str().unwrap()]);
    assert_eq!(texts(&u, "SELECT name FROM users ORDER BY id"), ["ada", "grace"]);
    assert!(texts(&u, "SELECT name FROM sqlite_master WHERE name='samples'").is_empty());
}

/// Scenario 5: the recovery story. A destructive statement is committed; clone just before it.
#[test]
fn recover_from_an_accidental_delete() {
    let load: Vec<String> = (1..=50).map(|i| format!("INSERT INTO items VALUES({i}, {})", i * 10)).collect();
    let o = Scenario::new(
        "inventory",
        "CREATE TABLE items(sku INTEGER PRIMARY KEY, qty INTEGER);",
        "inventory",
        vec![
            Step::Batch(load),
            Step::Exec("DELETE FROM items WHERE sku > 10".into()),
            Step::Exec("UPDATE items SET qty = 0".into()),
            // Commit 51 is the DELETE (checked below); clone the commit before it.
            Step::Clone(50),
        ],
    )
    .run(Driver::Cli);

    assert_eq!(commits_at(&o, o.observed.len() - 1), 52);
    let log = o.log();
    assert!(log[50].sql.contains("DELETE FROM items"), "commit 51 should be the DELETE, got {:?}", log[50].sql);
    assert_eq!(query_clone(&o.clones[0].1, |p| ints(p, "SELECT count(*) FROM items")), [50]);
    assert_eq!(
        query_clone(&o.clones[0].1, |p| ints(p, "SELECT sum(qty) FROM items")),
        [(1..=50).map(|i| i * 10).sum::<i64>()]
    );
}

/// Scenario 6: how you write does not change history. One process per statement and one batch
/// produce the same log and the same data.
#[test]
fn separate_processes_and_one_batch_agree() {
    let schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";
    let stmts: Vec<String> = (0..30)
        .map(|i| match i % 3 {
            0 => format!("INSERT INTO t VALUES({i}, 'v{i}')"),
            1 => format!("UPDATE t SET v = 'u{i}' WHERE id = {}", i - 1),
            _ => format!("DELETE FROM t WHERE id = {}", i - 2),
        })
        .collect();
    let n = stmts.len() as u64;

    let mut one_by_one: Vec<Step> = stmts.iter().cloned().map(Step::Exec).collect();
    one_by_one.push(Step::Clone(n));
    let one = Scenario::new("one_by_one", schema, "one_by_one", one_by_one).run(Driver::Cli);
    let batched = Scenario::new("batched", schema, "batched", vec![Step::Batch(stmts), Step::Clone(n)]).run(Driver::Cli);

    assert_eq!(commits_at(&one, one.observed.len() - 1), n);
    assert_eq!(commits_at(&batched, batched.observed.len() - 1), n);
    assert_eq!(one.log(), batched.log(), "INV-44: logs differ");
    assert_eq!(one.clones, batched.clones, "INV-44: clone bytes differ");
    let q = "SELECT id || ':' || v FROM t ORDER BY id";
    assert_eq!(
        query_clone(&one.clones[0].1, |p| texts(p, q)),
        query_clone(&batched.clones[0].1, |p| texts(p, q))
    );
}

/// Scenario 7: failure modes are reported, exit non-zero, and leave state intact.
#[test]
fn errors_are_clear_and_state_is_preserved() {
    let t_schema = "CREATE TABLE t(id INTEGER PRIMARY KEY);";
    let o = Scenario::new(
        "d",
        t_schema,
        "d",
        vec![
            Step::Exec("INSERT INTO t VALUES(1)".into()),
            Step::ExecFails("INSERT INTO t VALUES(1)".into()),
            Step::ExecFails("NOT SQL".into()),
            Step::Clone(1),
        ],
    )
    .run(Driver::Cli);
    assert_eq!(commits_at(&o, 3), 1, "failed statements are not captured");
    assert_eq!(query_clone(&o.clones[0].1, |p| ints(p, "SELECT count(*) FROM t")), [1]);

    // Error messages, the create/clone error paths and batch-stops-at-failure.
    let h = Harness::new();
    h.create("d", t_schema);
    assert!(h.err(&["create", "d"]).contains("already exists"));
    assert!(h.err(&["create", "bad/name"]).contains("invalid database name"));
    let missing = h.path("missing.sql");
    assert!(h.err(&["create", "x", "--schema", missing.to_str().unwrap()]).contains("reading schema"));
    let broken = h.path("broken.sql");
    std::fs::write(&broken, "CREATE TABLE ok(a);\nCREATE TABEL oops(b);").unwrap();
    assert!(h.err(&["create", "x", "--schema", broken.to_str().unwrap()]).contains("applying schema"));
    assert!(!h.root().join("x").exists(), "failed create leaves no folder behind");
    assert!(h.err(&["--db", "nope", "status"]).contains("no database named 'nope'"));
    assert!(h.err(&["status"]).contains("--db"));

    h.ok(&["--db", "d", "exec", "INSERT INTO t VALUES(1)"]);
    assert!(h.err(&["--db", "d", "exec", "INSERT INTO t VALUES(1)"]).contains("failed:"));
    assert!(h.err(&["--db", "d", "exec", "NOT SQL"]).contains("failed:"));

    // In a batch, statements before the failure are kept and nothing after it runs.
    let out = h.with_stdin(&["--db", "d", "exec", "-"], "INSERT INTO t VALUES(2)\nBOGUS\nINSERT INTO t VALUES(3)\n");
    assert!(!out.status.success());
    assert_eq!(h.commits("d"), 2);
    let mut model = Model::new(Some(t_schema));
    model.exec("INSERT INTO t VALUES(1)", 1);
    model.exec("INSERT INTO t VALUES(2)", 1);
    model.failed("BOGUS");
    assert_invariants(&h.root(), "d", Some(&model), Checks::full());

    let existing = h.path("exists.db");
    std::fs::write(&existing, b"x").unwrap();
    assert!(h.err(&["--db", "d", "clone", "-n", "1", "-o", existing.to_str().unwrap()]).contains("already exists"));
    let out = h.path("far.db");
    assert!(h.err(&["--db", "d", "clone", "-n", "99", "-o", out.to_str().unwrap()]).contains("only 2 commits"));
    assert!(!out.exists());
}

/// Scenario 8: no-op statements produce no WAL frames and so no commit; `-n 0` is the base.
#[test]
fn noops_add_no_commits_and_clone_zero_is_the_base() {
    let schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER);";
    let o = Scenario::new(
        "d",
        schema,
        "d",
        vec![
            Step::Exec("INSERT INTO t VALUES(1, 1)".into()),
            Step::Exec("UPDATE t SET v = 9 WHERE id = 404".into()),
            Step::Exec("UPDATE t SET v = 1 WHERE id = 1".into()),
            Step::Clone(0),
        ],
    )
    .run(Driver::Cli);
    assert_eq!(commits_at(&o, 3), 1);
    assert_eq!(query_clone(&o.clones[0].1, |p| ints(p, "SELECT count(*) FROM t")), [0]);
}

/// Output text of the no-op statements, which the step language does not expose.
#[test]
fn noops_cli_output_text() {
    let h = Harness::new();
    h.create("d", "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER);");
    h.ok(&["--db", "d", "exec", "INSERT INTO t VALUES(1, 1)"]);
    assert!(h.ok(&["--db", "d", "exec", "UPDATE t SET v = 9 WHERE id = 404"]).contains("captured 0 commit"));
    assert!(h.ok(&["--db", "d", "exec", "UPDATE t SET v = 1 WHERE id = 1"]).contains("captured 0 commit"));
    let zero = h.path("zero.db");
    let out = h.ok(&["--db", "d", "clone", "-n", "0", "-o", zero.to_str().unwrap()]);
    assert!(out.contains("t: 0 rows"));
}

/// Scenario 9: a schema file can hold several tables, indexes and seed rows. Seed rows are part
/// of the initial snapshot, so `clone -n 0` has them and commit 1 is the first change after.
#[test]
fn schema_file_with_tables_indexes_and_seed_rows() {
    let schema = "CREATE TABLE products(id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL);
         CREATE TABLE stock(product_id INTEGER REFERENCES products(id), qty INTEGER NOT NULL);
         CREATE INDEX stock_by_product ON stock(product_id);
         INSERT INTO products VALUES (1, 'pen', 1.5), (2, 'ink', 7.25);
         INSERT INTO stock VALUES (1, 100), (2, 20);";
    let o = Scenario::new(
        "shop",
        schema,
        "shop",
        vec![
            Step::Exec("UPDATE stock SET qty = qty - 5 WHERE product_id = 1".into()),
            Step::Clone(0),
            Step::Clone(1),
        ],
    )
    .run(Driver::Cli);

    assert_eq!(commits_at(&o, 0), 0, "schema is the base, not history");
    let (base, after) = (&o.clones[0].1, &o.clones[1].1);
    assert_eq!(query_clone(base, |p| ints(p, "SELECT qty FROM stock ORDER BY product_id")), [100, 20]);
    assert_eq!(query_clone(after, |p| ints(p, "SELECT qty FROM stock ORDER BY product_id")), [95, 20]);
    assert_eq!(
        query_clone(after, |p| texts(p, "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'")),
        ["stock_by_product"]
    );
}

/// Scenario 10: schema piped on stdin (`--schema -`), as from a migration tool or `cat`. The
/// step language always writes a schema file, so this stays a bespoke test.
#[test]
fn schema_from_stdin() {
    let schema = "CREATE TABLE a(x); CREATE TABLE b(y);";
    let h = Harness::new();
    let out = h.with_stdin(&["create", "piped", "--schema", "-"], schema);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let path = h.path("piped.db");
    h.ok(&["--db", "piped", "clone", "-n", "0", "-o", path.to_str().unwrap()]);
    assert_eq!(texts(&path, "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"), ["a", "b"]);

    assert_invariants(&h.root(), "piped", Some(&Model::new(Some(schema))), Checks::full());
}

/// Scenario 11: the shipped seed script, driven end to end (uses `ps` as its data source). The
/// script is not a step list, so the model is not available and the light check set is used.
#[test]
#[cfg(unix)]
fn seed_script_produces_500_commits() {
    let h = Harness::new();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/seed.sh");
    let o = std::process::Command::new("bash")
        .arg(&script)
        .env("WALFLIP", env!("CARGO_BIN_EXE_walflip"))
        .env("WALFLIP_ROOT", h.root())
        .env("WALFLIP_DB", "seeded")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(h.commits("seeded"), 500);
    assert_invariants(&h.root(), "seeded", None, Checks::light());

    let (a, b) = (h.path("a.db"), h.path("b.db"));
    h.ok(&["--db", "seeded", "clone", "-n", "250", "-o", a.to_str().unwrap()]);
    h.ok(&["--db", "seeded", "clone", "-n", "500", "-o", b.to_str().unwrap()]);
    assert_eq!(ints(&a, "SELECT count(*) FROM processes"), [250]);
    assert_eq!(ints(&b, "SELECT count(*) FROM processes"), [150]);
}

fn log_ns(out: &str) -> Vec<u64> {
    out.lines().skip(1).map(|l| l.split_whitespace().next().unwrap().parse().unwrap()).collect()
}

#[test]
fn log_defaults_to_most_recent_ascending_with_head_and_all() {
    let h = Harness::new();
    h.create("notes", NOTES);
    for i in 1..=25 {
        h.exec("notes", &format!("INSERT INTO notes(body) VALUES('n{i}')"));
    }
    let log = |extra: &[&str]| {
        let mut a = vec!["--db", "notes", "log"];
        a.extend_from_slice(extra);
        log_ns(&h.ok(&a))
    };
    assert_eq!(log(&[]), (6..=25).collect::<Vec<_>>(), "default: last 20, ascending");
    assert_eq!(log(&["--limit", "3"]), [23, 24, 25]);
    assert_eq!(log(&["--head", "--limit", "3"]), [1, 2, 3]);
    assert_eq!(log(&["--head"]), (1..=20).collect::<Vec<_>>());
    assert_eq!(log(&["--all"]), (1..=25).collect::<Vec<_>>());
    assert_eq!(log(&["--limit", "0"]), Vec::<u64>::new(), "--limit 0 prints only the header");
    assert!(h.err(&["--db", "notes", "log", "--head", "--all"]).contains("cannot be used"));
}
