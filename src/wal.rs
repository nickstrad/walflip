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
