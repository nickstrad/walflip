//! Concurrency scenarios S12-S15 (docs/TESTING_PLAN.md section 3.2).
//!
//! Every barrier is a pipe (see `common::helpers`); there are no sleeps. S13 and S15 were red
//! for B4 and B3 until T16 (history write lock per statement, `(recovered)` pre-capture).
#![cfg(unix)]

mod common;

use common::helpers::{self, Helper};
use common::invariants::{assert_invariants, Checks, FileHashes};
use common::model::Model;
use common::{open_ro, parse_captured, Harness};
use std::collections::BTreeMap;
use std::fs;
use walflip::db::{self, Db};

/// Dispatch point for helper processes; a no-op in a normal run.
#[test]
fn helper_process_entry() {
    helpers::entry();
}

const SCHEMA: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";

fn insert(v: &str) -> String {
    format!("INSERT INTO t(v) VALUES('{v}')")
}

/// Create `name` and run `n` inserts as one stdin batch; returns the model.
fn seeded(h: &Harness, name: &str, n: usize) -> Model {
    h.create(name, SCHEMA);
    let mut model = Model::new(Some(SCHEMA));
    if n > 0 {
        let stmts: Vec<String> = (1..=n).map(|i| insert(&format!("r{i}"))).collect();
        let o = h.with_stdin(&["--db", name, "exec", "-"], &stmts.join("\n"));
        assert!(o.status.success(), "seed batch: {}", String::from_utf8_lossy(&o.stderr));
        model.exec_batch(&stmts, parse_captured(&String::from_utf8(o.stdout).unwrap()));
    }
    model
}

/// (cursor_salt, cursor_commits) as stored in meta.db.
fn cursor(d: &Db) -> (Option<String>, Option<String>) {
    let conn = open_ro(&d.meta);
    let get = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get(0)).ok()
    };
    (get("cursor_salt"), get("cursor_commits"))
}

/// S12 (INV-49, INV-35, INV-34): 8 concurrent clones of one db, half `-n 10`, half `-n 20`, to
/// distinct paths, plus 2 clones racing for the same path. Every distinct-path clone succeeds
/// and equals the sequential clone for its k; the raced path ends up as a valid clone; the db
/// folder is untouched.
#[test]
fn s12_concurrent_clones() {
    let h = Harness::new();
    let model = seeded(&h, "c", 25);
    assert_invariants(&h.root(), "c", Some(&model), Checks::light());

    let d = Db::existing(&h.root(), "c").unwrap();
    let reference = |k: u64| {
        let p = h.path(&format!("ref-{k}.db"));
        db::clone_at(&d, k, &p).unwrap();
        fs::read(&p).unwrap()
    };
    let refs: BTreeMap<u64, Vec<u8>> = [10, 20].into_iter().map(|k| (k, reference(k))).collect();
    let before = FileHashes::of(&d.dir);

    let root = h.root();
    let mut jobs: Vec<(u64, std::path::PathBuf)> =
        (0..8).map(|i| (if i % 2 == 0 { 10 } else { 20 }, h.path(&format!("out-{i}.db")))).collect();
    let raced = h.path("raced.db");
    jobs.push((10, raced.clone()));
    jobs.push((10, raced.clone()));

    let mut gates: Vec<Helper> = jobs
        .iter()
        .map(|(k, p)| {
            helpers::gated_walflip(&root, &["--db", "c", "clone", "-n", &k.to_string(), "-o", p.to_str().unwrap()])
        })
        .collect();
    helpers::release_all(&mut gates, "");
    let done: Vec<_> = gates.into_iter().map(Helper::finish).collect();

    for (i, ((k, p), f)) in jobs.iter().zip(&done).enumerate().take(8) {
        assert!(f.status.success(), "INV-49: clone #{i} -n {k} failed: {}", f.stderr);
        assert!(f.stdout.contains("integrity_check: ok"), "INV-49: clone #{i}: {}", f.stdout);
        assert!(fs::read(p).unwrap() == refs[k], "INV-49: clone #{i} -n {k} differs from the sequential clone");
        for suf in ["-wal", "-shm", "-journal"] {
            let mut side = p.clone().into_os_string();
            side.push(suf);
            assert!(!std::path::Path::new(&side).exists(), "INV-34: clone #{i} left a {suf} file");
        }
    }

    // The racers (B9): `clone_at` opens the output with create_new, so exactly one succeeds and
    // the other fails with `already exists`; the file is a complete clone.
    let racers = &done[8..];
    let wins = racers.iter().filter(|f| f.status.success()).count();
    assert_eq!(wins, 1, "INV-35: expected exactly one racer to succeed, got {wins}: {racers:?}");
    for f in racers.iter().filter(|f| !f.status.success()) {
        assert!(f.stderr.contains("already exists"), "INV-35: racer failed for another reason: {}", f.stderr);
    }
    assert!(fs::read(&raced).unwrap() == refs[&10], "INV-35: raced output is not the -n 10 clone");

    assert_eq!(before, FileHashes::of(&d.dir), "INV-34: concurrent clones changed the db folder");
    assert_invariants(&h.root(), "c", Some(&model), Checks::light());
}

/// S13 (INV-24): 4 concurrent `exec -` batches of 20 inserts on one db must produce exactly
/// 80 commits, one per statement, with matching labels and no errors.
#[test]
fn s13_concurrent_exec() {
    const PROCS: usize = 4;
    const PER: usize = 20;
    let h = Harness::new();
    h.create("x", "CREATE TABLE t(id INTEGER PRIMARY KEY, w INTEGER, i INTEGER);");
    let root = h.root();

    let batches: Vec<Vec<String>> = (0..PROCS)
        .map(|w| (0..PER).map(|i| format!("INSERT INTO t(w, i) VALUES({w}, {i})")).collect())
        .collect();
    let mut gates: Vec<Helper> = (0..PROCS).map(|_| helpers::gated_walflip(&root, &["--db", "x", "exec", "-"])).collect();
    for (g, b) in gates.iter_mut().zip(&batches) {
        g.release(format!("go\n{}\n", b.join("\n")).as_bytes());
    }
    let done: Vec<_> = gates.into_iter().map(Helper::finish).collect();

    let failures: Vec<String> = done
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.status.success())
        .map(|(w, f)| format!("proc {w}: {}", f.stderr.trim()))
        .collect();
    let captured: u64 = done
        .iter()
        .filter(|f| f.status.success())
        .map(|f| parse_captured(&f.stdout))
        .sum();

    let d = Db::existing(&root, "x").unwrap();
    let status = db::status(&d).unwrap();
    let log = db::log(&d, u64::MAX >> 1).unwrap();
    let mut labels: BTreeMap<String, usize> = BTreeMap::new();
    for e in &log {
        *labels.entry(e.sql.clone()).or_default() += 1;
    }
    let expected_labels: Vec<&String> = batches.iter().flatten().collect();
    let missing = expected_labels.iter().filter(|s| !labels.contains_key(s.as_str())).count();
    let duplicated: usize = labels.values().filter(|&&c| c > 1).map(|c| c - 1).sum();
    let rows: i64 = {
        // Ask walflip: clone the full history and count rows (live.db may still have a WAL).
        let out = h.path("all.db");
        db::clone_at(&d, status.commits, &out).unwrap();
        open_ro(&out).query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap()
    };

    let want = (PROCS * PER) as u64;
    let ok = failures.is_empty()
        && status.commits == want
        && captured == want
        && missing == 0
        && duplicated == 0
        && rows == want as i64;
    assert!(
        ok,
        "INV-24 (B4): expected {want} commits, {want} captured, 0 failed procs, {want} rows in clone, \
         every label once; got {} commits, {captured} captured, {} failed procs, {rows} rows in clone, \
         {missing} labels missing, {duplicated} duplicate labels. failures: {failures:#?}",
        status.commits,
        failures.len(),
    );
    assert_invariants(&root, "x", None, Checks::light());
}

/// S14 (INV-48, INV-23): a reader holds `BEGIN; SELECT` on live.db while 10 `exec`s run, so
/// SQLite cannot checkpoint the WAL; then it is released and 10 more `exec`s run. Each step
/// captures exactly one commit, the cursor matches the WAL, and the final state is exact.
#[test]
fn s14_concurrent_reader() {
    let h = Harness::new();
    let mut model = seeded(&h, "r", 0);
    let root = h.root();
    let d = Db::existing(&root, "r").unwrap();
    let wal_path = d.wal_path();

    let (reader, seen) = helpers::reader(&d.live, "SELECT count(*) FROM t");
    assert_eq!(seen, 0);

    for i in 1..=10u64 {
        let sql = insert(&format!("held{i}"));
        let n = h.exec("r", &sql);
        assert_eq!(n, 1, "INV-48: exec {i} under a held reader captured {n} commits");
        model.exec(&sql, n);
        let o = assert_invariants(&root, "r", Some(&model), Checks::full());
        assert!(o.wal_bytes.is_some(), "INV-48: WAL vanished while a reader held it (exec {i})");

        // INV-23: the stored cursor names this WAL generation and counts all of its commits.
        let bytes = fs::read(&wal_path).unwrap();
        let wal = walflip::wal::parse(&bytes).unwrap().expect("WAL has a valid header");
        assert_eq!(wal.commits.len() as u64, i, "INV-48: WAL was checkpointed/restarted under a reader (exec {i})");
        let (salt, count) = cursor(&d);
        assert_eq!(salt, Some(format!("{}:{}", wal.salt.0, wal.salt.1)), "INV-23: cursor_salt after exec {i}");
        assert_eq!(count, Some(wal.commits.len().to_string()), "INV-23: cursor_commits after exec {i}");
    }

    let mut reader = reader;
    reader.release(b"");
    let f = reader.finish();
    assert!(f.status.success(), "reader failed: {}", f.stderr);
    assert_eq!(f.stdout.trim(), "RELEASED 0", "reader snapshot was not stable");

    for i in 11..=20u64 {
        let sql = insert(&format!("free{i}"));
        let n = h.exec("r", &sql);
        assert_eq!(n, 1, "INV-48: exec {i} after release captured {n} commits (duplicates?)");
        model.exec(&sql, n);
        assert_invariants(&root, "r", Some(&model), Checks::light());
    }
    // Last connection closed by the final exec: the WAL is folded back and removed, so the
    // strong oracle (clone(total) == live.db bytes) ran inside the last assert_invariants.
    let o = assert_invariants(&root, "r", Some(&model), Checks::full());
    assert_eq!(o.wal_bytes, None, "INV-48: WAL still present after the final exec");
    assert_eq!(o.commits, 20);
}

/// Q1: a process that dies while holding the history write lock (an open `BEGIN IMMEDIATE` with
/// an uncommitted change on meta.db) leaves no stale lock: the next `exec` gets the lock at once
/// (well within the busy timeout), the dead transaction is rolled back, history stays exact.
#[test]
fn s15b_lock_holder_dies() {
    let h = Harness::new();
    let mut model = seeded(&h, "l", 2);
    let root = h.root();
    let d = Db::existing(&root, "l").unwrap();

    let f = helpers::crash_writer(&d.meta, &["BEGIN IMMEDIATE", "INSERT INTO meta(key, value) VALUES('junk', 'x')"]);
    assert!(f.status.success() && f.stdout.contains("WROTE 2"), "lock holder: {f:?}");

    let t = std::time::Instant::now();
    let sql = insert("after");
    let n = h.exec("l", &sql);
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "exec waited {:?} for a dead lock", t.elapsed());
    model.exec(&sql, n);
    assert_invariants(&root, "l", Some(&model), Checks::full());
    let junk: i64 =
        open_ro(&d.meta).query_row("SELECT count(*) FROM meta WHERE key='junk'", [], |r| r.get(0)).unwrap();
    assert_eq!(junk, 0, "the dead process's uncommitted write survived");
}

/// S15 (INV-25): a writer dies without closing SQLite, leaving 3 uncaptured commits in
/// live.db-wal. `status` and `clone` still work; the next `exec` must not attribute the orphan
/// commits to its own statement (expected: they are captured under a `(recovered)` label).
#[test]
fn s15_writer_dies_mid_life() {
    let h = Harness::new();
    h.create("w", SCHEMA);
    let root = h.root();
    let d = Db::existing(&root, "w").unwrap();

    let orphans = [insert("orphan1"), insert("orphan2"), insert("orphan3")];
    let refs: Vec<&str> = orphans.iter().map(String::as_str).collect();
    let f = helpers::crash_writer(&d.live, &refs);
    assert!(f.status.success() && f.stdout.contains("WROTE 3"), "crash writer: {f:?}");

    // History knows nothing yet; the WAL holds the orphans; clone -n 0 is still base.db.
    let o = assert_invariants(&root, "w", None, Checks::full());
    assert_eq!(o.commits, 0);
    assert!(o.wal_bytes.unwrap_or(0) > 0, "S15: crashed writer left no WAL: {:?}", o.wal_bytes);

    // Plan Q3: `status` reports the orphans, `exec` warns on stderr and counts them.
    let st = h.ok(&["--db", "w", "status"]);
    assert!(st.contains("uncaptured: 3"), "S15: status should report 3 uncaptured commits: {st}");

    let stmt = insert("after");
    let out = h.with_stdin(&["--db", "w", "exec", &stmt], "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "S15: exec failed: {stderr}");
    assert!(stderr.contains("warning") && stderr.contains("(recovered)"), "S15: no recovery warning: {stderr}");
    let n = parse_captured(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(n, 4, "S15: exec reports the 3 recovered commits plus its own");

    // The rows themselves must all be in history (INV-39/INV-31 via the strong oracle).
    let o = assert_invariants(&root, "w", None, Checks::full());
    let labelled = o.log.iter().filter(|r| r.sql == stmt).count();
    let recovered = o.log.iter().filter(|r| r.sql.to_lowercase().contains("recovered")).count();
    assert!(
        o.commits == 4 && labelled == 1 && recovered == 3,
        "INV-25 (B3): expected 4 commits, 1 labelled {stmt:?} and 3 labelled (recovered); \
         got {} commits, {labelled} labelled with the statement, {recovered} (recovered); exec printed captured {n}",
        o.commits
    );
    // Order: the orphans come first, the statement last.
    assert_eq!(o.log.last().map(|r| r.sql.as_str()), Some(stmt.as_str()), "INV-25: statement is not the last commit");
    let st = h.ok(&["--db", "w", "status"]);
    assert!(!st.contains("uncaptured"), "S15: nothing should be left uncaptured: {st}");
}
