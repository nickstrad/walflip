//! Command-line front end: argument parsing and output only. Logic lives in the library.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::{Path, PathBuf};
use walflip::db::{self, Db};

#[derive(Parser)]
#[command(about = "Capture SQLite WAL commits and rebuild a database at any point in history")]
struct Cli {
    /// Directory holding one folder per database
    #[arg(long, global = true, env = "WALFLIP_ROOT", default_value = "walflip-data")]
    root: PathBuf,
    /// Database to operate on (a folder under --root)
    #[arg(long, global = true, env = "WALFLIP_DB")]
    db: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a database (live.db + base.db snapshot + meta.db) in its own folder
    Create {
        name: String,
        /// SQL file defining the initial schema (and optionally seed rows); `-` reads stdin.
        /// Without it the database starts empty.
        #[arg(long)]
        schema: Option<PathBuf>,
    },
    /// List databases and how many commits each has captured
    List,
    /// Rename a database folder
    Rename { old: String, new: String },
    /// Delete a database folder (live, base and meta files)
    Drop { name: String },
    /// Run SQL against --db; each statement is one transaction. Use `-` to read
    /// one statement per line from stdin.
    Exec { sql: String },
    /// List captured commits for --db
    Log {
        #[arg(short, long, default_value_t = 20)]
        limit: u64,
    },
    /// Summarise what has been captured for --db
    Status,
    /// Rebuild --db as of commit N: copy base.db, then replay frames 1..=N
    Clone {
        #[arg(short = 'n', long)]
        n: u64,
        #[arg(short, long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root;
    let open = || -> Result<Db> {
        let name = cli.db.as_deref().context("pass --db <name> (or set WALFLIP_DB)")?;
        Db::existing(&root, name)
    };

    match cli.cmd {
        Cmd::Create { name, schema } => {
            let d = Db::new(&root, &name)?;
            let sql = schema.as_deref().map(read_schema).transpose()?;
            let c = db::create(&d, sql.as_deref())?;
            println!("created {} (page size {})", d.dir.display(), c.page_size);
        }
        Cmd::List => {
            let dbs = db::list(&root)?;
            if dbs.is_empty() {
                println!("no databases under {} (try `walflip create <name>`)", root.display());
            } else {
                println!("{:<24} {:>8}", "name", "commits");
                for d in dbs {
                    println!("{:<24} {:>8}", d.name, d.commits);
                }
            }
        }
        Cmd::Rename { old, new } => {
            db::rename(&root, &old, &new)?;
            println!("renamed {old} -> {new}");
        }
        Cmd::Drop { name } => {
            let d = Db::existing(&root, &name)?;
            db::drop_db(&d)?;
            println!("dropped {}", d.dir.display());
        }
        Cmd::Exec { sql } => {
            let stmts = if sql == "-" {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                db::parse_statements(&text)
            } else {
                vec![sql]
            };
            let r = db::exec_with(&open()?, &stmts, walflip::store::DEFAULT_BUSY_TIMEOUT)?;
            if r.recovered > 0 {
                eprintln!(
                    "warning: {} commit(s) in live.db-wal were not in history (a writer crashed before \
                     capturing, or something else wrote live.db); captured them as {}",
                    r.recovered,
                    db::RECOVERED_LABEL
                );
            }
            println!("captured {} commit(s)", r.captured);
        }
        Cmd::Log { limit } => {
            println!("{:>5} {:>6} {:>8}  sql", "n", "frames", "db_pages");
            for e in db::log(&open()?, limit)? {
                let sql: String = e.sql.chars().take(90).collect();
                println!("{:>5} {:>6} {:>8}  {sql}", e.n, e.frames, e.db_pages);
            }
        }
        Cmd::Status => {
            let s = db::status(&open()?)?;
            println!("page size : {}", s.page_size.unwrap_or_default());
            println!("commits   : {}", s.commits);
            println!("frames    : {}", s.frames);
            match s.wal_bytes {
                Some(b) => println!("live wal  : {b} bytes"),
                None => println!("live wal  : absent (checkpointed)"),
            }
            if s.uncaptured > 0 {
                println!("uncaptured: {} (next exec stores them as {})", s.uncaptured, db::RECOVERED_LABEL);
            }
        }
        Cmd::Clone { n, out } => {
            let frames = db::clone_at(&open()?, n, &out)?;
            println!("cloned commits 1..={n} ({frames} frames replayed) -> {}", out.display());
            report(&out)?;
        }
    }
    Ok(())
}

fn read_schema(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        Ok(text)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading schema {}", path.display()))
    }
}

fn report(path: &Path) -> Result<()> {
    let s = db::summarize(path)?;
    println!("integrity_check: {}", s.integrity);
    for (t, rows) in s.tables {
        println!("  {t}: {rows} rows");
    }
    Ok(())
}
