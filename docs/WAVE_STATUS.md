# Wave status and how to run the next wave

Source of truth for tasks, invariants and ownership rules: `docs/TESTING_PLAN.md`
(section 4 task table, section 4.1 waves). This file records progress and the
launch recipe so a fresh session can continue.

## Done

| Wave | Tasks | Notes |
|---|---|---|
| W1 | T01 `wal::encode` behind `testing` feature, T02 harness core (`Model`, `Observed`, `Checks`, `assert_invariants`), T11 replay bound (B6 fixed) | merged, pushed |
| W2 | T03 `Scenario`/`Step`/`Driver`, T09 store capture tests, T10 WAL parser tests, T17 `wal_differential` fuzz target, T18 `replay_apply`/`store_capture`/`statements_split` fuzz targets | merged |
| W3 | T04 CLI scenarios on `Scenario`, T05 usage S2/S3/S4/S5/S8/S16/S17, T06 DDL S6/S7, T07 schema matrix S18, T08 library S9/S11/INV-23, T12 concurrency S12-S15, T19 fuzz seed corpus, T22 perf smoke + README | merged, pushed |
| extra | `docs/architecture/` C4 viewer built with c4-sdk (Vite + React ESM) | `cd docs/architecture && npm install && npm run dev` (Node 22.23.2) |

Known failing, deliberately `#[ignore]`d: INV-08 (WAL page size above 65536 accepted).
Open defects reported by the plan and not fixed yet: B1 (summarize quoting), B2 (statement
that ends its own transaction), B3 (orphan WAL commits mislabelled), B4 (conc## Wave 3: done

Run commands (first: `export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"`):

| Task | Command | Result |
|---|---|---|
| T04 | `cargo test --features testing --test cli` | 15 pass |
| T05 | `cargo test --features testing --test scenarios_usage` | 7 pass |
| T06 | `cargo test --features testing --test scenarios_ddl` (red case: `-- --ignored`) | 7 pass, 1 ignored (B1) |
| T07 | `cargo test --features testing --test scenarios_schema` (red case: `-- --ignored`) | 10 pass, 1 ignored (B5) |
| T08 | `cargo test --features testing --test library` | 4 pass |
| T12 | `cargo test --features testing --test scenarios_concurrency` (red cases: `-- --ignored`) | 3 pass, 2 ignored (B4, B3) |
| T19 | `./scripts/fuzz-seed.sh`; replay: `cargo +nightly fuzz run wal_parse fuzz/corpus/wal_parse -- -runs=0 -max_len=70000` and `cargo +nightly fuzz run store_capture fuzz/corpus/store_capture -- -runs=0` | corpus 720K |
| T22 | `cargo test --features testing --test perf -- --ignored --nocapture` | batch 53.9 s (over 10 s budget), per-process 7.5 s |

New findings from Wave 3 (not fixed; no `src/` changes allowed):
- B9: `clone_at` checks `out.exists()` then `fs::write`; two racing clones to one path can both succeed (36/100). Fix: `OpenOptions::create_new(true)`. No task yet.
- B7 widened: `init_files` enables WAL before the schema, so `PRAGMA auto_vacuum=FULL` is also ignored, and `db::tests::clone_works_after_a_later_commit_shrinks_the_db` never shrinks. Fix with T15/Q7.
- INV-51: 2000-statement stdin batch is ~27 ms/statement in debug (WAL re-read per statement). Perf test kept red-when-run (`#[ignore]`); decide budget or fix.
- No library entry point for VACUUM (Q4); `tests/fuzz_seed.rs` was added by T19 beyond its owned list.
- `tests/cli.rs` conversions use `Driver::Cli` only; the scenario step language cannot express create-without-schema, env-only selection, schema from stdin, multi-db runs, mid-batch failure.

ts/common/mod.rs` (expect a trivial merge conflict there).
- No source (`src/`) changes in Wave 3. Report suspected bugs instead of fixing them.

Merge order: T04 first, then the rest, then T19. Then run the verify block above.

## Wave 4 decisions (made 2026-10-14, supersede plan section 5 defaults where they differ)

| Q | Decision | Task |
|---|---|---|
| Q1 | Support concurrent `exec`. One writer lock (`BEGIN IMMEDIATE` on `meta.db`) taken before the statement tx and held until capture ends; capture runs inside it; `busy_timeout` on both connections. | T16 |
| Q2 | Reject transaction control in `exec`: authorizer denying `SQLITE_TRANSACTION` (exec only, not `create`). Nothing committed, non-zero exit, error names transaction control. Backstop: capture on the error path, fail if one statement yields >1 commit. | T14 |
| Q7 | Run schema in rollback mode, check autocommit, switch to WAL, assert `wal`, checkpoint. Reject `PRAGMA journal_mode` via authorizer (code 19). Do NOT deny PRAGMA wholesale: `page_size`/`auto_vacuum` must work. `exec` asserts WAL at the top. | T15 |
| Q8 | Deny `ATTACH`/`DETACH` (codes 24/25) in `create` AND `exec`. T15 adds the authorizer helper and rusqlite `hooks` feature; T14 reuses it. | T15 |
| Q9 | `log` default shows the most recent N printed oldest to newest; `--head` gives the old behaviour, `--all` everything. Library `db::log(db, limit, from_end)`. | follow-up after T16 |
| B9 | `clone_at` opens output with `create_new(true)`, keeps the `exists()` fast fail, removes the file if the write fails; tighten S12 racers to "exactly one succeeds". | folded into T13 |
| INV-51 | Own task after T16: store byte offset + running checksum in the cursor, parse only the new tail; un-ignore the S20 perf test. | T24 |

Merge order on `src/db.rs`: T13 (+B9), T15, T14, T16, T24, then the `log` change. Because they share
`db.rs`, these run sequentially (each branches from the previous merge); T20 (CI files only) runs alongside T13.

## Wave 4 (after Wave 3): bug fixes, needs your decisions first

T13 (B1 quoting), T14 (B2), T15 (B5), T16 (B4, B3), T20 (CI). Decisions needed from the
plan's section 5: Q1 (support concurrent exec?), Q2 (capture-and-error on self-ending
transactions?), Q7 (apply schema before WAL, reject journal_mode changes?), Q8 (deny
ATTACH in schemas?), Q9 (`log --tail`?). Merge order on `src/db.rs`: T13, T15, T14, T16.
