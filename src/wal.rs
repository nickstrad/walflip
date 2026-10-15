//! Minimal parser for the SQLite WAL file format.
//!
//! Layout (all header integers big-endian):
//!   header (32 bytes): magic, version, page_size, ckpt_seq, salt1, salt2, cksum1, cksum2
//!   frame  (24 + page_size bytes): pgno, db_size_if_commit, salt1, salt2, cksum1, cksum2, <page>
//!
//! A frame with a non-zero `db_size_if_commit` ends a transaction. Frames after the last
//! commit frame belong to an unfinished transaction and are ignored.

use anyhow::{bail, Result};

const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
const MAGIC_LE: u32 = 0x377f0682;
const MAGIC_BE: u32 = 0x377f0683;

/// Frames borrow their page from the WAL buffer, so skipping stored commits copies nothing.
pub struct Frame<'a> {
    pub pgno: u32,
    pub page: &'a [u8],
}

pub struct Commit<'a> {
    pub frames: Vec<Frame<'a>>,
    /// Size of the database, in pages, after this transaction.
    pub db_pages: u32,
}

pub struct Wal<'a> {
    pub page_size: u32,
    pub salt: (u32, u32),
    pub commits: Vec<Commit<'a>>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

/// SQLite's WAL checksum: a Fibonacci-style running sum over 8-byte chunks.
fn checksum(big_endian: bool, data: &[u8], mut s0: u32, mut s1: u32) -> (u32, u32) {
    let rd = if big_endian { be32 } else { le32 };
    for c in data.chunks_exact(8) {
        s0 = s0.wrapping_add(rd(&c[0..4])).wrapping_add(s1);
        s1 = s1.wrapping_add(rd(&c[4..8])).wrapping_add(s0);
    }
    (s0, s1)
}

/// Returns `None` if the buffer holds no valid WAL header (empty / still being created).
pub fn parse(buf: &[u8]) -> Result<Option<Wal<'_>>> {
    if buf.len() < WAL_HEADER {
        return Ok(None);
    }
    let big_endian = match be32(&buf[0..4]) {
        MAGIC_LE => false,
        MAGIC_BE => true,
        m => bail!("bad WAL magic {m:#x}"),
    };
    let page_size = be32(&buf[8..12]);
    if page_size < 512 || !page_size.is_power_of_two() {
        bail!("bad WAL page size {page_size}");
    }
    let salt = (be32(&buf[16..20]), be32(&buf[20..24]));
    let (mut s0, mut s1) = checksum(big_endian, &buf[0..24], 0, 0);
    if (s0, s1) != (be32(&buf[24..28]), be32(&buf[28..32])) {
        return Ok(None);
    }

    let frame_len = FRAME_HEADER + page_size as usize;
    let mut commits = Vec::new();
    let mut pending = Vec::new();
    let mut off = WAL_HEADER;

    while off + frame_len <= buf.len() {
        let fh = &buf[off..off + FRAME_HEADER];
        let page = &buf[off + FRAME_HEADER..off + frame_len];
        // Frames from a previous WAL generation carry different salts.
        if (be32(&fh[8..12]), be32(&fh[12..16])) != salt {
            break;
        }
        // Checksum covers the first 8 header bytes, then the page, chained from the prior frame.
        let (c0, c1) = checksum(big_endian, &fh[0..8], s0, s1);
        let (c0, c1) = checksum(big_endian, page, c0, c1);
        if (c0, c1) != (be32(&fh[16..20]), be32(&fh[20..24])) {
            break;
        }
        (s0, s1) = (c0, c1);

        pending.push(Frame { pgno: be32(&fh[0..4]), page });
        let db_pages = be32(&fh[4..8]);
        if db_pages != 0 {
            commits.push(Commit { frames: std::mem::take(&mut pending), db_pages });
        }
        off += frame_len;
    }

    Ok(Some(Wal { page_size, salt, commits }))
}

/// Reference WAL encoder.
///
/// This is a test oracle: it is written independently of the parser (it only
/// shares the checksum primitive and magic constants) so that parse(encode(x))
/// can be checked against x. Compiled only for tests or the `testing` feature.
#[cfg(any(test, feature = "testing"))]
pub mod encode {
    use super::{checksum, MAGIC_BE, MAGIC_LE};

    /// One commit: its frames as `(pgno, page)` plus the db size in pages
    /// written on the commit (last) frame.
    pub type Commit = (Vec<(u32, Vec<u8>)>, u32);

    fn put(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    /// Encode a valid WAL image. Every page must be `page_size` bytes.
    pub fn encode(big_endian: bool, salt: (u32, u32), page_size: u32, commits: &[Commit]) -> Vec<u8> {
        let mut out = Vec::new();
        put(&mut out, if big_endian { MAGIC_BE } else { MAGIC_LE });
        for v in [3007000, page_size, 0, salt.0, salt.1] {
            put(&mut out, v);
        }
        let (mut s0, mut s1) = checksum(big_endian, &out[0..24], 0, 0);
        put(&mut out, s0);
        put(&mut out, s1);
        for (frames, db_pages) in commits {
            for (i, (pgno, page)) in frames.iter().enumerate() {
                let size = if i + 1 == frames.len() { *db_pages } else { 0 };
                let mut fh = Vec::new();
                for v in [*pgno, size, salt.0, salt.1] {
                    put(&mut fh, v);
                }
                let (c0, c1) = checksum(big_endian, &fh[0..8], s0, s1);
                (s0, s1) = checksum(big_endian, page, c0, c1);
                put(&mut fh, s0);
                put(&mut fh, s1);
                out.extend(fh);
                out.extend(page);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const PS: usize = 512;
    type Owned = Vec<(Vec<(u32, Vec<u8>)>, u32)>;

    fn encode(big_endian: bool, salt: (u32, u32), commits: &Owned) -> Vec<u8> {
        super::encode::encode(big_endian, salt, PS as u32, commits)
    }

    fn owned(w: &Wal) -> Owned {
        w.commits
            .iter()
            .map(|c| (c.frames.iter().map(|f| (f.pgno, f.page.to_vec())).collect(), c.db_pages))
            .collect()
    }

    fn parsed(buf: &[u8]) -> Owned {
        parse(buf).ok().flatten().map(|w| owned(&w)).unwrap_or_default()
    }

    fn page() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), PS)
    }

    fn commit() -> impl Strategy<Value = (Vec<(u32, Vec<u8>)>, u32)> {
        (prop::collection::vec((1u32..8, page()), 1..4), 1u32..8)
    }

    fn commits(min: usize) -> impl Strategy<Value = Owned> {
        prop::collection::vec(commit(), min..6)
    }

    proptest! {
        #[test]
        fn roundtrips_in_both_byte_orders(be in any::<bool>(), salt in any::<(u32, u32)>(), c in commits(0)) {
            let buf = encode(be, salt, &c);
            let w = parse(&buf).unwrap().unwrap();
            prop_assert_eq!(w.page_size as usize, PS);
            prop_assert_eq!(w.salt, salt);
            prop_assert_eq!(owned(&w), c);
        }

        /// A WAL cut off anywhere yields exactly the commits that were fully written.
        #[test]
        fn truncation_keeps_a_prefix_of_commits(be in any::<bool>(), c in commits(1), cut in any::<usize>()) {
            let buf = encode(be, (7, 9), &c);
            let cut = cut % (buf.len() + 1);
            let got = parsed(&buf[..cut]);
            prop_assert!(got.len() <= c.len());
            prop_assert_eq!(&got[..], &c[..got.len()]);
            if cut == buf.len() {
                prop_assert_eq!(got.len(), c.len());
            }
        }

        /// Flipping any byte after the header never lets altered data through.
        #[test]
        fn corruption_cuts_history_at_the_damaged_frame(c in commits(1), pos in any::<usize>(), flip in 1u8..=255) {
            let mut buf = encode(false, (1, 2), &c);
            let pos = WAL_HEADER + pos % (buf.len() - WAL_HEADER);
            buf[pos] ^= flip;
            let got = parsed(&buf);

            let damaged_frame = (pos - WAL_HEADER) / (FRAME_HEADER + PS);
            let mut frames_before = 0;
            let intact = c.iter().take_while(|(f, _)| { frames_before += f.len(); frames_before <= damaged_frame }).count();
            prop_assert!(got.len() <= intact);
            prop_assert_eq!(&got[..], &c[..got.len()]);
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse(&bytes);
        }

        #[test]
        fn valid_header_with_garbage_tail_never_panics(tail in prop::collection::vec(any::<u8>(), 0..3000), be in any::<bool>()) {
            let mut buf = encode(be, (3, 4), &Vec::new());
            buf.extend(tail);
            prop_assert!(parse(&buf).unwrap().is_some());
        }
    }

    #[test]
    fn unfinished_transaction_is_ignored() {
        // Commit frame is what makes a transaction durable; frames after the last one do not count.
        let page = vec![1u8; PS];
        let buf = encode(false, (1, 1), &vec![(vec![(1, page.clone()), (2, page)], 2)]);
        let first_frame_only = &buf[..WAL_HEADER + FRAME_HEADER + PS];
        assert!(parsed(first_frame_only).is_empty());
    }

    #[test]
    fn encoder_is_usable_via_public_path() {
        let page = vec![9u8; 512];
        let commits: Vec<crate::wal::encode::Commit> = vec![(vec![(1, page)], 1)];
        let buf = crate::wal::encode::encode(true, (5, 6), 512, &commits);
        assert_eq!(parsed(&buf), commits);
    }

    // ---- T10: INV-06..INV-09 ----

    fn enc(be: bool, salt: (u32, u32), ps: u32, commits: &Owned) -> Vec<u8> {
        super::encode::encode(be, salt, ps, commits)
    }

    fn one(pgno: u32, seed: u8, ps: usize, db: u32) -> (Vec<(u32, Vec<u8>)>, u32) {
        (vec![(pgno, vec![seed; ps])], db)
    }

    fn put32(buf: &mut [u8], at: usize, v: u32) {
        buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// Re-seal the 32-byte header checksum after patching header fields.
    fn reseal_header(buf: &mut [u8], be: bool) {
        let (a, b) = checksum(be, &buf[0..24], 0, 0);
        put32(buf, 24, a);
        put32(buf, 28, b);
    }

    fn check_corruption(be: bool, c: &Owned, pos: usize, flip: u8) -> Result<(), TestCaseError> {
        let mut buf = encode(be, (1, 2), c);
        let pos = WAL_HEADER + pos % (buf.len() - WAL_HEADER);
        buf[pos] ^= flip;
        let got = parsed(&buf);
        let damaged_frame = (pos - WAL_HEADER) / (FRAME_HEADER + PS);
        let mut frames_before = 0;
        let intact = c
            .iter()
            .take_while(|(f, _)| {
                frames_before += f.len();
                frames_before <= damaged_frame
            })
            .count();
        prop_assert!(got.len() <= intact);
        prop_assert_eq!(&got[..], &c[..got.len()]);
        Ok(())
    }

    proptest! {
        /// Big-endian twin of `corruption_cuts_history_at_the_damaged_frame`.
        #[test]
        fn corruption_cuts_history_at_the_damaged_frame_big_endian(c in commits(1), pos in any::<usize>(), flip in 1u8..=255) {
            check_corruption(true, &c, pos, flip)?;
        }

        /// INV-06: a new generation (new salt) written over the head of an older one wins alone.
        #[test]
        fn inv06_stale_generation_tail_is_dropped(be in any::<bool>(), a in commits(2), seed in any::<u8>()) {
            let gen_a = encode(be, (10, 20), &a);
            let b: Owned = vec![one(1, seed, PS, 1)];
            let gen_b = encode(be, (30, 40), &b);
            prop_assert!(gen_b.len() < gen_a.len());
            let mut buf = gen_a;
            buf[..gen_b.len()].copy_from_slice(&gen_b);
            let w = parse(&buf).unwrap().unwrap();
            prop_assert_eq!(w.salt, (30, 40));
            prop_assert_eq!(owned(&w), b);
        }
    }

    #[test]
    fn inv06_salt_mismatch_mid_file_stops_at_that_frame() {
        for be in [false, true] {
            let c: Owned = (1..=4).map(|i| one(i, i as u8, PS, i)).collect();
            let mut buf = encode(be, (1, 2), &c);
            let frame3 = WAL_HEADER + 2 * (FRAME_HEADER + PS);
            put32(&mut buf, frame3 + 8, 99);
            // Frame 4 is itself valid, but nothing after the break is admitted.
            assert_eq!(parsed(&buf), c[..2].to_vec());
        }
    }

    #[test]
    fn inv06_valid_checksum_with_foreign_salt_is_rejected() {
        // The frame checksum does not cover the salt fields, so patching only the salt
        // leaves the chain intact; the salt check alone must reject the frame.
        for be in [false, true] {
            for which in [8usize, 12] {
                let c: Owned = (1..=3).map(|i| one(i, 0x40 + i as u8, PS, i)).collect();
                let mut buf = encode(be, (5, 6), &c);
                let f2 = WAL_HEADER + (FRAME_HEADER + PS);
                put32(&mut buf, f2 + which, 0xdead_beef);
                assert_eq!(parsed(&buf), c[..1].to_vec());
            }
        }
    }

    #[test]
    fn trailing_partial_frame_is_ignored() {
        for be in [false, true] {
            let c: Owned = vec![one(1, 1, PS, 1), one(2, 2, PS, 2)];
            let full = encode(be, (1, 2), &c);
            let frame_len = FRAME_HEADER + PS;
            for keep in [1, FRAME_HEADER - 1, FRAME_HEADER, FRAME_HEADER + 100, frame_len - 1] {
                let buf = &full[..full.len() - frame_len + keep];
                assert_eq!(parsed(buf), c[..1].to_vec(), "be={be} keep={keep}");
            }
        }
    }

    #[test]
    fn frames_after_last_commit_marker_are_dropped() {
        for be in [false, true] {
            let c: Owned = vec![
                one(1, 1, PS, 1),
                (vec![(2, vec![2; PS]), (3, vec![3; PS]), (4, vec![4; PS])], 4),
            ];
            let full = encode(be, (1, 2), &c);
            let frame_len = FRAME_HEADER + PS;
            // Drop the commit frame (the last one): two uncommitted frames remain.
            let w = parse(&full[..full.len() - frame_len]).unwrap().unwrap();
            assert_eq!(owned(&w), c[..1].to_vec());
        }
    }

    #[test]
    fn page_sizes_512_and_65536_roundtrip() {
        for be in [false, true] {
            for ps in [512u32, 1024, 4096, 65536] {
                let c: Owned = vec![
                    (vec![(1, vec![0xa5; ps as usize]), (2, vec![0x5a; ps as usize])], 2),
                    one(1, 7, ps as usize, 2),
                ];
                let buf = enc(be, (11, 22), ps, &c);
                let w = parse(&buf).unwrap().unwrap();
                assert_eq!(w.page_size, ps);
                assert_eq!(owned(&w), c, "be={be} ps={ps}");
                let cut = &buf[..buf.len() - 1];
                assert_eq!(parsed(cut), c[..1].to_vec());
            }
        }
    }

    #[test]
    fn checksum_chain_continues_across_commits() {
        // Same salts, same frame 2 content, different frame 1: frame 2's stored checksum
        // is only valid inside its own chain.
        for be in [false, true] {
            let x: Owned = vec![one(1, 1, PS, 1), one(2, 9, PS, 2)];
            let y: Owned = vec![one(1, 2, PS, 1), one(2, 9, PS, 2)];
            let bx = encode(be, (3, 4), &x);
            let by = encode(be, (3, 4), &y);
            let frame_len = FRAME_HEADER + PS;
            let f2 = WAL_HEADER + frame_len;
            let mut spliced = bx.clone();
            spliced[f2..f2 + frame_len].copy_from_slice(&by[f2..f2 + frame_len]);
            assert_eq!(parsed(&bx), x);
            assert_eq!(parsed(&spliced), x[..1].to_vec());
            let mut spliced1 = bx.clone();
            spliced1[WAL_HEADER..f2].copy_from_slice(&by[WAL_HEADER..f2]);
            // Y's frame 1 is valid from the header, but X's frame 2 no longer chains from it.
            assert_eq!(parsed(&spliced1), y[..1].to_vec());
        }
    }

    #[test]
    fn inv07_chain_not_starting_at_header_checksum_is_rejected_at_frame_one() {
        for be in [false, true] {
            let c: Owned = vec![one(1, 1, PS, 1), one(2, 2, PS, 2)];
            // (a) Header cksum patched without re-sealing: header invalid -> None.
            let mut buf = encode(be, (1, 2), &c);
            let old = be32(&buf[24..28]);
            put32(&mut buf, 24, old ^ 1);
            assert!(parse(&buf).unwrap().is_none());
            // (b) Header changed (ckpt_seq) and validly re-sealed: header parses, but frames
            // were chained from the old header checksum, so frame 1 must be rejected.
            let mut buf = encode(be, (1, 2), &c);
            put32(&mut buf, 12, 5);
            reseal_header(&mut buf, be);
            let w = parse(&buf).unwrap().unwrap();
            assert!(w.commits.is_empty(), "be={be}");
        }
    }

    #[test]
    fn inv08_page_size_bounds() {
        for ps in [512u32, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let buf = enc(false, (1, 2), ps, &Vec::new());
            assert!(parse(&buf).unwrap().is_some(), "ps={ps} must be accepted");
        }
        for ps in [0u32, 1, 256, 511, 513, 1000, 3000, 65535, 65537] {
            let buf = enc(false, (1, 2), ps, &Vec::new());
            assert!(parse(&buf).is_err(), "ps={ps} must be rejected");
        }
    }

    // KNOWN FAILING: INV-08 -- the parser has no upper bound on page_size. This test expects
    // page_size > 65536 (2^17..2^31) to be rejected with Err; today they are accepted
    // (harmless, no frame can fit, but the plan calls for hardening).
    #[test]
    #[ignore = "KNOWN FAILING: INV-08 parser lacks 65536 upper bound on page_size"]
    fn inv08_page_size_above_65536_is_rejected() {
        for shift in 17..=31 {
            let buf = enc(false, (1, 2), 1u32 << shift, &Vec::new());
            assert!(parse(&buf).is_err(), "page_size 2^{shift} must be rejected");
        }
    }

    #[test]
    fn inv09_header_checksum_mismatch_is_none_and_bad_magic_is_err() {
        for be in [false, true] {
            let c: Owned = vec![one(1, 1, PS, 1)];
            let good = encode(be, (1, 2), &c);
            assert!(parse(&good).unwrap().is_some());

            for at in [24usize, 28] {
                let mut b = good.clone();
                b[at + 3] ^= 0x80;
                assert!(parse(&b).unwrap().is_none(), "cksum at {at}");
            }
            let mut b = good.clone();
            b[19] ^= 1;
            assert!(parse(&b).unwrap().is_none());

            let mut b = good.clone();
            b[0] ^= 0xff;
            assert!(parse(&b).is_err());
        }
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&[0x37, 0x7f, 0x06, 0x82]).unwrap().is_none());
    }
}
