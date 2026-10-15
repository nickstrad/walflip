mod store;
mod wal;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::{Connection, OpenFlags};
use std::fs::{self, OpenOptions};
use std::io::BufRead;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use store::Store;

#[derive(Parser)]
#[command(about = "Capture SQLite WAL commits and rebuild the database at any point in history")]
struct Cli {
    /// Directory holding live.db, base.db and meta.db
    #[arg(long, global = true, default_value = "walflip-data")]
    dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the live db from a schema file, checkpoint it, and save it as the base snapshot
    Init {
        #[arg(long)]
        schema: PathBuf,
        /// Wipe an existing data dir
        #[arg(long)]
        force: bool,
    },
    /// Run SQL against the live db; each statement is one transaction. Use `-` to read
    /// one statement per line from stdin.
    Exec { sql: String },
    /// List captured commits
    Log {
        #[arg(short, long, default_value_t = 20)]
        limit: u64,
    },
    /// Summarise what has been captured
    Status,
    /// Rebuild the database as of commit N: copy base.db, then replay frames 1..=N
    Clone {
        #[arg(short = 'n', long)]
        n: u64,
        #[arg(short, long)]
        out: PathBuf,
    },
}

struct Paths {
    dir: PathBuf,
    live: PathBuf,
    base: PathBuf,
    meta: PathBuf,
}

impl Paths {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            live: dir.join("live.db"),
            base: dir.join("base.db"),
            meta: dir.join("meta.db"),
        }
    }
    fn wal(&self) -> PathBuf {
        let mut s = self.live.clone().into_os_string();
        s.push("-wal");
        s.into()
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let p = Paths::new(&cli.dir);
    match cli.cmd {
        Cmd::Init { schema, force } => init(&p, &schema, force),
        Cmd::Exec { sql } => exec(&p, &sql),
        Cmd::Log { limit } => log(&p, limit),
        Cmd::Status => status(&p),
        Cmd::Clone { n, out } => clone(&p, n, &out),
    }
}

fn init(p: &Paths, schema: &Path, force: bool) -> Result<()> {
    if p.live.exists() {
        if !force {
            bail!("{} already exists (use --force)", p.live.display());
        }
        fs::remove_dir_all(&p.dir)?;
    }
    fs::create_dir_all(&p.dir)?;
    let sql = fs::read_to_string(schema).with_context(|| format!("reading {}", schema.display()))?;

    let conn = Connection::open(&p.live)?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if mode != "wal" {
        bail!("could not enable WAL (got {mode})");
    }
    conn.execute_batch(&sql)?;
    // Fold the schema into the main file so base.db is a complete, WAL-free starting point.
    conn.pragma_update_and_check(None, "wal_checkpoint", "TRUNCATE", |_| Ok(()))?;
    let page_size: u32 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    drop(conn);

    fs::copy(&p.live, &p.base)?;
    Store::open(&p.meta)?.set("page_size", &page_size.to_string())?;
    println!("initialised {} (page size {page_size}), base snapshot at {}", p.live.display(), p.base.display());
    Ok(())
}

fn exec(p: &Paths, sql: &str) -> Result<()> {
    let mut store = Store::open(&p.meta)?;
    let conn = Connection::open(&p.live)?;
    // Stop SQLite from folding the WAL back into the db while we are still reading it.
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;

    let mut run = |stmt: &str| -> Result<usize> {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(stmt).with_context(|| format!("failed: {stmt}"))?;
        tx.commit()?;
        store.capture(&p.wal(), stmt)
    };

    let stmts: Vec<String> = if sql == "-" {
        std::io::stdin().lock().lines().collect::<std::io::Result<_>>()?
    } else {
        vec![sql.to_string()]
    };
    let total: usize = stmts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| run(s))
        .sum::<Result<_>>()?;
    println!("captured {total} commit(s)");
    Ok(())
}

fn log(p: &Paths, limit: u64) -> Result<()> {
    let store = Store::open(&p.meta)?;
    let mut st = store.conn().prepare(
        "SELECT c.idx, count(f.seq), c.db_pages, c.label
         FROM commits c LEFT JOIN frames f ON f.commit_idx = c.idx
         GROUP BY c.idx ORDER BY c.idx LIMIT ?1",
    )?;
    println!("{:>5} {:>6} {:>8}  sql", "n", "frames", "db_pages");
    let mut rows = st.query([limit])?;
    while let Some(r) = rows.next()? {
        let (n, frames, pages): (u64, u64, u64) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let sql: String = r.get::<_, String>(3)?.chars().take(90).collect();
        println!("{n:>5} {frames:>6} {pages:>8}  {sql}");
    }
    Ok(())
}

fn status(p: &Paths) -> Result<()> {
    let store = Store::open(&p.meta)?;
    let commits = store.commit_count()?;
    let frames: u64 = store.conn().query_row("SELECT count(*) FROM frames", [], |r| r.get(0))?;
    println!("page size : {}", store.page_size()?.unwrap_or_default());
    println!("commits   : {commits}");
    println!("frames    : {frames}");
    println!("live wal  : {}", fs::metadata(p.wal()).map(|m| format!("{} bytes", m.len())).unwrap_or("absent (checkpointed)".into()));
    Ok(())
}

fn clone(p: &Paths, n: u64, out: &Path) -> Result<()> {
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let store = Store::open(&p.meta)?;
    let total = store.commit_count()?;
    if n > total {
        bail!("only {total} commits captured, cannot clone -n {n}");
    }
    let page_size = store.page_size()?.context("run init first")?;

    fs::copy(&p.base, out)?;
    let file = OpenOptions::new().write(true).open(out)?;

    // Replay: every frame is a full page image, so applying a frame is just a positioned write.
    let mut st = store
        .conn()
        .prepare("SELECT pgno, page FROM frames WHERE commit_idx <= ?1 ORDER BY commit_idx, seq")?;
    let mut rows = st.query([n])?;
    let mut applied = 0u64;
    while let Some(r) = rows.next()? {
        let pgno: u64 = r.get(0)?;
        file.write_all_at(r.get_ref(1)?.as_blob()?, (pgno - 1) * page_size)?;
        applied += 1;
    }
    if n > 0 {
        // The commit frame records the db size; shrink in case later commits freed pages.
        let pages: u64 = store
            .conn()
            .query_row("SELECT db_pages FROM commits WHERE idx=?1", [n], |r| r.get(0))?;
        file.set_len(pages * page_size)?;
    }
    file.sync_all()?;
    println!("cloned commits 1..={n} ({applied} frames replayed) -> {}", out.display());
    summarize(out)
}

/// Open a clone read-only and report integrity plus per-table row counts.
fn summarize(path: &Path) -> Result<()> {
    // immutable=1 so opening the clone does not create -wal/-shm side files.
    let uri = format!("file:{}?immutable=1", path.display());
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let check: String = conn.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
    println!("integrity_check: {check}");
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for t in tables {
        let c: u64 = conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))?;
        println!("  {t}: {c} rows");
    }
    Ok(())
}
