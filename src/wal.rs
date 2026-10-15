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
    /// Where parsing can resume: just past the last commit frame accepted.
    pub end: Resume,
}

/// Parser state at a commit boundary of one WAL generation, enough to continue parsing later
/// from `offset` without re-reading the frames before it (see [`parse_from`]).
///
/// The state is tied to the exact 32-byte header (byte order, page size, salt and header checksum
/// all come from it), so a WAL restart, which writes a new salt, never matches an old state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    pub header: [u8; WAL_HEADER],
    /// File offset just past the last commit frame (`32` when the generation has no commit).
    pub offset: u64,
    /// The running checksum at `offset`: the stored checksum of the last commit frame, or the
    /// header checksum when there is none.
    pub cksum: (u32, u32),
    /// Commits of this generation before `offset`.
    pub commits: u64,
}

/// Byte order, page size and salt from a header that passed its checksum.
#[derive(Clone, Copy)]
struct Header {
    big_endian: bool,
    page_size: u32,
    salt: (u32, u32),
    cksum: (u32, u32),
}

impl Header {
    fn frame_len(&self) -> usize {
        FRAME_HEADER + self.page_size as usize
    }
}

/// `Ok(None)` for a short buffer or a header checksum mismatch, `Err` for bad magic / page size.
fn parse_header(buf: &[u8]) -> Result<Option<Header>> {
    if buf.len() < WAL_HEADER {
        return Ok(None);
    }
    let big_endian = match be32(&buf[0..4]) {
        MAGIC_LE => false,
        MAGIC_BE => true,
        m => bail!("bad WAL magic {m:#x}"),
    };
    let page_size = be32(&buf[8..12]);
    if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
        bail!("bad WAL page size {page_size}");
    }
    let salt = (be32(&buf[16..20]), be32(&buf[20..24]));
    let cksum = checksum(big_endian, &buf[0..24], 0, 0);
    if cksum != (be32(&buf[24..28]), be32(&buf[28..32])) {
        return Ok(None);
    }
    Ok(Some(Header {
        big_endian,
        page_size,
        salt,
        cksum,
    }))
}

impl Resume {
    /// The state before the first frame of the generation that `header` starts.
    fn start(header: &[u8], h: &Header) -> Self {
        Self {
            header: header[..WAL_HEADER].try_into().unwrap(),
            offset: WAL_HEADER as u64,
            cksum: h.cksum,
            commits: 0,
        }
    }

    pub fn salt(&self) -> (u32, u32) {
        (be32(&self.header[16..20]), be32(&self.header[20..24]))
    }

    /// The header this state belongs to, re-validated (a stored state may be damaged).
    fn checked_header(&self) -> Option<Header> {
        parse_header(&self.header).ok().flatten()
    }

    /// File offset the tail passed to [`parse_from`] must start at: the last commit frame (its
    /// header is checked against the stored checksum), or `offset` when there is no commit yet.
    pub fn tail_start(&self) -> u64 {
        let frame_len = self.checked_header().map_or(0, |h| h.frame_len() as u64);
        if self.offset > WAL_HEADER as u64 {
            self.offset.saturating_sub(frame_len)
        } else {
            self.offset
        }
    }

    /// Text form stored in `meta.db`: `v1 <header hex> <offset> <cksum0> <cksum1> <commits>`.
    pub fn to_meta(&self) -> String {
        let hex: String = self.header.iter().map(|b| format!("{b:02x}")).collect();
        format!(
            "v1 {hex} {} {} {} {}",
            self.offset, self.cksum.0, self.cksum.1, self.commits
        )
    }

    /// Inverse of [`Resume::to_meta`]; `None` for anything else.
    pub fn from_meta(s: &str) -> Option<Self> {
        let mut it = s.split(' ');
        if it.next()? != "v1" {
            return None;
        }
        let hex = it.next()?;
        if hex.len() != 2 * WAL_HEADER || !hex.is_ascii() {
            return None;
        }
        let mut header = [0u8; WAL_HEADER];
        for (i, b) in header.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
        }
        let offset = it.next()?.parse().ok()?;
        let cksum = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
        let commits = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        Some(Self {
            header,
            offset,
            cksum,
            commits,
        })
    }

    /// True when this state is a commit boundary of the WAL whose first bytes are `header` and
    /// whose bytes from [`Resume::tail_start`] on are `tail`, so [`parse_from`] can continue it.
    /// Checks: identical header (same generation), a self-consistent state, the file reaching
    /// `offset`, and the last commit frame there carrying this salt, a commit marker and exactly
    /// the stored running checksum.
    pub fn matches(&self, header: &[u8], tail: &[u8]) -> bool {
        self.anchor(header, tail).is_some()
    }

    /// The validated header and where scanning starts inside `tail`.
    fn anchor(&self, header: &[u8], tail: &[u8]) -> Option<(Header, usize)> {
        if header.len() < WAL_HEADER || header[..WAL_HEADER] != self.header {
            return None;
        }
        let h = self.checked_header()?;
        let frame_len = h.frame_len() as u64;
        let frames = self.offset.checked_sub(WAL_HEADER as u64)?;
        if frames % frame_len != 0 || self.commits > frames / frame_len {
            return None;
        }
        if frames == 0 {
            return (self.commits == 0 && self.cksum == h.cksum).then_some((h, 0));
        }
        if self.commits == 0 || (tail.len() as u64) < frame_len {
            return None;
        }
        let fh = &tail[..FRAME_HEADER];
        let ok = be32(&fh[4..8]) != 0
            && (be32(&fh[8..12]), be32(&fh[12..16])) == h.salt
            && (be32(&fh[16..20]), be32(&fh[20..24])) == self.cksum;
        ok.then_some((h, frame_len as usize))
    }
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
    for c in data.as_chunks::<8>().0 {
        s0 = s0.wrapping_add(rd(&c[0..4])).wrapping_add(s1);
        s1 = s1.wrapping_add(rd(&c[4..8])).wrapping_add(s0);
    }
    (s0, s1)
}

/// Returns `None` if the buffer holds no valid WAL header (empty / still being created).
pub fn parse(buf: &[u8]) -> Result<Option<Wal<'_>>> {
    let Some(h) = parse_header(buf)? else {
        return Ok(None);
    };
    Ok(Some(scan(&h, &buf[WAL_HEADER..], Resume::start(buf, &h))))
}

/// Continue parsing a WAL from a state returned by an earlier parse of the same generation.
///
/// `header` is the file's first 32 bytes and `tail` the file's bytes from `from.tail_start()` on.
/// Returns only the commits after `from` (`end.commits` counts the whole generation), and the
/// same result as [`parse`] of the whole file would give past `from`. `None` when `from` does not
/// fit this WAL (see [`Resume::matches`]): the caller must then parse the whole file.
pub fn parse_from<'a>(header: &[u8], tail: &'a [u8], from: &Resume) -> Option<Wal<'a>> {
    let (h, skip) = from.anchor(header, tail)?;
    Some(scan(&h, &tail[skip..], *from))
}

/// Read frames from `frames` (which starts at file offset `from.offset`) until the end of the
/// buffer, a frame of another generation, or a checksum mismatch.
fn scan<'a>(h: &Header, frames: &'a [u8], from: Resume) -> Wal<'a> {
    let frame_len = h.frame_len();
    let (mut s0, mut s1) = from.cksum;
    let mut end = from;
    let mut commits = Vec::new();
    let mut pending = Vec::new();
    let mut off = 0;

    while off + frame_len <= frames.len() {
        let fh = &frames[off..off + FRAME_HEADER];
        let page = &frames[off + FRAME_HEADER..off + frame_len];
        // Frames from a previous WAL generation carry different salts.
        if (be32(&fh[8..12]), be32(&fh[12..16])) != h.salt {
            break;
        }
        // Checksum covers the first 8 header bytes, then the page, chained from the prior frame.
        let (c0, c1) = checksum(h.big_endian, &fh[0..8], s0, s1);
        let (c0, c1) = checksum(h.big_endian, page, c0, c1);
        if (c0, c1) != (be32(&fh[16..20]), be32(&fh[20..24])) {
            break;
        }
        (s0, s1) = (c0, c1);
        off += frame_len;

        pending.push(Frame {
            pgno: be32(&fh[0..4]),
            page,
        });
        let db_pages = be32(&fh[4..8]);
        if db_pages != 0 {
            commits.push(Commit {
                frames: std::mem::take(&mut pending),
                db_pages,
            });
            end.offset = from.offset + off as u64;
            end.cksum = (s0, s1);
            end.commits += 1;
        }
    }

    Wal {
        page_size: h.page_size,
        salt: h.salt,
        commits,
        end,
    }
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
    pub fn encode(
        big_endian: bool,
        salt: (u32, u32),
        page_size: u32,
        commits: &[Commit],
    ) -> Vec<u8> {
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
            .map(|c| {
                (
                    c.frames.iter().map(|f| (f.pgno, f.page.to_vec())).collect(),
                    c.db_pages,
                )
            })
            .collect()
    }

    fn parsed(buf: &[u8]) -> Owned {
        parse(buf)
            .ok()
            .flatten()
            .map(|w| owned(&w))
            .unwrap_or_default()
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

    // ---- T24: resumed parsing (INV-51) ----

    /// Resume from `from` against the whole buffer `buf`, slicing it as a capture would read it.
    fn resume_on<'a>(buf: &'a [u8], from: &Resume) -> Option<Wal<'a>> {
        let start = (from.tail_start() as usize).min(buf.len());
        parse_from(&buf[..buf.len().min(WAL_HEADER)], &buf[start..], from)
    }

    /// Commits of `buf` read in pieces: parse `buf[..cuts[0]]`, then resume on each longer prefix.
    fn parsed_in_pieces(buf: &[u8], cuts: &[usize]) -> Option<(Owned, Resume)> {
        let w = parse(&buf[..cuts[0]]).ok()??;
        let mut got = owned(&w);
        let mut end = w.end;
        for &cut in &cuts[1..] {
            let w = resume_on(&buf[..cut], &end).expect("a prefix state fits every longer prefix");
            assert_eq!(w.end.commits, end.commits + w.commits.len() as u64);
            got.extend(owned(&w));
            end = w.end;
        }
        Some((got, end))
    }

    fn with_tail(c: &Owned, tail: &[(u32, Vec<u8>)]) -> Owned {
        let mut all = c.clone();
        if !tail.is_empty() {
            all.push((tail.to_vec(), 0)); // no commit marker: an unfinished transaction
        }
        all
    }

    proptest! {
        /// parse(whole) == parse(prefix) + parse_from(whole, prefix.end), for any split point,
        /// both byte orders, any salt, with or without an unfinished trailing transaction.
        #[test]
        fn resumed_parse_equals_full_parse_at_any_split(
            be in any::<bool>(), salt in any::<(u32, u32)>(), c in commits(0),
            tail in prop::collection::vec((1u32..8, page()), 0..3), cut in any::<prop::sample::Index>(),
        ) {
            let buf = encode(be, salt, &with_tail(&c, &tail));
            let full = parse(&buf).unwrap().unwrap();
            let cut = WAL_HEADER + cut.index(buf.len() - WAL_HEADER + 1);
            let (got, end) = parsed_in_pieces(&buf, &[cut, buf.len()]).unwrap();
            prop_assert_eq!(&got, &c);
            prop_assert_eq!(end, full.end);
            prop_assert_eq!(full.end.commits as usize, c.len());
            prop_assert_eq!(full.end.offset as usize, buf.len() - tail.len() * (FRAME_HEADER + PS));
        }

        /// The same through several resumes in a row, as successive captures do.
        #[test]
        fn chained_resumes_equal_full_parse(
            be in any::<bool>(), salt in any::<(u32, u32)>(), c in commits(0),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..6),
        ) {
            let buf = encode(be, salt, &c);
            let mut points: Vec<usize> = cuts.iter().map(|i| WAL_HEADER + i.index(buf.len() - WAL_HEADER + 1)).collect();
            points.push(buf.len());
            points.sort_unstable();
            let (got, end) = parsed_in_pieces(&buf, &points).unwrap();
            prop_assert_eq!(got, c);
            prop_assert_eq!(end, parse(&buf).unwrap().unwrap().end);
        }

        /// Resuming on any byte string that starts with the parsed prefix gives what the full parse
        /// gives after it, whatever follows (garbage, a stale generation, a cut frame).
        #[test]
        fn resume_matches_full_parse_on_arbitrary_continuations(
            be in any::<bool>(), c in commits(0), cut in any::<prop::sample::Index>(),
            garbage in prop::collection::vec(any::<u8>(), 0..1200), flip in any::<(prop::sample::Index, u8)>(),
        ) {
            let mut buf = encode(be, (5, 6), &c);
            buf.extend(&garbage);
            let cut = WAL_HEADER + cut.index(buf.len() - WAL_HEADER + 1);
            let (mut got, state) = {
                let p = parse(&buf[..cut]).unwrap().unwrap();
                (owned(&p), p.end)
            };
            // Damage a byte at or after the resume point (never inside the parsed prefix).
            let from = state.offset as usize;
            if from < buf.len() {
                let at = from + flip.0.index(buf.len() - from);
                buf[at] ^= flip.1;
            }
            let full = parse(&buf).unwrap().unwrap();
            let r = resume_on(&buf, &state).expect("prefix intact, so the state fits");
            got.extend(owned(&r));
            prop_assert_eq!(got, owned(&full));
            prop_assert_eq!(r.end, full.end);
        }

        /// A state never fits another generation (new salt in the header), a file shorter than the
        /// stored offset, or a different running checksum at the resume point.
        #[test]
        fn state_that_does_not_fit_is_refused(be in any::<bool>(), c in commits(1), salt2 in any::<(u32, u32)>(), d in 1u32..) {
            let buf = encode(be, (1, 2), &c);
            let end = parse(&buf).unwrap().unwrap().end;
            prop_assert!(resume_on(&buf, &end).is_some());
            // WAL restart: same layout, new salt.
            prop_assume!(salt2 != (1, 2));
            let other = encode(be, salt2, &c);
            prop_assert!(resume_on(&other, &end).is_none());
            // File shorter than the offset (each byte count below it).
            prop_assert!(resume_on(&buf[..end.offset as usize - 1], &end).is_none());
            // Stored checksum, offset or count do not describe this file.
            let bad = Resume { cksum: (end.cksum.0.wrapping_add(d), end.cksum.1), ..end };
            prop_assert!(resume_on(&buf, &bad).is_none());
            // One frame back is never a commit frame carrying the final checksum (nor the header).
            let bad = Resume { offset: end.offset - (FRAME_HEADER + PS) as u64, ..end };
            prop_assert!(resume_on(&buf, &bad).is_none());
            let bad = Resume { offset: end.offset + 1, ..end };
            prop_assert!(resume_on(&buf, &bad).is_none());
            let bad = Resume { commits: end.offset, ..end };
            prop_assert!(resume_on(&buf, &bad).is_none());
        }

        #[test]
        fn resume_meta_text_roundtrips(header in any::<[u8; 32]>(), offset in any::<u64>(), cksum in any::<(u32, u32)>(), commits in any::<u64>()) {
            let r = Resume { header, offset, cksum, commits };
            prop_assert_eq!(Resume::from_meta(&r.to_meta()), Some(r));
        }

        #[test]
        fn arbitrary_state_and_bytes_never_panic(
            header in prop::collection::vec(any::<u8>(), 0..40), tail in prop::collection::vec(any::<u8>(), 0..1200),
            h in any::<[u8; 32]>(), offset in any::<u64>(), cksum in any::<(u32, u32)>(), commits in any::<u64>(),
        ) {
            let r = Resume { header: h, offset, cksum, commits };
            let _ = r.tail_start();
            let _ = parse_from(&header, &tail, &r);
            let _ = Resume::from_meta(&String::from_utf8_lossy(&tail));
        }
    }

    #[test]
    fn empty_generation_resumes_from_the_header() {
        for be in [false, true] {
            let c: Owned = vec![one(1, 1, PS, 1), one(2, 2, PS, 2)];
            let buf = encode(be, (8, 9), &c);
            let empty = parse(&buf[..WAL_HEADER]).unwrap().unwrap();
            assert_eq!(
                (empty.end.offset, empty.end.commits),
                (WAL_HEADER as u64, 0)
            );
            assert_eq!(empty.end.tail_start(), WAL_HEADER as u64);
            assert_eq!(owned(&resume_on(&buf, &empty.end).unwrap()), c);
            // The header checksum is the chain seed: a state claiming another seed is refused.
            let bad = Resume {
                cksum: (1, 1),
                ..empty.end
            };
            assert!(resume_on(&buf, &bad).is_none());
        }
    }

    #[test]
    fn resume_meta_rejects_other_text() {
        let r = parse(&encode(false, (1, 2), &vec![one(1, 1, PS, 1)]))
            .unwrap()
            .unwrap()
            .end;
        let good = r.to_meta();
        assert_eq!(Resume::from_meta(&good), Some(r));
        for bad in [
            String::new(),
            "v2".to_string() + &good[2..],
            good.replacen("v1 ", "v1  ", 1),
            good.clone() + " 1",
            good[..good.len() - 2].to_string() + " x",
            good.replacen(' ', " zz", 1),
            "v1 é".to_string(),
        ] {
            assert_eq!(Resume::from_meta(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn unfinished_transaction_is_ignored() {
        // Commit frame is what makes a transaction durable; frames after the last one do not count.
        let page = vec![1u8; PS];
        let buf = encode(
            false,
            (1, 1),
            &vec![(vec![(1, page.clone()), (2, page)], 2)],
        );
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
            for keep in [
                1,
                FRAME_HEADER - 1,
                FRAME_HEADER,
                FRAME_HEADER + 100,
                frame_len - 1,
            ] {
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
                (
                    vec![(2, vec![2; PS]), (3, vec![3; PS]), (4, vec![4; PS])],
                    4,
                ),
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
                    (
                        vec![(1, vec![0xa5; ps as usize]), (2, vec![0x5a; ps as usize])],
                        2,
                    ),
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

    // INV-08: page_size above 65536 (2^17..2^31) is rejected with Err.
    #[test]
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
