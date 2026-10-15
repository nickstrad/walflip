//! Pure replay of WAL page images onto a database image held in memory.

use anyhow::{bail, Result};

/// Apply `frames` (pgno, full page image) in order, then optionally resize the image to
/// `db_pages` pages (the size recorded by the last replayed commit).
///
/// Every frame is a whole page, so applying one is an overwrite at `(pgno - 1) * page_size`.
pub fn apply<'a>(
    image: &mut Vec<u8>,
    page_size: usize,
    frames: impl IntoIterator<Item = (u32, &'a [u8])>,
    db_pages: Option<u32>,
) -> Result<()> {
    for (pgno, page) in frames {
        if pgno == 0 {
            bail!("frame has page number 0");
        }
        if page.len() != page_size {
            bail!("frame for page {pgno} is {} bytes, expected {page_size}", page.len());
        }
        let start = (pgno as usize - 1) * page_size;
        let end = start + page_size;
        if image.len() < end {
            image.resize(end, 0);
        }
        image[start..end].copy_from_slice(page);
    }
    if let Some(pages) = db_pages {
        image.resize(pages as usize * page_size, 0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    const PS: usize = 64;

    fn page() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), PS)
    }

    fn frames() -> impl Strategy<Value = Vec<(u32, Vec<u8>)>> {
        prop::collection::vec((1u32..7, page()), 0..12)
    }

    fn base() -> impl Strategy<Value = Vec<u8>> {
        (0usize..5).prop_flat_map(|n| prop::collection::vec(any::<u8>(), n * PS))
    }

    fn refs(f: &[(u32, Vec<u8>)]) -> impl Iterator<Item = (u32, &[u8])> {
        f.iter().map(|(p, d)| (*p, d.as_slice()))
    }

    proptest! {
        /// Last writer wins per page; untouched pages keep the base; new pages default to zeros.
        #[test]
        fn matches_page_map_model(base in base(), f in frames(), db_pages in 1u32..8) {
            let mut got = base.clone();
            apply(&mut got, PS, refs(&f), Some(db_pages)).unwrap();

            let mut model: HashMap<u32, &[u8]> = HashMap::new();
            for (p, d) in &f { model.insert(*p, d); }
            let mut want = Vec::new();
            for p in 1..=db_pages {
                let start = (p as usize - 1) * PS;
                match model.get(&p) {
                    Some(d) => want.extend_from_slice(d),
                    None if start + PS <= base.len() => want.extend_from_slice(&base[start..start + PS]),
                    None => want.extend(std::iter::repeat(0).take(PS)),
                }
            }
            prop_assert_eq!(got, want);
        }

        /// Replaying in two steps equals replaying at once, which is what makes -n prefixes sound.
        #[test]
        fn replay_composes(base in base(), f in frames(), split in 0usize..13, db_pages in 1u32..8) {
            let split = split.min(f.len());
            let mut once = base.clone();
            apply(&mut once, PS, refs(&f), Some(db_pages)).unwrap();

            let mut twice = base.clone();
            apply(&mut twice, PS, refs(&f[..split]), None).unwrap();
            apply(&mut twice, PS, refs(&f[split..]), Some(db_pages)).unwrap();
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn is_idempotent(base in base(), f in frames(), db_pages in 1u32..8) {
            let mut a = base.clone();
            apply(&mut a, PS, refs(&f), Some(db_pages)).unwrap();
            let mut b = a.clone();
            apply(&mut b, PS, refs(&f), Some(db_pages)).unwrap();
            prop_assert_eq!(a, b);
        }
    }

    #[test]
    fn rejects_bad_frames() {
        let mut img = Vec::new();
        assert!(apply(&mut img, PS, [(0u32, &[0u8; PS][..])], None).is_err());
        assert!(apply(&mut img, PS, [(1u32, &[0u8; PS - 1][..])], None).is_err());
    }
}
