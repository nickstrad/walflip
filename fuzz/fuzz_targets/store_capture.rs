//! Fuzzer bytes as a live WAL fed to Store::capture (no SQL from the input is executed).
//! Invariants: no panic; second capture adds 0; commit idx are 1..=n; frame pages are page_size.
//! Incremental capture (INV-51): capturing a prefix of the file first and then the whole file
//! stores exactly the history of one capture of the whole file (the second capture resumes from
//! the state the first one stored), and `uncaptured` agrees with what the second capture adds.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::{path::PathBuf, sync::OnceLock};
use walflip::{store::Store, wal};

/// (idx, db_pages, frames as (seq, pgno, page)) of every stored commit.
type History = Vec<(i64, i64, Vec<(i64, i64, Vec<u8>)>)>;

fn history(store: &Store) -> History {
    let c = store.conn();
    let commits: Vec<(i64, i64)> = c
        .prepare("SELECT idx, db_pages FROM commits ORDER BY idx")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    commits
        .into_iter()
        .map(|(idx, pages)| {
            let frames = c
                .prepare("SELECT seq, pgno, page FROM frames WHERE commit_idx=?1 ORDER BY seq")
                .unwrap()
                .query_map([idx], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (idx, pages, frames)
        })
        .collect()
}

fn wal_path() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("walflip-fuzz-capture-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("live.db-wal")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&ctl, rest)) = data.split_first() else {
        return;
    };
    let page_size = 512u32 << (ctl & 3);
    let mut bytes = Vec::new();
    if ctl & 0x80 != 0 {
        // Valid reference-encoded WAL (header + one commit); raw tail bytes follow.
        let page = vec![ctl; page_size as usize];
        bytes = wal::encode::encode(ctl & 4 != 0, (1, 2), page_size, &[(vec![(1, page)], 1)]);
    }
    bytes.extend_from_slice(rest);
    let path = wal_path();
    std::fs::write(path, &bytes).unwrap();

    let open = || {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.set("page_size", &page_size.to_string()).unwrap();
        s
    };
    let mut store = open();

    // Two-step capture first (it rewrites the file), so the checks below see the whole file.
    let cut = bytes.len() * usize::from(ctl >> 3 & 0xf) / 15;
    std::fs::write(path, &bytes[..cut]).unwrap();
    let mut stepped = open();
    let first = stepped.capture(path, "fuzz");
    std::fs::write(path, &bytes).unwrap();

    let whole = store.capture(path, "fuzz");
    if first.is_ok() {
        let pending = stepped.uncaptured(path).unwrap();
        let second = stepped.capture(path, "fuzz");
        assert_eq!(
            second.is_ok(),
            whole.is_ok(),
            "two-step and one-shot capture disagree on failure"
        );
        if let (Ok(n), Ok(_)) = (second, &whole) {
            assert_eq!(
                pending, n as u64,
                "uncaptured != what the next capture adds"
            );
            assert!(
                history(&stepped) == history(&store),
                "prefix + resumed capture != one-shot capture"
            );
        }
    }
    if whole.is_err() {
        return;
    }
    let n = store.commit_count().unwrap();
    assert_eq!(
        store.capture(path, "fuzz").unwrap(),
        0,
        "capture must be idempotent"
    );
    assert_eq!(store.commit_count().unwrap(), n);

    let c = store.conn();
    let idxs: Vec<i64> = c
        .prepare("SELECT idx FROM commits ORDER BY idx")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(idxs, (1..=n as i64).collect::<Vec<_>>());
    let bad: i64 = c
        .query_row(
            "SELECT count(*) FROM frames WHERE length(page) != ?1",
            [page_size],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0);
});
