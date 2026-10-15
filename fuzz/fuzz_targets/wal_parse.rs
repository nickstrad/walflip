//! Raw bytes into the WAL parser. Invariants: no panic, and any WAL it accepts is internally
//! consistent (commit markers non-zero, every frame is one whole page, nothing out-of-bounds).
#![no_main]

use libfuzzer_sys::fuzz_target;
use walflip::wal;

fuzz_target!(|data: &[u8]| {
    let Ok(Some(w)) = wal::parse(data) else { return };
    let page = w.page_size as usize;
    assert!(page >= 512 && w.page_size.is_power_of_two());
    let mut frames = 0;
    for c in &w.commits {
        assert!(c.db_pages != 0);
        assert!(!c.frames.is_empty());
        for f in &c.frames {
            assert_eq!(f.page.len(), page);
            frames += 1;
        }
    }
    assert!(32 + frames * (24 + page) <= data.len());
});
