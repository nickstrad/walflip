# walflip

A small Rust CLI for learning how SQLite's write-ahead log works. It captures every
committed transaction from a live database's WAL, then rebuilds the database as it was
after any commit `N`.

## Overview

`walflip` keeps three files in `walflip-data/`:

| File       | Purpose                                                        |
|------------|----------------------------------------------------------------|
| `live.db`  | The database you write to (WAL mode)                           |
| `base.db`  | Snapshot taken at `init`, after the schema is created          |
| `meta.db`  | SQLite metadata: `commits` log and the captured WAL `frames`   |

- **Capture:** `exec` runs each statement as one transaction, then parses `live.db-wal`
  (header, frames, salts, checksum chain) and stores each commit's page images in `meta.db`.
- **Clone:** copy `base.db`, then write the stored page images of commits `1..=N` at
  `(pgno - 1) * page_size` and truncate to the size recorded by commit `N`. Every WAL frame
  is a full page image, so replay is just positioned writes.
- A "change" is one committed transaction. The WAL holds pages, not SQL; the `sql` column in
  `meta.db` is the statement you ran.

Code: `src/wal.rs` (WAL parser), `src/store.rs` (metadata + capture), `src/main.rs` (CLI).

## Installation

Requires a Rust toolchain (SQLite is bundled via `rusqlite`) and a Unix-like OS.

```sh
brew install rustup && rustup-init -y   # or https://rustup.rs
cargo build --release                   # binary: target/release/walflip
```

## Usage

```sh
walflip init --schema schema.sql [--force]   # create live.db + base.db
walflip exec "INSERT INTO t VALUES (1)"      # one statement = one commit
walflip exec - < changes.sql                 # one statement per line, each its own commit
walflip log --limit 10                       # list captured commits
walflip status                               # page size, commit/frame counts, WAL state
walflip clone -n 200 -o out.db               # rebuild the db as of commit 200
```

`--dir <path>` (default `walflip-data`) selects the data directory.

## Example

`scripts/seed.sh` builds a `processes` table from `ps` output and applies exactly 500
changes: 250 inserts, 150 updates, 100 deletes.

```sh
make demo      # build, seed, clone at 250 and 500 into /tmp
make seed      # build and seed only
make clean     # remove walflip-data/ and the /tmp clones
make distclean # clean + cargo clean
```

Expected: the clone at 250 has 250 rows, the clone at 500 has 150.
