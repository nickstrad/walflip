//! Fuzzer bytes as a live WAL fed to Store::capture (no SQL from the input is executed).
//! Invariants: no panic; second capture adds 0; commit idx are 1..=n; frame pages are page_size.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::{path::PathBuf, sync::OnceLock};
use walflip::{store::Store, wal};

fn wal_path() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("walflip-fuzz-capture-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("live.db-wal")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&ctl, rest)) = data.split_first() else { return };
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

    let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
    store.set("page_size", &page_size.to_string()).unwrap();

    if store.capture(path, "fuzz").is_err() {
        return;
    }
    let n = store.commit_count().unwrap();
    assert_eq!(store.capture(path, "fuzz").unwrap(), 0, "capture must be idempotent");
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
        .query_row("SELECT count(*) FROM frames WHERE length(page) != ?1", [page_size], |r| r.get(0))
        .unwrap();
    assert_eq!(bad, 0);
});
