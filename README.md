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

Requires a Rust toolchain (1.75+) and a Unix-like OS. SQLite is bundled.

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
| `exec <sql>` / `exec -` | Run a statement, or read one statement per line from stdin. Each statement is its own commit. |
| `log [--limit N]` | Show captured commits: frames written, database size in pages, SQL. |
| `status` | Page size, commit and frame counts, whether a live WAL exists. |
| `clone -n N -o <file>` | Write the database as of commit `N` to `<file>`, then run `integrity_check` and print row counts. `-n 0` is the initial snapshot. |

The schema file may contain any SQL: several tables, indexes, triggers, and seed rows. Whatever
it produces is the initial snapshot, so `clone -n 0` returns it and history starts after it. If
the schema fails to apply, nothing is left on disk. Later schema changes made with
`exec "CREATE TABLE ..."` or `ALTER TABLE` are captured like any other statement.

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

1. `exec` opens `live.db` with automatic checkpointing disabled and runs the statement in
   a transaction.
2. Before the connection closes, walflip parses `live.db-wal` directly: header, salts,
   per-frame checksum chain, and commit markers. A frame whose `db_size` field is non-zero
   ends a transaction.
3. Each commit's frames (page number and full page image) are stored in `meta.db`. A cursor
   of WAL salts and commits seen makes capture idempotent and survives WAL restarts.
4. `clone` copies `base.db` and overwrites pages at `(pgno - 1) * page_size` with the
   stored images up to commit `N`, then resizes to the database size recorded by commit `N`.

Because every WAL frame is a complete page image, replay is a sequence of positioned writes
with no SQL involved.

## Limitations

- **All writes must go through `walflip exec`.** Changes made to `live.db` by other
  programs are not captured, and a checkpoint by another writer can discard WAL frames
  before walflip reads them.
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
make test      # unit, property and end-to-end tests
make fuzz      # coverage-guided fuzzing of the WAL parser (nightly + cargo-fuzz)
make demo      # build, seed a 500-commit database from `ps` output, clone at 250 and 500
make clean     # remove walflip-data/ and demo clones
```

Layout:

| Path | Role |
|---|---|
| `src/wal.rs` | Pure WAL parser (header, frames, checksums, commit grouping) |
| `src/replay.rs` | Pure page replay onto an in-memory database image |
| `src/store.rs` | `meta.db` persistence and idempotent capture cursor |
| `src/db.rs` | Database operations and name validation; return data, never print |
| `src/main.rs` | CLI: argument parsing and output only |

Tests:

- **Property tests** (`proptest`) cover the WAL parser with a reference encoder: round-trip in
  both byte orders, truncation yields a prefix of commits, any single-byte corruption never
  admits altered data, arbitrary bytes never panic. Replay is checked against a page-map
  model for last-writer-wins, composition and idempotence.
- **Fuzzing** (`fuzz/`, cargo-fuzz) runs `wal::parse` on raw bytes, seeded with real WAL files,
  asserting it never panics and only accepts internally consistent WALs.
- **Round-trip tests** (`tests/roundtrip.rs`) run random insert/update/delete sequences on a
  real database and assert that `clone -n k` equals the live contents after step *k*.
- **End-to-end tests** (`tests/cli.rs`) drive the compiled binary through: the full
  lifecycle, schema files (multi-table, seed rows, stdin), stdin batches with time travel to
  every commit, environment-based configuration, multiple isolated databases, accident recovery, one-process-per-statement
  versus batch equivalence, error handling, no-op statements, and the seed script.
