//! The metadata SQLite file: base-snapshot facts, a commit log, and the captured WAL frames.

use crate::wal;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{fs, io, path::Path};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS commits(
    idx          INTEGER PRIMARY KEY,   -- 1-based position in history
    label        TEXT NOT NULL,         -- the SQL that produced it
    db_pages     INTEGER NOT NULL,      -- database size after this commit
    committed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS frames(
    commit_idx INTEGER NOT NULL REFERENCES commits(idx),
    seq        INTEGER NOT NULL,        -- order inside the commit
    pgno       INTEGER NOT NULL,
    page       BLOB NOT NULL,
    PRIMARY KEY(commit_idx, seq)
);";

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", [key, value])?;
    Ok(())
}

/// How many commits of the WAL generation identified by `salt` are already stored.
///
/// The cursor is (salt1:salt2, commits seen). A different salt means SQLite restarted the WAL
/// after a checkpoint, so nothing in it has been seen yet.
pub fn commits_seen(
    stored_salt: Option<&str>,
    stored_commits: Option<&str>,
    salt: (u32, u32),
) -> usize {
    let current = format!("{}:{}", salt.0, salt.1);
    match stored_salt {
        Some(s) if s == current => stored_commits.and_then(|v| v.parse().ok()).unwrap_or(0),
        _ => 0,
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn, key, value)
    }

    pub fn page_size(&self) -> Result<Option<u64>> {
        Ok(self.get("page_size")?.map(|v| v.parse()).transpose()?)
    }

    pub fn commit_count(&self) -> Result<u64> {
        Ok(self.conn.query_row("SELECT count(*) FROM commits", [], |r| r.get(0))?)
    }

    /// Copy any commits in the live WAL we have not stored yet. Idempotent: re-reading the
    /// same WAL adds nothing. Returns the number of commits added.
    pub fn capture(&mut self, wal_path: &Path, label: &str) -> Result<usize> {
        let bytes = match fs::read(wal_path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let Some(wal) = wal::parse(&bytes)? else { return Ok(0) };

        // Clone writes pages at (pgno-1)*page_size using the size saved at create; a different
        // size in the WAL would make replayed frames land at the wrong offsets.
        if let Some(stored) = self.page_size()? {
            if stored != u64::from(wal.page_size) {
                bail!("WAL page size {} does not match base snapshot page size {stored}", wal.page_size);
            }
        }

        let seen = commits_seen(
            self.get("cursor_salt")?.as_deref(),
            self.get("cursor_commits")?.as_deref(),
            wal.salt,
        );
        let total = wal.commits.len();
        let first_idx = self.commit_count()? + 1;

        let tx = self.conn.transaction()?;
        for (idx, commit) in (first_idx..).zip(wal.commits.iter().skip(seen)) {
            tx.execute(
                "INSERT INTO commits(idx,label,db_pages) VALUES(?1,?2,?3)",
                params![idx, label, commit.db_pages],
            )?;
            for (seq, f) in commit.frames.iter().enumerate() {
                tx.execute(
                    "INSERT INTO frames(commit_idx,seq,pgno,page) VALUES(?1,?2,?3,?4)",
                    params![idx, seq, f.pgno, f.page],
                )?;
            }
        }
        set_meta(&tx, "cursor_salt", &format!("{}:{}", wal.salt.0, wal.salt.1))?;
        set_meta(&tx, "cursor_commits", &total.to_string())?;
        tx.commit()?;
        Ok(total.saturating_sub(seen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn cursor_cases() {
        assert_eq!(commits_seen(None, None, (1, 2)), 0);
        assert_eq!(commits_seen(Some("1:2"), Some("5"), (1, 2)), 5);
        assert_eq!(commits_seen(Some("1:2"), Some("5"), (1, 3)), 0, "new salt restarts the count");
        assert_eq!(commits_seen(Some("1:2"), Some("junk"), (1, 2)), 0);
        assert_eq!(commits_seen(Some("1:2"), None, (1, 2)), 0);
    }

    proptest! {
        #[test]
        fn matching_salt_returns_stored_count(s in any::<(u32, u32)>(), n in any::<u32>()) {
            let stored = format!("{}:{}", s.0, s.1);
            prop_assert_eq!(commits_seen(Some(&stored), Some(&n.to_string()), s), n as usize);
        }

        #[test]
        fn different_salt_always_restarts(a in any::<(u32, u32)>(), b in any::<(u32, u32)>(), n in any::<u32>()) {
            prop_assume!(a != b);
            let stored = format!("{}:{}", a.0, a.1);
            prop_assert_eq!(commits_seen(Some(&stored), Some(&n.to_string()), b), 0);
        }
    }
}
