//! Structure-aware differential target for the WAL parser.
//!
//! The fuzzer describes a WAL (byte order, salts, page size, commits of frames, plus an optional
//! unfinished tail) and at most one `Mutation`. We build the bytes with the reference encoder
//! `wal::encode`, apply the mutation, parse, and compare against the oracle:
//!
//! - no mutation: the parse equals the input exactly (page size, salt, every commit byte for byte);
//! - any mutation: the parsed commits are a prefix of the input commits (never altered data, never
//!   more commits; `AppendGarbage` alone may add commits *after* the full input, because garbage
//!   can legitimately complete the unfinished tail);
//! - every mutation also yields a bound `lo <= parsed <= hi` on the commit count. `lo` is the
//!   number of commits that lie entirely before the first changed byte (the parser is sequential,
//!   so it must still accept them). When a mutation provably damages frame k, `hi` is the number
//!   of commits whose frames are all `< k`.
//!
//! Resumed parsing (`wal::parse_from`, INV-51) is checked on every input, mutated or not:
//!
//! - split: parse `buf[..cut]`, resume on the whole `buf` from the returned state; the prefix
//!   commits plus the resumed ones, and the final state, must equal the full parse exactly;
//! - stale state: a state taken from a prefix of the *unmutated* WAL is applied to the mutated
//!   one. If the mutation left everything before the state's offset intact, the resume must
//!   succeed and equal the full parse past that offset; if the file is now shorter than the offset
//!   or its header changed (truncation, header patch, a WAL restart), it must be refused.
//!
//! `Restart` writes a new generation (new salt) over the head of the WAL, as SQLite does after a
//! checkpoint; the full parse must then return exactly the new generation.
//!
//! "Provably damaged" uses the fact that the WAL checksum step is a bijection of the running state
//! for fixed data, and changes whenever a data word changes: a single altered word in the header or
//! in a frame's first 8 bytes / page always changes the computed checksum, a changed stored
//! checksum never matches the unchanged computed one, and a changed salt fails the salt check.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use walflip::wal;

const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
const MAGIC_LE: u32 = 0x377f0682;
const MAGIC_BE: u32 = 0x377f0683;
const MAX_COMMITS: usize = 8;
const MAX_FRAMES_PER_COMMIT: usize = 4;
const MAX_TAIL: usize = 3;
const MAX_GARBAGE: usize = 9000;

#[derive(Arbitrary, Debug)]
struct FrameSpec {
    pgno: u32,
    /// Page contents are expanded from this seed so inputs stay small.
    seed: u32,
}

#[derive(Arbitrary, Debug)]
struct CommitSpec {
    frames: Vec<FrameSpec>,
    db_pages: u32,
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum HeaderField {
    Magic,
    Version,
    PageSize,
    CkptSeq,
    Salt1,
    Salt2,
}

#[derive(Arbitrary, Debug)]
enum Mutation {
    FlipByte { pos: usize, mask: u8 },
    Truncate { at: usize },
    OverwriteSalt { frame: usize, second: bool, value: u32 },
    DuplicateFrame { frame: usize },
    SwapFrames { a: usize, b: usize },
    AppendGarbage { bytes: Vec<u8> },
    /// Frames of another WAL generation (different salt) appended after the current one.
    AppendStaleGeneration { salt_xor: (u32, u32), frames: Vec<FrameSpec> },
    ZeroChecksum { frame: usize },
    /// Overwrite a header field; `resum` recomputes the header checksum afterwards.
    PatchHeader { field: HeaderField, value: u32, resum: bool },
    /// WAL restart: a new generation (salt xor `salt_xor`) written over the head of the file.
    Restart { salt_xor: (u32, u32), commits: Vec<CommitSpec> },
}

#[derive(Arbitrary, Debug)]
struct Input {
    big_endian: bool,
    salt: (u32, u32),
    page_size: u8,
    commits: Vec<CommitSpec>,
    /// Frames after the last commit frame (an unfinished transaction); must be ignored.
    tail: Vec<FrameSpec>,
    mutation: Option<Mutation>,
    /// Split points for the resumed-parse checks.
    split: u32,
    stale_split: u32,
}

type Commits = Vec<wal::encode::Commit>;

fn page(seed: u32, page_size: usize) -> Vec<u8> {
    let mut x = (seed as u64) << 1 | 1;
    let mut p = Vec::with_capacity(page_size);
    while p.len() < page_size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        p.extend_from_slice(&x.to_le_bytes());
    }
    p.truncate(page_size);
    p
}

fn frames(specs: &[FrameSpec], limit: usize, ps: usize) -> Vec<(u32, Vec<u8>)> {
    specs.iter().take(limit).map(|f| (f.pgno, page(f.seed, ps))).collect()
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn put32(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_be_bytes());
}

/// Independent copy of SQLite's WAL checksum, used only to re-seal a patched header.
fn checksum(big_endian: bool, data: &[u8]) -> (u32, u32) {
    let rd = |b: &[u8]| {
        let a: [u8; 4] = b.try_into().unwrap();
        if big_endian { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    };
    let (mut s0, mut s1) = (0u32, 0u32);
    for c in data.chunks_exact(8) {
        s0 = s0.wrapping_add(rd(&c[0..4])).wrapping_add(s1);
        s1 = s1.wrapping_add(rd(&c[4..8])).wrapping_add(s0);
    }
    (s0, s1)
}

/// What the oracle accepts after a mutation.
struct Expect {
    lo: usize,
    hi: usize,
    /// Commits beyond the full input are tolerated (only for appended garbage).
    extra_ok: bool,
}

fuzz_target!(|input: Input| {
    let ps = [512usize, 1024, 4096][input.page_size as usize % 3];
    let flen = FRAME_HEADER + ps;

    let commits: Commits = input
        .commits
        .iter()
        .take(MAX_COMMITS)
        .filter(|c| !c.frames.is_empty())
        .map(|c| (frames(&c.frames, MAX_FRAMES_PER_COMMIT, ps), c.db_pages.max(1)))
        .collect();
    let tail = frames(&input.tail, MAX_TAIL, ps);

    // ends[i] = number of frames up to and including commit i.
    let mut ends = Vec::new();
    let mut n_frames = 0;
    for (f, _) in &commits {
        n_frames += f.len();
        ends.push(n_frames);
    }
    n_frames += tail.len();
    // Commits whose frames all have index < k.
    let before = |k: usize| ends.iter().filter(|&&e| e <= k).count();
    let all = commits.len();

    let mut to_encode = commits.clone();
    if !tail.is_empty() {
        to_encode.push((tail, 0)); // db_pages 0 on the last frame: no commit marker
    }
    let mut buf = wal::encode::encode(input.big_endian, input.salt, ps as u32, &to_encode);
    assert_eq!(buf.len(), WAL_HEADER + n_frames * flen);
    let original = buf.clone();
    let mut restarted: Option<Commits> = None;

    let frame_at = |k: usize| WAL_HEADER + k * flen;
    let exact = Expect { lo: all, hi: all, extra_ok: false };
    let damaged = |k: usize| Expect { lo: before(k), hi: before(k), extra_ok: false };

    let expect = match &input.mutation {
        None => exact,
        Some(Mutation::FlipByte { pos, mask }) => {
            let pos = pos % buf.len();
            buf[pos] ^= (*mask).max(1);
            if pos < 4 {
                // A flip can turn one valid magic into the other: the header is then checksummed
                // in the other byte order, which is not provably a mismatch. Prefix only.
                Expect { lo: 0, hi: all, extra_ok: false }
            } else if pos < WAL_HEADER {
                damaged(0)
            } else {
                damaged((pos - WAL_HEADER) / flen)
            }
        }
        Some(Mutation::Truncate { at }) => {
            let at = at % (buf.len() + 1);
            buf.truncate(at);
            if at < WAL_HEADER {
                damaged(0)
            } else {
                damaged((at - WAL_HEADER) / flen)
            }
        }
        Some(Mutation::OverwriteSalt { frame, second, value }) if n_frames > 0 => {
            let k = frame % n_frames;
            let off = frame_at(k) + if *second { 12 } else { 8 };
            if be32(&buf[off..]) == *value {
                exact
            } else {
                put32(&mut buf[off..], *value);
                damaged(k)
            }
        }
        Some(Mutation::ZeroChecksum { frame }) if n_frames > 0 => {
            let k = frame % n_frames;
            let off = frame_at(k) + 16;
            if buf[off..off + 8].iter().all(|&b| b == 0) {
                exact
            } else {
                buf[off..off + 8].fill(0);
                damaged(k)
            }
        }
        Some(Mutation::DuplicateFrame { frame }) if n_frames > 0 => {
            // The copy sits at index k+1 carrying frame k's checksum; accepting it would need a
            // checksum fixed point, so it is rejected in practice but not provably: prefix only.
            let k = frame % n_frames;
            let copy = buf[frame_at(k)..frame_at(k + 1)].to_vec();
            buf.splice(frame_at(k + 1)..frame_at(k + 1), copy);
            Expect { lo: before(k + 1), hi: all, extra_ok: false }
        }
        Some(Mutation::SwapFrames { a, b }) if n_frames > 1 => {
            let (a, b) = (a % n_frames, b % n_frames);
            let (a, b) = (a.min(b), a.max(b));
            let fa = buf[frame_at(a)..frame_at(a + 1)].to_vec();
            let fb = buf[frame_at(b)..frame_at(b + 1)].to_vec();
            if fa == fb {
                exact
            } else {
                buf[frame_at(a)..frame_at(a + 1)].copy_from_slice(&fb);
                buf[frame_at(b)..frame_at(b + 1)].copy_from_slice(&fa);
                Expect { lo: before(a), hi: all, extra_ok: false }
            }
        }
        Some(Mutation::AppendGarbage { bytes }) => {
            buf.extend(bytes.iter().take(MAX_GARBAGE));
            Expect { lo: all, hi: all, extra_ok: !bytes.is_empty() }
        }
        Some(Mutation::AppendStaleGeneration { salt_xor, frames: specs }) if *salt_xor != (0, 0) => {
            let stale_salt = (input.salt.0 ^ salt_xor.0, input.salt.1 ^ salt_xor.1);
            let stale: Commits = vec![(frames(specs, MAX_FRAMES_PER_COMMIT, ps), 1)];
            let other = wal::encode::encode(input.big_endian, stale_salt, ps as u32, &stale);
            buf.extend_from_slice(&other[WAL_HEADER..]);
            exact
        }
        Some(Mutation::PatchHeader { field, value, resum }) => {
            let off = match field {
                HeaderField::Magic => 0,
                HeaderField::Version => 4,
                HeaderField::PageSize => 8,
                HeaderField::CkptSeq => 12,
                HeaderField::Salt1 => 16,
                HeaderField::Salt2 => 20,
            };
            if be32(&buf[off..]) == *value {
                exact
            } else {
                put32(&mut buf[off..], *value);
                if matches!(field, HeaderField::Magic) && !*resum {
                    Expect { lo: 0, hi: all, extra_ok: false }
                } else if !*resum {
                    damaged(0)
                } else {
                    let be = match be32(&buf[0..4]) {
                        MAGIC_BE => Some(true),
                        MAGIC_LE => Some(false),
                        _ => None,
                    };
                    if let Some(be) = be {
                        let (c0, c1) = checksum(be, &buf[0..24]);
                        put32(&mut buf[24..], c0);
                        put32(&mut buf[28..], c1);
                    }
                    match field {
                        // Byte order or frame layout changed: no clean proof, prefix only.
                        HeaderField::Magic | HeaderField::PageSize => Expect { lo: 0, hi: all, extra_ok: false },
                        // Salt mismatch, or the header checksum that seeds frame 0's chain changed.
                        _ => damaged(0),
                    }
                }
            }
        }
        Some(Mutation::Restart { salt_xor, commits: specs }) if *salt_xor != (0, 0) => {
            let salt = (input.salt.0 ^ salt_xor.0, input.salt.1 ^ salt_xor.1);
            let new: Commits = specs
                .iter()
                .take(MAX_COMMITS)
                .filter(|c| !c.frames.is_empty())
                .map(|c| (frames(&c.frames, MAX_FRAMES_PER_COMMIT, ps), c.db_pages.max(1)))
                .collect();
            let head = wal::encode::encode(input.big_endian, salt, ps as u32, &new);
            if buf.len() < head.len() {
                buf.resize(head.len(), 0);
            }
            buf[..head.len()].copy_from_slice(&head);
            restarted = Some(new);
            exact // unused: the restart oracle below replaces the prefix oracle
        }
        // Frame-indexed mutations on a WAL with too few frames are no-ops.
        Some(_) => exact,
    };

    check_resume(&buf, &original, input.split, input.stale_split);
    if let Some(new) = restarted {
        // Old-generation frames left after the new ones carry the old salt: never admitted.
        let w = wal::parse(&buf).expect("restart header is valid").expect("restart header is sealed");
        assert!(owned(&w) == new, "restart: parse is not exactly the new generation");
        return;
    }

    // Must never panic, whatever the bytes.
    let parsed = wal::parse(&buf);
    let got: Commits = match &parsed {
        Ok(Some(w)) => owned(w),
        _ => Vec::new(),
    };

    if input.mutation.is_none() {
        let w = parsed.as_ref().expect("valid WAL rejected").as_ref().expect("valid WAL reported empty");
        assert_eq!(w.page_size as usize, ps);
        assert_eq!(w.salt, input.salt);
        assert!(got == commits, "unmutated parse differs: {} commits, expected {}", got.len(), all);
        return;
    }

    let n = if expect.extra_ok { got.len().min(all) } else { got.len() };
    assert!(n <= all, "parsed {} commits, input has only {}", got.len(), all);
    assert!(got[..n] == commits[..n], "parsed commits are not a prefix of the input (altered data)");
    assert!(
        expect.lo <= n && n <= expect.hi,
        "parsed {} commits, oracle expects between {} and {}",
        n,
        expect.lo,
        expect.hi
    );
    if expect.extra_ok && got.len() > all {
        assert_eq!(n, all);
    }
});

fn owned(w: &wal::Wal) -> Commits {
    w.commits.iter().map(|c| (c.frames.iter().map(|f| (f.pgno, f.page.to_vec())).collect(), c.db_pages)).collect()
}

/// Resume from `from` against the whole file `buf`, reading it as a capture does.
fn resume_on<'a>(buf: &'a [u8], from: &wal::Resume) -> Option<wal::Wal<'a>> {
    let start = (from.tail_start() as usize).min(buf.len());
    wal::parse_from(&buf[..buf.len().min(WAL_HEADER)], &buf[start..], from)
}

/// The resumed-parse oracle (see the module docs).
fn check_resume(buf: &[u8], original: &[u8], split: u32, stale_split: u32) {
    let Ok(Some(full)) = wal::parse(buf) else {
        // No valid header now: no state can fit it.
        let o = wal::parse(original).unwrap().unwrap();
        let cut = WAL_HEADER + stale_split as usize % (original.len() - WAL_HEADER + 1);
        let state = wal::parse(&original[..cut]).unwrap().unwrap().end;
        assert!(resume_on(buf, &o.end).is_none() && resume_on(buf, &state).is_none(), "state fit a WAL without a valid header");
        return;
    };

    // Split: any prefix of this very buffer.
    let cut = WAL_HEADER + split as usize % (buf.len() - WAL_HEADER + 1);
    let prefix = wal::parse(&buf[..cut]).expect("prefix of a valid header").expect("prefix has the header");
    let rest = resume_on(buf, &prefix.end).expect("a state from a prefix fits the whole buffer");
    let mut got = owned(&prefix);
    got.extend(owned(&rest));
    assert!(got == owned(&full), "prefix + resumed != full parse");
    assert_eq!(rest.end, full.end, "resumed end state != full parse end state");
    assert_eq!(rest.end.commits as usize, full.commits.len());

    // Stale state from the unmutated WAL.
    let cut = WAL_HEADER + stale_split as usize % (original.len() - WAL_HEADER + 1);
    let state = wal::parse(&original[..cut]).unwrap().unwrap().end;
    let first_diff = buf.iter().zip(original).position(|(a, b)| a != b).unwrap_or(buf.len().min(original.len()));
    let resumed = resume_on(buf, &state);
    if (buf.len() as u64) < state.offset || first_diff < WAL_HEADER {
        assert!(resumed.is_none(), "state accepted by a shorter file or another header");
    } else if first_diff as u64 >= state.offset {
        let r = resumed.expect("prefix up to the state is intact, so the state must fit");
        let all = owned(&full);
        assert!(all.len() >= state.commits as usize, "full parse lost commits before an intact state");
        assert!(owned(&r)[..] == all[state.commits as usize..], "stale resume != full parse past the state");
        assert_eq!(r.end, full.end);
    }
    // A change inside the prefix (after the header) is outside the resume contract: the WAL is
    // append-only within a generation. Only "no panic" is required there.
}
