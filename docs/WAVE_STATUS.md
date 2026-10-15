# Wave status

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
| extra | `docs/architecture/` C4 viewer built with c4-sdk | `cd docs/architecture && npm install && npm run dev` (Node 22.23.2) |

## Defects

All defects B1-B9 from the plan are fixed. Open items:

| Item | State |
|---|---|
| INV-08 | WAL page size above 65536 is accepted. `#[ignore]`d unit test, harmless (zero frames fit). Decide: adopt the upper bound or document. |
| CI lint | `.github/workflows/*.yml` and `fuzz/README.md` were never linted (`actionlint` not installed when written). Run `actionlint .github/workflows/*.yml`. |
| Q4 | `exec VACUUM` still errors (no library entry point either). Undecided; needs its own bypass of the transaction-control denial (`VACUUM INTO` trips SQLITE_TRANSACTION). |
| Q5, Q6, Q11 | Line-based `exec -` splitting, sector-padding duplicate commit frames, read-only `list`/`status`: undecided, unchanged from plan section 5. |
| T21 | Sandboxed schema-text fuzz target. Unblocked by Q8 (authorizer exists) but not started. |
| T23 | Final review task, not started. |

Known limits (documented, by design):
- A writer that does not use walflip's locks can commit between a statement's commit and its capture. `exec` captures everything and fails with "statement produced N commits".
- If an outside writer's commits are checkpointed and the WAL restarted before any `exec` runs, those changes are in `live.db` but cannot be recovered into history.
- Incremental capture assumes the WAL is append-only within a generation (bytes before the stored offset are not re-verified).

## Verify the current state

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
cargo test --features testing        # 129+ pass, ignored: INV-08, perf per-process, fuzz_seed generator
cargo +nightly fuzz build
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
`statements_split`. Triage procedure: `fuzz/README.md`.

## Decisions (2026-10-14; supersede plan section 5 defaults where they differ)

| Q | Decision | Landed in |
|---|---|---|
| Q1 | Support concurrent `exec`: per statement `BEGIN IMMEDIATE` on `meta.db` then on `live.db`, capture inside the `meta.db` transaction, 10 s busy timeout. SQLite locks, no lock file. | T16 |
| Q2 | Reject transaction control (BEGIN/COMMIT/END/ROLLBACK) in `exec` via authorizer; `create` still allows it; SAVEPOINT allowed. Error-path capture backstop; >1 commit per statement is an error. | T14 |
| Q3 | Orphan commits captured as `(recovered)` before the statement, stderr warning, `status` shows `uncaptured: N`. | T16 |
| Q7 | Schema runs in rollback mode, then autocommit check, WAL switch, `wal` assert, checkpoint. `page_size`/`auto_vacuum` in a schema now take effect. `PRAGMA journal_mode` denied. | T15 |
| Q8 | `ATTACH`/`DETACH` denied in `create` and `exec`. | T15 |
| Q9 | `log` shows the most recent N, oldest to newest; `--head` for the first N, `--all` for everything. | Q9 task |
| B9 | `clone_at` opens output with `create_new(true)`. | T13 |
| INV-51 | Cursor stores header, offset, running checksum (`cursor_resume`); capture parses only the new tail, full-parse fallback on any inconsistency. 2000-insert batch: 52 s -> 0.95 s debug. | T24 |

Merge-order note: all of W4 shared `src/db.rs`, so tasks ran sequentially, each branching from the
previous merge.
