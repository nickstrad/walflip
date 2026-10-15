//! S20 perf smoke. The stdin batch runs by default (INV-51); the per-process run is ignored
//! (2000 process spawns). Run both with
//! `cargo test --features testing --test perf -- --include-ignored --nocapture`.
//!
//! Prints wall-clock timings and asserts loose budgets. INV-51: a 2000-insert stdin batch
//! finishes in bounded time (< 10 s in a debug build). Capture resumes from the WAL offset stored
//! in the cursor (T24), so each statement reads only its own new frames, not the whole WAL.

mod common;
use common::{ints, Harness};
use std::time::{Duration, Instant};

const N: usize = 2000;

/// INV-51 budget for the batch in a debug build.
const BATCH_BUDGET: Duration = Duration::from_secs(10);
/// Loose ceiling for 2000 separate process runs (not a spec budget; catches hangs only).
const PER_PROCESS_CEILING: Duration = Duration::from_secs(600);

fn schema(h: &Harness, name: &str) {
    h.create(
        name,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER NOT NULL);",
    );
}

/// One statement per line; the batch reader treats each non-blank line as one statement.
fn batch_script(start: usize, n: usize) -> String {
    (start..start + n)
        .map(|i| format!("INSERT INTO t(id, v) VALUES({i}, {})\n", i * 3))
        .collect()
}

#[test]
fn s20_stdin_batch_of_2000_inserts() {
    let h = Harness::new();
    schema(&h, "batch");

    let script = batch_script(1, N);
    let start = Instant::now();
    let out = h.with_stdin(&["--db", "batch", "exec", "-"], &script);
    let elapsed = start.elapsed();

    assert!(
        out.status.success(),
        "batch exec failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("captured {N} commit(s)")),
        "unexpected output: {stdout}"
    );
    assert_eq!(h.commits("batch"), N as u64);

    println!(
        "S20 stdin batch: {N} inserts in {:.3} s ({:.2} ms/stmt), budget {:.0} s (INV-51, debug)",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / N as f64,
        BATCH_BUDGET.as_secs_f64(),
    );
    assert!(
        elapsed < BATCH_BUDGET,
        "INV-51: batch took {elapsed:?}, budget {BATCH_BUDGET:?}"
    );
}

#[test]
#[ignore = "perf smoke S20: run with --ignored --nocapture"]
fn s20_2000_per_process_execs() {
    let h = Harness::new();
    schema(&h, "procs");

    let start = Instant::now();
    for i in 1..=N {
        h.exec(
            "procs",
            &format!("INSERT INTO t(id, v) VALUES({i}, {})", i * 3),
        );
    }
    let elapsed = start.elapsed();

    assert_eq!(h.commits("procs"), N as u64);
    let out = h.path("procs-final.db");
    h.ok(&[
        "--db",
        "procs",
        "clone",
        "-n",
        &N.to_string(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(ints(&out, "SELECT count(*) FROM t"), vec![N as i64]);

    println!(
        "S20 per-process exec: {N} processes in {:.3} s ({:.2} ms/exec)",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / N as f64,
    );
    assert!(
        elapsed < PER_PROCESS_CEILING,
        "per-process execs took {elapsed:?}"
    );
}
