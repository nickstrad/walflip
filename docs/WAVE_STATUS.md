# Wave status

**Project state (2026-10-15): all waves (W1-W6) are complete and merged to `main`.** Defects
B1-B10 are fixed, all decisions are implemented, CI is green, and the fuzz corpus is minimised.
Nothing is required. The open items below are optional follow-ups or things to watch.

Source of truth for tasks, invariants and ownership rules: `docs/TESTING_PLAN.md`
(section 4 task table, section 4.1 waves). This file records progress, decisions and how to run
everything so a fresh session can continue.

## Done

| Wave | Tasks | Notes |
|---|---|---|
| W1 | T01 `wal::encode` behind `testing` feature, T02 harness core, T11 replay bound (B6) | merged |
| W2 | T03 `Scenario`/`Step`/`Driver`, T09 store tests, T10 WAL parser tests, T17 `wal_differential`, T18 `replay_apply`/`store_capture`/`statements_split` fuzz targets | merged |
| W3 | T04 CLI scenarios, T05 usage S2/S3/S4/S5/S8/S16/S17, T06 DDL S6/S7, T07 schema matrix S18, T08 library S9/S11/INV-23, T12 concurrency S12-S15, T19 fuzz seed corpus, T22 perf smoke + README | merged |
| W4 | T13 B1 quoting + B9 clone race, T15 schema-before-WAL + authorizer (B5/B7/B8, Q7/Q8), T14 reject transaction control (B2, Q2), T16 writer lock + `(recovered)` pre-capture (B4/B3, Q1), T24 incremental capture (INV-51), T20 CI workflows, Q9 `log` default | merged |
| W6 | B10 percent-encoded clone path, CI seed-script fix (green on GitHub), INV-08 page-size bound, Q4 `exec VACUUM`, Q5 (keep line splitting, docs), Q6 (pin padding behaviour, leave parser), Q11 `Store::open_existing`, T21 `exec_sql` fuzz target | merged |
| W5 | T23 final review: re-ran everything and reconciled the plan's status cells (INV, S1-S20, T01-T24) with the tests. Results are at the top of `docs/TESTING_PLAN.md`. | docs only; found B10 |
| final | INV-56/58(exec)/60 asserts, S20 hot-journal proof, Q5 `exec --script` + one-row `log` labels, CI pinned to `ubuntu-24.04` + fuzz `SQLITE_NO_SYNC`, actionlint, fmt/clippy clean, MSRV 1.85 declared, fuzz corpus minimised | merged |
| extra | `docs/architecture/` C4 viewer built with c4-sdk | `cd docs/architecture && npm install && npm run dev` (Node 22.23.2) |

## Defects

Defects B1-B10 are fixed, each with a regression test. Questions Q1-Q9, Q11 and INV-08 are decided
and implemented (see Decisions). INV-56/58/60 are fully asserted, the S20 hot-journal test is proven
to leave a real `meta.db-journal`, Q5 (`exec --script`, one-row `log` labels) is done, both
workflows pass `actionlint`, CI runs on a pinned `ubuntu-24.04`, fuzz builds use
`SQLITE_NO_SYNC`, `cargo fmt` and `cargo clippy --features testing --all-targets` are clean, and
the MSRV is declared (`rust-version = "1.85"`; the test suite needs 1.88 because of `proptest`).

Open items, all optional (same list as `docs/TESTING_PLAN.md` section 1.6):

| Item | State |
|---|---|
| T21 nightly `exec_sql` timeout | `exec_sql` once hit libFuzzer's 30 s timeout in `fsync` on a loaded machine. Fuzz builds now use `SQLITE_NO_SYNC`; watch the nightly `fuzz.yml` runs to confirm it does not come back. |
| `SQLITE_NO_SYNC` effect | Verified only indirectly (build flag reaches libsqlite3-sys, no timeouts since). No test proves fsync is a no-op in fuzz builds. |
| Ubuntu 26 runner | CI is pinned to `ubuntu-24.04`. Moving to Ubuntu 26 is a future, deliberate change: bump the pin and watch the first run. |
| fuzz crate lint | `cargo clippy` covers the main crate only; `fuzz/` is not clippy-linted. |
| Tests on Rust 1.85 | The library builds on 1.85; tests need 1.88 (`proptest`). Whether the test code compiles on 1.85 with an older `proptest` is untried. |
| `--script` splitting cost | `exec --script` re-runs `sqlite3_complete` on a growing prefix, so one statement containing many `;` (e.g. a long trigger body or string) splits in quadratic time. Fine for normal scripts. |

Known limits (documented, by design):
- A writer that does not use walflip's locks can commit between a statement's commit and its capture. `exec` captures everything and fails with "statement produced N commits".
- If an outside writer's commits are checkpointed and the WAL restarted before any `exec` runs, those changes are in `live.db` but cannot be recovered into history.
- Incremental capture assumes the WAL is append-only within a generation (bytes before the stored offset are not re-verified).
- Sector-padding frames from a foreign writer appear as `(recovered)` commits (Q6: the parser is left alone; if merging is ever wanted, do it in `wal::scan`, not in `capture`).
- `exec -` splits on lines, not on `;`; multi-line SQL needs `exec --script` (Q5).
- Fuzzing writes new inputs into `fuzz/corpus/<target>`. Minimise with `-merge=1` against the committed seeds before committing any, and delete the rest.

## Verify the current state

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
cargo test --features testing        # 155 pass, 2 ignored (perf per-process, fuzz_seed generator); needs Rust 1.88+
# per suite: unit 71, cli 17, library 7, perf 1, roundtrip 3, concurrency 6, core 3, ddl 8,
# failure 14, schema 15, usage 10
cargo fmt --check
cargo clippy --features testing --all-targets   # 0 warnings
actionlint                           # both workflows clean
cargo test --features testing --no-fail-fast -- --ignored   # both pass
cargo +nightly fuzz build
make fuzz-replay                     # all 6 corpora, -runs=0
```

The `fuzz_seed` generator writes into `fuzz/corpus` unless `WALFLIP_SEED_OUT` is set, so set it
to a scratch dir when you run `--ignored` (as `scripts/fuzz-seed.sh` does):

```sh
WALFLIP_SEED_OUT=$(mktemp -d) cargo test --features testing --no-fail-fast -- --ignored
```

Per-suite commands (`--features testing --test <name>`):

| Suite | Covers |
|---|---|
| `cli` | 11 original CLI scenarios on `Scenario` + invariants (T04) |
| `scenarios_core` | S1 library vs CLI parity, harness self-checks |
| `scenarios_usage` | S2 S3 S4 S5 S8 S16 S17 (T05) |
| `scenarios_ddl` | S6 DDL incl. quoted names, S7 schema evolution (T06) |
| `scenarios_schema` | S18 schema-file matrix, incl. journal_mode / page_size / ATTACH policy (T07, T15) |
| `scenarios_failure` | S10 failed statements, transaction control, orphan recovery (T14, T16) |
| `scenarios_concurrency` | S12 clones, S13 concurrent exec, S14 reader, S15 crashed writer (T12, T16) |
| `library` | S9 VACUUM shrink, S11 clone chain, INV-23 cursor (T08) |
| `roundtrip` | random kv ops vs real SQLite |
| `perf` | S20: `-- --ignored --nocapture` for the per-process test; the stdin batch runs by default |
| `fuzz_seed` | `-- --ignored`: corpus generator behind `scripts/fuzz-seed.sh` |

Fuzz: `make fuzz` (30 s per target), `make fuzz-replay` (corpus regression), `./scripts/fuzz-seed.sh`
(regenerate seeds). Targets: `wal_parse`, `wal_differential`, `replay_apply`, `store_capture`,
`statements_split`, `exec_sql` (T21: structured programs through the real `create`/`exec`, with a
fuzz-only sandbox hook behind the `testing` feature). Triage procedure: `fuzz/README.md`.

## Decisions (2026-10-14; supersede plan section 5 defaults where they differ)

| Q | Decision | Landed in |
|---|---|---|
| Q1 | Support concurrent `exec`: per statement `BEGIN IMMEDIATE` on `meta.db` then on `live.db`, capture inside the `meta.db` transaction, 10 s busy timeout. SQLite locks, no lock file. | T16 |
| Q2 | Reject transaction control (BEGIN/COMMIT/END/ROLLBACK) in `exec` via authorizer; `create` still allows it; SAVEPOINT allowed. Error-path capture backstop; >1 commit per statement is an error. | T14 |
| Q3 | Orphan commits captured as `(recovered)` before the statement, stderr warning, `status` shows `uncaptured: N`. | T16 |
| Q7 | Schema runs in rollback mode, then autocommit check, WAL switch, `wal` assert, checkpoint. `page_size`/`auto_vacuum` in a schema now take effect. `PRAGMA journal_mode` denied. | T15 |
| Q8 | `ATTACH`/`DETACH` denied in `create` and `exec`. | T15 |
| Q9 | `log` shows the most recent N, oldest to newest; `--head` for the first N, `--all` for everything. | Q9 task |
| Q4 | `exec VACUUM` supported via a strict path: exactly `VACUUM` or `VACUUM main`, run in autocommit under the meta lock after the recovery capture; always one commit. `VACUUM INTO` and other forms refused. Known gap and cost documented. | Q4 task |
| Q5 | Keep line-based `exec -` splitting (`sqlite3_complete` needs a trailing `;` and would break existing batches); README states the rule. | docs |
| Q6 | Leave the parser alone; pin real SQLite padding (`?psow=0` foreign writer) with a test; padding frames appear as `(recovered)` commits, replay stays exact. | tests/library.rs |
| Q11 | `Store::open_existing` (no create, no schema) for `list`/`status`/`log`/`clone`; `list` skips folders without `meta.db`. Not `OPEN_READ_ONLY` (breaks on a hot `meta.db-journal`). | Q11 task |
| INV-08 | WAL header page size must be a power of two in 512..=65536. | wal.rs |
| T21 | `exec_sql` fuzz target with a testing-only sandbox hook; production authorizer unchanged. | T21 |
| B9 | `clone_at` opens output with `create_new(true)`. | T13 |
| INV-51 | Cursor stores header, offset, running checksum (`cursor_resume`); capture parses only the new tail, full-parse fallback on any inconsistency. 2000-insert batch: 52 s -> 0.95 s debug. | T24 |

Merge-order note: all of W4 shared `src/db.rs`, so tasks ran sequentially, each branching from the
previous merge.
