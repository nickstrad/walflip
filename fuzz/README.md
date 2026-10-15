# Fuzzing

Targets (see `fuzz/Cargo.toml`): `wal_parse`, `wal_differential`, `replay_apply`, `store_capture`, `statements_split`.

    make fuzz FUZZ_SECS=60      # all targets, nightly + cargo-fuzz required
    make fuzz-replay            # replay committed corpora (-runs=0)

CI: `ci.yml` builds targets and replays corpora on every PR; `fuzz.yml` runs each target 300 s nightly (5 targets in parallel matrix; wall time ~6 min, ~25 min total CPU budget).
Use `-max_len=70000` for `wal_parse` and `-dict=fuzz/wal.dict`.

## Regression-test procedure for a crash

1. The crash lands in `fuzz/artifacts/<target>/` (CI uploads it as an artifact).
2. Minimise: `cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/<crash>`
3. Print the decoded input: `cargo +nightly fuzz fmt <target> fuzz/artifacts/<target>/<minimised>`
4. Add a `#[test]` in the module under test embedding the minimised bytes
   (`include_bytes!("../fuzz/regressions/<target>/<hash>")`, or an inline literal under 200 bytes) asserting the fixed behaviour.
5. Copy the minimised file into `fuzz/corpus/<target>/` so the corpus replays it.
6. One commit per crash: test + fix. Keep corpus small with `cargo +nightly fuzz cmin <target>`.
