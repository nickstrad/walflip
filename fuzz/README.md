# Fuzzing

Targets (see `fuzz/Cargo.toml`): `wal_parse`, `wal_differential`, `replay_apply`, `store_capture`, `statements_split`, `exec_sql`.

`exec_sql` (T21) is the only target that executes SQL. Its input is a structured program
(schema from a fixed grammar, then `exec` calls built from DML/DDL/VACUUM/pragma pieces,
must-reject and must-fail statements, and token soup), run through the real `db::create` and
`db::exec_with` in a per-input directory under `$TMPDIR/walflip-fuzz-exec-<pid>/`. After every
exec it checks integrity of `live.db`, INV-22 on `meta.db`, commit accounting, `clone(total)`
byte-equal to `live.db` and integrity of other clones, and that no file appeared outside the case
directory (ATTACH canaries, cwd, SQLite temp dir). The sandbox (`db::sandbox`, `testing` feature
only) adds pragma/function denials, length/depth limits, `SQLITE_LIMIT_ATTACHED=0`,
`DBCONFIG_DEFENSIVE` and a deterministic progress-handler budget. Run it with `-timeout=30`;
`EXEC_SQL_TRACE=1` prints every exec call and its outcome (handy with `-runs=0` on a crash).

Fuzz builds compile the bundled SQLite with `LIBSQLITE3_FLAGS=-DSQLITE_NO_SYNC` (read by
libsqlite3-sys 0.30's build script), so fsync is a no-op; `exec_sql` otherwise hit the 30 s
libFuzzer timeout stuck in fsync on a loaded machine. The Makefile fuzz targets and the
archived `fuzz-build-replay` job set it; plain `cargo test`/release builds do not. Crash safety is not
under test here, only logic, so durability is irrelevant. If you run `cargo +nightly fuzz` by hand,
export it yourself: `export LIBSQLITE3_FLAGS=-DSQLITE_NO_SYNC`. Changing it rebuilds libsqlite3-sys.

    make fuzz FUZZ_SECS=60      # all targets, nightly + cargo-fuzz required
    make fuzz-replay            # replay committed corpora (-runs=0)

CI: `ci.yml` runs `cargo test` only. There is no fuzz job in CI. The nightly run and the PR-time
build + corpus replay are archived in `archive/ci/fuzz-nightly.yml` and
`archive/ci/fuzz-build-replay.yml` for reference. Fuzz locally with `make fuzz`, replay with
`make fuzz-replay`.
Use `-max_len=70000` for `wal_parse` and `-dict=fuzz/wal.dict`.

## Regression-test procedure for a crash

1. The crash lands in `fuzz/artifacts/<target>/` (CI uploads it as an artifact).
2. Minimise: `cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/<crash>`
3. Print the decoded input: `cargo +nightly fuzz fmt <target> fuzz/artifacts/<target>/<minimised>`
4. Add a `#[test]` in the module under test embedding the minimised bytes
   (`include_bytes!("../fuzz/regressions/<target>/<hash>")`, or an inline literal under 200 bytes) asserting the fixed behaviour.
5. Copy the minimised file into `fuzz/corpus/<target>/` so the corpus replays it.
6. One commit per crash: test + fix. Keep corpus small with `cargo +nightly fuzz cmin <target>`.
