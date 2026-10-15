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
    /// one statement per line from stdin, or --script for `;`-terminated, multi-line SQL.
    Exec {
        #[arg(required_unless_present = "script")]
        sql: Option<String>,
        /// Run an SQL script file (`-` reads stdin) instead: statements end with `;` and may span
        /// lines (split as the sqlite3 shell does). A statement without its `;` at the end of
        /// the script is `incomplete input` and nothing runs.
        #[arg(long, conflicts_with = "sql")]
        script: Option<PathBuf>,
    },
    /// List captured commits for --db: the most recent --limit, oldest first
    Log {
        /// How many commits to show (0 shows none, only the header)
        #[arg(short, long, default_value_t = 20)]
        limit: u64,
        /// Show the first --limit commits instead of the most recent
        #[arg(long, conflicts_with = "all")]
        head: bool,
        /// Show every commit (ignores --limit)
        #[arg(long)]
        all: bool,
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
            let sql = schema.as_deref().map(|p| read_sql(p, "schema")).transpose()?;
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
        Cmd::Exec { sql, script } => {
            let stmts = match (script, sql) {
                (Some(path), _) => db::split_script(&read_sql(&path, "script")?)?,
                (None, Some(sql)) if sql == "-" => db::parse_statements(&read_sql(Path::new("-"), "stdin")?),
                (None, Some(sql)) => vec![sql],
                (None, None) => unreachable!("clap requires sql or --script"),
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
        Cmd::Log { limit, head, all } => {
            let (limit, from_end) = if all { (i64::MAX as u64, false) } else { (limit, !head) };
            println!("{:>5} {:>6} {:>8}  sql", "n", "frames", "db_pages");
            for e in db::log(&open()?, limit, from_end)? {
                println!("{:>5} {:>6} {:>8}  {}", e.n, e.frames, e.db_pages, db::display_label(&e.sql));
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

/// Read an SQL file, or stdin for `-`; `what` names it in errors.
fn read_sql(path: &Path, what: &str) -> Result<String> {
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        Ok(text)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {what} {}", path.display()))
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
