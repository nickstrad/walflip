//! Usage scenarios S2, S3, S4, S5, S8, S16, S17 from docs/TESTING_PLAN.md section 3.2.
mod common;

use common::invariants::{assert_invariants, hash_file, Checks, FileHashes};
use common::model::Model;
use common::scenario::{assert_same_outcome, Driver, Scenario, Step};
use common::Harness;
use std::fs;

const KV: &str = "CREATE TABLE kv(id INTEGER PRIMARY KEY, v TEXT);";

/// S2: one stdin batch and one process per statement give the same log and the same clone bytes.
#[test]
fn s2_stdin_batch_equals_per_process_bytes() {
    let stmts: Vec<String> = (1..=12)
        .map(|i| match i % 3 {
            0 => format!("UPDATE kv SET v = 'u{i}' WHERE id = {}", i - 1),
            _ => format!("INSERT INTO kv(id, v) VALUES({i}, 'row {i}')"),
        })
        .collect();
    let n = stmts.len() as u64;
    let clones = |mut steps: Vec<Step>| {
        steps.extend((0..=n).map(Step::Clone));
        steps
    };
    let per_process = Scenario::new(
        "S2 per-process",
        KV,
        "kv",
        clones(stmts.iter().cloned().map(Step::Exec).collect()),
    );
    let batch = Scenario::new(
        "S2 batch",
        KV,
        "kv",
        clones(vec![Step::Batch(stmts.clone())]),
    );

    let a = per_process.run(Driver::Cli);
    let b = batch.run(Driver::Cli);
    assert_eq!(
        a.log(),
        b.log(),
        "INV-44: log differs (frames, db_pages, labels)"
    );
    assert_eq!(a.log().len() as u64, n);
    assert_eq!(a.clones.len(), b.clones.len());
    for ((k, x), (_, y)) in a.clones.iter().zip(&b.clones) {
        assert!(
            x == y,
            "INV-44: clone -n {k} bytes differ between per-process and batch"
        );
    }
    // The library batch agrees too.
    assert_same_outcome(&b, &batch.run(Driver::Lib));
}

/// S3: env only, env root + flag db, and flags overriding wrong env all reach the same database.
#[test]
fn s3_env_vs_flags_vs_mixed() {
    let h = Harness::new();
    let root = h.root();
    h.create("envdb", KV);
    let mut model = Model::new(Some(KV));

    let env_only = |args: &[&str]| {
        let o = h
            .bare()
            .env("WALFLIP_ROOT", &root)
            .env("WALFLIP_DB", "envdb")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    };

    let sql = "INSERT INTO kv(id, v) VALUES(1, 'via env')";
    let n = common::parse_captured(&env_only(&["exec", sql]));
    model.exec(sql, n);
    let s_env = env_only(&["status"]);

    // env root, db from the flag
    let o = h
        .bare()
        .env("WALFLIP_ROOT", &root)
        .args(["--db", "envdb", "status"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let s_mixed = String::from_utf8(o.stdout).unwrap();

    // wrong env for both, right flags win
    let o = h
        .bare()
        .env("WALFLIP_ROOT", h.path("nowhere"))
        .env("WALFLIP_DB", "wrongdb")
        .arg("--root")
        .arg(&root)
        .args(["--db", "envdb", "status"])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "INV-45: flags must override env: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let s_flags = String::from_utf8(o.stdout).unwrap();

    assert_eq!(s_env, s_mixed, "INV-45");
    assert_eq!(s_env, s_flags, "INV-45");
    assert!(s_env.contains("commits   : 1"));

    // A second write through flags lands in the same history.
    let sql = "INSERT INTO kv(id, v) VALUES(2, 'via flags')";
    let n = common::parse_captured(&h.ok(&["--db", "envdb", "exec", sql]));
    model.exec(sql, n);
    assert!(env_only(&["status"]).contains("commits   : 2"));
    assert_invariants(&root, "envdb", Some(&model), Checks::full());
}

/// S4: many small dbs next to a decoy folder and a stray file.
#[test]
fn s4_many_small_dbs() {
    let h = Harness::new();
    let root = h.root();
    let names: Vec<String> = (0..12).map(|i| format!("db{:02}", (i * 7) % 12)).collect();
    let mut models = vec![];
    for (i, name) in names.iter().enumerate() {
        h.create(name, KV);
        let mut m = Model::new(Some(KV));
        for j in 0..(i % 3 + 1) {
            let sql = format!("INSERT INTO kv(id, v) VALUES({j}, '{name}-{j}')");
            let n = h.exec(name, &sql);
            m.exec(&sql, n);
        }
        models.push(m);
    }
    fs::create_dir_all(root.join("decoy")).unwrap();
    fs::write(root.join("decoy").join("notes.txt"), "not a db").unwrap();
    fs::write(root.join("stray-file"), "x").unwrap();
    // A folder with only a bare live.db is not a walflip db: not listed, nothing created in it.
    let bare = root.join("bare");
    fs::create_dir_all(&bare).unwrap();
    fs::write(bare.join("live.db"), "").unwrap();

    let list = h.ok(&["list"]);
    let rows: Vec<(String, u64)> = list
        .lines()
        .skip(1)
        .map(|l| {
            let mut it = l.split_whitespace();
            (
                it.next().unwrap().to_string(),
                it.next().unwrap().parse().unwrap(),
            )
        })
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(),
        sorted,
        "INV-46: list sorted, no decoy"
    );
    for (name, count) in &rows {
        assert_eq!(*count, h.commits(name), "INV-46: list count for {name}");
    }
    let mut dir: Vec<String> = fs::read_dir(root.join("decoy"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into())
        .collect();
    dir.sort();
    assert_eq!(
        dir,
        ["notes.txt"],
        "INV-46: list/status must not create files in a decoy folder"
    );
    assert!(
        !rows.iter().any(|r| r.0 == "bare"),
        "INV-46: bare live.db folder must not be listed"
    );
    let mut bare_files: Vec<String> = fs::read_dir(&bare)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into())
        .collect();
    bare_files.sort();
    assert_eq!(
        bare_files,
        ["live.db"],
        "INV-46: list must not create meta.db next to a bare live.db"
    );

    for (name, m) in names.iter().zip(&models) {
        assert_invariants(&root, name, Some(m), Checks::full());
    }
}

/// S5: a blob that spans many overflow pages, grown, deleted and replaced.
#[test]
fn s5_large_payload() {
    let schema = "CREATE TABLE b(id INTEGER PRIMARY KEY, data BLOB);";
    let blob = |n: usize| {
        format!(
            "CAST(replace(hex(zeroblob({})), '00', '5A') AS BLOB)",
            n / 2
        )
    };
    let s = Scenario::new(
        "S5 large payload",
        schema,
        "blobs",
        vec![
            Step::Exec(format!(
                "INSERT INTO b(id, data) VALUES(1, {})",
                blob(300_000)
            )),
            Step::Exec(format!(
                "UPDATE b SET data = {} WHERE id = 1",
                blob(600_000)
            )),
            Step::Exec("DELETE FROM b WHERE id = 1".into()),
            Step::Exec(format!(
                "INSERT INTO b(id, data) VALUES(2, {})",
                blob(1_000)
            )),
            Step::Clone(1),
            Step::Clone(4),
        ],
    );
    for d in [Driver::Cli, Driver::Lib] {
        let o = s.run(d);
        let log = o.log();
        assert_eq!(log.len(), 4);
        assert!(
            log[0].frames > 70,
            "INV-41: 300 KB insert has {} frames",
            log[0].frames
        );
        assert!(
            log[1].frames > 140,
            "INV-41: 600 KB update has {} frames",
            log[1].frames
        );
    }
}

/// S8: delete everything, reinsert, delete half. `db_pages` never shrinks without auto_vacuum.
#[test]
fn s8_delete_everything_then_reinsert() {
    let ins = |base: u32| -> Step {
        Step::Batch(
            (0..200)
                .map(|i| {
                    format!(
                        "INSERT INTO t(id, v) VALUES({}, 'row {}')",
                        base + i,
                        base + i
                    )
                })
                .collect(),
        )
    };
    let s = Scenario::new(
        "S8 delete-reinsert",
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);",
        "t",
        vec![
            ins(1),
            Step::Exec("DELETE FROM t".into()),
            ins(1000),
            Step::Exec("DELETE FROM t WHERE id % 2 = 0".into()),
            Step::Clone(200),
            Step::Clone(201),
            Step::Clone(401),
            Step::Clone(402),
        ],
    )
    .checks(Checks::light());
    let o = s.run(Driver::Cli);
    let log = o.log();
    assert_eq!(log.len(), 402);
    for w in log.windows(2) {
        assert!(
            w[1].db_pages >= w[0].db_pages,
            "INV-33: db_pages shrank at commit {}",
            w[1].n
        );
    }
}

/// S16: rename A while B keeps writing, then drop B; A's history is untouched.
#[test]
fn s16_rename_and_drop_mid_life() {
    let h = Harness::new();
    let root = h.root();
    h.create("a", KV);
    h.create("b", KV);
    let (mut ma, mut mb) = (Model::new(Some(KV)), Model::new(Some(KV)));
    for i in 0..3 {
        let sql = format!("INSERT INTO kv(id, v) VALUES({i}, 'a{i}')");
        let n = h.exec("a", &sql);
        ma.exec(&sql, n);
    }
    let before = FileHashes::of(&root.join("a"));
    let meta_before = hash_file(&root.join("a").join("meta.db"));

    h.ok(&["rename", "a", "a2"]);
    for i in 0..3 {
        let sql = format!("INSERT INTO kv(id, v) VALUES({i}, 'b{i}')");
        let n = h.exec("b", &sql);
        mb.exec(&sql, n);
    }
    assert!(!root.join("a").exists(), "INV-43: old folder remains");
    assert_eq!(
        hash_file(&root.join("a2").join("meta.db")),
        meta_before,
        "INV-43: meta.db changed by rename"
    );
    assert_eq!(
        FileHashes::of(&root.join("a2")),
        before,
        "INV-43: rename changed db files"
    );
    assert_invariants(&root, "b", Some(&mb), Checks::full());

    h.ok(&["drop", "b"]);
    assert!(!root.join("b").exists(), "INV-43: drop left the folder");
    assert_eq!(
        FileHashes::of(&root.join("a2")),
        before,
        "INV-43: drop of b touched a2"
    );
    let list = h.ok(&["list"]);
    assert!(
        list.contains("a2") && !list.lines().skip(1).any(|l| l.starts_with("b ")),
        "list: {list}"
    );
    assert_invariants(&root, "a2", Some(&ma), Checks::full());
}

/// S17: clone into a read-only directory fails cleanly (skipped when running as root).
#[test]
fn s17_read_only_output_dir() {
    use std::os::unix::fs::PermissionsExt;
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
        eprintln!("skipping S17: running as root, chmod 555 does not restrict writes");
        return;
    }
    let h = Harness::new();
    let root = h.root();
    h.create("kv", KV);
    let sql = "INSERT INTO kv(id, v) VALUES(1, 'x')";
    let mut m = Model::new(Some(KV));
    let n = h.exec("kv", sql);
    m.exec(sql, n);

    let ro = h.path("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let out = ro.join("clone.db");
    let before = FileHashes::of(&root.join("kv"));

    let r = h.run(&[
        "--db",
        "kv",
        "clone",
        "-n",
        "1",
        "-o",
        out.to_str().unwrap(),
    ]);
    let leftovers: Vec<_> = fs::read_dir(&ro)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !r.status.success(),
        "INV-36: clone into a read-only dir succeeded"
    );
    assert!(
        leftovers.is_empty(),
        "INV-36: partial files left behind: {leftovers:?}"
    );
    assert_eq!(
        FileHashes::of(&root.join("kv")),
        before,
        "INV-36: db files changed"
    );
    assert_invariants(&root, "kv", Some(&m), Checks::full());
}

/// Dispatch point for helper processes; a no-op in a normal run.
#[test]
fn helper_process_entry() {
    common::helpers::entry();
}

fn is_root() -> bool {
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&uid.stdout).trim() == "0"
}

/// Q11: on a read-only root, `list`, `status`, `log` and `clone` still work, even with a bare
/// `live.db` folder next to the real databases (skipped when running as root).
#[test]
fn s19_read_only_root_read_commands_work() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        eprintln!("skipping S19: running as root, chmod 555 does not restrict writes");
        return;
    }
    let h = Harness::new();
    let root = h.root();
    h.create("kv", KV);
    h.exec("kv", "INSERT INTO kv(id, v) VALUES(1, 'x')");
    fs::create_dir_all(root.join("bare")).unwrap();
    fs::write(root.join("bare").join("live.db"), "").unwrap();

    let chmod =
        |p: &std::path::Path, m| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
    for d in [root.join("kv"), root.join("bare"), root.clone()] {
        chmod(&d, 0o555);
    }
    let out = h.path("out.db");
    let list = h.run(&["list"]);
    let status = h.run(&["--db", "kv", "status"]);
    let log = h.run(&["--db", "kv", "log"]);
    let clone = h.run(&[
        "--db",
        "kv",
        "clone",
        "-n",
        "1",
        "-o",
        out.to_str().unwrap(),
    ]);
    for d in [root.join("kv"), root.join("bare"), root.clone()] {
        chmod(&d, 0o755);
    }
    for (what, r) in [
        ("list", &list),
        ("status", &status),
        ("log", &log),
        ("clone", &clone),
    ] {
        assert!(
            r.status.success(),
            "Q11: {what} failed on a read-only root: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    let l = String::from_utf8_lossy(&list.stdout);
    assert!(
        l.contains("kv") && !l.contains("bare"),
        "Q11: list output: {l}"
    );
    assert!(out.exists(), "Q11: clone wrote nothing");
    assert!(!root.join("bare").join("meta.db").exists());
}

/// Q11: a hot `meta.db-journal` left by a crashed writer does not break `status`/`list`/`log`
/// (the read-write open rolls it back; `SQLITE_OPEN_READ_ONLY` would fail here).
#[test]
fn s20_leftover_meta_journal_does_not_break_reads() {
    let h = Harness::new();
    let root = h.root();
    h.create("kv", KV);
    let mut m = Model::new(Some(KV));
    let sql = "INSERT INTO kv(id, v) VALUES(1, 'x')";
    let n = h.exec("kv", sql);
    m.exec(sql, n);
    let d = walflip::db::Db::existing(&root, "kv").unwrap();
    let size_before = std::fs::metadata(&d.meta).unwrap().len();
    let f = common::helpers::crash_spill(&d.meta);
    assert!(
        f.status.success() && f.stdout.contains("WROTE 400"),
        "lock holder: {f:?}"
    );
    // Prove the precondition: a non-empty hot journal, and the db file was really modified
    // (dirty pages spilled), so the reads below must roll it back.
    let mut journal = d.meta.as_os_str().to_owned();
    journal.push("-journal");
    let journal = std::path::PathBuf::from(journal);
    let jlen = std::fs::metadata(&journal)
        .unwrap_or_else(|e| panic!("setup failed: no {}: {e}", journal.display()))
        .len();
    assert!(jlen > 0, "setup failed: {} is empty", journal.display());
    let size_after = std::fs::metadata(&d.meta).unwrap().len();
    assert!(
        size_after > size_before,
        "setup failed: no spill ({size_before} -> {size_after})"
    );

    let st = h.ok(&["--db", "kv", "status"]);
    assert!(
        st.contains("commits   : 1"),
        "Q11: status after crashed writer: {st}"
    );
    assert!(h.ok(&["list"]).contains("kv"));
    h.ok(&["--db", "kv", "log"]);
    h.ok(&[
        "--db",
        "kv",
        "clone",
        "-n",
        "1",
        "-o",
        h.path("kv_clone").to_str().unwrap(),
    ]);
    assert!(!journal.exists(), "hot journal was not rolled back");
    assert_eq!(
        std::fs::metadata(&d.meta).unwrap().len(),
        size_before,
        "meta.db not rolled back"
    );
    assert_invariants(&root, "kv", Some(&m), Checks::full());
}
