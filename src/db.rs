//! Database-level operations. Everything here returns plain data; printing belongs to the CLI.

use crate::{replay, store::Store};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_NAME: usize = 64;

/// Database names become folder names, so keep them boring.
fn validate_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= MAX_NAME
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        bail!("invalid database name '{s}': use 1-{MAX_NAME} letters, digits, '_' or '-'");
    }
    Ok(())
}

/// One managed database: a folder holding `live.db`, `base.db` and `meta.db`.
pub struct Db {
    pub dir: PathBuf,
    pub live: PathBuf,
    pub base: PathBuf,
    pub meta: PathBuf,
}

impl Db {
    pub fn new(root: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let dir = root.join(name);
        Ok(Self {
            live: dir.join("live.db"),
            base: dir.join("base.db"),
            meta: dir.join("meta.db"),
            dir,
        })
    }

    /// Like `new`, but the database must already exist.
    pub fn existing(root: &Path, name: &str) -> Result<Self> {
        let db = Self::new(root, name)?;
        if !db.live.exists() {
            bail!("no database named '{name}' under {} (see `walflip list`)", root.display());
        }
        Ok(db)
    }

    pub fn wal_path(&self) -> PathBuf {
        let mut s = self.live.clone().into_os_string();
        s.push("-wal");
        s.into()
    }
}

/// Split SQL text into statements: one per non-blank line.
pub fn parse_statements(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

pub struct Created {
    pub page_size: u32,
}

/// Create the folder, a WAL-mode live db, and the base snapshot. `schema` is arbitrary SQL
/// (typically DDL, optionally seed rows) run before the snapshot is taken, so it becomes the
/// state that `clone -n 0` returns. If it fails, nothing is left on disk.
pub fn create(db: &Db, schema: Option<&str>) -> Result<Created> {
    if db.dir.exists() {
        bail!("{} already exists", db.dir.display());
    }
    fs::create_dir_all(&db.dir)?;
    let result = init_files(db, schema);
    if result.is_err() {
        let _ = fs::remove_dir_all(&db.dir);
    }
    Ok(Created { page_size: result? })
}

fn init_files(db: &Db, schema: Option<&str>) -> Result<u32> {
    let conn = Connection::open(&db.live)?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if mode != "wal" {
        bail!("could not enable WAL (got {mode})");
    }
    if let Some(sql) = schema {
        conn.execute_batch(sql).context("applying schema")?;
    }
    // Fold the schema into the main file so base.db is a complete, WAL-free starting point.
    conn.pragma_update_and_check(None, "wal_checkpoint", "TRUNCATE", |_| Ok(()))?;
    let page_size: u32 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    drop(conn);

    fs::copy(&db.live, &db.base)?;
    Store::open(&db.meta)?.set("page_size", &page_size.to_string())?;
    Ok(page_size)
}

pub struct DbSummary {
    pub name: String,
    pub commits: u64,
}

pub fn list(root: &Path) -> Result<Vec<DbSummary>> {
    let entries = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if !path.join("live.db").exists() {
            continue;
        }
        let Ok(name) = e.file_name().into_string() else { continue };
        let commits = Store::open(&path.join("meta.db"))?.commit_count()?;
        out.push(DbSummary { name, commits });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn rename(root: &Path, old: &str, new: &str) -> Result<()> {
    let from = Db::existing(root, old)?;
    let to = Db::new(root, new)?;
    if to.dir.exists() {
        bail!("{} already exists", to.dir.display());
    }
    fs::rename(&from.dir, &to.dir)?;
    Ok(())
}

pub fn drop_db(db: &Db) -> Result<()> {
    fs::remove_dir_all(&db.dir)?;
    Ok(())
}

/// Run each statement as its own transaction and capture the commits it produced.
/// Returns the total number of commits captured. A statement that changes nothing
/// (e.g. an UPDATE that matches no rows) writes no WAL frames and so adds no commit.
pub fn exec(db: &Db, statements: &[String]) -> Result<usize> {
    let mut store = Store::open(&db.meta)?;
    let conn = Connection::open(&db.live)?;
    // Stop SQLite from folding the WAL back into the db while we are still reading it.
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;

    let wal = db.wal_path();
    let mut total = 0;
    for stmt in statements {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(stmt).with_context(|| format!("failed: {stmt}"))?;
        tx.commit()?;
        total += store.capture(&wal, stmt)?;
    }
    Ok(total)
}

pub struct LogEntry {
    pub n: u64,
    pub frames: u64,
    pub db_pages: u64,
    pub sql: String,
}

pub fn log(db: &Db, limit: u64) -> Result<Vec<LogEntry>> {
    let store = Store::open(&db.meta)?;
    let mut st = store.conn().prepare(
        "SELECT c.idx, count(f.seq), c.db_pages, c.label
         FROM commits c LEFT JOIN frames f ON f.commit_idx = c.idx
         GROUP BY c.idx ORDER BY c.idx LIMIT ?1",
    )?;
    let rows = st.query_map([limit], |r| {
        Ok(LogEntry { n: r.get(0)?, frames: r.get(1)?, db_pages: r.get(2)?, sql: r.get(3)? })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub struct Status {
    pub page_size: Option<u64>,
    pub commits: u64,
    pub frames: u64,
    /// Size of the live WAL file, if one exists.
    pub wal_bytes: Option<u64>,
}

pub fn status(db: &Db) -> Result<Status> {
    let store = Store::open(&db.meta)?;
    Ok(Status {
        page_size: store.page_size()?,
        commits: store.commit_count()?,
        frames: store.conn().query_row("SELECT count(*) FROM frames", [], |r| r.get(0))?,
        wal_bytes: fs::metadata(db.wal_path()).ok().map(|m| m.len()),
    })
}

/// Rebuild the database as of commit `n` into `out`. Returns the number of frames replayed.
pub fn clone_at(db: &Db, n: u64, out: &Path) -> Result<usize> {
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let store = Store::open(&db.meta)?;
    let total = store.commit_count()?;
    if n > total {
        bail!("only {total} commits captured, cannot clone -n {n}");
    }
    let page_size = store.page_size()?.context("database has no recorded page size")? as usize;

    let mut st = store
        .conn()
        .prepare("SELECT pgno, page FROM frames WHERE commit_idx <= ?1 ORDER BY commit_idx, seq")?;
    let frames: Vec<(u32, Vec<u8>)> = st
        .query_map([n], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    // The commit frame records the db size; applying it shrinks the image if later commits freed pages.
    let db_pages: Option<u32> = if n > 0 {
        Some(store.conn().query_row("SELECT db_pages FROM commits WHERE idx=?1", [n], |r| r.get(0))?)
    } else {
        None
    };

    let mut image = fs::read(&db.base)?;
    replay::apply(&mut image, page_size, frames.iter().map(|(p, d)| (*p, d.as_slice())), db_pages)?;
    fs::write(out, &image)?;
    Ok(frames.len())
}

pub struct Summary {
    pub integrity: String,
    pub tables: Vec<(String, u64)>,
}

/// Open a db file read-only and report integrity plus per-table row counts.
pub fn summarize(path: &Path) -> Result<Summary> {
    // immutable=1 so opening does not create -wal/-shm side files.
    let uri = format!("file:{}?immutable=1", path.display());
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let integrity: String = conn.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut tables = Vec::new();
    for t in names {
        let c: u64 = conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))?;
        tables.push((t, c));
    }
    Ok(Summary { integrity, tables })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn accepted_names_are_safe_folder_names(s in any::<String>()) {
            if validate_name(&s).is_ok() {
                prop_assert!(!s.is_empty() && s.len() <= MAX_NAME);
                prop_assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
            }
        }
    }

    #[test]
    fn rejects_path_like_names() {
        for bad in ["", ".", "..", "a/b", "a b", "名前", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_name("orders-2024_v1").is_ok());
    }

    #[test]
    fn statements_are_one_per_nonblank_line() {
        assert_eq!(
            parse_statements("  INSERT 1;\n\n\t\nUPDATE 2;  \n"),
            vec!["INSERT 1;".to_string(), "UPDATE 2;".to_string()]
        );
        assert!(parse_statements("").is_empty());
    }
}
