# Wave status and how to run the next wave

Source of truth for tasks, invariants and ownership rules: `docs/TESTING_PLAN.md`
(section 4 task table, section 4.1 waves). This file records progress and the
launch recipe so a fresh session can continue.

## Done

| Wave | Tasks | Notes |
|---|---|---|
| W1 | T01 `wal::encode` behind `testing` feature, T02 harness core (`Model`, `Observed`, `Checks`, `assert_invariants`), T11 replay bound (B6 fixed) | merged, pushed |
| W2 | T03 `Scenario`/`Step`/`Driver`, T09 store capture tests, T10 WAL parser tests, T17 `wal_differential` fuzz target, T18 `replay_apply`/`store_capture`/`statements_split` fuzz targets | merged |
| extra | `docs/architecture/` C4 viewer built with c4-sdk (Vite + React ESM) | `cd docs/architecture && npm install && npm run dev` (Node 22.23.2) |

Known failing, deliberately `#[ignore]`d: INV-08 (WAL page size above 65536 accepted).
Open defects reported by the plan and not fixed yet: B1 (summarize quoting), B2 (statement
that ends its own transaction), B3 (orphan WAL commits mislabelled), B4 (concurrent exec),
B5 (schema changes journal_mode), B7/B8 (see plan).

## Verify the current state

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
cargo test --features testing
cargo +nightly fuzz build
```

## Wave 3 (next): all tasks parallel, one worktree each

| Task | Model | Owns | Goal (see plan section 4) |
|---|---|---|---|
| T04 | haiku | `tests/cli.rs` | convert the existing CLI scenarios to `Scenario`s with `assert_invariants`; no behaviour assertions lost |
| T05 | sonnet | `tests/scenarios_usage.rs` | scenarios S2, S3, S4, S5, S8, S16, S17 |
| T06 | sonnet | `tests/scenarios_ddl.rs` | S6 (quoted names, red on B1) and S7 (schema evolution) |
| T07 | sonnet | `tests/scenarios_schema.rs` | S18 schema-file matrix (B5 red until fixed; Q7/Q8 cases assert current behaviour with `// DECISION:`) |
| T08 | opus | `tests/library.rs` | S9 external VACUUM shrink, S11 clone chain, INV-23 cursor check |
| T12 | opus | `tests/scenarios_concurrency.rs`, `tests/common/helpers.rs` | S12-S15 concurrency; S13/S15 red on B4/B3; deterministic barriers, no sleeps |
| T19 | haiku | `fuzz/corpus/**`, `scripts/fuzz-seed.sh`, one helper in `tests/common/mod.rs` | seed corpus generator (`Harness::wal_snapshot`) and committed corpus; un-ignore `fuzz/corpus` in `fuzz/.gitignore` instead of `git add -f`; land after T04 |
| T22 | haiku | `tests/perf.rs`, `README.md` | `#[ignore]` perf smoke S20; README accuracy (clone memory is roughly the frames up to N) |

Rules for every Wave 3 agent:
- Use `isolation: "worktree"`; commit on the branch, do not push; reply with files, what
  was covered, and the exact command(s) to run it.
- Known-red scenarios merge as `#[ignore = "KNOWN FAILING: Bn, fixed by Tnn"]` so
  `cargo test` stays green; list them in plan section 1.6.
- New test files own themselves; each adds at most one `pub mod` line to
  `tests/common/mod.rs` (expect a trivial merge conflict there).
- No source (`src/`) changes in Wave 3. Report suspected bugs instead of fixing them.

Merge order: T04 first, then the rest, then T19. Then run the verify block above.

## Wave 4 (after Wave 3): bug fixes, needs your decisions first

T13 (B1 quoting), T14 (B2), T15 (B5), T16 (B4, B3), T20 (CI). Decisions needed from the
plan's section 5: Q1 (support concurrent exec?), Q2 (capture-and-error on self-ending
transactions?), Q7 (apply schema before WAL, reject journal_mode changes?), Q8 (deny
ATTACH in schemas?), Q9 (`log --tail`?). Merge order on `src/db.rs`: T13, T15, T14, T16.
