//! End-to-end scenarios driving the compiled `walflip` binary, each showing a different way
//! the tool is used.

mod common;
use common::{ints, texts, Harness};

/// Scenario 1: the whole database lifecycle through the CLI.
#[test]
fn lifecycle_create_write_inspect_clone_rename_drop() {
    let h = Harness::new();
    assert!(h.ok(&["list"]).contains("no databases"));

    let out = h.create("notes", "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT);");
    assert!(out.contains("created") && out.contains("page size 4096"));
    assert!(h.root().join("notes").join("live.db").exists());
    assert!(h.root().join("notes").join("base.db").exists());
    assert!(h.root().join("notes").join("meta.db").exists());

    for body in ["first", "second", "third"] {
        let sql = format!("INSERT INTO notes(body) VALUES('{body}')");
        assert!(h.ok(&["--db", "notes", "exec", &sql]).contains("captured 1 commit"));
    }
    assert_eq!(h.commits("notes"), 3);
    assert!(h.ok(&["list"]).contains("notes"));

    let log = h.ok(&["--db", "notes", "log"]);
    assert_eq!(log.lines().filter(|l| l.contains("INSERT INTO notes")).count(), 3);

    let two = h.path("two.db");
    let out = h.ok(&["--db", "notes", "clone", "-n", "2", "-o", two.to_str().unwrap()]);
    assert!(out.contains("integrity_check: ok") && out.contains("notes: 2 rows"));
    assert_eq!(texts(&two, "SELECT body FROM notes ORDER BY id"), ["first", "second"]);

    h.ok(&["rename", "notes", "journal"]);
    assert_eq!(h.commits("journal"), 3, "history survives a rename");
    h.err(&["--db", "notes", "status"]);

    h.ok(&["drop", "journal"]);
    assert!(h.ok(&["list"]).contains("no databases"));
}

/// Scenario 2: bulk loading from stdin, then time travel to every point along the way.
#[test]
fn stdin_batch_then_clone_at_every_commit() {
    let h = Harness::new();
    h.create("ledger", "CREATE TABLE entries(id INTEGER PRIMARY KEY, amount INTEGER);");

    let script = "INSERT INTO entries VALUES(1, 100)\n\nINSERT INTO entries VALUES(2, 250)\n  UPDATE entries SET amount = 175 WHERE id = 2  \nDELETE FROM entries WHERE id = 1\n";
    let out = h.with_stdin(&["--db", "ledger", "exec", "-"], script);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("captured 4 commit(s)"));

    let expect: [&[i64]; 5] = [&[], &[100], &[100, 250], &[100, 175], &[175]];
    for (n, want) in expect.iter().enumerate() {
        let path = h.path(&format!("at{n}.db"));
        h.ok(&["--db", "ledger", "clone", "-n", &n.to_string(), "-o", path.to_str().unwrap()]);
        assert_eq!(ints(&path, "SELECT amount FROM entries ORDER BY id"), *want, "clone -n {n}");
    }
}

/// Scenario 3: configured entirely through the environment, as in a shell session or CI job.
#[test]
fn environment_selects_root_and_database() {
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
    let schema = h.path("env.sql");
    std::fs::write(&schema, "CREATE TABLE t(x INTEGER);").unwrap();
    run(&["create", "envdb", "--schema", schema.to_str().unwrap()]);
    run(&["exec", "INSERT INTO t VALUES(42)"]);
    assert!(run(&["status"]).contains("commits   : 1"));
    assert!(run(&["list"]).contains("envdb"));
}

/// Scenario 4: several databases side by side stay independent.
#[test]
fn multiple_databases_are_isolated() {
    let h = Harness::new();
    h.create("users", "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL);");
    h.create("metrics", "CREATE TABLE samples(ts INTEGER, v REAL);");
    h.ok(&["create", "scratch"]);

    h.ok(&["--db", "users", "exec", "INSERT INTO users VALUES(1,'ada')"]);
    h.ok(&["--db", "users", "exec", "INSERT INTO users VALUES(2,'grace')"]);
    h.ok(&["--db", "metrics", "exec", "INSERT INTO samples VALUES(1, 0.5)"]);
    h.ok(&["--db", "scratch", "exec", "CREATE TABLE tmp(a)"]);

    assert_eq!((h.commits("users"), h.commits("metrics"), h.commits("scratch")), (2, 1, 1));
    let list = h.ok(&["list"]);
    for n in ["metrics", "scratch", "users"] {
        assert!(list.contains(n));
    }

    let u = h.path("users.db");
    h.ok(&["--db", "users", "clone", "-n", "2", "-o", u.to_str().unwrap()]);
    assert_eq!(texts(&u, "SELECT name FROM users ORDER BY id"), ["ada", "grace"]);
    assert!(texts(&u, "SELECT name FROM sqlite_master WHERE name='samples'").is_empty());
}

/// Scenario 5: the recovery story. A destructive statement is committed; clone just before it.
#[test]
fn recover_from_an_accidental_delete() {
    let h = Harness::new();
    h.create("inventory", "CREATE TABLE items(sku INTEGER PRIMARY KEY, qty INTEGER);");
    let load: String = (1..=50).map(|i| format!("INSERT INTO items VALUES({i}, {})\n", i * 10)).collect();
    h.with_stdin(&["--db", "inventory", "exec", "-"], &load);
    h.ok(&["--db", "inventory", "exec", "DELETE FROM items WHERE sku > 10"]);
    h.ok(&["--db", "inventory", "exec", "UPDATE items SET qty = 0"]);
    assert_eq!(h.commits("inventory"), 52);

    // Find the DELETE in the log and clone the commit before it.
    let log = h.ok(&["--db", "inventory", "log", "--limit", "100"]);
    let bad: u64 = log
        .lines()
        .find(|l| l.contains("DELETE FROM items"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .parse()
        .unwrap();
    let restored = h.path("restored.db");
    h.ok(&["--db", "inventory", "clone", "-n", &(bad - 1).to_string(), "-o", restored.to_str().unwrap()]);

    assert_eq!(ints(&restored, "SELECT count(*) FROM items"), [50]);
    assert_eq!(ints(&restored, "SELECT sum(qty) FROM items"), [(1..=50).map(|i| i * 10).sum::<i64>()]);
}

/// Scenario 6: how you write does not change history. One process per statement and one batch
/// produce the same log and the same data.
#[test]
fn separate_processes_and_one_batch_agree() {
    let h = Harness::new();
    let stmts: Vec<String> = (0..30)
        .map(|i| match i % 3 {
            0 => format!("INSERT INTO t VALUES({i}, 'v{i}')"),
            1 => format!("UPDATE t SET v = 'u{i}' WHERE id = {}", i - 1),
            _ => format!("DELETE FROM t WHERE id = {}", i - 2),
        })
        .collect();
    for db in ["one_by_one", "batched"] {
        h.create(db, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);");
    }
    for s in &stmts {
        h.ok(&["--db", "one_by_one", "exec", s]);
    }
    let batch = stmts.join("\n");
    assert!(h.with_stdin(&["--db", "batched", "exec", "-"], &batch).status.success());

    let n = h.commits("one_by_one");
    assert_eq!(n, h.commits("batched"));
    let (a, b) = (h.path("a.db"), h.path("b.db"));
    h.ok(&["--db", "one_by_one", "clone", "-n", &n.to_string(), "-o", a.to_str().unwrap()]);
    h.ok(&["--db", "batched", "clone", "-n", &n.to_string(), "-o", b.to_str().unwrap()]);
    let q = "SELECT id || ':' || v FROM t ORDER BY id";
    assert_eq!(texts(&a, q), texts(&b, q));
}

/// Scenario 7: failure modes are reported, exit non-zero, and leave state intact.
#[test]
fn errors_are_clear_and_state_is_preserved() {
    let h = Harness::new();
    h.create("d", "CREATE TABLE t(id INTEGER PRIMARY KEY);");

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
    assert_eq!(h.commits("d"), 1, "failed statements are not captured");

    // In a batch, statements before the failure are kept.
    let out = h.with_stdin(&["--db", "d", "exec", "-"], "INSERT INTO t VALUES(2)\nBOGUS\nINSERT INTO t VALUES(3)\n");
    assert!(!out.status.success());
    assert_eq!(h.commits("d"), 2);

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
    let h = Harness::new();
    h.create("d", "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER);");
    h.ok(&["--db", "d", "exec", "INSERT INTO t VALUES(1, 1)"]);
    assert!(h.ok(&["--db", "d", "exec", "UPDATE t SET v = 9 WHERE id = 404"]).contains("captured 0 commit"));
    assert!(h.ok(&["--db", "d", "exec", "UPDATE t SET v = 1 WHERE id = 1"]).contains("captured 0 commit"));
    assert_eq!(h.commits("d"), 1);

    let zero = h.path("zero.db");
    let out = h.ok(&["--db", "d", "clone", "-n", "0", "-o", zero.to_str().unwrap()]);
    assert!(out.contains("t: 0 rows"));
}

/// Scenario 9: a schema file can hold several tables, indexes and seed rows. Seed rows are part
/// of the initial snapshot, so `clone -n 0` has them and commit 1 is the first change after.
#[test]
fn schema_file_with_tables_indexes_and_seed_rows() {
    let h = Harness::new();
    h.create(
        "shop",
        "CREATE TABLE products(id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL);
         CREATE TABLE stock(product_id INTEGER REFERENCES products(id), qty INTEGER NOT NULL);
         CREATE INDEX stock_by_product ON stock(product_id);
         INSERT INTO products VALUES (1, 'pen', 1.5), (2, 'ink', 7.25);
         INSERT INTO stock VALUES (1, 100), (2, 20);",
    );
    assert_eq!(h.commits("shop"), 0, "schema is the base, not history");

    h.ok(&["--db", "shop", "exec", "UPDATE stock SET qty = qty - 5 WHERE product_id = 1"]);
    let (base, after) = (h.path("base.db"), h.path("after.db"));
    h.ok(&["--db", "shop", "clone", "-n", "0", "-o", base.to_str().unwrap()]);
    h.ok(&["--db", "shop", "clone", "-n", "1", "-o", after.to_str().unwrap()]);

    assert_eq!(ints(&base, "SELECT qty FROM stock ORDER BY product_id"), [100, 20]);
    assert_eq!(ints(&after, "SELECT qty FROM stock ORDER BY product_id"), [95, 20]);
    assert_eq!(texts(&after, "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'"), ["stock_by_product"]);
}

/// Scenario 10: schema piped on stdin (`--schema -`), as from a migration tool or `cat`.
#[test]
fn schema_from_stdin() {
    let h = Harness::new();
    let out = h.with_stdin(&["create", "piped", "--schema", "-"], "CREATE TABLE a(x); CREATE TABLE b(y);");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let path = h.path("piped.db");
    h.ok(&["--db", "piped", "clone", "-n", "0", "-o", path.to_str().unwrap()]);
    assert_eq!(texts(&path, "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"), ["a", "b"]);
}

/// Scenario 11: the shipped seed script, driven end to end (uses `ps` as its data source).
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

    let (a, b) = (h.path("a.db"), h.path("b.db"));
    h.ok(&["--db", "seeded", "clone", "-n", "250", "-o", a.to_str().unwrap()]);
    h.ok(&["--db", "seeded", "clone", "-n", "500", "-o", b.to_str().unwrap()]);
    assert_eq!(ints(&a, "SELECT count(*) FROM processes"), [250]);
    assert_eq!(ints(&b, "SELECT count(*) FROM processes"), [150]);
}
