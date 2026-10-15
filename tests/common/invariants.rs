//! Invariant layer (docs/TESTING_PLAN.md section 3.1): snapshot a managed db through the
//! library and its files, then check the catalogued invariants. Every failure message starts
//! with the invariant ID so a red test points straight at the catalog row.

use super::model::{Model, TableDump};
use super::open_ro;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walflip::db::{self, Db};

/// 64-bit FNV-1a. Not cryptographic; only used to detect accidental mutation.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash of a file, `None` when it does not exist.
pub fn hash_file(path: &Path) -> Option<u64> {
    fs::read(path).ok().map(|b| fnv1a(&b))
}

/// Hashes of every file in a db folder that walflip owns (absent files are recorded as `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHashes(pub BTreeMap<&'static str, Option<u64>>);

impl FileHashes {
    pub const FILES: [&'static str; 6] =
        ["live.db", "live.db-wal", "live.db-shm", "base.db", "meta.db", "meta.db-journal"];

    pub fn of(dir: &Path) -> Self {
        Self(Self::FILES.iter().map(|f| (*f, hash_file(&dir.join(f)))).collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRow {
    pub n: u64,
    pub frames: u64,
    pub db_pages: u64,
    pub sql: String,
}

/// Truth about one managed db, derived only from what the library and files expose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub commits: u64,
    pub frames: u64,
    pub page_size: u64,
    pub log: Vec<LogRow>,
    pub wal_bytes: Option<u64>,
}

impl Observed {
    pub fn of(d: &Db) -> Self {
        let s = db::status(d).expect("status");
        let log = db::log(d, u64::MAX >> 1)
            .expect("log")
            .into_iter()
            .map(|e| LogRow { n: e.n, frames: e.frames, db_pages: e.db_pages, sql: e.sql })
            .collect();
        Self {
            commits: s.commits,
            frames: s.frames,
            page_size: s.page_size.expect("meta.db has a page_size"),
            log,
            wal_bytes: s.wal_bytes,
        }
    }
}

/// Which checks `assert_invariants` runs. Expensive ones can be turned off per scenario.
#[derive(Clone, Copy, Debug)]
pub struct Checks {
    /// INV-22: direct SQL checks over `meta.db` (contiguous idx/seq, no orphan frames, sizes).
    pub meta_structure: bool,
    /// Clone every k in `0..=commits` (true) or only {0, middle, last} (false).
    pub clone_every_k: bool,
    /// INV-30: compare clone(k) tables against the model's snapshot k (needs a model).
    pub model: bool,
    /// INV-31: clone(total) bytes == live.db when there is no WAL; INV-39: live == model.
    pub live_oracle: bool,
    /// INV-34: hashes of the db folder are unchanged by the clone loop.
    pub hashes: bool,
    /// INV-46: `list` reports this db with the same commit count.
    pub list: bool,
}

impl Checks {
    pub fn full() -> Self {
        Self { meta_structure: true, clone_every_k: true, model: true, live_oracle: true, hashes: true, list: true }
    }

    /// Same checks, but clones only a sample of commits (for long histories, plan Q12).
    pub fn light() -> Self {
        Self { clone_every_k: false, ..Self::full() }
    }
}

/// Commit numbers to clone: all, or {0, middle, last}.
fn sample_ks(total: u64, every: bool) -> Vec<u64> {
    if every {
        return (0..=total).collect();
    }
    let mut ks = vec![0, total / 2, total];
    ks.dedup();
    ks
}

fn integrity(path: &Path) -> String {
    let conn = open_ro(path);
    let rows: Vec<String> = conn
        .prepare("PRAGMA integrity_check")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows.join("; ")
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

fn meta_i64(meta: &Path, sql: &str) -> i64 {
    open_ro(meta).query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// INV-22 via a read-only, immutable open of `meta.db`.
fn check_meta_structure(name: &str, d: &Db, o: &Observed) {
    let m = &d.meta;
    let q = |sql: &str| meta_i64(m, sql);
    let (count, min, max) = (
        q("SELECT count(*) FROM commits"),
        q("SELECT coalesce(min(idx), 1) FROM commits"),
        q("SELECT coalesce(max(idx), 0) FROM commits"),
    );
    assert!(
        min == 1 && max == count,
        "INV-22 [{name}]: commits.idx not contiguous 1..={count} (min {min}, max {max})"
    );
    let orphans = q("SELECT count(*) FROM frames WHERE commit_idx NOT IN (SELECT idx FROM commits)");
    assert_eq!(orphans, 0, "INV-22 [{name}]: {orphans} frames reference a missing commit");
    let empty = q("SELECT count(*) FROM commits WHERE idx NOT IN (SELECT commit_idx FROM frames)");
    assert_eq!(empty, 0, "INV-22 [{name}]: {empty} commits have no frames");
    let bad_seq = q("SELECT count(*) FROM (SELECT commit_idx FROM frames GROUP BY commit_idx
                     HAVING min(seq) != 0 OR max(seq) != count(*) - 1)");
    assert_eq!(bad_seq, 0, "INV-22 [{name}]: {bad_seq} commits have a non-contiguous frame seq");
    let frames = q("SELECT count(*) FROM frames") as u64;
    assert_eq!(frames, o.frames, "INV-22 [{name}]: frames rows {frames} != status.frames {}", o.frames);
    let bad_len = q(&format!("SELECT count(*) FROM frames WHERE length(page) != {} OR pgno < 1", o.page_size));
    assert_eq!(bad_len, 0, "INV-22 [{name}]: {bad_len} frames have pgno < 1 or a page not {} bytes", o.page_size);
}

/// Check the invariant catalog against db `name` under `root`. `model`, when given, is the
/// oracle for table contents and commit counts. Returns what was observed.
pub fn assert_invariants(root: &Path, name: &str, model: Option<&Model>, checks: Checks) -> Observed {
    let d = Db::existing(root, name).unwrap_or_else(|e| panic!("assert_invariants [{name}]: {e}"));
    let o = Observed::of(&d);

    // INV-27: log length == status.commits (== sum of exec results, == model commits).
    assert_eq!(o.log.len() as u64, o.commits, "INV-27 [{name}]: log has {} rows, status.commits {}", o.log.len(), o.commits);
    for (i, row) in o.log.iter().enumerate() {
        assert_eq!(row.n, i as u64 + 1, "INV-27 [{name}]: log row {i} has n={}", row.n);
    }
    if let Some(m) = model {
        assert_eq!(o.commits, m.exec_sum, "INV-27 [{name}]: status.commits {} != sum of exec results {}", o.commits, m.exec_sum);
        assert_eq!(o.commits, m.commits(), "INV-38 [{name}]: status.commits {} != model commits {}", o.commits, m.commits());
    }

    // INV-28: frames per commit sum to status.frames.
    let sum: u64 = o.log.iter().map(|r| r.frames).sum();
    assert_eq!(sum, o.frames, "INV-28 [{name}]: sum of log frames {sum} != status.frames {}", o.frames);

    if checks.meta_structure {
        check_meta_structure(name, &d, &o);
    }

    if checks.list {
        let listed = db::list(root).expect("list");
        let hit = listed.iter().find(|s| s.name == name);
        assert_eq!(hit.map(|s| s.commits), Some(o.commits), "INV-46 [{name}]: list does not report {} commits", o.commits);
    }

    let before = FileHashes::of(&d.dir);
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::read(&d.base).unwrap();
    let ks = sample_ks(o.commits, checks.clone_every_k);
    for &k in &ks {
        let out = tmp.path().join(format!("clone-{k}.db"));
        db::clone_at(&d, k, &out).unwrap_or_else(|e| panic!("INV-29 [{name}]: clone -n {k} failed: {e:#}"));
        let bytes = fs::read(&out).unwrap();

        // INV-33: clone(0) is base.db; clone(k) is db_pages(k) pages long.
        if k == 0 {
            assert!(bytes == base, "INV-33 [{name}]: clone -n 0 differs from base.db");
        } else {
            let want = o.log[k as usize - 1].db_pages * o.page_size;
            assert_eq!(bytes.len() as u64, want, "INV-33 [{name}]: clone -n {k} is {} bytes, want db_pages*page_size = {want}", bytes.len());
        }

        // INV-29: every clone is a valid SQLite database.
        let ic = integrity(&out);
        assert_eq!(ic, "ok", "INV-29 [{name}]: integrity_check of clone -n {k}: {ic}");

        // INV-30: table contents (and schema) equal the model at commit k.
        if checks.model {
            if let Some(Some(want)) = model.map(|m| &m.snapshots[k as usize]) {
                let got = TableDump::of_file(&out);
                assert!(&got == want, "INV-30 [{name}]: clone -n {k} != model: {}", got.diff(want));
            }
        }

        // INV-34: clone creates no side files beside its output.
        for suf in ["-wal", "-shm", "-journal"] {
            assert!(!sidecar(&out, suf).exists(), "INV-34 [{name}]: clone -n {k} left a {suf} file");
        }
    }

    if checks.live_oracle && o.wal_bytes.is_none() {
        // INV-31: the strong oracle. With the WAL checkpointed, replaying all history must
        // reproduce live.db byte for byte.
        let out = tmp.path().join("clone-total.db");
        db::clone_at(&d, o.commits, &out).unwrap();
        let (got, live) = (fs::read(&out).unwrap(), fs::read(&d.live).unwrap());
        if got != live {
            let first = got.iter().zip(&live).position(|(a, b)| a != b);
            panic!(
                "INV-31 [{name}]: clone -n {} ({} bytes) != live.db ({} bytes); first differing byte {first:?}",
                o.commits,
                got.len(),
                live.len()
            );
        }
        // INV-39: live.db contents are what the model says history produced.
        if let (true, Some(m)) = (checks.model, model) {
            let (got, want) = (TableDump::of_file(&d.live), m.current());
            assert!(got == want, "INV-39 [{name}]: live.db != model: {}", got.diff(&want));
        }
    }

    // INV-34: none of the above (clones, list, status, log) mutated the db folder.
    if checks.hashes {
        let after = FileHashes::of(&d.dir);
        for (f, h) in &before.0 {
            assert_eq!(Some(h), after.0.get(f), "INV-34 [{name}]: {f} changed while cloning");
        }
    }
    o
}
