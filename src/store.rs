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

    use crate::wal::encode::{encode, Commit as EC};
    use tempfile::TempDir;

    const PS: usize = 512;

    fn page(b: u8) -> Vec<u8> {
        vec![b; PS]
    }

    fn setup() -> (TempDir, Store, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("meta.db")).unwrap();
        let wal = dir.path().join("db-wal");
        (dir, store, wal)
    }

    fn write_wal(path: &Path, salt: (u32, u32), ps: u32, commits: &[EC]) {
        fs::write(path, encode(false, salt, ps, commits)).unwrap();
    }

    /// (commits rows, frames rows) as stored, ordered.
    type Rows = (Vec<(i64, i64)>, Vec<(i64, i64, i64, Vec<u8>)>);

    fn rows(s: &Store) -> Rows {
        let c = s
            .conn()
            .prepare("SELECT idx, db_pages FROM commits ORDER BY idx")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let f = s
            .conn()
            .prepare("SELECT commit_idx, seq, pgno, page FROM frames ORDER BY commit_idx, seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (c, f)
    }

    /// What `rows` should return for commits stored in order.
    fn expected(commits: &[EC]) -> Rows {
        let mut c = Vec::new();
        let mut f = Vec::new();
        for (i, (frames, db_pages)) in commits.iter().enumerate() {
            let idx = i as i64 + 1;
            c.push((idx, i64::from(*db_pages)));
            for (seq, (pgno, p)) in frames.iter().enumerate() {
                f.push((idx, seq as i64, i64::from(*pgno), p.clone()));
            }
        }
        (c, f)
    }

    fn sample(n: usize) -> Vec<EC> {
        (0..n)
            .map(|i| {
                let b = i as u8 + 1;
                (vec![(1, page(b)), (2 + i as u32, page(b.wrapping_add(100)))], 2 + i as u32)
            })
            .collect()
    }

    #[test]
    fn capture_is_idempotent() {
        let (_d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (7, 8), PS as u32, &c);
        assert_eq!(s.capture(&wal, "a").unwrap(), 3);
        let before = rows(&s);
        assert_eq!(s.capture(&wal, "a").unwrap(), 0);
        assert_eq!(s.capture(&wal, "a").unwrap(), 0);
        assert_eq!(rows(&s), before);
        assert_eq!(before, expected(&c));
    }

    #[test]
    fn inv19_growth_adds_only_new_commits() {
        let (_d, mut s, wal) = setup();
        let c = sample(5);
        write_wal(&wal, (1, 2), PS as u32, &c[..2]);
        assert_eq!(s.capture(&wal, "x").unwrap(), 2);
        write_wal(&wal, (1, 2), PS as u32, &c);
        assert_eq!(s.capture(&wal, "y").unwrap(), 3);
        assert_eq!(rows(&s), expected(&c));
        let labels: Vec<String> = s
            .conn()
            .prepare("SELECT label FROM commits ORDER BY idx")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(labels, ["x", "x", "y", "y", "y"]);
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("5"));
    }

    #[test]
    fn inv20_new_salt_restarts_cursor_keeps_history() {
        let (_d, mut s, wal) = setup();
        let g1 = sample(4);
        write_wal(&wal, (1, 2), PS as u32, &g1);
        assert_eq!(s.capture(&wal, "g1").unwrap(), 4);
        // New generation: fewer frames than before.
        let g2: Vec<EC> = vec![(vec![(1, page(0xAA))], 1), (vec![(3, page(0xBB))], 3)];
        write_wal(&wal, (9, 10), PS as u32, &g2);
        assert_eq!(s.capture(&wal, "g2").unwrap(), 2);
        assert_eq!(s.capture(&wal, "g2").unwrap(), 0);
        let all: Vec<EC> = g1.iter().chain(g2.iter()).cloned().collect();
        assert_eq!(rows(&s), expected(&all));
        assert_eq!(s.get("cursor_salt").unwrap().as_deref(), Some("9:10"));
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn inv21_page_size_mismatch_rejected_without_side_effects() {
        let (_d, mut s, wal) = setup();
        s.set("page_size", "4096").unwrap();
        s.set("cursor_salt", "5:6").unwrap();
        s.set("cursor_commits", "1").unwrap();
        write_wal(&wal, (5, 6), 512, &sample(2));
        assert!(s.capture(&wal, "bad").is_err());
        assert_eq!(rows(&s), (vec![], vec![]));
        assert_eq!(s.commit_count().unwrap(), 0);
        assert_eq!(s.get("cursor_salt").unwrap().as_deref(), Some("5:6"));
        assert_eq!(s.get("cursor_commits").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn inv26_failed_capture_leaves_store_unchanged() {
        let (_d, mut s, wal) = setup();
        let c = sample(3);
        write_wal(&wal, (1, 2), PS as u32, &c[..1]);
        assert_eq!(s.capture(&wal, "a").unwrap(), 1);
        let before = rows(&s);
        let meta_before = (s.get("cursor_salt").unwrap(), s.get("cursor_commits").unwrap());
        // Make the final cursor write fail, after commits/frames were inserted in the tx.
        s.conn()
            .execute_batch(
                "CREATE TRIGGER boom BEFORE UPDATE ON meta WHEN NEW.key='cursor_commits' \
                 BEGIN SELECT RAISE(ABORT,'boom'); END;
                 CREATE TRIGGER boom2 BEFORE INSERT ON meta WHEN NEW.key='cursor_commits' \
                 BEGIN SELECT RAISE(ABORT,'boom'); END;",
            )
            .unwrap();
        write_wal(&wal, (1, 2), PS as u32, &c);
        assert!(s.capture(&wal, "b").is_err());
        assert_eq!(rows(&s), before);
        assert_eq!((s.get("cursor_salt").unwrap(), s.get("cursor_commits").unwrap()), meta_before);
        // Once the fault is gone the same capture succeeds and adds the missing commits.
        s.conn().execute_batch("DROP TRIGGER boom; DROP TRIGGER boom2;").unwrap();
        assert_eq!(s.capture(&wal, "b").unwrap(), 2);
        assert_eq!(rows(&s), expected(&c));
    }

    #[test]
    fn frame_order_within_commit_is_preserved() {
        let (_d, mut s, wal) = setup();
        let c: Vec<EC> = vec![(
            vec![(5, page(1)), (2, page(2)), (5, page(3)), (1, page(4)), (9, page(5))],
            9,
        )];
        write_wal(&wal, (1, 1), PS as u32, &c);
        s.capture(&wal, "o").unwrap();
        let (_, f) = rows(&s);
        let got: Vec<(i64, Vec<u8>)> = f.into_iter().map(|(_, _, pg, p)| (pg, p)).collect();
        let want: Vec<(i64, Vec<u8>)> =
            c[0].0.iter().map(|(pg, p)| (i64::from(*pg), p.clone())).collect();
        assert_eq!(got, want);
    }

    fn commit_strategy() -> impl Strategy<Value = EC> {
        (
            prop::collection::vec((1u32..8, any::<u8>()), 1..5),
            1u32..16,
        )
            .prop_map(|(fr, n)| (fr.into_iter().map(|(p, b)| (p, vec![b; PS])).collect(), n))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn prefix_captures_equal_encoder_input(
            commits in prop::collection::vec(commit_strategy(), 0..12),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..5),
        ) {
            let (_d, mut s, wal) = setup();
            let mut points: Vec<usize> =
                cuts.iter().map(|i| i.index(commits.len() + 1)).collect();
            points.push(commits.len());
            points.sort_unstable();
            let mut prev = 0;
            for p in points {
                write_wal(&wal, (3, 4), PS as u32, &commits[..p]);
                let added = s.capture(&wal, "p").unwrap();
                prop_assert_eq!(added, p - prev);
                prop_assert_eq!(rows(&s), expected(&commits[..p]));
                prev = p;
            }
        }
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
