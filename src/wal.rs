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
}
