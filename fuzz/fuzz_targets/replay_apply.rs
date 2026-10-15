//! Replay onto an in-memory image. Invariants: no panic; image never exceeds
//! max_pages * page_size; Err leaves the image byte-identical; on Ok every frame inside the
//! final image is present unless a later frame overwrote the same page.
#![no_main]

use libfuzzer_sys::fuzz_target;
use walflip::replay::apply;

struct Rd<'a>(&'a [u8]);
impl Rd<'_> {
    fn u8(&mut self) -> u8 {
        match self.0.split_first() {
            Some((b, rest)) => {
                self.0 = rest;
                *b
            }
            None => 0,
        }
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }
}

const MAX_CAP: u32 = 256;

fuzz_target!(|data: &[u8]| {
    let mut r = Rd(data);
    let ps = 512usize << (r.u8() % 4);
    let max_pages = 1 + u32::from(r.u8()) % MAX_CAP;
    let base_len = (usize::from(r.u8()) * 37 + usize::from(r.u8())) % (8 * ps + 1);
    let db_pages = match r.u8() % 4 {
        0 => None,
        1 => Some(r.u32()), // usually > max_pages
        _ => Some(u32::from(r.u8()) % (MAX_CAP + 2)),
    };
    let salt = r.u8();
    let mut image: Vec<u8> = (0..base_len).map(|i| (i % 251) as u8 ^ salt).collect();
    let before = image.clone();

    let mut frames: Vec<(u32, Vec<u8>)> = Vec::new();
    while frames.len() < 24 && !r.0.is_empty() {
        let tag = r.u8();
        let pgno = match tag & 7 {
            0 => r.u32(),                                      // full range, incl. 0
            1 => MAX_CAP + 1 + r.u32() % (u32::MAX - MAX_CAP), // beyond any cap
            _ => 1 + u32::from(r.u8()) % 12,
        };
        let len = if tag >> 4 == 0 { ps - 1 - usize::from(tag & 1) } else { ps };
        let mut page = vec![r.u8(); len];
        page[0] = frames.len() as u8;
        frames.push((pgno, page));
    }

    let res = apply(
        &mut image,
        ps,
        frames.iter().map(|(p, d)| (*p, d.as_slice())),
        db_pages,
        max_pages,
    );
    match res {
        Err(_) => assert_eq!(image, before, "Err must not modify the image"),
        Ok(()) => {
            // A base image longer than the cap is never shrunk unless db_pages says so.
            assert!(image.len() <= before.len().max(max_pages as usize * ps));
            let pages = image.len() / ps;
            for (i, (pgno, page)) in frames.iter().enumerate() {
                assert_eq!(page.len(), ps);
                if *pgno as usize > pages || frames[i + 1..].iter().any(|(p, _)| p == pgno) {
                    continue;
                }
                let s = (*pgno as usize - 1) * ps;
                assert_eq!(&image[s..s + ps], page.as_slice());
            }
            if let Some(d) = db_pages {
                assert_eq!(image.len(), d as usize * ps);
            }
        }
    }
});
