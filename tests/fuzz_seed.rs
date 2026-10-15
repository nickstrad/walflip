//! Seed generator for `fuzz/corpus/` (docs/TESTING_PLAN.md section 2.2). Not a regular test:
//! `scripts/fuzz-seed.sh` runs it with `--ignored`, then runs `cargo fuzz cmin`.
//!
//! Writes raw WAL files into `<out>/wal_parse/` and the same bytes behind a one-byte control
//! prefix (page size index, see `fuzz_targets/store_capture.rs`) into `<out>/store_capture/`,
//! where `<out>` is `$WALFLIP_SEED_OUT` (default `fuzz/corpus`). `scripts/fuzz-seed.sh` sets
//! `WALFLIP_SEED_OUT` to a scratch directory, minimises the result with `cargo fuzz cmin` and
//! copies it into the committed corpus.
#![cfg(feature = "testing")]

mod common;

use common::Harness;
use std::fs;
use std::path::{Path, PathBuf};
use walflip::wal::encode;

const SCHEMA: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);";

fn corpus_dir() -> PathBuf {
    match std::env::var_os("WALFLIP_SEED_OUT") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus"),
    }
}

fn write_seed(target: &str, name: &str, bytes: &[u8]) {
    let dir = corpus_dir().join(target);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("seed-{name}")), bytes).unwrap();
}

/// Raw WAL bytes for the parser, and the same with a store_capture control byte in front.
/// The control byte selects page size 512 << (ctl & 3), so only 512..=4096 can be expressed.
fn write_wal(name: &str, wal: &[u8], page_size: u32) {
    write_seed("wal_parse", name, wal);
    if let Some(idx) = [512u32, 1024, 2048, 4096]
        .iter()
        .position(|&p| p == page_size)
    {
        let mut cap = vec![idx as u8];
        cap.extend_from_slice(wal);
        write_seed("store_capture", name, &cap);
    }
}

/// Read `live.db-wal`. Call while the connection that wrote it is still open, so that closing
/// it (last connection, checkpoint and delete the WAL) happens after the copy.
fn read_live_wal(db_dir: &Path) -> Vec<u8> {
    fs::read(db_dir.join("live.db-wal")).unwrap()
}

fn real_wals(h: &Harness) {
    // 1 frame: a single-row insert into a table whose page is already in the base file.
    h.create("one", SCHEMA);
    write_wal(
        "real-one-frame",
        &h.wal_snapshot("one", "INSERT INTO t(v) VALUES('a')"),
        4096,
    );

    // Multi-page commit: a ~18 KB text value overflows onto several pages.
    h.create("blob", SCHEMA);
    let blob = "INSERT INTO t(v) VALUES(replace(hex(zeroblob(9000)),'0','x'))";
    write_wal("real-blob", &h.wal_snapshot("blob", blob), 4096);

    // DDL commit: schema change in its own transaction.
    h.create("ddl", SCHEMA);
    write_wal(
        "real-ddl",
        &h.wal_snapshot("ddl", "CREATE TABLE u(x, y)"),
        4096,
    );

    // 50 commits in one batch: one statement per line, each its own commit.
    h.create("batch", SCHEMA);
    let batch: Vec<String> = (0..50)
        .map(|i| format!("INSERT INTO t(v) VALUES('row {i}')"))
        .collect();
    write_wal(
        "real-batch50",
        &h.wal_snapshot("batch", &batch.join("\n")),
        4096,
    );

    // VACUUM generation: grow the file, delete, then VACUUM (db_pages shrinks). The WAL is
    // read while the VACUUM connection is still open.
    h.create("vac", SCHEMA);
    let fill: Vec<String> = (0..60)
        .map(|i| {
            format!(
                "INSERT INTO t(v) VALUES(replace(hex(zeroblob(3000)),'0','{}'))",
                i % 10
            )
        })
        .collect();
    h.with_stdin(&["--db", "vac", "exec", "-"], &fill.join("\n"));
    h.exec("vac", "DELETE FROM t WHERE id > 1");
    let db_dir = h.root().join("vac");
    let conn = rusqlite::Connection::open(db_dir.join("live.db")).unwrap();
    conn.execute_batch("VACUUM").unwrap();
    write_wal("real-vacuum", &read_live_wal(&db_dir), 4096);
    drop(conn);

    // Restarted WAL: generation 1 (several frames) is captured, then the WAL is restarted (not
    // truncated) and generation 2 (one frame) is written over the start of the same file. The
    // file holds the new frame followed by stale generation-1 frames.
    // Everything runs on one connection: closing the last connection would delete the WAL.
    h.create("restart", SCHEMA);
    let db_dir = h.root().join("restart");
    let conn = rusqlite::Connection::open(db_dir.join("live.db")).unwrap();
    conn.execute_batch("INSERT INTO t(v) VALUES(replace(hex(zeroblob(9000)),'0','g'))")
        .unwrap();
    write_wal("real-restart-gen1", &read_live_wal(&db_dir), 4096);
    conn.execute_batch("PRAGMA wal_checkpoint(RESTART)")
        .unwrap();
    conn.execute_batch("INSERT INTO t(v) VALUES('gen2')")
        .unwrap();
    write_wal("real-restart-two-gens", &read_live_wal(&db_dir), 4096);
    drop(conn);
}

/// Encoder-generated WALs: both byte orders, each page size, 0/1/5 commits, with and without a
/// stale generation appended after the valid frames.
fn encoded_wals() {
    for big_endian in [false, true] {
        let order = if big_endian { "be" } else { "le" };
        for page_size in [512u32, 1024, 4096, 65536] {
            for n in [0usize, 1, 5] {
                // 5 full-size 64 KB pages would be ~1 MB per seed; two commits cover the case.
                if page_size == 65536 && n == 5 {
                    continue;
                }
                let commits: Vec<encode::Commit> = (0..n)
                    .map(|c| {
                        let frames = (0..=c as u32)
                            .map(|k| {
                                let seed = (c * 7 + k as usize) as u8;
                                let page: Vec<u8> = (0..page_size as usize)
                                    .map(|i| (i as u8).wrapping_mul(seed | 1))
                                    .collect();
                                (k + 1, page)
                            })
                            .collect();
                        (frames, c as u32 + 1)
                    })
                    .collect();
                let salt = (0x1111_0000 + n as u32, 0x2222_0000 + page_size);
                let mut wal = encode::encode(big_endian, salt, page_size, &commits);
                let name = format!("enc-{order}-{page_size}-{n}c");
                write_wal(&name, &wal, page_size);

                if n > 0 {
                    // Stale tail: frames of another generation (different salt) after the valid ones.
                    let stale = encode::encode(
                        big_endian,
                        (salt.0 ^ 0xffff, salt.1 ^ 0xffff),
                        page_size,
                        &commits[..1],
                    );
                    wal.extend_from_slice(&stale[32..]);
                    write_wal(&format!("{name}-stale"), &wal, page_size);
                }
            }
        }
    }
}

#[test]
#[ignore = "seed generator: run via scripts/fuzz-seed.sh"]
fn generate_fuzz_seeds() {
    real_wals(&Harness::new());
    encoded_wals();
}
