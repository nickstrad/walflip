//! Shared harness: runs the real `walflip` binary against a throwaway data directory.
#![allow(dead_code)]

pub mod invariants;
pub mod model;
pub mod scenario;

use rusqlite::{Connection, OpenFlags};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

pub struct Harness {
    pub tmp: TempDir,
}

impl Harness {
    pub fn new() -> Self {
        Self { tmp: tempfile::tempdir().unwrap() }
    }

    pub fn root(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    /// A path for output files (clones) inside the temp dir.
    pub fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// Bare command with the ambient walflip env vars cleared.
    pub fn bare(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_walflip"));
        c.env_remove("WALFLIP_ROOT").env_remove("WALFLIP_DB");
        c
    }

    /// Command pointed at this harness's data root via `--root`.
    pub fn cmd(&self) -> Command {
        let mut c = self.bare();
        c.arg("--root").arg(self.root());
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    /// Run, expect success, return stdout.
    pub fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(o.status.success(), "walflip {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    /// Run, expect failure, return stderr.
    pub fn err(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(!o.status.success(), "walflip {args:?} unexpectedly succeeded");
        String::from_utf8(o.stderr).unwrap()
    }

    /// Run with `input` piped to stdin; returns the raw output.
    pub fn with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .cmd()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Write `schema` to a file and run `create <name> --schema <file>`; returns stdout.
    pub fn create(&self, name: &str, schema: &str) -> String {
        let file = self.path(&format!("{name}.schema.sql"));
        std::fs::write(&file, schema).unwrap();
        self.ok(&["create", name, "--schema", file.to_str().unwrap()])
    }

    /// `exec <sql>` on `db`; expects success and returns N from `captured N commit(s)`.
    pub fn exec(&self, db: &str, sql: &str) -> u64 {
        parse_captured(&self.ok(&["--db", db, "exec", sql]))
    }

    /// Commit count as reported by `status --db <db>`.
    pub fn commits(&self, db: &str) -> u64 {
        let out = self.ok(&["--db", db, "status"]);
        out.lines()
            .find_map(|l| l.strip_prefix("commits   : "))
            .expect("status has a commits line")
            .trim()
            .parse()
            .unwrap()
    }
}

/// Read-only, immutable open: never creates `-wal`/`-shm`/`-journal` beside `path`.
pub fn open_ro(path: &Path) -> Connection {
    Connection::open_with_flags(
        format!("file:{}?immutable=1", path.display()),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
}

/// First column of every row, as integers.
pub fn ints(path: &Path, sql: &str) -> Vec<i64> {
    let conn = open_ro(path);
    let mut st = conn.prepare(sql).unwrap();
    let rows = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows
}

/// First column of every row, as text.
pub fn texts(path: &Path, sql: &str) -> Vec<String> {
    let conn = open_ro(path);
    let mut st = conn.prepare(sql).unwrap();
    let rows = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows
}

/// N from the `captured N commit(s)` line printed by `exec`.
pub fn parse_captured(stdout: &str) -> u64 {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("captured ")?.split_whitespace().next()?.parse().ok())
        .unwrap_or_else(|| panic!("no `captured N` line in {stdout:?}"))
}
