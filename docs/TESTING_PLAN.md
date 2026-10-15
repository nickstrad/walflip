# walflip testing plan

**T23 final review (2026-10-14, at `cb23796`).** The status cells in sections 1, 3.2 and 4 were
reconciled against the tests that exist. Each `ok` names the test that proves it. Measured results:

| Check | Result |
|---|---|
| `cargo test --features testing` | 130 passed, 0 failed, 3 ignored (unit 60, cli 16, library 4, perf 1, roundtrip 3, concurrency 6, core 3, ddl 8, failure 10, schema 12, usage 7) |
| `cargo test --features testing --no-fail-fast -- --ignored` | `wal::tests::inv08_page_size_above_65536_is_rejected` **fails** (INV-08, open). `perf::s20_2000_per_process_execs` passes (5.5 s, 2.74 ms/exec). `fuzz_seed::generate_fuzz_seeds` passes (set `WALFLIP_SEED_OUT` or it writes into `fuzz/corpus`). |
| `--test scenarios_concurrency` x 20 | 20/20 green (6 tests each, about 0.3 s per run) |
| `cargo +nightly fuzz build` | green, 5 targets |
| corpus replay `-runs=0` | all 5 targets green (`wal_parse` with `-max_len=70000`); committed corpus is 603 KB |
| T17 planted bug (salt check removed, scratch copy) | `wal_differential` panics at its "altered data" assertion after about 1,100 execs (under 16 s including the build) |
| clean fuzz runs | `wal_differential` 300 s (3.09M runs) and `replay_apply` 120 s (7.70M runs, `-rss_limit_mb=1024`): no crashes, no artifacts |
| `cargo clippy --features testing --all-targets` | 0 errors, 5 warnings (lib 1, lib tests 2, `scenarios_failure` 2) |
| `actionlint` | not available on this machine. Both workflows parse as YAML (Ruby); not linted. |
| `cargo build` without `testing` | the debug and release `walflip` binaries contain no `wal::encode` symbols |

New defect found during the review: **B10** (INV-63, section 1.4).

The rest of this header is the original snapshot from when the plan was written. It is kept for history.

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
| B1 | `summarize` does not double `"` in table names: `exec 'CREATE TABLE "a""b"(x)'` then `clone` writes the file, then exits 1 (`unrecognized token`). **Fixed (T13).** | medium |
| B3 | Commits already in `live.db-wal` when `exec` starts (crashed writer, external writer) are captured under the label of the next statement, and `exec` reports more commits than statements. **Fixed (T16):** captured first as `(recovered)` with a stderr warning; `status` shows `uncaptured: N`. | medium |
| B4 | Concurrent `exec` on one db (6 procs x 15 inserts): 11 failures `UNIQUE constraint failed: commits.idx`, 120 commits recorded for 90 rows, duplicated labels. The cursor is read outside the `meta.db` transaction. **Fixed (T16):** per-statement history lock (`BEGIN IMMEDIATE` on `meta.db`), capture inside it. | high |
| B6 | `replay::apply` with a huge `pgno` or `db_pages` calls `Vec::resize` to terabytes: abort, not `Err`. Reachable from a crafted `meta.db`/WAL; the fuzzer will hit it immediately. **Fixed (T11).** | medium |
| B10 | (found by T23) `summarize` builds `file:{path}?immutable=1` without escaping. `clone -o` to a path with `?`, `#` or `%` writes the file, then exits 1 (`unable to open database file`). With `%XX`, it could report on a different existing file. **Fixed** (T25): `summarize` percent-encodes the path; tests `summarize_and_clone_handle_uri_special_paths` and `clone_to_uri_special_paths_reports_the_written_file`. | low |

Also verified: `clone -n total` is **byte-identical** to `live.db` once the WAL is checkpointed, including after an external `VACUUM` shrank the db (16 -> 9 pages). This is the strongest oracle available and the harness is built around it.

---

## 1. Invariant catalog

Legend. Status: `ok` covered today, `gap` not tested, `FAIL` confirmed failing (B-number), `sus` suspected failing. Technique: U unit, P proptest, F fuzz, E e2e (harness), L library-level integration (calls `walflip::db`/`store` directly, no process).

### 1.1 WAL parser (`wal::parse`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-01 | `parse(encode(c))` returns `c` for both byte orders and any salt. | existing proptest | P | ok: `wal::tests::roundtrips_in_both_byte_orders`, `page_sizes_512_and_65536_roundtrip` |
| INV-02 | Any prefix of a valid WAL parses to a prefix of its commits; a full buffer gives all of them. | existing proptest | P | ok: `truncation_keeps_a_prefix_of_commits`, `trailing_partial_frame_is_ignored`, `frames_after_last_commit_marker_are_dropped` |
| INV-03 | Flipping any byte after the header never admits altered data; history is cut at or before the damaged frame. | extend existing proptest to BE and to header bytes (header corruption must yield `None` or `Err`, never a commit). | P/F | ok: `corruption_cuts_history_at_the_damaged_frame{,_big_endian}`; header bytes by `inv09_*` and `inv07_*`; `wal_differential` FlipByte |
| INV-04 | `parse` never panics, never allocates proportional to a header field, for arbitrary bytes. | `wal_parse` fuzz target + proptest | F/P | ok: `arbitrary_bytes_never_panic`, `valid_header_with_garbage_tail_never_panics`, `wal_parse` |
| INV-05 | Every accepted commit has `db_pages != 0`, >=1 frame, frames exactly `page_size` long, total frames fit in the buffer. | `wal_parse` target asserts | F | ok: `fuzz_targets/wal_parse.rs` |
| INV-06 | Frames whose salt differs from the header (stale generation after a WAL restart) end parsing; nothing after them is admitted. | encode gen A, overwrite head with gen B (fewer frames, new salt) -> parse == gen B only. | U/P | ok: `inv06_stale_generation_tail_is_dropped`, `inv06_salt_mismatch_mid_file_stops_at_that_frame`, `inv06_valid_checksum_with_foreign_salt_is_rejected`; `wal_differential` AppendStaleGeneration (T23: a planted removal of the salt check is caught in about 1,100 execs) |
| INV-07 | A WAL whose frames are chained to a checksum that does not start from the header cksum is rejected at frame 1. | encode, then patch header cksum fields -> `None`. | U | ok: `inv07_chain_not_starting_at_header_checksum_is_rejected_at_frame_one`, `checksum_chain_continues_across_commits` |
| INV-08 | `page_size` accepted iff `512 <= ps <= 65536` and power of two. | unit | U | ok: `wal::tests::inv08_page_size_bounds` and `wal::tests::inv08_page_size_above_65536_is_rejected` (the parser now rejects `ps > 65536`; the test is no longer `#[ignore]`d). |
| INV-09 | Header checksum mismatch -> `Ok(None)` (file being created), bad magic -> `Err`. | unit documenting the contract | U | ok: `inv09_header_checksum_mismatch_is_none_and_bad_magic_is_err` |
| INV-10 | Sector-padding frames (SQLite repeats the commit frame to fill a sector when `POWERSAFE_OVERWRITE` is off) become extra commits by policy (Q6: left as is). | real-SQLite test: foreign `psow=0` writer pads commit frames; assert extra commits are counted and labelled `(recovered)`, replay exact. | U/F | ok: pinned by `library::inv10_sector_padding_frames_are_counted_as_recovered_commits` (real SQLite, foreign `psow=0` writer: 3 inserts -> 6 `(recovered)` commits, `clone(total)` == live, integrity ok). Policy (Q6) = leave: padding frames count as commits; no merge. The `wal_differential` `DuplicateFrame` mutation copies a frame with its old checksum, so the copy breaks the chain. SQLite writes padding frames with a fresh, valid chained checksum, so that case is not tested. With the default `POWERSAFE_OVERWRITE=1`, SQLite does not pad. |

### 1.2 Replay (`replay::apply`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-11 | Last writer wins per page; untouched pages keep base; new pages zero. | existing proptest | P | ok: `replay::tests::matches_page_map_model`, `replay_apply` |
| INV-12 | `apply(all)` == `apply(prefix); apply(rest)` (prefix soundness of `-n`). | existing | P | ok: `replay_composes` |
| INV-13 | Idempotent. | existing | P | ok: `is_idempotent` |
| INV-14 | `pgno == 0` or wrong page length -> `Err`, image untouched. | existing (add: image unchanged on `Err`) | U | ok: `rejects_bad_frames`; image unchanged on `Err` in `huge_pgno_is_rejected_without_allocating` and `replay_apply` |
| INV-15 | Bounded allocation: when `db_pages` is `Some(n)`, any `pgno > n` is `Err`; `n * page_size` above a cap (e.g. 1 << 40 bytes or a caller-supplied max) is `Err`. Never aborts. | unit with `pgno = u32::MAX`; fuzz target `replay_apply` | U/F | ok (B6 fixed in T11): `huge_pgno_is_rejected_without_allocating` (< 50 ms, capacity unchanged), `bound_is_enforced`, `replay_apply` (clean 2-minute run, see T18) |
| INV-16 | Output length == `db_pages * page_size` when `Some`; == `max(base, highest written page)` when `None`. | proptest | P | ok: `matches_page_map_model`; `replay_apply` asserts the length rule |

### 1.3 Store (`store::Store`, `commits_seen`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-17 | Cursor: same salt -> stored count, different salt or junk -> 0. The resume state (`cursor_resume`, T24) is used only while it agrees with `cursor_salt`/`cursor_commits` and fits the WAL (same 32-byte header, file reaches the offset, last commit frame carries the stored checksum); otherwise capture parses the whole WAL with the count cursor. A `meta.db` without it (pre-T24) migrates on the next capture. | existing + `store::tests::old_cursor_format_is_migrated`, `stale_or_damaged_resume_state_*`, `rewritten_wal_with_the_same_salt_*` | U/P | ok |
| INV-18 | `capture` on the same WAL twice adds 0 the second time. | roundtrip | L | ok: `roundtrip::capture_is_idempotent`, `store::tests::capture_is_idempotent`, `store_capture` |
| INV-19 | WAL grows from k to k+j commits -> `capture` adds exactly j, idx continues. | write `encode(...)` output to a temp WAL file, capture, extend file, capture again. | U | ok: `store::tests::inv19_growth_adds_only_new_commits`, `capture_reads_only_the_tail_after_the_stored_state` |
| INV-20 | WAL restart (new salt, fewer frames than before) -> all commits of the new generation are added, none duplicated, cursor updated. | as INV-19 with two generations | U | ok: `inv20_new_salt_restarts_cursor_keeps_history`; `library::inv23_*` across a real restart |
| INV-21 | WAL page size != stored page size -> `Err`, no rows inserted, cursor unchanged. | encode with PS=512 against a store whose `page_size` is 4096 | U | ok: `inv21_page_size_mismatch_rejected_without_side_effects` |
| INV-22 | `commits.idx` is contiguous `1..=count`; every `frames.commit_idx` exists; per commit `seq` is contiguous from 0; `frames` count == `status.frames`. | SQL check in `assert_invariants` | E | ok: `assert_invariants` (`meta_structure`, on in every scenario); negative tests `cli::invariants_catch_a_deleted_frame`, `invariants_catch_a_flipped_page_byte`; `store_capture` |
| INV-23 | `cursor_commits` == number of commits in the current WAL generation at last capture; `cursor_salt` == header salt. | library check after each `exec` while WAL still exists (hold a reader to prevent the close-checkpoint) | L | ok: `library::inv23_cursor_matches_wal_while_reader_pins_it`, S14 |
| INV-24 | Capture is atomic under concurrent writers: N processes doing M statements each produce exactly N*M commits, no `UNIQUE` errors, labels match statements. | e2e concurrency scenario | E | ok: S13 (T16; 20/20 soak), `db::tests::exec_fails_cleanly_*`, S15b (lock released on process death) |
| INV-25 | Commits present in the WAL before `exec` runs its first statement are either captured with a distinct label (e.g. `(uncaptured)`) or `exec` refuses; they are never attributed to the new statement. | e2e: kill a writer with `os._exit`, then `exec` | E | ok: S15 (T16: label `(recovered)`, warning, `status` `uncaptured: N`), `db::tests::orphans_are_recovered_under_their_own_label` |
| INV-26 | A failed `capture` (meta tx error) leaves `commits`/`frames`/`meta` exactly as before. | unit: failing trigger, page-size mismatch and SQLITE_BUSY, standalone and inside the write lock (savepoint) | U | ok: `store::tests::inv26_*` |

### 1.4 Database operations (`db::*`)

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-27 | `status.commits == log(limit=inf).len() == sum of exec return values` (CLI: sum of `captured N`). | harness tracks the sum | E/L | ok: `assert_invariants` checks log length and numbering against `status.commits`, and `status.commits` against `Model::exec_sum` |
| INV-28 | `status.frames == sum(log[i].frames)`. | harness | E | ok: `assert_invariants` |
| INV-29 | `clone -n k` passes `integrity_check` for every `k in 0..=total`. | harness after every step (cheap for small dbs) | E/L | ok: `assert_invariants` after every step, every k under `Checks::full`; {0, mid, last} under `Checks::light` (S8, S12, S13, S19) |
| INV-30 | `clone -n k` table contents == model after the k-th capturing statement, for every table. | model oracle: a shadow `rusqlite` in-memory db fed the same statements, dumped per table | E/L | ok: shadow model (`tests/common/model.rs`) in every `Scenario` (kv, DDL, blobs, schema evolution); `roundtrip::every_commit_is_reproducible` |
| INV-31 | `clone -n total` is byte-identical to `live.db` when `live.db-wal` is absent. | `fs::read` compare | E/L | ok: `assert_invariants` (`live_oracle`); explicit in S9b, S11, S14 |
| INV-32 | Clone chain: `apply(frames(n+1..=m), db_pages(m))` onto `clone(n)` == `clone(m)` bytes, for all n < m. | library test reading frames from `meta.db` | L | ok: `library::s11_clone_chain` (all pairs, history includes a shrink) |
| INV-33 | `len(clone(k)) == db_pages(k) * page_size` for k >= 1; `clone(0) == base.db` bytes. | harness | E | ok: `assert_invariants` |
| INV-34 | `clone` never mutates `live.db`, `base.db`, `meta.db` (hash before/after) and never creates `-wal`/`-shm` beside the output. | harness | E | ok: `assert_invariants` (`hashes` + sidecar check); S12 |
| INV-35 | `clone` to an existing path refuses and leaves that file byte-identical; `-n > total` refuses and creates nothing. | existing | E | ok: `cli::errors_are_clear_and_state_is_preserved`, `db::tests::clone_refuses_existing_output_and_keeps_it`; S12 race (B9 fixed in T13) |
| INV-36 | `clone` into an unwritable directory -> non-zero exit, no partial file, db state unchanged. | chmod 0o555 dir (skip when root) | E | ok: `scenarios_usage::s17_read_only_output_dir` (skips as root) |
| INV-37 | A failing statement changes nothing: live rows, `status`, WAL commit count, `meta.db` hash all unchanged. | harness | E | ok: S10 (`tests/scenarios_failure.rs`); transaction control in `exec` is rejected by the authorizer (Q2, T14) |
| INV-38 | One `exec` statement produces at most one commit; a statement that writes nothing produces zero. | harness: `captured N` with N <= 1 per statement | E | ok (T14: `exec` errors after capturing when one statement yields >1 commit, covered by `db::tests::backstop_more_than_one_commit_is_captured_then_reported`; T16: orphan WAL commits are captured as `(recovered)` before the statement, so they no longer count against it) |
| INV-39 | Rows committed to `live.db` by `exec` are always represented in history: after any `exec` (success or error), `clone -n total` == `live.db` contents. | harness `assert_invariants` after failed steps too | E | ok: S10; backstop captures on the error path whenever the failed statement changed the WAL |
| INV-40 | DDL via `exec` (`CREATE TABLE/INDEX/TRIGGER`, `ALTER TABLE ADD/RENAME COLUMN`, `DROP TABLE`) is captured and visible in `clone` at that commit; `sqlite_master` of clone(k) == model. | schema-evolution scenario | E | ok: S6, S7 (`sqlite_master` of every clone compared with the shadow model) |
| INV-41 | Large payloads: a 300 KB blob spans ~75 overflow pages, all captured; blob hash equal in clone. | large-payload scenario | E | ok: `scenarios_usage::s5_large_payload` (frames > 70 and > 140; blob bytes compared per clone through the model) |
| INV-42 | `db_pages` may shrink (VACUUM, auto_vacuum). After a shrinking commit s, `len(clone(s)) < len(clone(s-1))`, integrity ok, later growth replays correctly. | library scenario: `exec VACUUM` (Q4), and open `live.db` with `wal_autocheckpoint=0`, run `VACUUM`, call `Store::capture`, then clone | L | ok: `library::s9a_exec_vacuum_is_one_shrinking_commit`, `library::s9b_external_vacuum_shrink_is_captured_and_replayed`, `db::tests::clone_works_after_a_later_commit_shrinks_the_db` |
| INV-43 | `rename` preserves history byte-for-byte (`meta.db` hash) and the old name disappears; `drop` removes exactly that folder; other dbs' hashes unchanged. | isolation scenario | E | ok: `scenarios_usage::s16_rename_and_drop_mid_life`, `Step::Rename`/`Step::Drop`, `cli::multiple_databases_are_isolated` |
| INV-44 | Per-process `exec` and one stdin batch produce the same `log` (frames, db_pages, labels) and byte-identical `clone -n k` for all k. | upgrade scenario 6 from "same rows" to "same bytes" | E | ok: `s2_stdin_batch_equals_per_process_bytes`; CLI == library in S1, S6, S7, S10 (`assert_same_outcome`) |
| INV-45 | `--root/--db` flags, `WALFLIP_ROOT/WALFLIP_DB`, and mixed (flag overrides env) select the same database. | env scenario | E | ok: `s3_env_vs_flags_vs_mixed`, `cli::environment_selects_root_and_database` |
| INV-46 | `list` is sorted by name, counts equal each db's `status.commits`, ignores folders without `live.db` or without `meta.db` (a bare `live.db` is skipped, not listed with 0 commits), and does not create files in them. | many-dbs scenario; check no `meta.db` appears in a decoy folder | E | ok: `s4_many_small_dbs` (decoy folder, stray file, bare `live.db` folder); `s19_read_only_root_read_commands_work`; `s20_leftover_meta_journal_does_not_break_reads`; `assert_invariants` (`list`) |
| INV-47 | `summarize`/clone report works for any table name SQLite accepts (quotes, spaces, unicode, keywords). | DDL scenario with `CREATE TABLE "a""b"`, `"with space"`, `"select"` | E | ok (B1 fixed in T13): `s6_ddl_quoted_names_{cli,lib}`, `db::tests::summarize_counts_tables_with_doubled_quotes_in_names`. Unicode names are not tested. |
| INV-48 | Concurrent readers on `live.db` (a held read transaction) during `exec`: exec succeeds, capture complete, no duplicates on the next `exec` even though the WAL was not checkpointed. | spawn `python3 -I`/rusqlite reader holding `BEGIN; SELECT` | E | ok: S14, `library::inv23_*` |
| INV-49 | Concurrent `clone -n k` of the same db (same and different k) all succeed and are byte-identical per k. | spawn 8 processes | E | ok: `scenarios_concurrency::s12_concurrent_clones` (20/20 soak) |
| INV-50 | `exec -` splits on lines: a multi-line statement fails with `incomplete input`, nothing committed, earlier lines kept. Documented behaviour. `exec --script` splits with `sqlite3_complete`: a multi-line statement is one statement and one commit; a tail without `;` is `incomplete input` and nothing from the script runs; a failing statement stops the script, earlier ones kept. `log` shows a multi-line label on one row (line breaks as `\n`), stored label unchanged. | e2e + unit + fuzz | E | ok: S10 (`s10_multi_line_statement_fails_with_incomplete_input`, `s10_script_mode_runs_multi_line_ddl_batch`, `s10_script_unterminated_tail_runs_nothing`, `s10_script_failing_statement_stops_batch`, `log_shows_multi_line_labels_on_one_row`); `db::tests::script_*`, `log_labels_are_one_line_and_cut`; fuzz `statements_split` covers both splitters |
| INV-51 | `exec` with 2000 single-row inserts in one batch finishes in bounded time. Budget: < 10 s debug. | timed scenario S20 (batch runs by default; per-process run `#[ignore]`); `wal` proptests `resumed_parse_equals_full_parse_at_any_split`, `chained_resumes_*`, `resume_matches_full_parse_on_arbitrary_continuations`, `state_that_does_not_fit_is_refused`; `store` proptest `incremental_capture_equals_full_parse_capture` + fallback/migration units; `wal_differential` and `store_capture` fuzz oracles for resumed parsing | E/P/U/F | ok (T24: cursor stores a resume state (`cursor_resume`: header, offset, running checksum, commit count), capture parses only the new tail; batch 52.1 s -> 0.94 s debug) |
| INV-63 | (added by T23) `clone -o <path>` works for any path the OS accepts, including `?`, `#`, `%`. The post-clone `summarize` opens exactly that file. | e2e: clone to `a?b.db`, `c#d.db`, `e%41.db` | E | **PASS** (B10 fixed): path is percent-encoded before the immutable `file:` URI. Unit test `db::tests::summarize_and_clone_handle_uri_special_paths` and e2e `cli::clone_to_uri_special_paths_reports_the_written_file` (`?`, `#`, `%41`, space, unicode; decoy file untouched). |

### 1.5 Create / schema file

| ID | Invariant | Check | Tech | Status |
|---|---|---|---|---|
| INV-52 | Multi-statement schema (tables, indexes, triggers, views, seed rows) is applied in full; `clone -n 0` shows all of it; `status.commits == 0`. | existing scenario 9 + add trigger/view | E | ok: `cli::schema_file_with_tables_indexes_and_seed_rows`, `s18_multi_statement_with_seed_rows`. The schema-file tests have no trigger or view; `exec` DDL covers those in S6. |
| INV-53 | Seed rows are in `clone -n 0` and `base.db`; they never appear in `log`. | existing | E | ok: `s18_multi_statement_with_seed_rows`, `s18_seeded_scenario_both_drivers` |
| INV-54 | Schema with a syntax error, a constraint violation in its seed rows, or a missing file: non-zero exit, message names the stage (`reading schema` / `applying schema`), no folder left, `list` unchanged. | existing + add constraint-violation case | E | ok: `s18_syntax_error_leaves_nothing`, `s18_constraint_violation_in_seed_leaves_nothing`, `s18_unterminated_begin` (`applying schema`), `cli::errors_are_clear_and_state_is_preserved` (`reading schema`) |
| INV-55 | `--schema -` reads stdin; identical result to the file form (byte-identical `base.db`). | existing + byte compare | E | ok: `s18_stdin_and_file_give_identical_base` |
| INV-56 | Empty schema file == no `--schema`: `base.db` identical, `page size 4096`. | e2e | E | partial: `s18_empty_file` checks that create succeeds, `base == live`, and the first `exec` is captured. The byte comparison against a no-`--schema` create and the 4096 page size are not asserted. |
| INV-57 | Schema containing `BEGIN; ... COMMIT;` works; an unterminated `BEGIN` fails cleanly with an `applying schema` message and no folder. | e2e | E | ok: S18 (`s18_unterminated_begin`, `s18_begin_commit_in_schema`) |
| INV-58 | A schema that changes `journal_mode` is rejected (or WAL is re-asserted) so that `live.db` is in WAL mode after `create`; `exec` refuses to run against a non-WAL `live.db`. | e2e with `PRAGMA journal_mode=DELETE` | E | partial: create side ok (`s18_journal_mode_delete_is_rejected`, `s18_exec_attach_and_journal_mode_are_denied`, T15). **Gap:** `exec` against a `live.db` that another program switched out of WAL mode (`db::assert_wal`) has no test. |
| INV-59 | `PRAGMA page_size=N` in a schema either takes effect (reported page size N, `meta.page_size == N`, clones valid) or is rejected with a message; never silently ignored. Decision needed (Q7). | e2e | E | ok: S18 `s18_page_size_pragma` expects 8192 (T15) |
| INV-60 | `base.db` has no `-wal`/`-shm`, `PRAGMA journal_mode` of a copy of `base.db` is `wal`, and `base.db` is byte-identical to `live.db` right after `create`. | e2e | E | partial: `base == live` after every successful S18 create (`assert_base_is_live`). The missing `base.db-wal`/`-shm` and the `wal` journal mode of a copy are not asserted. |
| INV-61 | A schema containing `ATTACH` can write outside the root. Decide: allow (document) or deny (`SQLITE_DBCONFIG_DEFENSIVE` / authorizer denying `SQLITE_ATTACH`). Until decided, the fuzzer must never execute schema text. | e2e once decided | E | ok: denied in create and exec (T15, Q8): `s18_attach_in_schema`, `s18_exec_attach_and_journal_mode_are_denied`. Fuzzed by `exec_sql` (T21): ATTACH to an absolute and a relative canary path, `VACUUM INTO`, and ATTACH in token soup are always refused, and no file appears outside the case directory. |
| INV-62 | Database names: accepted iff `[A-Za-z0-9_-]{1,64}`; path-like names rejected; `create` with an invalid name creates nothing. | existing `validate_name` tests + e2e | U/E | ok: `db::tests::rejects_path_like_names`, `accepted_names_are_safe_folder_names`, `cli::errors_are_clear_and_state_is_preserved` (`bad/name`) |

### 1.6 Open items (after T23)

Defects B1-B9 are fixed, and each fix has the regression test named in its catalog row. No
scenario is known-red. The only `#[ignore]`d test that fails is INV-08. These items are open:

| Item | State | Next step |
|---|---|---|
| INV-08 | `wal::parse` accepts page sizes 2^17..2^31. This is harmless (no frame fits), but `inv08_page_size_above_65536_is_rejected` is `#[ignore]` and red. | Add the upper bound to `wal.rs` and un-ignore the test, or document the behaviour and delete the test. |
| INV-63 / B10 | `clone -o` to a path containing `?`, `#` or `%` writes the file, then exits 1. `db::summarize` puts the raw path in a `file:...?immutable=1` URI. | Percent-encode the path, or open with `SQLITE_OPEN_READ_ONLY` and no URI. Add an e2e test. **Done.** |
| INV-10 | Sector-padding duplicate commit frames: decided (leave) and pinned with a real-SQLite test. | None. |
| INV-58 (exec side) | The `exec` refusal on a non-WAL `live.db` (`db::assert_wal`) has no test. | Add an e2e: switch `live.db` to DELETE mode externally, then `exec` must fail and change nothing. |
| INV-56, INV-60 | Partial; see the rows. | Add the missing asserts to `scenarios_schema.rs`. |

Ignored tests that pass and are ignored on purpose: `perf::s20_2000_per_process_execs` (slow
smoke test) and `fuzz_seed::generate_fuzz_seeds` (corpus generator).

---

## 2. Fuzzing plan (cargo-fuzz, `fuzz/`)

### 2.1 Targets

| Target | Input | Oracle / assertions | Notes |
|---|---|---|---|
| `wal_parse` (exists) | raw bytes | no panic; INV-05 self-consistency | keep as is; add `-max_len=70000` so frames with 4096/65536 pages fit |
| `wal_differential` (new) | `arbitrary` struct: `{ big_endian, salt, page_size in {512,1024,4096}, commits: Vec<{frames: Vec<(pgno, seed)>, db_pages}>, mutations: Vec<Mutation> }` where `Mutation in { Truncate(usize), FlipByte(pos, mask), AppendStaleGeneration(..), DuplicateLastFrame(k), PatchHeaderField(idx, val) }` | encode with the reference encoder (moved from `wal.rs` tests into `src/wal.rs` under `#[cfg(any(test, feature = "testing"))]` as `pub mod encode`), apply mutations, parse. Assert: unmutated -> equal; `Truncate` -> prefix; `FlipByte` -> prefix of commits strictly before the damaged frame; `AppendStaleGeneration` -> unchanged; `DuplicateLastFrame` -> each copy is a commit (Q6 = leave); `PatchHeaderField(page_size)` -> rejected unless valid. | this is where INV-06/07/10 live permanently |
| `replay_apply` (new) | `arbitrary`: `{ base_pages: u8, page_size in {64,512}, frames: Vec<(u32, [u8; N])>, db_pages: Option<u32> }` | no abort (run with `-rss_limit_mb=512`); result length rule INV-16; page-map model INV-11; `Err` leaves image untouched | will find B6 on the first run; add the fix first or expect red |
| `store_capture` (new) | `arbitrary` list of WAL generations + a mutation per generation | in a `tempdir`: write WAL file, `Store::capture`, assert INV-19/20/21/22 against a model (`Vec<Vec<Commit>>`); also random interleaving of "truncate file to 0" and "rewrite header" | slower (SQLite I/O); run with `-runs` budget, not time |
| `statements_split` (new, tiny) | raw bytes as UTF-8 | `parse_statements` never panics; output lines are trimmed, non-empty, and `join("\n")` re-splits to the same vector (idempotence) | 1 minute of CI time is enough |
| `exec_sql` (T21) | `arbitrary` program: schema of 1-3 tables from a fixed grammar (quoted names, WITHOUT ROWID, page_size, auto_vacuum, seed rows, index), then up to 8 `exec` calls of up to 4 statements, each 1-3 pieces joined by `;`: INSERT/UPDATE/DELETE (blobs 0..70000 B and one over the length limit), ALTER ADD/DROP/RENAME, CREATE/DROP TABLE/INDEX/TRIGGER/VIEW, SAVEPOINT, bare VACUUM forms, allowed pragmas (`wal_checkpoint`, `wal_autocheckpoint`, `incremental_vacuum`, ...), must-reject (ATTACH/DETACH/BEGIN/COMMIT/ROLLBACK/END/`journal_mode`/VACUUM INTO/denied pragmas and functions), must-fail, and token soup | per input a fresh dir, real `db::create` + `db::exec_with`. After every exec: WAL gone, `live.db` integrity ok, `uncaptured == 0`, INV-22 on `meta.db`, no `(recovered)`, history grew by exactly the reported count and at most one commit per statement run, no commit => `live.db` byte-identical, must-reject/must-fail calls error, `clone(total)` == `live.db` bytes, clones at 0/1/total-1 (every k at the end) pass `integrity_check`, no file outside the case dir | `-timeout=30`; ~200 exec/s. `EXEC_SQL_TRACE=1` prints each call |

**Executing fuzzer-controlled SQL** is limited to one target, `exec_sql` (T21, below). It never runs raw schema text: the schema comes from a fixed grammar and only `exec` statements contain fuzzer-chosen pieces. It keeps walflip's production denials (ATTACH/DETACH, `journal_mode`, transaction control) and adds, through the `testing`-only `db::sandbox` hook (one authorizer per connection, so the extra deny rule is consulted by walflip's own authorizer): denial of the `temp_store_directory`, `data_store_directory`, `mmap_size`, `cache_size`, `max_page_count`, `locking_mode`, `writable_schema`, `schema_version`, `hard_heap_limit`, `soft_heap_limit` pragmas and the `fts3_tokenizer`/`load_extension` functions; `SQLITE_LIMIT_LENGTH` 1 MB, `SQL_LENGTH` 64 KB, `EXPR_DEPTH` 100, `COMPOUND_SELECT` 10, `TRIGGER_DEPTH` 10, `ATTACHED` 0; `DBCONFIG_DEFENSIVE`; and a deterministic progress-handler budget (1000 x 1000 VM ops per `create`/`exec` call) instead of a wall-clock interrupt. SQLite temp files go to a sandbox dir (`SQLITE_TMPDIR`). Raw schema text (`create --schema` with fuzzer bytes) stays excluded.

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

| # | Scenario | Steps | Model / oracle | Key invariants | Status (T23) |
|---|---|---|---|---|---|
| S1 | Library API vs CLI parity | same 20-statement script via `LibExec` and via `CliExec` into two dbs | compare `Observed` and all `clone(k)` bytes between the two | INV-44, INV-27 | done: `scenarios_core::s1_lifecycle_{cli,lib}`, `s1_cli_and_lib_agree` (12 steps with 8 statements, rather than 20 statements) |
| S2 | stdin batch vs per-process (bytes) | existing scenario 6 extended: compare `log` rows and `clone(k)` bytes for all k | byte compare | INV-44 | done: `scenarios_usage::s2_stdin_batch_equals_per_process_bytes` (plus the library batch) |
| S3 | env vs flags vs mixed | create with flags; exec with env only; status with env root + flag db; wrong env + right flag wins | same `Observed` from all three | INV-45 | done: `s3_env_vs_flags_vs_mixed` |
| S4 | many small dbs | 12 dbs, 1-3 statements each, a decoy folder without `live.db`, a file in root | `list` sorted, counts == each `status`, decoy untouched (no `meta.db` created) | INV-46, INV-43 | done: `s4_many_small_dbs` |
| S5 | large payload | 300 KB blob insert, update to 600 KB, delete, reinsert 1 KB | blob sha256 per clone; frames per commit > 70 | INV-41, INV-33 | done: `s5_large_payload` (blob bytes are compared through the model; no sha256) |
| S6 | DDL-heavy | 15 steps: `CREATE TABLE`, `CREATE INDEX`, `CREATE TRIGGER`, `CREATE VIEW`, `DROP TABLE`, `CREATE TABLE "a""b"`, `CREATE TABLE "with space"`, `CREATE TABLE "select"` | `sqlite_master` of clone(k) == shadow | INV-40, INV-47 | done: `scenarios_ddl::s6_ddl_plain_*`, `s6_ddl_quoted_names_*` (B1 fixed in T13) |
| S7 | schema evolution | `ALTER TABLE ADD COLUMN` with default, `ALTER TABLE RENAME COLUMN`, `ALTER TABLE RENAME TO`, inserts between | shadow | INV-40, INV-30 | done: `s7_schema_evolution_*`, `s7_cli_and_lib_agree` |
| S8 | delete-everything-then-reinsert | 200 inserts (batch), `DELETE FROM t`, 200 inserts again, `DELETE WHERE id%2=0` | shadow; `db_pages` non-decreasing without auto_vacuum | INV-30, INV-33 | done: `s8_delete_everything_then_reinsert` (`Checks::light`) |
| S9 | VACUUM / shrink | (a) `exec VACUUM` (Q4) -> one commit labelled with the statement text, `db_pages` shrinks; also `vacuum;`, `VACUUM main`, `VACUUM "main"`, and VACUUM on its own line of an `exec -` batch between inserts; `VACUUM INTO` (4 spellings) denied, target file never created, state unchanged; `VACUUM; DROP TABLE t` (one line = one statement) rejected whole; (b) library step `ExternalVacuum` (open live with `wal_autocheckpoint=0`, `VACUUM`, `Store::capture(wal, "VACUUM")`) then more inserts | `len(clone(s)) < len(clone(s-1))`; final clone == live bytes | INV-42, INV-37 | done: `library::s9a_exec_vacuum_is_one_shrinking_commit`, `s9a_vacuum_into_is_denied_and_creates_no_file`, `s9a_vacuum_in_a_stdin_batch`, `s9b_external_vacuum_shrink_is_captured_and_replayed`; unit: `db::tests::vacuum_kind_matches_strictly`, `vacuum_mode_allows_only_the_temp_attach`, `vacuum_backstop_captures_a_foreign_commit_and_errors`. |
| S10 | interrupted / failed batch | stdin batch with a bad line in the middle; a statement with `; COMMIT;` inside; a `BEGIN` without `COMMIT`; a multi-line statement (line mode and `exec --script`) | shadow fed only successful statements; after each failure `clone(total) == live` | INV-37, INV-39, INV-50 | done: `scenarios_failure::s10_*` (11 tests, 3 of them `exec --script`), `failed_batch_reports_commits_captured_before_it`, `log_shows_multi_line_labels_on_one_row` |
| S11 | clone chain | 30 commits; for all n<m sampled pairs, `apply(frames n+1..=m)` on clone(n) == clone(m); clone(total) == live | byte compare | INV-32, INV-31 | done: `library::s11_clone_chain` (all pairs) |
| S12 | concurrent clones | 8 processes clone the same db, half `-n 10` half `-n 20`, to distinct paths; 2 processes race for the same path | all `-n 10` identical; exactly one of the racers succeeds or both fail, file (if any) is a valid clone | INV-49, INV-35 | done: `scenarios_concurrency::s12_concurrent_clones` (exactly one racer wins; B9 fixed in T13) |
| S13 | concurrent exec | 4 processes x 20 inserts on one db | 80 commits, labels == statements (B4 fixed in T16) | INV-24 | done: `s13_concurrent_exec` (20/20 green in T23) |
| S14 | concurrent reader | hold `BEGIN; SELECT` on live.db from a helper process while running 10 `exec`; release; 10 more | no dup commits; `status.wal_bytes` present during, cleared after a final exec | INV-48, INV-23 | done: `s14_concurrent_reader` |
| S15 | writer dies mid-life | helper process inserts with `wal_autocheckpoint=0` and `os._exit(0)`; then `status`, `clone`, `exec` | `status` shows WAL bytes; `status` shows `uncaptured: 3`; `exec` warns and captures the orphans under `(recovered)` before its statement (B3 fixed in T16) | INV-25 | done: `s15_writer_dies_mid_life`, `s15b_lock_holder_dies`, `scenarios_failure::orphan_commit_is_recovered_before_the_statement` |
| S16 | rename/drop mid-life | two dbs; rename A while B keeps writing; drop B; A's history byte-identical to before rename | `meta.db` hash of A unchanged; B folder gone; `list` | INV-43 | done: `s16_rename_and_drop_mid_life` |
| S17 | read-only output dir | `chmod 555` target dir; `clone` fails; no file; db hashes unchanged (skipped as root) | hashes | INV-36 | done: `s17_read_only_output_dir` |
| S18 | schema file matrix | multi-statement + seed rows; stdin vs file (byte-identical `base.db`); empty file; syntax error; constraint violation in seed; `BEGIN/COMMIT`; unterminated `BEGIN`; `PRAGMA journal_mode=DELETE`; `PRAGMA page_size=8192`; `ATTACH` (asserts chosen policy) | `base.db == live.db` after create; folder absent after failures; `exec` after each successful create captures 1 commit | INV-52..INV-61 (B5/B7/B8 fixed in T15) | done: `scenarios_schema::s18_*` (12 tests). The INV-56 and INV-60 sub-checks are partial (section 1.5). |
| S19 | seed script | existing scenario 11 plus `assert_invariants` with `Checks::light` (no per-k clone; 500 commits) | counts, `clone(500) == live` | INV-27, INV-31 | done: `cli::seed_script_produces_500_commits` |
| S20 | perf smoke (batch runs by default since T24; per-process `#[ignore]`) | 2000-insert stdin batch; 2000 per-process execs | wall-clock budget printed and asserted loosely | INV-51 | done: `perf::s20_stdin_batch_of_2000_inserts` (default, 0.9 s debug); `s20_2000_per_process_execs` (`#[ignore]`, 5.5 s in T23) |

---

## 4. Task graph

Model tiers: `haiku` mechanical, `sonnet` well-specified implementation, `opus` design-sensitive or likely to uncover/fix bugs.

Status column (T23): `done` means every acceptance criterion was re-checked at `cb23796` unless
the cell says otherwise. Commits are listed oldest first.

| ID | Title | Files | Acceptance | Depends | Tier (why) | Status (T23) |
|---|---|---|---|---|---|---|
| T01 | Extract reference WAL encoder into `wal::encode` behind `#[cfg(any(test, feature = "testing"))]`; add `testing` feature to `Cargo.toml`; fuzz crate enables it | `src/wal.rs`, `Cargo.toml`, `fuzz/Cargo.toml` | existing wal proptests pass unchanged using `encode::wal(...)`; `cargo build` without the feature has no encoder symbols | – | sonnet (refactor with a clear target) | done (`4acd528`): no `wal::encode` symbols in the debug or release binary |
| T02 | Harness core: `Observed`, `Model`, `TableDump`, file hashes, `assert_invariants` with `Checks` flags | `tests/common/mod.rs` (new submodules `tests/common/model.rs`, `tests/common/invariants.rs`) | one existing CLI scenario converted and green; `assert_invariants` catches a deliberately corrupted `meta.db` (negative test) | – | opus (oracle design decides what every later scenario can detect) | done (`d09d373`): negative tests `cli::invariants_catch_a_deleted_frame`, `invariants_catch_a_flipped_page_byte` |
| T03 | `Scenario`/`Step` abstraction and `Driver::{Cli,Lib}` | `tests/common/scenario.rs` | S1 implemented and green through both drivers | T02 | sonnet | done (`194eb02`): `scenarios_core` S1 via both drivers |
| T04 | Convert existing 11 CLI scenarios to `Scenario`s with `assert_invariants` | `tests/cli.rs` | all 11 still green; no behaviour assertions lost (diff reviewed) | T03 | haiku (mechanical conversion) | done (`827a65f`): `tests/cli.rs` has 16 tests (the 11 originals, 2 negative invariant tests, output-text and `log` tests) |
| T05 | New e2e scenarios S2, S3, S4, S5, S8, S16, S17 | `tests/scenarios_usage.rs` | each green; S17 skips when uid 0 | T03 | sonnet | done (`0eaff95`): `scenarios_usage` 7/7 |
| T06 | New e2e scenarios S6, S7 (DDL, schema evolution) | `tests/scenarios_ddl.rs` | S7 green; S6 red on B1 with a clear assertion message, marked `#[should_panic]`-free but listed in the "known failing" section of the PR until T13 lands | T03 | sonnet | done (`1af36f9`; quoted-name tests un-ignored in `8edef03`): `scenarios_ddl` 8/8 |
| T07 | Scenario S18 schema-file matrix | `tests/scenarios_schema.rs` | all cases green except B5 (red until T15) and the two decision cases (Q7/Q8) which assert the documented current behaviour and carry a `// DECISION:` comment | T03 | sonnet | done (`3874a9f`; decision cases updated to the Q7/Q8 policy in `a16d6bf`): `scenarios_schema` 12/12 |
| T08 | Library-level tests: S9 (external VACUUM shrink), S11 (clone chain), INV-23 cursor check | `tests/library.rs` | green; S9 shows `db_pages` decreasing in `log` | T02 | opus (needs SQLite WAL/checkpoint behaviour understanding) | done (`db154b1`): `library` 4/4; S9b asserts a shrinking `db_pages` |
| T09 | Store unit tests INV-19/20/21/26 using the encoder to write WAL files | `src/store.rs` tests | green; INV-21 asserts no rows inserted | T01 | sonnet | done (`f7b4b47`): `store::tests::inv19_*`, `inv20_*`, `inv21_*`, `inv26_*` |
| T10 | WAL parser tests INV-06/07/08/09; BE variant of corruption test | `src/wal.rs` tests | green except INV-08 if upper bound not adopted (then documented) | T01 | sonnet | done (`8871aa9`). INV-08 is not adopted: an `#[ignore]`d red test, listed in 1.6. |
| T11 | Replay bound (B6): `apply` returns `Err` for `pgno > db_pages` and for images above a cap; unit tests INV-15 | `src/replay.rs`, `src/db.rs` (pass cap) | `apply(.., [(u32::MAX, page)], None)` is `Err` in < 1 ms; all replay proptests green | – | sonnet | done (`9d72acf`): `huge_pgno_is_rejected_without_allocating` asserts < 50 ms |
| T12 | Concurrency scenarios S12, S13, S14, S15 (helpers spawn `python3 -I` or a tiny Rust bin under `tests/bin/`) | `tests/scenarios_concurrency.rs`, `tests/common/helpers.rs` | S12, S14 green; S13, S15 red with messages quoting counts (expected vs got) | T03 | opus (timing-sensitive, must avoid flakiness, documents real bugs) | done (`e299a7d`). The helpers are re-executions of the test binary over pipe barriers (no Python, no sleeps). S13/S15 went green in T16. |
| T13 | Fix B1 quoting in `summarize` | `src/db.rs` | S6 green; unit test for `"a""b"` | T06 | haiku | done (`8edef03`, which also fixes B9): `db::tests::summarize_counts_tables_with_doubled_quotes_in_names`. A related path-quoting bug is still open: B10. |
| T14 | Fix B2 (done; per Q2 decision: authorizer rejects BEGIN/COMMIT/ROLLBACK in `exec`, capture on the error path as backstop, error if >1 commit per statement); S10 implementation | `src/db.rs`, `tests/scenarios_failure.rs` | S10 green: after `; COMMIT;` statement, history has the committed rows, `exec` exits non-zero | T03 | opus (semantics decision + bug fix) | done (`7d8aef7`). Per Q2 the statement is rejected, so nothing is committed; `s10_commit_inside_statement_changes_nothing`. |
| T15 | Fix B5: verify `journal_mode == wal` after schema; `exec` asserts WAL mode | `src/db.rs` | S18 journal_mode case green: `create` fails with `schema changed journal_mode`, no folder | T07 | sonnet | done (`a16d6bf`): the authorizer denies `PRAGMA journal_mode` (the message names `journal_mode`); `s18_journal_mode_delete_is_rejected`. The `exec` WAL-mode check is untested (INV-58). |
| T16 | Fix B4 concurrency: `BEGIN IMMEDIATE` on `meta.db` before reading WAL+cursor (or lock file); fix B3: pre-capture orphans with `(recovered)` label | `src/store.rs`, `src/db.rs` | S13 and S15 green 20 runs in a row; roundtrip still green | T12 | opus (locking design across two SQLite files) | done (`964de6d`): `scenarios_concurrency` 20/20 green, re-run in T23 |
| T17 | Fuzz target `wal_differential` + `Mutation` enum + dictionary | `fuzz/fuzz_targets/wal_differential.rs`, `fuzz/wal.dict`, `fuzz/Cargo.toml` | 5 min run clean after T10; finds a planted parser bug (temporarily remove the salt check) within 60 s | T01 | opus (differential design, mutation semantics) | done (`42c0151`). Re-checked in T23: a 300 s clean run, and the planted salt-check removal is caught after about 1,100 execs (well under 60 s). |
| T18 | Fuzz targets `replay_apply`, `store_capture`, `statements_split` | `fuzz/fuzz_targets/*.rs`, `fuzz/Cargo.toml` | each builds; `replay_apply` 2 min clean after T11 | T01, T11 | sonnet | done (`7caa1ef`). Re-checked in T23: all targets build, and `replay_apply` ran 120 s clean. |
| T19 | Seed corpus generator (`Harness::wal_snapshot`) and committed corpus; `cmin` | `tests/common/mod.rs`, `fuzz/corpus/**`, `scripts/fuzz-seed.sh` | corpus < 2 MB; replaying corpus with `-runs=0` passes | T02, T17 | haiku | done (`11a87e3`): the committed corpus is 603 KB (41 files) and all 5 replays pass. Note: `make fuzz` (and the default `cargo fuzz run`) writes new inputs into `fuzz/corpus/<target>`, so a local fuzz run leaves untracked files there. Run `cmin` before committing any of them. |
| T20 | CI: PR job (test + fuzz build + corpus replay), nightly fuzz matrix with time budget, artifact upload, regression-test procedure doc | `.github/workflows/ci.yml`, `.github/workflows/fuzz.yml`, `fuzz/README.md`, `Makefile` (`make fuzz`) | workflows lint (`actionlint`); nightly budget <= 25 min | T17, T18 | sonnet | **partly done** (`c20e284`). The nightly budget is met: 5 x 300 s in a parallel matrix, `timeout-minutes: 12`. Not verified: `actionlint` is not installed where T20 or T23 ran, and the workflows only parse as YAML. The jobs have not been seen running on GitHub. |
| T21 | (Optional, after Q8) sandboxed SQL fuzz target with authorizer denylist | `fuzz/fuzz_targets/exec_sql.rs`, `db::sandbox` (testing only) | production denials kept plus a fuzz-only denylist; runs create no file outside the case dir | T20, Q8 | opus (security-sensitive) | **done**: `exec_sql` (structured program, oracle after every exec; section 2.1). 120 s acceptance run and a 626 s run (39k programs): no crash, no file outside the sandbox. Planted bug (capture drops the last frame of multi-frame commits, scratch copy) caught in about 4 s. Seed corpus: 13 inputs, 5 KB. No walflip defect found. Note: on a loaded macOS machine three runs hit the 30 s libFuzzer timeout inside SQLite's `fsync` (`unixSync`; `create`'s `meta.db` setup, a `live.db` commit, `clone_at` I/O). None reproduced (each input runs in about 15 ms alone), and a run without other jobs on the machine was clean. |
| T22 | Perf smoke S20 (`#[ignore]`) and README accuracy (clone memory ~ frames up to N, not db size) | `tests/perf.rs`, `README.md` | runs under `cargo test -- --ignored` with printed timings | T03 | haiku | done (`9069d54`). The per-process test prints `2000 processes in 5.483 s`; the batch test runs by default since T24. The README clone-memory note matches `db::clone_at`. |
| T23 | Final review: run everything, reconcile known-failing list, update README test section, update this plan's status column | all docs, no code | `cargo test` green except explicitly listed red scenarios with linked decisions; `cargo +nightly fuzz build` green | all | opus (judgement on what is done vs deferred) | done (this pass). Results are at the top of the file; open items are in 1.6. |
| T24 | (added in W4) Incremental capture: store a resume state in the cursor so that capture parses only the new WAL tail (INV-51) | `src/wal.rs`, `src/store.rs`, fuzz oracles | 2000-insert batch within the INV-51 budget; resumed parse == full parse (proptests + `wal_differential`/`store_capture`); fallback on any inconsistency | T16 | opus | done (`f029e02`): the S20 batch runs by default in 0.9 s debug; `resumed_parse_equals_full_parse_at_any_split`, `incremental_capture_equals_full_parse_capture`, `stale_or_damaged_resume_state_falls_back_to_the_count_cursor`, `old_cursor_format_is_migrated` |

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

State at T23. Q1, Q2, Q3, Q7, Q8 and Q9 are decided and implemented (see
`docs/WAVE_STATUS.md`, Decisions). Q10 is implemented by T11: `apply` takes `max_pages`, which
`clone_at` derives from the base size and `max(db_pages)`. Q12 is implemented as `Checks::light`.
R2 is resolved: the helpers re-execute the test binary, so no Python is needed. R4 is
documented in the README; streaming is not done. Q4, Q5, Q6 and Q11 are decided and done.

| ID | Question | Why it matters | Suggested default |
|---|---|---|---|
| Q1 | Concurrent `exec` on one db: supported, or documented as unsupported with a lock-file error? | B4 corrupts history silently today. Supporting it needs `meta.db` `BEGIN IMMEDIATE` around read-cursor + read-WAL + insert; the WAL itself is single-writer so SQLite serialises the commits. | support it (T16); a lock would add a failure mode without saving much |
| Q2 | A statement that ends its own transaction (`; COMMIT;`, `BEGIN ...`): reject up front, or capture whatever landed and report an error? | B2 loses history. Rejecting requires parsing SQL; capturing is cheap: check `conn.is_autocommit()` before `tx.commit()`. | capture, label `<sql> (partial)`, exit non-zero |
| Q3 | Orphan commits already in the WAL at `exec` start (crashed writer, external tool): capture with label `(recovered)`, or refuse until the user runs `walflip --db x capture`? | B3 mislabels. Refusing protects label accuracy but blocks the user. | capture as `(recovered)` and print a warning; also show `uncaptured: N` in `status` |
| Q4 | Should `exec` support `VACUUM` (run outside a transaction when the statement is exactly `VACUUM`)? | It is the only CLI path to a shrinking `db_pages`. Replay handles shrink correctly (verified). An external VACUUM whose WAL is checkpointed before the next `exec` captures it breaks `clone -n total` ("database disk image is malformed"), so walflip should run it itself. | **Decided, done: strict special path.** Only `^\s*VACUUM(\s+(main\|"main"))?\s*;?\s*$` (any case) qualifies; other statements starting with `VACUUM` (`VACUUM INTO`, other schema, extra SQL) are rejected before any lock. Path: history lock, `BEGIN IMMEDIATE` on `live.db`, `(recovered)` capture, roll back the live tx, walflip's own literal `VACUUM` in autocommit (authorizer allows transaction control and only `ATTACH ''`), capture under the still-held history lock. VACUUM is one commit (one write transaction on `main`; verified up to 1502 frames with `cache_size=5`), so >1 commit keeps the INV-38 backstop: a foreign commit in the gap between the rollback and VACUUM is captured with the VACUUM label and `exec` errors (documented, not closed). Cost: each VACUUM stores the whole db in the WAL and `meta.db`. Tests: S9a. |
| Q5 | `exec -` splitting on lines: keep, or split on `;` with a real tokenizer (`sqlite3_complete`)? | Multi-line DDL fails with `incomplete input`. `rusqlite` exposes `Connection::is_complete`-like via `ffi::sqlite3_complete`. | **Decided: keep line splitting** as the default for `exec -`; README states it; INV-50 pins behaviour. Done, optional mode: `exec --script <file\|->` splits with `sqlite3_complete` (`db::split_script`, the one splitter, pure); statements need their `;`, an unterminated tail is `incomplete input` and nothing runs; each statement keeps its own transaction, authorizer, VACUUM path and locks. `log` escapes line breaks in labels for display. |
| Q6 | Sector-padding duplicate commit frames: merge into one commit or count each? | Only occurs on filesystems without powersafe-overwrite; counting inflates commits vs statements. | **Decided: leave parser/capture alone.** Padding frames are valid commits, counted and labelled `(recovered)`; replay is exact. Pinned by `inv10_sector_padding_frames_are_counted_as_recovered_commits`; documented in README |
| Q7 | `PRAGMA page_size` / `auto_vacuum` in a schema are silently ignored because WAL is enabled first. Apply pragmas before switching to WAL, or reject them? | Users will expect the schema to control page size; the README says page size is fixed at create. | run the schema first, then switch to WAL and checkpoint; reject `journal_mode` changes (fixes B5 and B7 together) |
| Q8 | `ATTACH` in schema files writes to arbitrary paths. Allow (user's own file) or deny via authorizer? | Security scope; also blocks any schema fuzzing. | deny `ATTACH`/`DETACH` with an authorizer in `create`; document |
| Q9 | `log` shows the first N commits (`LIMIT` without offset). Should the default show the most recent N, or add `--tail`? | For 500 commits the default output is 1..20, which is rarely useful for "find the bad DELETE". | add `--tail`, keep default |
| Q10 | Replay allocation cap value and where the cap lives (`apply` param vs `db.rs` constant)? | B6. SQLite max db is 2^31-2 pages; a hostile `meta.db` should not abort the process. | `apply` rejects `pgno > db_pages` when known, and `db.rs` passes `max_pages = db_pages.unwrap_or(base_pages + total_frames)` |
| Q11 | `list` opening `meta.db` with `CREATE TABLE IF NOT EXISTS` writes into any folder that merely has a `live.db`. Make `list`/`status` read-only? | Side effects from read commands; also fails on read-only roots. | DECIDED, done: narrow fix, not `SQLITE_OPEN_READ_ONLY` (that fails with SQLITE_READONLY_ROLLBACK when a crashed writer left `meta.db-journal`; a read-write open rolls it back). `Store::open_existing` opens read-write without creating and without the schema, and errors if `meta.db` is absent; used by `list`/`status`/`log`/`clone`. `list` skips folders without `meta.db`. `exec`/`create` keep the creating `Store::open`. Tests: `s4_many_small_dbs`, `s19_read_only_root_read_commands_work`, `s20_leftover_meta_journal_does_not_break_reads`. |
| Q12 | Test runtime budget: per-k cloning after every step is O(steps^2). Cap at 30 steps for full checks, `Checks::light` above? | Keeps `cargo test` under ~30 s. | yes; seed scenario uses `light` |
| R1 | Flakiness in concurrency scenarios (process scheduling). | Mitigate with deterministic barriers (helper waits on a file), 20-run soak in T12/T16 acceptance, no sleeps. | |
| R2 | `python3 -I` dependency for helper writers. | Prefer a tiny Rust helper binary (`tests/bin/writer.rs` via `[[bin]]` with `required-features = ["testing"]`) so CI needs no Python. | |
| R3 | Fuzz nightly budget vs findings. | Start at 5 min/target; raise for `wal_differential` if coverage report shows unreached branches. | |
| R4 | README claims clone needs RAM ~ db size; actual is ~ sum of frames up to N (`clone_at` collects all frames into a `Vec`). | Document or stream frames with last-writer-wins dedupe (`SELECT ... WHERE rowid IN (SELECT max(rowid) ... GROUP BY pgno)`). T22 documents; a follow-up task may stream. | |
