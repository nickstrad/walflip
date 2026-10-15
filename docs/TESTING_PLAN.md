# walflip testing plan

Status of the tree this plan was written against (2026-10-14, after the `create --schema` pivot):

| Area | What exists | Result |
|---|---|---|
| Unit / proptest | `wal` (round-trip both byte orders, truncation prefix, LE corruption, no-panic), `replay` (page-map model, compose, idempotent, bad frames), `store::commits_seen`, `db::validate_name`, `parse_statements` | green |
| `tests/roundtrip.rs` | random kv ops vs real SQLite, per-process and single-WAL, capture idempotent (24 cases each) | green |
| `tests/cli.rs` | 11 scenarios via `tests/common::Harness` (lifecycle, stdin batch, env, isolation, recovery, batch==per-process, errors, no-ops, schema file, schema stdin, seed script) | green, ~5 s |
| `fuzz/` | cargo-fuzz crate, one target `wal_parse` (raw bytes), corpus seeded | exists |

Confirmed by running the debug binary while writing this plan (reproductions in section 1 notes):

| # | Confirmed defect | Severity |
|---|---|---|
| B1 | `summarize` does not double `"` in table names: `exec 'CREATE TABLE "a""b"(x)'` then `clone` writes the file, then exits 1 (`unrecognized token`). | medium |
| B2 | A statement containing `COMMIT` (`INSERT ...; COMMIT; INSERT ...;`) commits to `live.db`, `tx.commit()` then fails, capture never runs, the process closes and SQLite checkpoints the WAL. History has 0 commits while `live.db` has 2 rows; the next `exec` produces commit 1 whose page already holds 3 rows. Lost history. | high |
| B3 | Commits already in `live.db-wal` when `exec` starts (crashed writer, external writer) are captured under the label of the next statement, and `exec` reports more commits than statements. | medium |
| B4 | Concurrent `exec` on one db (6 procs x 15 inserts): 11 failures `UNIQUE constraint failed: commits.idx`, 120 commits recorded for 90 rows, duplicated labels. The cursor is read outside the `meta.db` transaction. | high |
| B5 | A schema containing `PRAGMA journal_mode=DELETE` is accepted; `live.db` is left in rollback mode and every later `exec` reports `captured 0 commit(s)` forever. | high |
| B6 | `replay::apply` with a huge `pgno` or `db_pages` calls `Vec::resize` to terabytes: abort, not `Err`. Reachable from a crafted `meta.db`/WAL; the fuzzer will hit it immediately. | medium |
| B7 | `PRAGMA page_size=8192` / `PRAGMA auto_vacuum=FULL` in a schema are silently ignored (WAL mode is switched on before the schema runs, which initialises page 1). `create` reports 4096. | low / UX |
| B8 | Schema with an unterminated `BEGIN` fails with `database table is locked` (from the checkpoint), not an "applying schema" message. Folder is cleaned up. | low |

Also verified: `clone -n total` is **byte-identical** to `live.db` once the WAL is checkpointed, including after an external `VACUUM` shrank the db (16 -> 9 pages). This is the strongest oracle available and the harness is built around it.

---

## 1. Invariant catalog

Legend. Status: `ok` covered today, `gap` not tested, `FAIL` confirmed failing (B-number), `sus` suspected failing. Technique: U unit, P proptest, F fuzz, E e2e (harness), L library-level integration (calls `walflip::db`/`store` directly, no process).

### 1.1 WAL parser (`wal::parse`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-01 | `parse(encode(c))` returns `c` for both byte orders and any salt. | existing proptest | P | ok |
| INV-02 | Any prefix of a valid WAL parses to a prefix of its commits; a full buffer gives all of them. | existing proptest | P | ok |
| INV-03 | Flipping any byte after the header never admits altered data; history is cut at or before the damaged frame. | extend existing proptest to BE and to header bytes (header corruption must yield `None` or `Err`, never a commit). | P/F | ok (LE only) |
| INV-04 | `parse` never panics, never allocates proportional to a header field, for arbitrary bytes. | `wal_parse` fuzz target + proptest | F/P | ok |
| INV-05 | Every accepted commit has `db_pages != 0`, >=1 frame, frames exactly `page_size` long, total frames fit in the buffer. | `wal_parse` target asserts | F | ok |
| INV-06 | Frames whose salt differs from the header (stale generation after a WAL restart) end parsing; nothing after them is admitted. | encode gen A, overwrite head with gen B (fewer frames, new salt) -> parse == gen B only. | U/P | gap |
| INV-07 | A WAL whose frames are chained to a checksum that does not start from the header cksum is rejected at frame 1. | encode, then patch header cksum fields -> `None`. | U | gap |
| INV-08 | `page_size` accepted iff `512 <= ps <= 65536` and power of two. | unit; today 2^17..2^31 are accepted (harmless: zero frames fit) | U | sus (hardening) |
| INV-09 | Header checksum mismatch -> `Ok(None)` (file being created), bad magic -> `Err`. | unit documenting the contract | U | gap |
| INV-10 | Sector-padding frames (SQLite repeats the commit frame to fill a sector when `POWERSAFE_OVERWRITE` is off) must not become extra commits. | structured fuzz / unit: duplicate the last frame k times with identical header -> parser or `capture` yields 1 commit. Policy decision needed (see section 5, Q6). | U/F | sus |

### 1.2 Replay (`replay::apply`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-11 | Last writer wins per page; untouched pages keep base; new pages zero. | existing proptest | P | ok |
| INV-12 | `apply(all)` == `apply(prefix); apply(rest)` (prefix soundness of `-n`). | existing | P | ok |
| INV-13 | Idempotent. | existing | P | ok |
| INV-14 | `pgno == 0` or wrong page length -> `Err`, image untouched. | existing (add: image unchanged on `Err`) | U | ok |
| INV-15 | Bounded allocation: when `db_pages` is `Some(n)`, any `pgno > n` is `Err`; `n * page_size` above a cap (e.g. 1 << 40 bytes or a caller-supplied max) is `Err`. Never aborts. | unit with `pgno = u32::MAX`; fuzz target `replay_apply` | U/F | FAIL (B6) |
| INV-16 | Output length == `db_pages * page_size` when `Some`; == `max(base, highest written page)` when `None`. | proptest | P | ok (implicit) |

### 1.3 Store (`store::Store`, `commits_seen`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-17 | Cursor: same salt -> stored count, different salt or junk -> 0. | existing | U/P | ok |
| INV-18 | `capture` on the same WAL twice adds 0 the second time. | roundtrip | L | ok |
| INV-19 | WAL grows from k to k+j commits -> `capture` adds exactly j, idx continues. | write `encode(...)` output to a temp WAL file, capture, extend file, capture again. | U | gap |
| INV-20 | WAL restart (new salt, fewer frames than before) -> all commits of the new generation are added, none duplicated, cursor updated. | as INV-19 with two generations | U | gap |
| INV-21 | WAL page size != stored page size -> `Err`, no rows inserted, cursor unchanged. | encode with PS=512 against a store whose `page_size` is 4096 | U | gap |
| INV-22 | `commits.idx` is contiguous `1..=count`; every `frames.commit_idx` exists; per commit `seq` is contiguous from 0; `frames` count == `status.frames`. | SQL check in `assert_invariants` | E | gap |
| INV-23 | `cursor_commits` == number of commits in the current WAL generation at last capture; `cursor_salt` == header salt. | library check after each `exec` while WAL still exists (hold a reader to prevent the close-checkpoint) | L | gap |
| INV-24 | Capture is atomic under concurrent writers: N processes doing M statements each produce exactly N*M commits, no `UNIQUE` errors, labels match statements. | e2e concurrency scenario | E | FAIL (B4) |
| INV-25 | Commits present in the WAL before `exec` runs its first statement are either captured with a distinct label (e.g. `(uncaptured)`) or `exec` refuses; they are never attributed to the new statement. | e2e: kill a writer with `os._exit`, then `exec` | E | FAIL (B3) |
| INV-26 | A failed `capture` (meta tx error) leaves `commits`/`frames`/`meta` exactly as before. | unit with a read-only `meta.db` | U | gap |

### 1.4 Database operations (`db::*`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-27 | `status.commits == log(limit=inf).len() == sum of exec return values` (CLI: sum of `captured N`). | harness tracks the sum | E/L | ok (status vs sum) / gap (log) |
| INV-28 | `status.frames == sum(log[i].frames)`. | harness | E | gap |
| INV-29 | `clone -n k` passes `integrity_check` for every `k in 0..=total`. | harness after every step (cheap for small dbs) | E/L | partial |
| INV-30 | `clone -n k` table contents == model after the k-th capturing statement, for every table. | model oracle: a shadow `rusqlite` in-memory db fed the same statements, dumped per table | E/L | ok (kv only) |
| INV-31 | `clone -n total` is byte-identical to `live.db` when `live.db-wal` is absent. | `fs::read` compare | E/L | gap (verified manually) |
| INV-32 | Clone chain: `apply(frames(n+1..=m), db_pages(m))` onto `clone(n)` == `clone(m)` bytes, for all n < m. | library test reading frames from `meta.db` | L | gap |
| INV-33 | `len(clone(k)) == db_pages(k) * page_size` for k >= 1; `clone(0) == base.db` bytes. | harness | E | gap |
| INV-34 | `clone` never mutates `live.db`, `base.db`, `meta.db` (hash before/after) and never creates `-wal`/`-shm` beside the output. | harness | E | gap |
| INV-35 | `clone` to an existing path refuses and leaves that file byte-identical; `-n > total` refuses and creates nothing. | existing | E | ok |
| INV-36 | `clone` into an unwritable directory -> non-zero exit, no partial file, db state unchanged. | chmod 0o555 dir (skip when root) | E | gap |
| INV-37 | A failing statement changes nothing: live rows, `status`, WAL commit count, `meta.db` hash all unchanged. | harness | E | ok (simple) / FAIL for `COMMIT`-containing statements (B2) |
| INV-38 | One `exec` statement produces at most one commit; a statement that writes nothing produces zero. | harness: `captured N` with N <= 1 per statement | E | ok (no-op) / FAIL (B2, B3) |
| INV-39 | Rows committed to `live.db` by `exec` are always represented in history: after any `exec` (success or error), `clone -n total` == `live.db` contents. | harness `assert_invariants` after failed steps too | E | FAIL (B2) |
| INV-40 | DDL via `exec` (`CREATE TABLE/INDEX/TRIGGER`, `ALTER TABLE ADD/RENAME COLUMN`, `DROP TABLE`) is captured and visible in `clone` at that commit; `sqlite_master` of clone(k) == model. | schema-evolution scenario | E | partial |
| INV-41 | Large payloads: a 300 KB blob spans ~75 overflow pages, all captured; blob hash equal in clone. | large-payload scenario | E | gap (manual ok) |
| INV-42 | `db_pages` may shrink (VACUUM, auto_vacuum). After a shrinking commit s, `len(clone(s)) < len(clone(s-1))`, integrity ok, later growth replays correctly. | library scenario: open `live.db` with `wal_autocheckpoint=0`, run `VACUUM`, call `Store::capture`, then clone | L | gap (manual ok) |
| INV-43 | `rename` preserves history byte-for-byte (`meta.db` hash) and the old name disappears; `drop` removes exactly that folder; other dbs' hashes unchanged. | isolation scenario | E | partial |
| INV-44 | Per-process `exec` and one stdin batch produce the same `log` (frames, db_pages, labels) and byte-identical `clone -n k` for all k. | upgrade scenario 6 from "same rows" to "same bytes" | E | partial |
| INV-45 | `--root/--db` flags, `WALFLIP_ROOT/WALFLIP_DB`, and mixed (flag overrides env) select the same database. | env scenario | E | partial |
| INV-46 | `list` is sorted by name, counts equal each db's `status.commits`, ignores folders without `live.db`, and does not create files in them. | many-dbs scenario; check no `meta.db` appears in a decoy folder | E | gap |
| INV-47 | `summarize`/clone report works for any table name SQLite accepts (quotes, spaces, unicode, keywords). | DDL scenario with `CREATE TABLE "a""b"`, `"with space"`, `"select"` | E | FAIL (B1) |
| INV-48 | Concurrent readers on `live.db` (a held read transaction) during `exec`: exec succeeds, capture complete, no duplicates on the next `exec` even though the WAL was not checkpointed. | spawn `python3 -I`/rusqlite reader holding `BEGIN; SELECT` | E | gap |
| INV-49 | Concurrent `clone -n k` of the same db (same and different k) all succeed and are byte-identical per k. | spawn 8 processes | E | gap |
| INV-50 | `exec -` splits on lines: a multi-line statement fails with `incomplete input`, nothing committed, earlier lines kept. Documented behaviour. | e2e | E | gap (manual ok) |
| INV-51 | `exec` with 2000 single-row inserts in one batch finishes in bounded time (WAL is re-read per statement: O(N * WAL)). Budget: < 10 s debug. | timed scenario, `#[ignore]` by default | E | gap |

### 1.5 Create / schema file

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-52 | Multi-statement schema (tables, indexes, triggers, views, seed rows) is applied in full; `clone -n 0` shows all of it; `status.commits == 0`. | existing scenario 9 + add trigger/view | E | ok |
| INV-53 | Seed rows are in `clone -n 0` and `base.db`; they never appear in `log`. | existing | E | ok |
| INV-54 | Schema with a syntax error, a constraint violation in its seed rows, or a missing file: non-zero exit, message names the stage (`reading schema` / `applying schema`), no folder left, `list` unchanged. | existing + add constraint-violation case | E | ok |
| INV-55 | `--schema -` reads stdin; identical result to the file form (byte-identical `base.db`). | existing + byte compare | E | partial |
| INV-56 | Empty schema file == no `--schema`: `base.db` identical, `page size 4096`. | e2e | E | gap (manual ok) |
| INV-57 | Schema containing `BEGIN; ... COMMIT;` works; an unterminated `BEGIN` fails cleanly with an `applying schema` message and no folder. | e2e | E | ok / sus (B8 message) |
| INV-58 | A schema that changes `journal_mode` is rejected (or WAL is re-asserted) so that `live.db` is in WAL mode after `create`; `exec` refuses to run against a non-WAL `live.db`. | e2e with `PRAGMA journal_mode=DELETE` | E | FAIL (B5) |
| INV-59 | `PRAGMA page_size=N` in a schema either takes effect (reported page size N, `meta.page_size == N`, clones valid) or is rejected with a message; never silently ignored. Decision needed (Q7). | e2e | E | sus (B7) |
| INV-60 | `base.db` has no `-wal`/`-shm`, `PRAGMA journal_mode` of a copy of `base.db` is `wal`, and `base.db` is byte-identical to `live.db` right after `create`. | e2e | E | gap |
| INV-61 | A schema containing `ATTACH` can write outside the root. Decide: allow (document) or deny (`SQLITE_DBCONFIG_DEFENSIVE` / authorizer denying `SQLITE_ATTACH`). Until decided, the fuzzer must never execute schema text. | e2e once decided | E | open (Q8) |
| INV-62 | Database names: accepted iff `[A-Za-z0-9_-]{1,64}`; path-like names rejected; `create` with an invalid name creates nothing. | existing `validate_name` tests + e2e | U/E | ok |

### 1.6 Priority order for fixing (likely real bugs)

1. INV-39/INV-37 (B2) lost history on `COMMIT` inside a statement: capture must run even when `tx.commit()` fails, or `exec` must reject statements that end the transaction (e.g. check `conn.is_autocommit()` before `tx.commit()`, then capture with a `(partial)` label).
2. INV-24 (B4) concurrent exec: open the `meta.db` transaction with `BEGIN IMMEDIATE` *before* reading the cursor and the WAL, or hold a lock file in the db folder for the whole `exec`.
3. INV-58 (B5) schema flips journal mode: after applying the schema, `PRAGMA journal_mode` must still be `wal`, else fail `create`; `exec` should assert it too.
4. INV-25 (B3) uncaptured-commit labelling: capture before the first statement with label `(recovered)`.
5. INV-15 (B6) replay allocation bound.

Known-red scenarios currently merged as `#[ignore]` (T23 removes this list):

- `tests/scenarios_schema.rs` journal_mode=DELETE case: KNOWN FAILING: B5, fixed by T15.
- `tests/perf.rs::s20_stdin_batch_of_2000_inserts`: `#[ignore]`, 53.9 s debug vs 10 s INV-51 budget (no B-number; WAL re-read per statement).
- `tests/scenarios_concurrency.rs::s13_concurrent_exec`: KNOWN FAILING: B4, fixed by T16.
- `tests/scenarios_concurrency.rs::s15_writer_dies_mid_life`: KNOWN FAILING: B3, fixed by T16.
- B9 (found by T12): `clone_at` checked `out.exists()` then wrote, so concurrent clones to one
  path could both succeed. Fixed in T13: `create_new(true)`; S12 asserts exactly one racer wins.

---

## 2. Fuzzing plan (cargo-fuzz, `fuzz/`)

### 2.1 Targets

| Target | Input | Oracle / assertions | Notes |
|---|---|---|---|
| `wal_parse` (exists) | raw bytes | no panic; INV-05 self-consistency | keep as is; add `-max_len=70000` so frames with 4096/65536 pages fit |
| `wal_differential` (new) | `arbitrary` struct: `{ big_endian, salt, page_size in {512,1024,4096}, commits: Vec<{frames: Vec<(pgno, seed)>, db_pages}>, mutations: Vec<Mutation> }` where `Mutation in { Truncate(usize), FlipByte(pos, mask), AppendStaleGeneration(..), DuplicateLastFrame(k), PatchHeaderField(idx, val) }` | encode with the reference encoder (moved from `wal.rs` tests into `src/wal.rs` under `#[cfg(any(test, feature = "testing"))]` as `pub mod encode`), apply mutations, parse. Assert: unmutated -> equal; `Truncate` -> prefix; `FlipByte` -> prefix of commits strictly before the damaged frame; `AppendStaleGeneration` -> unchanged; `DuplicateLastFrame` -> policy from Q6; `PatchHeaderField(page_size)` -> rejected unless valid. | this is where INV-06/07/10 live permanently |
| `replay_apply` (new) | `arbitrary`: `{ base_pages: u8, page_size in {64,512}, frames: Vec<(u32, [u8; N])>, db_pages: Option<u32> }` | no abort (run with `-rss_limit_mb=512`); result length rule INV-16; page-map model INV-11; `Err` leaves image untouched | will find B6 on the first run; add the fix first or expect red |
| `store_capture` (new) | `arbitrary` list of WAL generations + a mutation per generation | in a `tempdir`: write WAL file, `Store::capture`, assert INV-19/20/21/22 against a model (`Vec<Vec<Commit>>`); also random interleaving of "truncate file to 0" and "rewrite header" | slower (SQLite I/O); run with `-runs` budget, not time |
| `statements_split` (new, tiny) | raw bytes as UTF-8 | `parse_statements` never panics; output lines are trimmed, non-empty, and `join("\n")` re-splits to the same vector (idempotence) | 1 minute of CI time is enough |

**Explicitly excluded:** any target that executes fuzzer-controlled SQL (`create --schema`, `exec`). `ATTACH DATABASE '/path'` writes arbitrary files (verified), `PRAGMA`s change global state, and `randomblob`/`VACUUM` make runs slow. If a schema-text target is ever wanted, it must: open SQLite with an authorizer that denies `SQLITE_ATTACH`, `SQLITE_DETACH`, `SQLITE_PRAGMA`, and any `SQLITE_FUNCTION` outside an allowlist; run in a `tempdir` under `chroot`-like path checks; set `SQLITE_LIMIT_LENGTH`/`SQLITE_LIMIT_SQL_LENGTH`; and use a 100 ms `progress_handler` interrupt. Treat as a separate, later task (T21) only after Q8 is decided.

### 2.2 Seed corpus

- Real WAL files: a harness helper `Harness::wal_snapshot(db)` that runs `exec` while holding a read transaction on `live.db` (prevents the close-checkpoint), then copies `live.db-wal`. Generate: 1-frame commit, multi-page commit (blob), DDL commit, 50-commit batch, a VACUUM generation (shrink), a restarted WAL (two generations in one file).
- Encoder-generated: both byte orders, each page size, 0/1/5 commits, a stale tail.
- Minimised crash inputs (see 2.4).
- Keep `fuzz/corpus/<target>/` committed; cap at ~2 MB by running `cargo fuzz cmin` before committing.

### 2.3 Dictionary (`fuzz/wal.dict`)

Magic in both orders (`"\x37\x7f\x06\x82"`, `"\x37\x7f\x06\x83"`), version `"\x00\x2d\xe2\x18"` (3007000), page sizes `512/1024/4096/65536` as BE u32, small `db_pages` values `1..8` as BE u32, `"\x00\x00\x00\x00"`. Pass with `-dict=fuzz/wal.dict`.

### 2.4 CI and triage

| Step | Command / rule |
|---|---|
| Nightly job (not on PR) | `cargo +nightly fuzz run <target> -- -max_total_time=300 -rss_limit_mb=1024 -dict=fuzz/wal.dict` for each target, in a matrix; total budget ~25 min. |
| PR job | `cargo +nightly fuzz build` only (compiles targets), plus `cargo fuzz run <target> fuzz/corpus/<target> -- -runs=0` to replay the committed corpus as regression tests. |
| Crash found | artifact lands in `fuzz/artifacts/<target>/`; run `cargo fuzz tmin <target> <artifact>` then `cargo fuzz fmt <target> <artifact>` to print the `arbitrary` value. |
| Regression test | add a `#[test]` in the module under test that embeds the minimised bytes (`include_bytes!("../fuzz/regressions/<target>/<hash>")` or an inline literal for < 200 bytes) and asserts the fixed behaviour; copy the artifact into `fuzz/corpus/<target>/` so the corpus replays it. One commit per crash: test + fix. |
| Coverage check (monthly) | `cargo fuzz coverage wal_differential` and confirm every `bail!`/`break` in `wal.rs` is hit. |

---

## 3. E2E harness design

### 3.1 Extend `tests/common/mod.rs`

Keep the existing `Harness` (process runner) and add an invariant layer. New pieces:

```rust
/// Truth about one managed db, derived only from what the CLI/library expose.
pub struct Observed { pub commits: u64, pub frames: u64, pub page_size: u64, pub log: Vec<LogRow>, pub wal_bytes: Option<u64> }

pub struct Model {
    pub shadow: rusqlite::Connection,          // in-memory, fed every *successful* statement
    pub snapshots: Vec<TableDump>,              // snapshots[k] = tables after the k-th capturing statement; [0] = schema+seed
    pub exec_sum: u64,                          // sum of `captured N`
    pub hashes: FileHashes,                     // live/base/meta sha256 after the last step
}

pub trait Step { fn run(&self, h: &Harness, m: &mut Model) -> StepResult; }   // CliExec, StdinBatch, LibExec, Clone, Rename, Drop, ExternalVacuum, KillWriterMidCommit, ...

pub struct Scenario { pub name: &'static str, pub schema: &'static str, pub steps: Vec<Box<dyn Step>>, pub checks: Checks }

impl Scenario {
    pub fn run(self) { let h = Harness::new(); h.create(..); let mut m = Model::new(..); for s in self.steps { s.run(&h, &mut m); h.assert_invariants(&self.db, &m, self.checks); } }
}
```

`Harness::assert_invariants(db, model, checks)` runs, in order (each is a flag in `Checks` so slow ones can be turned off per scenario):

| Check | Invariants |
|---|---|
| `status` parsed; `log --limit 1_000_000` parsed | INV-27, INV-28, INV-22 (via a direct read-only open of `meta.db`) |
| `commits == model.exec_sum`; `commits == model.snapshots.len() - 1` | INV-27, INV-38 |
| for k in 0..=commits: `clone -n k` -> integrity ok, `len == db_pages(k)*ps` (k>=1) or `== base.len()` (k==0), table dump == `model.snapshots[k]` | INV-29, INV-30, INV-33, INV-40, INV-53 |
| if `wal_bytes.is_none()`: `clone(commits)` bytes == `live.db` bytes | INV-31 |
| for a sampled pair n<m: `apply(frames n+1..=m)` on `clone(n)` == `clone(m)` (library) | INV-32 |
| hashes of live/base/meta unchanged by the clone loop | INV-34 |
| `list` contains db with the same count | INV-46 |

Model oracle: the shadow connection runs the same statement text (plus `PRAGMA foreign_keys` equal to live). Statements that use non-deterministic SQL (`randomblob`, `CURRENT_TIMESTAMP`) are modelled by comparing against `live.db` dumps instead of the shadow (`Model::from_live` flag). `TableDump` = `BTreeMap<table, Vec<Vec<rusqlite::types::Value>>>` ordered by rowid or all columns for `WITHOUT ROWID`.

Library vs CLI: `Step` impls come in pairs (`CliExec` spawns the binary; `LibExec` calls `walflip::db::exec`). A scenario can be run twice via `Scenario::via(Driver::Cli | Driver::Lib)`; both must produce identical `Observed`.

### 3.2 Scenarios

Each scenario asserts the full `assert_invariants` set after every step unless stated. "Model" column names the oracle beyond the default shadow db.

| # | Scenario | Steps | Model / oracle | Key invariants |
|---|---|---|---|---|
| S1 | Library API vs CLI parity | same 20-statement script via `LibExec` and via `CliExec` into two dbs | compare `Observed` and all `clone(k)` bytes between the two | INV-44, INV-27 |
| S2 | stdin batch vs per-process (bytes) | existing scenario 6 extended: compare `log` rows and `clone(k)` bytes for all k | byte compare | INV-44 |
| S3 | env vs flags vs mixed | create with flags; exec with env only; status with env root + flag db; wrong env + right flag wins | same `Observed` from all three | INV-45 |
| S4 | many small dbs | 12 dbs, 1-3 statements each, a decoy folder without `live.db`, a file in root | `list` sorted, counts == each `status`, decoy untouched (no `meta.db` created) | INV-46, INV-43 |
| S5 | large payload | 300 KB blob insert, update to 600 KB, delete, reinsert 1 KB | blob sha256 per clone; frames per commit > 70 | INV-41, INV-33 |
| S6 | DDL-heavy | 15 steps: `CREATE TABLE`, `CREATE INDEX`, `CREATE TRIGGER`, `CREATE VIEW`, `DROP TABLE`, `CREATE TABLE "a""b"`, `CREATE TABLE "with space"`, `CREATE TABLE "select"` | `sqlite_master` of clone(k) == shadow | INV-40, INV-47 (B1 fails) |
| S7 | schema evolution | `ALTER TABLE ADD COLUMN` with default, `ALTER TABLE RENAME COLUMN`, `ALTER TABLE RENAME TO`, inserts between | shadow | INV-40, INV-30 |
| S8 | delete-everything-then-reinsert | 200 inserts (batch), `DELETE FROM t`, 200 inserts again, `DELETE WHERE id%2=0` | shadow; `db_pages` non-decreasing without auto_vacuum | INV-30, INV-33 |
| S9 | VACUUM / shrink | (a) `exec VACUUM` -> error, state unchanged; (b) library step `ExternalVacuum` (open live with `wal_autocheckpoint=0`, `VACUUM`, `Store::capture(wal, "VACUUM")`) then more inserts | `len(clone(s)) < len(clone(s-1))`; final clone == live bytes | INV-42, INV-37 |
| S10 | interrupted / failed batch | stdin batch with a bad line in the middle; a statement with `; COMMIT;` inside; a `BEGIN` without `COMMIT`; a multi-line statement | shadow fed only successful statements; after each failure `clone(total) == live` | INV-37, INV-39 (B2 fails), INV-50 |
| S11 | clone chain | 30 commits; for all n<m sampled pairs, `apply(frames n+1..=m)` on clone(n) == clone(m); clone(total) == live | byte compare | INV-32, INV-31 |
| S12 | concurrent clones | 8 processes clone the same db, half `-n 10` half `-n 20`, to distinct paths; 2 processes race for the same path | all `-n 10` identical; exactly one of the racers succeeds or both fail, file (if any) is a valid clone | INV-49, INV-35 |
| S13 | concurrent exec (open question) | 4 processes x 20 inserts on one db | expected: 80 commits, labels == statements; today 120 and `UNIQUE` errors | INV-24 (B4 fails) |
| S14 | concurrent reader | hold `BEGIN; SELECT` on live.db from a helper process while running 10 `exec`; release; 10 more | no dup commits; `status.wal_bytes` present during, cleared after a final exec | INV-48, INV-23 |
| S15 | writer dies mid-life | helper process inserts with `wal_autocheckpoint=0` and `os._exit(0)`; then `status`, `clone`, `exec` | `status` shows WAL bytes; expected (after fix): `exec` captures the orphan under `(recovered)` | INV-25 (B3 fails) |
| S16 | rename/drop mid-life | two dbs; rename A while B keeps writing; drop B; A's history byte-identical to before rename | `meta.db` hash of A unchanged; B folder gone; `list` | INV-43 |
| S17 | read-only output dir | `chmod 555` target dir; `clone` fails; no file; db hashes unchanged (skipped as root) | hashes | INV-36 |
| S18 | schema file matrix | multi-statement + seed rows; stdin vs file (byte-identical `base.db`); empty file; syntax error; constraint violation in seed; `BEGIN/COMMIT`; unterminated `BEGIN`; `PRAGMA journal_mode=DELETE`; `PRAGMA page_size=8192`; `ATTACH` (asserts chosen policy) | `base.db == live.db` after create; folder absent after failures; `exec` after each successful create captures 1 commit | INV-52..INV-61 (B5 fails; B7/B8 per decision) |
| S19 | seed script | existing scenario 11 plus `assert_invariants` with `Checks::light` (no per-k clone; 500 commits) | counts, `clone(500) == live` | INV-27, INV-31 |
| S20 | perf smoke (`#[ignore]`) | 2000-insert stdin batch; 2000 per-process execs | wall-clock budget printed and asserted loosely | INV-51 |

---

## 4. Task graph

Model tiers: `haiku` mechanical, `sonnet` well-specified implementation, `opus` design-sensitive or likely to uncover/fix bugs.

| ID | Title | Files | Acceptance | Depends | Tier (why) |
|---|---|---|---|---|---|
| T01 | Extract reference WAL encoder into `wal::encode` behind `#[cfg(any(test, feature = "testing"))]`; add `testing` feature to `Cargo.toml`; fuzz crate enables it | `src/wal.rs`, `Cargo.toml`, `fuzz/Cargo.toml` | existing wal proptests pass unchanged using `encode::wal(...)`; `cargo build` without the feature has no encoder symbols | – | sonnet (refactor with a clear target) |
| T02 | Harness core: `Observed`, `Model`, `TableDump`, file hashes, `assert_invariants` with `Checks` flags | `tests/common/mod.rs` (new submodules `tests/common/model.rs`, `tests/common/invariants.rs`) | one existing CLI scenario converted and green; `assert_invariants` catches a deliberately corrupted `meta.db` (negative test) | – | opus (oracle design decides what every later scenario can detect) |
| T03 | `Scenario`/`Step` abstraction and `Driver::{Cli,Lib}` | `tests/common/scenario.rs` | S1 implemented and green through both drivers | T02 | sonnet |
| T04 | Convert existing 11 CLI scenarios to `Scenario`s with `assert_invariants` | `tests/cli.rs` | all 11 still green; no behaviour assertions lost (diff reviewed) | T03 | haiku (mechanical conversion) |
| T05 | New e2e scenarios S2, S3, S4, S5, S8, S16, S17 | `tests/scenarios_usage.rs` | each green; S17 skips when uid 0 | T03 | sonnet |
| T06 | New e2e scenarios S6, S7 (DDL, schema evolution) | `tests/scenarios_ddl.rs` | S7 green; S6 red on B1 with a clear assertion message, marked `#[should_panic]`-free but listed in the "known failing" section of the PR until T13 lands | T03 | sonnet |
| T07 | Scenario S18 schema-file matrix | `tests/scenarios_schema.rs` | all cases green except B5 (red until T15) and the two decision cases (Q7/Q8) which assert the documented current behaviour and carry a `// DECISION:` comment | T03 | sonnet |
| T08 | Library-level tests: S9 (external VACUUM shrink), S11 (clone chain), INV-23 cursor check | `tests/library.rs` | green; S9 shows `db_pages` decreasing in `log` | T02 | opus (needs SQLite WAL/checkpoint behaviour understanding) |
| T09 | Store unit tests INV-19/20/21/26 using the encoder to write WAL files | `src/store.rs` tests | green; INV-21 asserts no rows inserted | T01 | sonnet |
| T10 | WAL parser tests INV-06/07/08/09; BE variant of corruption test | `src/wal.rs` tests | green except INV-08 if upper bound not adopted (then documented) | T01 | sonnet |
| T11 | Replay bound (B6): `apply` returns `Err` for `pgno > db_pages` and for images above a cap; unit tests INV-15 | `src/replay.rs`, `src/db.rs` (pass cap) | `apply(.., [(u32::MAX, page)], None)` is `Err` in < 1 ms; all replay proptests green | – | sonnet |
| T12 | Concurrency scenarios S12, S13, S14, S15 (helpers spawn `python3 -I` or a tiny Rust bin under `tests/bin/`) | `tests/scenarios_concurrency.rs`, `tests/common/helpers.rs` | S12, S14 green; S13, S15 red with messages quoting counts (expected vs got) | T03 | opus (timing-sensitive, must avoid flakiness, documents real bugs) |
| T13 | Fix B1 quoting in `summarize` | `src/db.rs` | S6 green; unit test for `"a""b"` | T06 | haiku |
| T14 | Fix B2: detect transaction ended by the statement (`conn.is_autocommit()`), capture anyway, surface error; S10 implementation | `src/db.rs`, `tests/scenarios_failure.rs` | S10 green: after `; COMMIT;` statement, history has the committed rows, `exec` exits non-zero | T03 | opus (semantics decision + bug fix) |
| T15 | Fix B5: verify `journal_mode == wal` after schema; `exec` asserts WAL mode | `src/db.rs` | S18 journal_mode case green: `create` fails with `schema changed journal_mode`, no folder | T07 | sonnet |
| T16 | Fix B4 concurrency: `BEGIN IMMEDIATE` on `meta.db` before reading WAL+cursor (or lock file); fix B3: pre-capture orphans with `(recovered)` label | `src/store.rs`, `src/db.rs` | S13 and S15 green 20 runs in a row; roundtrip still green | T12 | opus (locking design across two SQLite files) |
| T17 | Fuzz target `wal_differential` + `Mutation` enum + dictionary | `fuzz/fuzz_targets/wal_differential.rs`, `fuzz/wal.dict`, `fuzz/Cargo.toml` | 5 min run clean after T10; finds a planted parser bug (temporarily remove the salt check) within 60 s | T01 | opus (differential design, mutation semantics) |
| T18 | Fuzz targets `replay_apply`, `store_capture`, `statements_split` | `fuzz/fuzz_targets/*.rs`, `fuzz/Cargo.toml` | each builds; `replay_apply` 2 min clean after T11 | T01, T11 | sonnet |
| T19 | Seed corpus generator (`Harness::wal_snapshot`) and committed corpus; `cmin` | `tests/common/mod.rs`, `fuzz/corpus/**`, `scripts/fuzz-seed.sh` | corpus < 2 MB; replaying corpus with `-runs=0` passes | T02, T17 | haiku |
| T20 | CI: PR job (test + fuzz build + corpus replay), nightly fuzz matrix with time budget, artifact upload, regression-test procedure doc | `.github/workflows/ci.yml`, `.github/workflows/fuzz.yml`, `fuzz/README.md`, `Makefile` (`make fuzz`) | workflows lint (`actionlint`); nightly budget <= 25 min | T17, T18 | sonnet |
| T21 | (Optional, after Q8) sandboxed schema-text fuzz target with authorizer denylist | `fuzz/fuzz_targets/schema_sandboxed.rs` | authorizer denies `ATTACH`/`PRAGMA`; 5 min run creates no file outside tempdir | T20, Q8 | opus (security-sensitive) |
| T22 | Perf smoke S20 (`#[ignore]`) and README accuracy (clone memory ~ frames up to N, not db size) | `tests/perf.rs`, `README.md` | runs under `cargo test -- --ignored` with printed timings | T03 | haiku |
| T23 | Final review: run everything, reconcile known-failing list, update README test section, update this plan's status column | all docs, no code | `cargo test` green except explicitly listed red scenarios with linked decisions; `cargo +nightly fuzz build` green | all | opus (judgement on what is done vs deferred) |

### 4.1 Waves

| Wave | Tasks (parallel) | Notes |
|---|---|---|
| W1 | T01, T02, T11 | three disjoint file sets: `src/wal.rs`+`Cargo.toml`+`fuzz/Cargo.toml`; `tests/common/**`; `src/replay.rs` (+ one call site in `src/db.rs::clone_at`, T11 owns that line only) |
| W2 | T03, T09, T10, T17, T18 | T03 needs T02; T09/T10/T17/T18 need T01 (and T18 needs T11). Files disjoint: `tests/common/scenario.rs`; `src/store.rs`; `src/wal.rs` tests; `fuzz/**` (T17 and T18 add separate target files; T17 owns `fuzz/Cargo.toml` edits, T18 appends its `[[bin]]` blocks in a second commit after T17 merges) |
| W3 | T04, T05, T06, T07, T08, T12, T19, T22 | all need T03 (T08 only T02). Each owns one new test file; T04 owns `tests/cli.rs`; T19 owns `fuzz/corpus/**` and adds one helper fn to `tests/common/mod.rs` (coordinate: T19 lands after T04's conversion) |
| W4 | T13, T14, T15, T16, T20 | bug fixes. File ownership: T13 `summarize` only; T14 `db::exec` + `tests/scenarios_failure.rs`; T15 `db::init_files` + `db::exec` WAL check (T14 and T15 both touch `exec`: T15 adds a single pragma check at the top, T14 edits the loop; merge T15 first); T16 `src/store.rs` + `db::exec` pre-capture (lands after T14); T20 CI files only |
| W5 | T21 (if Q8 decided), T23 | review/verification |

Ownership and merge rules:

- One writer per file per wave. `src/db.rs` is the contended file: W1 T11 (one line in `clone_at`), W4 in order T13 -> T15 -> T14 -> T16, each rebased on the previous.
- `tests/common/mod.rs` is owned by T02 in W1; afterwards additions go into new submodule files (`model.rs`, `invariants.rs`, `scenario.rs`, `helpers.rs`) with a single `pub mod` line added to `mod.rs` by the task that creates the submodule.
- `fuzz/Cargo.toml`: T01 (feature), then T17, then T18, sequentially.
- Known-red scenarios are allowed to merge only with a `// KNOWN FAILING: Bn, fixed by Tnn` comment and an entry in `docs/TESTING_PLAN.md` section 1.6; T23 removes the list.
- Every fix task must include the regression test that was red before it.

---

## 5. Risks and open questions

| ID | Question | Why it matters | Suggested default |
|---|---|---|---|
| Q1 | Concurrent `exec` on one db: supported, or documented as unsupported with a lock-file error? | B4 corrupts history silently today. Supporting it needs `meta.db` `BEGIN IMMEDIATE` around read-cursor + read-WAL + insert; the WAL itself is single-writer so SQLite serialises the commits. | support it (T16); a lock would add a failure mode without saving much |
| Q2 | A statement that ends its own transaction (`; COMMIT;`, `BEGIN ...`): reject up front, or capture whatever landed and report an error? | B2 loses history. Rejecting requires parsing SQL; capturing is cheap: check `conn.is_autocommit()` before `tx.commit()`. | capture, label `<sql> (partial)`, exit non-zero |
| Q3 | Orphan commits already in the WAL at `exec` start (crashed writer, external tool): capture with label `(recovered)`, or refuse until the user runs `walflip --db x capture`? | B3 mislabels. Refusing protects label accuracy but blocks the user. | capture as `(recovered)` and print a warning; also show `uncaptured: N` in `status` |
| Q4 | Should `exec` support `VACUUM` (run outside a transaction when the statement is exactly `VACUUM`)? | It is the only CLI path to a shrinking `db_pages`; today it errors. Replay handles shrink correctly (verified). | yes, special-case `VACUUM` |
| Q5 | `exec -` splitting on lines: keep, or split on `;` with a real tokenizer (`sqlite3_complete`)? | Multi-line DDL fails with `incomplete input`. `rusqlite` exposes `Connection::is_complete`-like via `ffi::sqlite3_complete`. | keep lines for now; document; INV-50 pins behaviour |
| Q6 | Sector-padding duplicate commit frames: merge into one commit or count each? | Only occurs on filesystems without powersafe-overwrite; counting inflates commits vs statements. | merge adjacent commit frames with identical `(pgno, db_pages, page)` in `capture`, not in `parse` |
| Q7 | `PRAGMA page_size` / `auto_vacuum` in a schema are silently ignored because WAL is enabled first. Apply pragmas before switching to WAL, or reject them? | Users will expect the schema to control page size; the README says page size is fixed at create. | run the schema first, then switch to WAL and checkpoint; reject `journal_mode` changes (fixes B5 and B7 together) |
| Q8 | `ATTACH` in schema files writes to arbitrary paths. Allow (user's own file) or deny via authorizer? | Security scope; also blocks any schema fuzzing. | deny `ATTACH`/`DETACH` with an authorizer in `create`; document |
| Q9 | `log` shows the first N commits (`LIMIT` without offset). Should the default show the most recent N, or add `--tail`? | For 500 commits the default output is 1..20, which is rarely useful for "find the bad DELETE". | add `--tail`, keep default |
| Q10 | Replay allocation cap value and where the cap lives (`apply` param vs `db.rs` constant)? | B6. SQLite max db is 2^31-2 pages; a hostile `meta.db` should not abort the process. | `apply` rejects `pgno > db_pages` when known, and `db.rs` passes `max_pages = db_pages.unwrap_or(base_pages + total_frames)` |
| Q11 | `list` opening `meta.db` with `CREATE TABLE IF NOT EXISTS` writes into any folder that merely has a `live.db`. Make `list`/`status` read-only (`OPEN_READ_ONLY`, no schema create)? | Side effects from read commands; also fails on read-only roots. | add `Store::open_ro` for list/status/log/clone |
| Q12 | Test runtime budget: per-k cloning after every step is O(steps^2). Cap at 30 steps for full checks, `Checks::light` above? | Keeps `cargo test` under ~30 s. | yes; seed scenario uses `light` |
| R1 | Flakiness in concurrency scenarios (process scheduling). | Mitigate with deterministic barriers (helper waits on a file), 20-run soak in T12/T16 acceptance, no sleeps. | |
| R2 | `python3 -I` dependency for helper writers. | Prefer a tiny Rust helper binary (`tests/bin/writer.rs` via `[[bin]]` with `required-features = ["testing"]`) so CI needs no Python. | |
| R3 | Fuzz nightly budget vs findings. | Start at 5 min/target; raise for `wal_differential` if coverage report shows unreached branches. | |
| R4 | README claims clone needs RAM ~ db size; actual is ~ sum of frames up to N (`clone_at` collects all frames into a `Vec`). | Document or stream frames with last-writer-wins dedupe (`SELECT ... WHERE rowid IN (SELECT max(rowid) ... GROUP BY pgno)`). T22 documents; a follow-up task may stream. | |
