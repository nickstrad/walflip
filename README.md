# walflip

Point-in-time copies of SQLite databases, built from the write-ahead log.

`walflip` manages SQLite databases and records every committed transaction as the page
images SQLite wrote to the WAL. Any database can then be rebuilt exactly as it stood after
commit *N*, as a standalone file you can open with any SQLite client.

Typical uses:

- Recover from a bad `DELETE` or `UPDATE` by cloning the commit just before it.
- Reproduce a bug against the exact state a database was in at a given step.
- Inspect how a workload touches pages: frames and database size per commit.

## Install

Requires a Rust toolchain (1.85+, the minimum for the `clap` version in `Cargo.lock`) and a
Unix-like OS. SQLite is bundled.

```sh
git clone https://github.com/nickstrad/walflip.git
cd walflip
cargo install --path .        # installs `walflip` to ~/.cargo/bin
```

## Quick start

```sh
cat > orders.sql <<'SQL'
CREATE TABLE orders(id INTEGER PRIMARY KEY, item TEXT NOT NULL, qty INTEGER);
SQL
walflip create orders --schema orders.sql
walflip --db orders exec "INSERT INTO orders(item, qty) VALUES('widget', 3)"
walflip --db orders exec "INSERT INTO orders(item, qty) VALUES('gadget', 1)"
walflip --db orders exec "DELETE FROM orders"

walflip --db orders log
walflip --db orders clone -n 2 -o orders-before-delete.db   # both rows are back
```

Every `exec` statement is one transaction and one commit. `clone -n N` replays the first
`N` commits onto the database's initial snapshot.

## Commands

| Command | Description |
|---|---|
| `create <name> [--schema <file>]` | Create a database in `<root>/<name>/`. `--schema` is an SQL file (`-` for stdin) applied before the initial snapshot is taken; without it the database starts empty. |
| `list` | List databases and their commit counts. |
| `rename <old> <new>` | Rename a database; history is preserved. |
| `drop <name>` | Delete a database and its history. |
| `exec <sql>` / `exec -` | Run a statement, or read statements from stdin, one per line (no trailing `;` needed; a statement that spans lines fails with `incomplete input` and that line commits nothing). Each statement is its own commit. Statements before a failing one stay committed and captured. |
| `log [--limit N] [--head] [--all]` | Show captured commits (frames written, database size in pages, SQL), oldest first. Default: the most recent 20. `--head` shows the first N instead; `--all` shows everything; `--limit 0` shows only the header. `--head` and `--all` conflict. |
| `status` | Page size, commit and frame counts, and whether a live WAL exists. If the WAL holds commits that are not in history yet, it also prints `uncaptured: N`. |
| `clone -n N -o <file>` | Write the database as of commit `N` to `<file>`, then run `integrity_check` and print row counts. `-n 0` is the initial snapshot. |

The schema file may contain SQL such as tables, indexes, triggers, views, seed rows and
`BEGIN`/`COMMIT`. Whatever it produces is the initial snapshot, so `clone -n 0` returns it and
history starts after it. The schema runs before WAL mode is switched on, so `PRAGMA page_size`
and `PRAGMA auto_vacuum` in it take effect. If the schema fails to apply, or leaves a `BEGIN`
open, nothing is left on disk. Later schema changes made with `exec "CREATE TABLE ..."` or
`ALTER TABLE` are captured like any other statement.

Statements that walflip refuses (in the schema and in `exec`): `ATTACH`, `DETACH` and
`PRAGMA journal_mode`. `exec` also refuses transaction control (`BEGIN`, `COMMIT`, `END`,
`ROLLBACK`), because every statement already runs in its own transaction. `SAVEPOINT` is
allowed.

`exec VACUUM` is supported: a statement that is exactly `VACUUM` or `VACUUM main` (any case,
optional trailing `;`) runs outside a transaction and is captured as one commit, so `log` shows
`db_pages` shrinking there. Anything else that starts with `VACUUM` is refused before anything
runs: `VACUUM INTO` (no file is created), another schema, or more SQL after it in the same
statement (`VACUUM; DROP TABLE t` is one statement, so it is refused as a whole).

### Selecting a database

Commands that act on a database take `--db <name>`. Both settings can come from the
environment:

| Flag | Environment | Default |
|---|---|---|
| `--root <dir>` | `WALFLIP_ROOT` | `walflip-data` |
| `--db <name>` | `WALFLIP_DB` | none |

```sh
export WALFLIP_DB=orders
walflip exec "UPDATE orders SET qty = 5 WHERE id = 1"
walflip status
```

## On-disk layout

```
walflip-data/
  orders/
    live.db    the database you write to (WAL mode)
    base.db    snapshot taken at create, after the schema was applied
    meta.db    SQLite: commit log (commits) and captured page images (frames)
```

## How it works

1. `exec` opens `live.db` with automatic checkpointing disabled. For each statement it takes
   the history write lock (`BEGIN IMMEDIATE` on `meta.db`) and then the `live.db` write lock.
   It runs the statement in its own transaction and commits it. `VACUUM` is the exception:
   after the recovery step below, walflip releases the `live.db` transaction and runs `VACUUM`
   in autocommit, still under the history lock.
2. Still holding the history lock, walflip parses `live.db-wal` directly: header, salts,
   per-frame checksum chain, and commit markers. A frame whose `db_size` field is non-zero
   ends a transaction. Commits that were already in the WAL before the statement ran (for
   example, from a writer that crashed) are stored first under the label `(recovered)`, with a
   warning, so they are never attributed to the statement.
3. Each commit's frames (page number and full page image) are stored in `meta.db`. A cursor
   of WAL salts and commits seen makes capture idempotent and survives WAL restarts. It also
   keeps the byte offset and running checksum of the last captured commit, so each capture
   reads only the frames added since (falling back to a full parse if the WAL was rewritten).
4. `clone` copies `base.db` and overwrites pages at `(pgno - 1) * page_size` with the
   stored images up to commit `N`, then resizes to the database size recorded by commit `N`.

Because every WAL frame is a complete page image, replay is a sequence of positioned writes
with no SQL involved.

## Limitations

- **Writes should go through `walflip exec`.** Several `walflip exec` processes may run
  against one database at the same time: the locks serialise them, and each statement is
  captured exactly once under its own text. Commits that another program leaves in
  `live.db-wal` are captured by the next `exec` as `(recovered)`, without their SQL. If the
  other program checkpoints and restarts the WAL first, those changes are in `live.db` but
  are lost from history. If it commits between a walflip statement's commit and its capture,
  that `exec` fails with `statement produced N commits` (all of them are captured).
- **`exec VACUUM` is expensive in history.** VACUUM rewrites every page, so each one writes
  the whole database into `live.db-wal` and stores it again in `meta.db`. A small database
  vacuumed often grows `meta.db` by its full size every time.
- **`exec VACUUM` leaves a short gap for other programs.** VACUUM cannot run inside walflip's
  `live.db` transaction, so walflip releases `live.db` (still holding the history lock, so other
  `walflip exec` processes wait) and VACUUM then takes the write lock itself. A program that
  writes `live.db` directly can commit in between; its commit is captured with the VACUUM's
  label and that `exec` fails with `VACUUM produced 2 commits` (all of them are captured).
- **`exec -` splits on lines**, not on `;`: one statement per non-blank line, no trailing `;` required. A statement that spans several lines fails with `incomplete input` and nothing from that line is committed (earlier lines stay committed). Pass multi-line or multi-statement SQL as one statement per `exec` argument, or put each statement on its own line.
- **Foreign writers with `POWERSAFE_OVERWRITE` off** (`?psow=0` plus `synchronous=FULL`) make SQLite repeat each commit frame to pad a sector. walflip does not merge them: each padding frame is a separate commit labelled `(recovered)`, so history can show more commits than statements. Replay is still exact, since the copies are identical pages. walflip's own connection never pads.
- **History is page-level.** The WAL contains pages, not statements. The `sql` shown by
  `log` is the statement you ran, recorded alongside the commit.
- **A statement that changes nothing is not a commit.** SQLite writes no frames for an
  `UPDATE` that matches no rows or sets identical values.
- **Each `exec` statement is one commit.** Multi-statement transactions are not exposed.
- **`clone` builds the image in memory.** It reads the base snapshot and loads every captured
  page image up to commit `N` (no de-duplication), so peak RAM is roughly the base size plus the
  sum of frames up to `N`. It scales with history length, not only with the size of the live
  database. Cloning an early commit of a long-lived database is cheap; cloning the latest commit
  of a heavily rewritten one may need several times the database size.
- Page size is fixed at `create`; changing it afterwards is rejected at capture time.


## Development

```sh
make test         # cargo test --features testing: unit, property and end-to-end tests
make fuzz         # 30 s per fuzz target (FUZZ_SECS=N to change); nightly + cargo-fuzz
make fuzz-replay  # replay the committed corpora once (-runs=0), as CI does
make demo         # build, seed a 500-commit database from `ps` output, clone at 250 and 500
make clean        # remove walflip-data/ and demo clones
```

The test suites need the `testing` feature, which exposes the reference WAL encoder
(`wal::encode`). A normal `cargo build` leaves the encoder out. Run one suite with
`cargo test --features testing --test <name>`. `docs/WAVE_STATUS.md` lists the suites.

Layout:

| Path | Role |
|---|---|
| `src/wal.rs` | Pure WAL parser (header, frames, checksums, commit grouping) |
| `src/replay.rs` | Pure page replay onto an in-memory database image |
| `src/store.rs` | `meta.db` persistence and idempotent capture cursor |
| `src/db.rs` | Database operations and name validation; return data, never print |
| `src/main.rs` | CLI: argument parsing and output only |

Tests (130 run by default, about 6 s of test time in a debug build after compiling). Every invariant they check is
catalogued in `docs/TESTING_PLAN.md`, section 1:

- **Unit and property tests** (`src/*.rs`) cover the WAL parser with a reference encoder:
  round-trip in both byte orders, truncation gives a prefix of commits, a corrupted byte never
  admits altered data, stale salts and broken checksum chains end parsing, and resumed parsing
  equals a full parse. Replay is checked against a page-map model and an allocation bound.
  Store capture is checked for idempotence, growth, WAL restarts, page-size mismatch and
  atomic failure.
- **Scenario tests** (`tests/scenarios_*.rs`, `tests/cli.rs`, `tests/library.rs`) run scripted
  steps through the CLI binary and through the library. After every step they check the
  structure of `meta.db`, `log`/`status` consistency, every `clone -n k` against a shadow
  SQLite model, `clone` of the last commit against `live.db` byte for byte, and that cloning
  changes no file. The steps cover DDL and quoted table names, schema files, large blobs,
  `exec VACUUM` and external `VACUUM`, failed statements, rename and drop, env vs flags, and
  concurrency: parallel clones, parallel `exec`, a held reader, a writer killed mid-life, and a lock holder
  that dies.
- **Round-trip tests** (`tests/roundtrip.rs`) run random insert/update/delete sequences on a
  real database and assert that `clone -n k` equals the live contents after step *k*.
- **Perf smoke** (`tests/perf.rs`): a 2000-statement stdin batch (default). Run the
  2000-process variant with `-- --ignored --nocapture`.
- **Fuzzing** (`fuzz/`, cargo-fuzz, 6 targets): `wal_parse` (raw bytes), `wal_differential`
  (encoder plus mutations, differential against the input), `replay_apply`, `store_capture`
  (real SQLite I/O), `statements_split`, and `exec_sql`, which runs structured SQL programs
  through `create` and `exec` in a sandbox and checks after every `exec` that history clones
  to exactly `live.db`. CI builds the targets and replays the committed
  corpora on every PR, and fuzzes each target for 5 minutes nightly. `fuzz/README.md` describes
  crash triage. A local `make fuzz` adds new inputs to `fuzz/corpus/`; minimise them before
  committing (`cargo +nightly fuzz cmin <target>`).

Known gaps: `cargo test -- --ignored` includes one red test,
`wal::tests::inv08_page_size_above_65536_is_rejected` (the parser accepts page sizes above
65536; this is harmless). `docs/TESTING_PLAN.md` section 1.6 lists the remaining open items.
