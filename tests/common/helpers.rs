//! Helper processes for concurrency scenarios (docs/TESTING_PLAN.md S12-S15, risks R1/R2).
//!
//! Helpers are plain Rust, no Python: the test binary re-executes itself, running only the
//! test named [`ENTRY`], with the role in an environment variable. Every test file that uses
//! helpers must contain
//!
//! ```ignore
//! #[test]
//! fn helper_process_entry() {
//!     common::helpers::entry();
//! }
//! ```
//!
//! which is a no-op in a normal test run.
//!
//! All synchronisation goes through pipes, never through sleeps:
//! - a helper prints a line starting with [`READY`] once it reached its barrier;
//!   [`Helper::wait_ready`] blocks on that line;
//! - a helper waiting at a barrier blocks on its stdin; [`Helper::release`] writes to it
//!   and closes it.
//!
//! Roles:
//! - `gate`: barrier, then `exec`s the walflip binary in place (same pid, same pipes). Used to
//!   start many walflip processes as close together as the scheduler allows.
//! - `reader`: opens `live.db`, holds a read transaction (`BEGIN; SELECT ...`) until released.
//! - `crash-writer`: writes with `wal_autocheckpoint=0`, then exits without closing the
//!   connection (like `os._exit`), leaving commits in `live.db-wal` that nothing captured.
#![cfg(unix)]

use rusqlite::Connection;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

/// Name of the `#[test]` function that dispatches to a helper role.
pub const ENTRY: &str = "helper_process_entry";
/// Prefix of the line a helper prints when it reached its barrier.
pub const READY: &str = "WALFLIP-HELPER-READY";

const ROLE_VAR: &str = "WALFLIP_TEST_HELPER";
const ARGS_VAR: &str = "WALFLIP_TEST_HELPER_ARGS";
const SEP: char = '\u{1f}';

/// Body of the [`ENTRY`] test: returns at once in a normal run, otherwise runs the role and
/// never returns.
pub fn entry() {
    let Ok(role) = std::env::var(ROLE_VAR) else {
        return;
    };
    let raw = std::env::var(ARGS_VAR).unwrap_or_default();
    let args: Vec<String> = if raw.is_empty() {
        vec![]
    } else {
        raw.split(SEP).map(String::from).collect()
    };
    match role.as_str() {
        "gate" => role_gate(&args),
        "reader" => role_reader(&args),
        "crash-writer" => role_crash_writer(&args),
        "crash-spill" => role_crash_spill(&args),
        other => {
            eprintln!("unknown helper role {other}");
            std::process::exit(2);
        }
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

/// Read fd 0 one byte at a time up to and including `\n`, without buffering past it, so
/// whatever follows on stdin is left for the program we `exec` next. Returns false on EOF.
fn read_gate_line() -> bool {
    // SAFETY: fd 0 is open for the life of the process; ManuallyDrop keeps it open.
    let mut fd0 = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut b = [0u8; 1];
    loop {
        match fd0.read(&mut b) {
            Ok(0) => return false,
            Ok(_) if b[0] == b'\n' => return true,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("gate: reading stdin: {e}"),
        }
    }
}

fn role_gate(args: &[String]) -> ! {
    say(READY);
    if !read_gate_line() {
        eprintln!("gate: stdin closed before release");
        std::process::exit(3);
    }
    let err = Command::new(env!("CARGO_BIN_EXE_walflip"))
        .args(args)
        .env_remove("WALFLIP_ROOT")
        .env_remove("WALFLIP_DB")
        .env_remove(ROLE_VAR)
        .env_remove(ARGS_VAR)
        .exec();
    eprintln!("gate: exec walflip failed: {err}");
    std::process::exit(4);
}

/// args: `[live_db, query]`. Prints `READY <value>` with the query's value inside the read
/// transaction, waits for stdin EOF, re-runs the query in the same transaction, prints
/// `RELEASED <value>`, ends the transaction and closes the connection normally.
fn role_reader(args: &[String]) -> ! {
    let [live, query] = args else {
        panic!("reader: want [live_db, query], got {args:?}")
    };
    let conn = Connection::open(live).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    let before: i64 = conn.query_row(query, [], |r| r.get(0)).unwrap();
    say(&format!("{READY} {before}"));
    let mut sink = Vec::new();
    std::io::stdin().read_to_end(&mut sink).unwrap();
    let after: i64 = conn.query_row(query, [], |r| r.get(0)).unwrap();
    conn.execute_batch("COMMIT").unwrap();
    drop(conn);
    say(&format!("RELEASED {after}"));
    std::process::exit(0);
}

/// args: `[live_db, stmt...]`. Runs each statement as its own autocommit transaction with
/// `wal_autocheckpoint=0`, prints `WROTE <n>`, then exits without closing the connection.
fn role_crash_writer(args: &[String]) -> ! {
    let (live, stmts) = args
        .split_first()
        .expect("crash-writer: want [live_db, stmt...]");
    let conn = Connection::open(live).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    for s in stmts {
        conn.execute_batch(s)
            .unwrap_or_else(|e| panic!("crash-writer: {s}: {e}"));
    }
    say(&format!("WROTE {}", stmts.len()));
    // No sqlite3_close: no checkpoint, no WAL cleanup, exactly like a killed process.
    std::mem::forget(conn);
    std::process::exit(0);
}

/// args: `[db]`. Opens `db` in rollback-journal mode with a one-page cache, `BEGIN IMMEDIATE`,
/// inserts enough rows into `meta` to force dirty pages out to the db file (so the journal
/// holds real before-images), prints `WROTE <n>`, then exits without commit or close. Leaves
/// a hot `<db>-journal`, like a writer killed mid-transaction.
fn role_crash_spill(args: &[String]) -> ! {
    let conn = Connection::open(&args[0]).unwrap();
    conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    conn.pragma_update(None, "synchronous", "OFF").unwrap();
    conn.pragma_update(None, "cache_size", 1).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let n = 400;
    for i in 0..n {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, hex(zeroblob(2000)))",
            [format!("junk{i}")],
        )
        .unwrap();
    }
    say(&format!("WROTE {n}"));
    std::mem::forget(conn);
    std::process::exit(0);
}

/// Leave a hot rollback journal on the sqlite db at `db` (needs a `meta` table); see
/// `role_crash_spill`.
pub fn crash_spill(db: &Path) -> Finished {
    let mut h = spawn("crash-spill", &[db.display().to_string()]);
    h.release(b"");
    h.finish()
}

/// A running helper process with piped stdin/stdout/stderr.
pub struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Lines read before (and including) the READY line, for diagnostics.
    seen: Vec<String>,
}

/// Exit status and remaining output of a finished helper.
#[derive(Debug)]
pub struct Finished {
    pub status: ExitStatus,
    /// Stdout after the READY line (for a gate: everything walflip printed).
    pub stdout: String,
    pub stderr: String,
}

fn spawn(role: &str, args: &[String]) -> Helper {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([ENTRY, "--exact", "--nocapture", "--test-threads=1", "-q"])
        .env(ROLE_VAR, role)
        .env(ARGS_VAR, args.join(&SEP.to_string()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    Helper {
        child,
        stdin,
        stdout,
        seen: vec![],
    }
}

/// A walflip process (`--root <root> <args>`) that starts only when released. Waits until the
/// gate is up before returning.
pub fn gated_walflip(root: &Path, args: &[&str]) -> Helper {
    let mut full = vec!["--root".to_string(), root.display().to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    let mut h = spawn("gate", &full);
    h.wait_ready();
    h
}

/// A reader holding `BEGIN; <query>` on `live`; returns once the transaction is open, with
/// the value the query returned inside it.
pub fn reader(live: &Path, query: &str) -> (Helper, i64) {
    let mut h = spawn("reader", &[live.display().to_string(), query.to_string()]);
    let v = h
        .wait_ready()
        .trim()
        .parse()
        .expect("reader prints an integer");
    (h, v)
}

/// Run `stmts` against `live` from a process that dies without closing SQLite. Returns
/// after the process exited.
pub fn crash_writer(live: &Path, stmts: &[&str]) -> Finished {
    let mut args = vec![live.display().to_string()];
    args.extend(stmts.iter().map(|s| s.to_string()));
    let mut h = spawn("crash-writer", &args);
    h.release(b"");
    h.finish()
}

impl Helper {
    /// Block until the helper printed its READY line; returns the text after `READY`.
    pub fn wait_ready(&mut self) -> String {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).unwrap();
            if n == 0 {
                let mut err = String::new();
                if let Some(mut e) = self.child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                panic!(
                    "helper exited before READY; stdout {:?}; stderr {err}",
                    self.seen
                );
            }
            let line = line.trim_end().to_string();
            if let Some(rest) = line.strip_prefix(READY) {
                let rest = rest.to_string();
                self.seen.push(line);
                return rest;
            }
            self.seen.push(line);
        }
    }

    /// Write `input` to the helper's stdin and close it (the release barrier).
    pub fn release(&mut self, input: &[u8]) {
        if let Some(mut s) = self.stdin.take() {
            // A helper that already died cannot read; its exit status reports why.
            let _ = s.write_all(input);
        }
    }

    /// Close stdin if still open, wait for exit, collect remaining output.
    pub fn finish(mut self) -> Finished {
        self.stdin.take();
        let mut stdout = String::new();
        self.stdout.read_to_string(&mut stdout).unwrap();
        let mut stderr = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            e.read_to_string(&mut stderr).unwrap();
        }
        let status = self.child.wait().unwrap();
        Finished {
            status,
            stdout,
            stderr,
        }
    }
}

/// Release every gate with the same `input` (after `go\n`), back to back.
pub fn release_all(helpers: &mut [Helper], input: &str) {
    let payload = format!("go\n{input}");
    for h in helpers.iter_mut() {
        h.release(payload.as_bytes());
    }
}
