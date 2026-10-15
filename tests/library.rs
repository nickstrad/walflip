//! Library-level scenarios (docs/TESTING_PLAN.md section 3.2): steps the CLI cannot express,
//! driven through `walflip::db`, `walflip::store` and `walflip::replay` directly.
//!
//! - S9: `VACUUM` shrinks the db; history records the shrink and replays it (INV-42). (a) through
//!   `exec` (plan Q4: one commit, also inside a stdin batch; `VACUUM INTO` and look-alikes are
//!   rejected without changing anything, INV-37); (b) by an external connection.
//! - S11: clone chain; replaying frames n+1..=m onto clone(n) gives clone(m) (INV-32, INV-31).
//! - INV-23: the capture cursor matches the live WAL header while a reader pins the WAL.
mod common;

use common::invariants::{assert_invariants, Checks, FileHashes, Observed};
use common::model::Model;
use common::{parse_captured, Harness};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use walflip::db::{self, Db};
use walflip::store::Store;
use walflip::{replay, wal};

/// Fresh managed db named `name` under `root`, created through the library.
fn create(root: &Path, name: &str, schema: &str) -> Db {
    let d = Db::new(root, name).unwrap();
    db::create(&d, Some(schema)).unwrap();
    d
}

/// Run one statement through `db::exec` and feed the model; returns the commits captured.
fn exec(d: &Db, m: &mut Model, sql: &str) -> u64 {
    let n = db::exec(d, &[sql.to_string()]).unwrap_or_else(|e| panic!("exec {sql}: {e:#}")) as u64;
    m.exec(sql, n);
    n
}

/// ExternalVacuum step: a foreign connection vacuums live.db with checkpointing off, and the
/// still-present WAL is captured before that connection closes (and checkpoints it away).
/// `exec VACUUM` does the same through walflip (S9a); this step covers a foreign VACUUM.
fn external_vacuum(d: &Db, m: &mut Model) {
    let conn = Connection::open(&d.live).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch("VACUUM").unwrap();
    let added = Store::open(&d.meta)
        .unwrap()
        .capture(&d.wal_path(), "VACUUM")
        .unwrap();
    assert_eq!(added, 1, "VACUUM should be exactly one new commit");
    m.exec_sum += 1;
    m.snapshots.push(Some(m.current())); // contents unchanged, one more commit
}

/// Bytes of `clone -n k`, written to a fresh file under `dir`.
fn clone_bytes(d: &Db, k: u64, dir: &Path) -> Vec<u8> {
    let out = dir.join(format!(
        "clone-{k}-{}.db",
        fs::read_dir(dir).unwrap().count()
    ));
    db::clone_at(d, k, &out).unwrap_or_else(|e| panic!("clone -n {k}: {e:#}"));
    fs::read(out).unwrap()
}

fn page_size(d: &Db) -> usize {
    Store::open(&d.meta).unwrap().page_size().unwrap().unwrap() as usize
}

fn root_and_tmp() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("data");
    fs::create_dir_all(&root).unwrap();
    (tmp, root)
}

// ---------------------------------------------------------------------------------------------
// S9: VACUUM / shrink
// ---------------------------------------------------------------------------------------------

const S9_SCHEMA: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, body BLOB);";

/// Model step for a `VACUUM` that walflip captured as `n` commits: contents are unchanged, so
/// each commit's snapshot is the current one (the shadow cannot run VACUUM inside its
/// per-statement transaction).
fn vacuum_in_model(m: &mut Model, n: u64) {
    m.exec_sum += n;
    for _ in 0..n {
        m.snapshots.push(Some(m.current()));
    }
}

/// Run `sql` (a bare VACUUM form) through `db::exec`; it must be exactly one commit.
fn exec_vacuum(d: &Db, m: &mut Model, sql: &str) {
    let n = db::exec(d, &[sql.to_string()]).unwrap_or_else(|e| panic!("exec {sql}: {e:#}")) as u64;
    assert_eq!(n, 1, "Q4: `{sql}` should be exactly one commit");
    vacuum_in_model(m, n);
}

/// Q4: `exec VACUUM` runs outside a transaction and is captured as one commit; `db_pages`
/// shrinks at it (INV-42) and `clone -n total` is `live.db` byte for byte (INV-31).
#[test]
fn s9a_exec_vacuum_is_one_shrinking_commit() {
    let (tmp, root) = root_and_tmp();
    let d = create(&root, "vac", S9_SCHEMA);
    let mut m = Model::new(Some(S9_SCHEMA));
    for i in 0..4 {
        exec(
            &d,
            &mut m,
            &format!("INSERT INTO t(body) VALUES (zeroblob({}))", 30000 + i),
        );
    }
    exec(&d, &mut m, "DELETE FROM t WHERE id > 1");
    let pages_before = Observed::of(&d).log.last().unwrap().db_pages;

    exec_vacuum(&d, &mut m, "VACUUM");
    let o = assert_invariants(&root, "vac", Some(&m), Checks::full());
    let s = o.commits;
    let last = o.log.last().unwrap();
    assert_eq!(last.sql, "VACUUM");
    assert!(
        last.db_pages < pages_before,
        "INV-42: db_pages {pages_before} -> {}",
        last.db_pages
    );
    let c_prev = clone_bytes(&d, s - 1, tmp.path());
    let c_s = clone_bytes(&d, s, tmp.path());
    assert!(
        c_s.len() < c_prev.len(),
        "INV-42: clone({s}) !< clone({})",
        s - 1
    );
    assert!(
        fs::metadata(d.wal_path()).is_err(),
        "WAL checkpointed away after exec"
    );
    assert!(
        c_s == fs::read(&d.live).unwrap(),
        "INV-31: clone(total) != live.db after exec VACUUM"
    );

    // Lowercase, trailing `;`, and the explicit `main` schema take the same path.
    for sql in ["vacuum;", "VACUUM main", "  Vacuum \"main\" ;  "] {
        exec_vacuum(&d, &mut m, sql);
        assert_eq!(Observed::of(&d).log.last().unwrap().sql, sql);
    }
    exec(&d, &mut m, "INSERT INTO t(body) VALUES (zeroblob(40000))");
    let fin = assert_invariants(&root, "vac", Some(&m), Checks::full());
    assert!(
        clone_bytes(&d, fin.commits, tmp.path()) == fs::read(&d.live).unwrap(),
        "INV-31 after growth"
    );
}

/// Q4: `VACUUM INTO` stays denied, in any spelling that reaches SQLite: nothing is committed, no
/// file of the db folder changes (INV-37), and the target file is never created.
#[test]
fn s9a_vacuum_into_is_denied_and_creates_no_file() {
    let (tmp, root) = root_and_tmp();
    let d = create(&root, "vac", S9_SCHEMA);
    let mut m = Model::new(Some(S9_SCHEMA));
    exec(&d, &mut m, "INSERT INTO t(body) VALUES (zeroblob(5000))");
    let target = tmp.path().join("copy.db");
    let t = target.display();
    for (i, sql) in [
        format!("VACUUM INTO '{t}'"),
        format!("vacuum main into '{t}';"),
        format!("SELECT 1; VACUUM INTO '{t}'"),
        format!("/* c */ VACUUM INTO '{t}'"),
    ]
    .iter()
    .enumerate()
    {
        let (obs, hashes) = (Observed::of(&d), FileHashes::of(&d.dir));
        let err = format!(
            "{:#}",
            db::exec(&d, &[sql.clone()]).expect_err("VACUUM INTO must fail")
        );
        if i < 2 {
            assert!(
                err.contains("VACUUM INTO") && err.contains("not allowed"),
                "clear message: {err}"
            );
        }
        assert!(!target.exists(), "VACUUM INTO created {t}: {sql}");
        assert_eq!(
            Observed::of(&d),
            obs,
            "INV-37: status/log changed after: {sql}"
        );
        let after = FileHashes::of(&d.dir);
        for f in ["live.db", "base.db", "meta.db"] {
            assert_eq!(hashes.0[f], after.0[f], "INV-37: {f} changed after: {sql}");
        }
        m.failed(sql);
    }
    assert_invariants(&root, "vac", Some(&m), Checks::full());
}

/// Q4 with `exec -`: VACUUM on its own line between inserts is one commit among three; a line
/// that only starts with VACUUM (`VACUUM; DROP TABLE t`, one statement per line) is rejected as a
/// whole, so `t` is not dropped, earlier lines stay committed and later lines never run.
#[test]
fn s9a_vacuum_in_a_stdin_batch() {
    let h = Harness::new();
    h.create("vb", S9_SCHEMA);
    let d = Db::existing(&h.root(), "vb").unwrap();
    let mut m = Model::new(Some(S9_SCHEMA));
    let (a, b) = (
        "INSERT INTO t(body) VALUES (zeroblob(9000))",
        "DELETE FROM t WHERE id = 1",
    );
    let o = h.with_stdin(
        &["--db", "vb", "exec", "-"],
        &format!("{a}\n{a}\n{b}\nVACUUM\n{a}\n"),
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(parse_captured(&String::from_utf8(o.stdout).unwrap()), 5);
    m.exec(a, 1);
    m.exec(a, 1);
    m.exec(b, 1);
    vacuum_in_model(&mut m, 1);
    m.exec(a, 1);
    let labels: Vec<String> = Observed::of(&d).log.iter().map(|r| r.sql.clone()).collect();
    assert_eq!(labels, [a, a, b, "VACUUM", a]);
    assert_invariants(&h.root(), "vb", Some(&m), Checks::full());

    let o = h.with_stdin(
        &["--db", "vb", "exec", "-"],
        &format!("{a}\nVACUUM; DROP TABLE t\n{a}\n"),
    );
    assert!(!o.status.success());
    let err = String::from_utf8(o.stderr).unwrap();
    assert!(
        err.contains("only a bare `VACUUM`") && err.contains("statement 2"),
        "{err}"
    );
    m.exec(a, 1);
    let o = assert_invariants(&h.root(), "vb", Some(&m), Checks::full());
    assert_eq!(
        o.commits, 6,
        "line 1 committed, line 2 rejected, line 3 never ran"
    );
    // The same text as one `exec` argument is one statement too, so it is rejected whole.
    let err = h.err(&["--db", "vb", "exec", "VACUUM; DROP TABLE t"]);
    assert!(err.contains("only a bare `VACUUM`"), "{err}");
    assert_eq!(
        common::ints(
            &d.live,
            "SELECT count(*) FROM sqlite_master WHERE name = 't'"
        ),
        [1]
    );
    assert_invariants(&h.root(), "vb", Some(&m), Checks::full());
}

#[test]
fn s9b_external_vacuum_shrink_is_captured_and_replayed() {
    let (tmp, root) = root_and_tmp();
    let d = create(&root, "vac", S9_SCHEMA);
    let mut m = Model::new(Some(S9_SCHEMA));
    for i in 0..4 {
        exec(
            &d,
            &mut m,
            &format!("INSERT INTO t(body) VALUES (zeroblob({}))", 30000 + i),
        );
    }
    // Free most pages; without auto_vacuum they stay in the freelist, so db_pages is unchanged.
    exec(&d, &mut m, "DELETE FROM t WHERE id > 1");
    let pre = Observed::of(&d);
    let pages_before = pre.log.last().unwrap().db_pages;

    external_vacuum(&d, &mut m);
    let after = Observed::of(&d);
    let s = after.commits;
    let pages_vac = after.log[s as usize - 1].db_pages;
    assert_eq!(after.log[s as usize - 1].sql, "VACUUM");
    assert!(
        pages_vac < pages_before,
        "INV-42: log db_pages should decrease at the VACUUM commit: {pages_before} -> {pages_vac}"
    );

    // len(clone(s)) < len(clone(s-1)), both valid.
    let ps = page_size(&d) as u64;
    let c_prev = clone_bytes(&d, s - 1, tmp.path());
    let c_s = clone_bytes(&d, s, tmp.path());
    assert!(
        c_s.len() < c_prev.len(),
        "INV-42: clone({s}) {} bytes !< clone({}) {} bytes",
        c_s.len(),
        s - 1,
        c_prev.len()
    );
    assert_eq!(c_s.len() as u64, pages_vac * ps);
    assert_invariants(&root, "vac", Some(&m), Checks::full());

    // Later growth past the shrunken size replays correctly.
    for i in 0..3 {
        exec(
            &d,
            &mut m,
            &format!("INSERT INTO t(body) VALUES (zeroblob({}))", 50000 + i),
        );
    }
    exec(&d, &mut m, "UPDATE t SET body = zeroblob(10) WHERE id = 1");
    let fin = assert_invariants(&root, "vac", Some(&m), Checks::full());
    assert!(
        fin.log.last().unwrap().db_pages > pages_vac,
        "growth after the shrink"
    );
    let pages: Vec<u64> = fin.log.iter().map(|r| r.db_pages).collect();
    eprintln!("S9 db_pages per commit: {pages:?}");

    // Final clone == live bytes (INV-31; also inside assert_invariants, asserted here explicitly).
    assert!(
        fs::metadata(d.wal_path()).is_err(),
        "WAL checkpointed away after the last exec"
    );
    assert!(
        clone_bytes(&d, fin.commits, tmp.path()) == fs::read(&d.live).unwrap(),
        "INV-31: final clone != live.db"
    );
}

// ---------------------------------------------------------------------------------------------
// S11: clone chain
// ---------------------------------------------------------------------------------------------

/// Captured frames of commits `from..=to` (1-based) in replay order, read straight from meta.db.
fn frames_between(d: &Db, from: u64, to: u64) -> Vec<(u32, Vec<u8>)> {
    let s = Store::open(&d.meta).unwrap();
    let mut st = s
        .conn()
        .prepare("SELECT pgno, page FROM frames WHERE commit_idx BETWEEN ?1 AND ?2 ORDER BY commit_idx, seq")
        .unwrap();
    let rows = st
        .query_map([from, to], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    rows.collect::<Result<_, _>>().unwrap()
}

/// Marker in the S11 script for the library-only external VACUUM step.
const VACUUM_STEP: &str = "-- external VACUUM";

fn s11_statements() -> Vec<String> {
    let mut v = Vec::new();
    for i in 0..12 {
        v.push(format!(
            "INSERT INTO kv(k, v) VALUES ('k{i}', zeroblob({}))",
            100 + i * 700
        ));
    }
    v.push("CREATE INDEX kv_v ON kv(length(v))".into());
    for i in (0..12).step_by(3) {
        v.push(format!("UPDATE kv SET v = zeroblob(5) WHERE k = 'k{i}'"));
    }
    v.push("DELETE FROM kv WHERE rowid % 2 = 0".into());
    v.push(VACUUM_STEP.into()); // external VACUUM: the shrinking commit
    v.push("CREATE TABLE other(x)".into());
    for i in 0..6 {
        v.push(format!("INSERT INTO other VALUES ({i})"));
    }
    v.push("DROP INDEX kv_v".into());
    for i in 0..6 {
        v.push(format!(
            "INSERT INTO kv(k, v) VALUES ('n{i}', zeroblob(3000))"
        ));
    }
    v
}

#[test]
fn s11_clone_chain() {
    // The shrink comes from an external VACUUM (auto_vacuum=FULL in the schema would also work
    // now that `create` runs the schema before enabling WAL).
    let schema = "CREATE TABLE kv(k TEXT PRIMARY KEY, v BLOB);";
    let (tmp, root) = root_and_tmp();
    let d = create(&root, "chain", schema);
    let mut m = Model::new(Some(schema));
    for s in s11_statements() {
        if s == VACUUM_STEP {
            external_vacuum(&d, &mut m);
            continue;
        }
        assert_eq!(exec(&d, &mut m, &s), 1, "every S11 statement writes: {s}");
    }
    let o = assert_invariants(&root, "chain", Some(&m), Checks::full());
    assert!(o.commits >= 30, "S11 needs 30 commits, got {}", o.commits);
    let total = o.commits;
    let ps = o.page_size as usize;
    let pages: Vec<u64> = o.log.iter().map(|r| r.db_pages).collect();
    assert!(
        pages.windows(2).any(|w| w[1] < w[0]),
        "S11 should include a shrinking commit: {pages:?}"
    );

    let clones: Vec<Vec<u8>> = (0..=total)
        .map(|k| clone_bytes(&d, k, tmp.path()))
        .collect();
    // INV-31: with the WAL checkpointed, clone(total) is live.db byte for byte.
    assert!(fs::metadata(d.wal_path()).is_err());
    assert!(
        clones[total as usize] == fs::read(&d.live).unwrap(),
        "INV-31: clone({total}) != live.db"
    );

    // INV-32 for every pair n < m (31 commits -> 496 pairs; dbs are small).
    let mut failures = Vec::new();
    for n in 0..total {
        for mm in n + 1..=total {
            let frames = frames_between(&d, n + 1, mm);
            let db_pages = pages[mm as usize - 1] as u32;
            let mut img = clones[n as usize].clone();
            replay::apply(
                &mut img,
                ps,
                frames.iter().map(|(p, b)| (*p, b.as_slice())),
                Some(db_pages),
                u32::MAX,
            )
            .unwrap();
            if img != clones[mm as usize] {
                failures.push((n, mm));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "INV-32: chain != direct clone for (n, m) pairs {failures:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// INV-23: cursor matches the live WAL
// ---------------------------------------------------------------------------------------------

/// (salt, commit count) from the live WAL header and frames.
fn wal_cursor(d: &Db) -> (String, String) {
    let bytes = fs::read(d.wal_path()).expect("WAL is pinned by the reader");
    let w = wal::parse(&bytes).unwrap().expect("WAL has a valid header");
    (
        format!("{}:{}", w.salt.0, w.salt.1),
        w.commits.len().to_string(),
    )
}

fn stored_cursor(d: &Db) -> (String, String) {
    let s = Store::open(&d.meta).unwrap();
    (
        s.get("cursor_salt").unwrap().unwrap(),
        s.get("cursor_commits").unwrap().unwrap(),
    )
}

/// A connection holding an open read transaction on live.db, which stops exec's connection
/// from checkpointing and deleting the WAL when it closes.
fn reader(d: &Db) -> Connection {
    let c = Connection::open(&d.live).unwrap();
    c.execute_batch("BEGIN").unwrap();
    let _: i64 = c
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    c
}

#[test]
fn inv23_cursor_matches_wal_while_reader_pins_it() {
    let schema = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";
    let (_tmp, root) = root_and_tmp();
    let d = create(&root, "cur", schema);
    let mut m = Model::new(Some(schema));

    // Generation 1: five execs (one a batch of two) while a reader pins the WAL.
    let r = reader(&d);
    for i in 0..4 {
        exec(&d, &mut m, &format!("INSERT INTO t(v) VALUES ('a{i}')"));
        let (salt, n) = wal_cursor(&d);
        assert_eq!(n, (i + 1).to_string(), "WAL keeps growing while pinned");
        assert_eq!(stored_cursor(&d), (salt, n), "INV-23 after exec {i}");
    }
    let batch = vec![
        "INSERT INTO t(v) VALUES ('b1')".to_string(),
        "UPDATE t SET v = 'b2' WHERE id = 1".to_string(),
    ];
    let got = db::exec(&d, &batch).unwrap() as u64;
    m.exec_batch(&batch, got);
    assert_eq!(stored_cursor(&d), wal_cursor(&d), "INV-23 after a batch");
    assert_eq!(stored_cursor(&d).1, "6");
    let gen1_salt = stored_cursor(&d).0;
    assert!(
        Observed::of(&d).wal_bytes.is_some(),
        "INV-48: WAL still present while the reader is open"
    );
    assert_invariants(&root, "cur", Some(&m), Checks::full());

    // Releasing the reader closes the last connection, which checkpoints and deletes the WAL.
    // The stored cursor still names generation 1 until the next capture.
    drop(r);
    assert!(
        Observed::of(&d).wal_bytes.is_none(),
        "INV-48: WAL cleared once nobody pins it"
    );
    assert_eq!(stored_cursor(&d), (gen1_salt.clone(), "6".to_string()));
    assert_invariants(&root, "cur", Some(&m), Checks::full());

    // Generation 2: a new WAL file gets a new salt and the cursor restarts from 1.
    let r = reader(&d);
    exec(&d, &mut m, "INSERT INTO t(v) VALUES ('c')");
    let (salt2, n2) = wal_cursor(&d);
    assert_eq!(
        stored_cursor(&d),
        (salt2.clone(), n2.clone()),
        "INV-23 in a new generation"
    );
    assert_eq!(n2, "1");
    assert_ne!(salt2, gen1_salt, "a fresh WAL has a fresh salt");
    exec(&d, &mut m, "INSERT INTO t(v) VALUES ('d')");
    assert_eq!(
        stored_cursor(&d),
        (salt2, "2".to_string()),
        "INV-23 after the second exec of gen 2"
    );
    drop(r);

    let o = assert_invariants(&root, "cur", Some(&m), Checks::full());
    assert_eq!(
        o.commits, 8,
        "no duplicated or lost commits across generations"
    );
}

/// INV-10 / Q6: sector-padding duplicate commit frames. A foreign writer with `POWERSAFE_OVERWRITE`
/// off (`?psow=0`) and `synchronous=FULL` makes SQLite repeat each commit frame to fill a sector.
/// Policy (Q6): the parser and `capture` are left alone, so every padding frame counts as its own
/// commit, each labelled `(recovered)`; replay is still exact because the copies carry identical
/// pages. Self-skips (with a printed reason) if SQLite does not pad on this machine.
#[test]
fn inv10_sector_padding_frames_are_counted_as_recovered_commits() {
    let (_tmp, root) = root_and_tmp();
    let d = create(
        &root,
        "pad",
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)",
    );
    let base = Store::open(&d.meta).unwrap().commit_count().unwrap();

    let uri = format!("file:{}?psow=0", d.live.display());
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let foreign = Connection::open_with_flags(&uri, flags).unwrap();
    foreign
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    foreign.pragma_update(None, "synchronous", "FULL").unwrap();
    for i in 0..3 {
        foreign
            .execute("INSERT INTO t(v) VALUES (?1)", [format!("row{i}")])
            .unwrap();
    }
    // Count real frames in the WAL, independent of walflip's capture.
    let wal_bytes = fs::read(d.wal_path()).unwrap();
    let ps = page_size(&d);
    let frames = (wal_bytes.len() - 32) / (24 + ps);
    if frames <= 3 {
        eprintln!("SKIP inv10: SQLite wrote {frames} frames for 3 inserts (no sector padding on this platform)");
        return;
    }

    // A no-op statement: exec's recovery step captures what the foreign writer left in the WAL.
    let report = db::exec_with(
        &d,
        &["UPDATE t SET v = v WHERE 0".to_string()],
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let new_commits = Store::open(&d.meta).unwrap().commit_count().unwrap() - base;
    eprintln!(
        "inv10: {frames} WAL frames, {new_commits} commits captured, {} recovered",
        report.recovered
    );
    assert!(
        new_commits > 3,
        "padding should inflate 3 inserts to more than 3 commits, got {new_commits}"
    );
    assert_eq!(
        report.recovered as u64, new_commits,
        "every extra commit is labelled (recovered)"
    );
    assert_eq!(report.captured as u64, new_commits);
    let log = db::log(&d, 1000, false).unwrap();
    let recovered = log.iter().filter(|e| e.sql == db::RECOVERED_LABEL).count() as u64;
    assert_eq!(recovered, new_commits);

    let total = new_commits + base;
    drop(foreign); // checkpoints and removes the WAL
    let out = root.join("pad-clone.db");
    db::clone_at(&d, total, &out).unwrap();
    assert_eq!(
        fs::read(&out).unwrap(),
        fs::read(&d.live).unwrap(),
        "clone(total) == live bytes"
    );
    let c = Connection::open(&d.live).unwrap();
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let n: i64 = c
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
}
