// Generates src/bundle.json. Edit the model here, then run: npm run generate && npm run validate
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? fileURLToPath(new URL('../src/bundle.json', import.meta.url))
const REPO = 'https://github.com/nickstrad/walflip/blob/main/'

// ---------- sources ----------
const srcFiles = {
  'src-readme': ['README.md', 'walflip README'],
  'src-cargo': ['Cargo.toml', 'Cargo manifest'],
  'src-lib': ['src/lib.rs', 'Library root'],
  'src-main': ['src/main.rs', 'CLI entry point'],
  'src-db': ['src/db.rs', 'Database operations'],
  'src-wal': ['src/wal.rs', 'WAL parser and reference encoder'],
  'src-replay': ['src/replay.rs', 'Page replay'],
  'src-store': ['src/store.rs', 'meta.db store and capture cursor'],
  'src-tests-common': ['tests/common/mod.rs', 'E2E harness'],
  'src-tests-invariants': ['tests/common/invariants.rs', 'Invariant checks'],
  'src-tests-model': ['tests/common/model.rs', 'Shadow-database oracle'],
  'src-tests-cli': ['tests/cli.rs', 'End-to-end scenarios'],
  'src-tests-scenarios': ['tests/scenarios_usage.rs', 'Scenario suites (usage, ddl, schema, failure, concurrency, core)'],
  'src-tests-library': ['tests/library.rs', 'Library-level scenarios'],
  'src-tests-perf': ['tests/perf.rs', 'Performance smoke test'],
  'src-tests-fuzzseed': ['tests/fuzz_seed.rs', 'Fuzz corpus generator'],
  'src-ci': ['.github/workflows/ci.yml', 'CI workflow'],
  'src-ci-archive': ['archive/ci/fuzz-nightly.yml', 'Archived nightly fuzz workflow'],
  'src-status': ['docs/WAVE_STATUS.md', 'Wave status, decisions and known limits'],
  'src-fuzz-readme': ['fuzz/README.md', 'Fuzzing guide'],
  'src-tests-roundtrip': ['tests/roundtrip.rs', 'Round-trip property tests'],
  'src-fuzz': ['fuzz/fuzz_targets/wal_parse.rs', 'wal_parse fuzz target'],
  'src-fuzz-exec': ['fuzz/fuzz_targets/exec_sql.rs', 'exec_sql fuzz target (structured SQL programs)'],
  'src-fuzz-cargo': ['fuzz/Cargo.toml', 'Fuzz crate manifest'],
  'src-seed': ['scripts/seed.sh', 'Seed script'],
  'src-example': ['examples/processes.sql', 'Example schema'],
  'src-make': ['Makefile', 'Make targets'],
  'src-plan': ['docs/TESTING_PLAN.md', 'Testing plan (bug IDs B1-B10, invariants INV-xx)'],
}
const sources = Object.entries(srcFiles).map(([id, [path, title]]) => ({
  id, title, url: REPO + path, path,
  notes: 'Read from the local checkout on 2026-10-15. The GitHub URL assumes the repository is pushed to main.',
}))

// ---------- elements ----------
const el = (o) => ({ status: 'existing', tags: [], plannedNotes: [], docIds: [], sourceIds: [], ...o })
const elements = [
  // people
  el({ id: 'operator', kind: 'person', name: 'Operator / developer', description: 'Creates databases, runs every write through walflip exec, inspects history and clones past states. May set WALFLIP_ROOT and WALFLIP_DB.', docIds: ['doc-operator'], sourceIds: ['src-readme', 'src-main'] }),
  el({ id: 'ci-operator', kind: 'person', name: 'CI job or fuzzer operator', description: 'Secondary consumer. GitHub Actions runs cargo test on pull requests and pushes to main. Fuzzing (cargo +nightly fuzz) is run locally; the fuzz workflows are archived.', sourceIds: ['src-make', 'src-ci', 'src-status'] }),
  // systems
  el({ id: 'walflip', kind: 'softwareSystem', name: 'walflip', description: 'Captures every committed SQLite transaction as WAL page images and rebuilds a database as of any commit.', technology: 'Rust 2021 CLI + library', docIds: ['doc-decisions', 'doc-known-gaps'], sourceIds: ['src-readme', 'src-cargo', 'src-lib'] }),
  el({ id: 'host-fs', kind: 'softwareSystem', name: 'Local disk (SQLite files)', description: 'The host filesystem. Holds the data root (default walflip-data/ relative to the working directory) and the clone files.', technology: 'POSIX filesystem, std::fs', tags: ['external'], sourceIds: ['src-db', 'src-main'] }),
  el({ id: 'other-writers', kind: 'softwareSystem', name: 'Other programs that open live.db', description: 'Unsupported. Their writes are not captured, and their checkpoints can fold WAL frames into live.db before walflip reads them.', technology: 'Any SQLite writer', tags: ['external', 'unsupported'], sourceIds: ['src-readme', 'src-plan'] }),
  el({ id: 'sqlite-clients', kind: 'softwareSystem', name: 'Any SQLite client', description: 'Opens a clone file. A clone is a standalone SQLite database with no WAL.', technology: 'SQLite file format', tags: ['external'], sourceIds: ['src-readme'] }),
  el({ id: 'sqlite-engine', kind: 'softwareSystem', name: 'SQLite engine (bundled)', description: 'Third-party SQL engine compiled into the binary through rusqlite feature bundled. Owns journal_mode, checkpoints and the WAL writer.', technology: 'rusqlite 0.32, bundled SQLite', tags: ['external'], sourceIds: ['src-cargo'] }),

  // containers: runtime
  el({ id: 'cli', kind: 'container', parentId: 'walflip', name: 'walflip CLI binary', description: 'Parses arguments and environment, calls one library operation per command and prints the returned data. No logic of its own.', technology: 'Rust, clap 4 (derive, env)', docIds: ['doc-cli'], sourceIds: ['src-main'] }),
  el({ id: 'lib', kind: 'container', parentId: 'walflip', name: 'walflip library crate', description: 'Modules wal, replay, store and db. Returns plain data; never prints. Also linked directly by the test suites and the fuzz crate (feature testing).', technology: 'Rust, rusqlite 0.32 (bundled), anyhow', docIds: ['doc-decisions'], sourceIds: ['src-lib'] }),
  el({ id: 'live-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'live.db', description: 'The database you write to, in WAL journal mode. Only walflip exec may write it. exec installs an authorizer that denies ATTACH, DETACH and PRAGMA journal_mode, and transaction control inside a statement.', technology: 'SQLite, journal_mode=WAL', tags: ['per-database-folder'], sourceIds: ['src-db'] }),
  el({ id: 'live-wal', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'live.db-wal', description: 'SQLite write-ahead log beside live.db. walflip reads it as raw bytes after each commit. Absent after a checkpoint on close.', technology: 'SQLite WAL file format', tags: ['per-database-folder', 'transient'], docIds: ['doc-wal-format'], sourceIds: ['src-wal', 'src-store'] }),
  el({ id: 'base-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'base.db', description: 'Snapshot copied from live.db at create: the schema runs first in rollback mode, then WAL is enabled and checkpointed. clone -n 0 returns it.', technology: 'SQLite file (byte copy)', tags: ['per-database-folder'], sourceIds: ['src-db'] }),
  el({ id: 'meta-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'meta.db', description: 'History store: tables meta (page_size, cursor_salt, cursor_commits, cursor_resume), commits and frames (full page images). Its BEGIN IMMEDIATE lock serialises concurrent exec.', technology: 'SQLite: meta, commits, frames', tags: ['per-database-folder'], docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  el({ id: 'clone-file', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'Clone output file', description: 'Database as of commit N at the -o path. walflip refuses to overwrite an existing file (opened with create_new).', technology: 'SQLite file, std::fs::write', sourceIds: ['src-db'] }),

  // containers: verification and tooling
  el({ id: 'unit-tests', kind: 'container', parentId: 'walflip', name: 'In-crate unit and property tests', description: '#[cfg(test)] modules in wal, replay, store and db: WAL round-trip, truncation, corruption, no-panic; replay page-map model, composition, idempotence, max_pages bound; cursor cases; name validation.', technology: 'cargo test, proptest 1', sourceIds: ['src-wal', 'src-replay', 'src-store', 'src-db'] }),
  el({ id: 'roundtrip-tests', kind: 'container', parentId: 'walflip', name: 'tests/roundtrip.rs', description: 'Random insert/update/delete sequences on a real database; asserts clone -n k equals live contents after step k, single-WAL batches and idempotent capture.', technology: 'proptest, tempfile; links the library', sourceIds: ['src-tests-roundtrip'] }),
  el({ id: 'cli-tests', kind: 'container', parentId: 'walflip', name: 'Scenario suites: tests/*.rs + tests/common', description: 'Scenarios S1-S20 that spawn the compiled binary through Harness (cli, scenarios_core/usage/ddl/schema/failure/concurrency), library-level scenarios (library.rs), a perf smoke test and a fuzz-seed generator. Results are compared with a shadow SQLite Model and the invariant catalog (INV-xx) is checked through the library.', technology: 'std::process::Command, CARGO_BIN_EXE_walflip, rusqlite', sourceIds: ['src-tests-cli', 'src-tests-scenarios', 'src-tests-library', 'src-tests-perf', 'src-tests-fuzzseed', 'src-tests-common', 'src-tests-invariants', 'src-tests-model'] }),
  el({ id: 'fuzz', kind: 'container', parentId: 'walflip', name: 'walflip-fuzz crate', description: 'cargo-fuzz crate with six targets: wal_parse, wal_differential (against the reference encoder), replay_apply, store_capture, statements_split and exec_sql (structured SQL programs through the real create/exec, with a testing-only sandbox). Minimised corpora in fuzz/corpus, dictionary fuzz/wal.dict.', technology: 'libfuzzer-sys 0.4, Rust nightly, walflip feature testing', sourceIds: ['src-fuzz', 'src-fuzz-exec', 'src-fuzz-cargo', 'src-fuzz-readme'] }),
  el({ id: 'seed-script', kind: 'container', parentId: 'walflip', name: 'scripts/seed.sh + examples/processes.sql', description: 'Demo tooling. Samples ps twice, generates 250 INSERT, 150 UPDATE and 100 DELETE statements, and feeds them to the binary. Fails unless status reports 500 commits. fuzz-seed.sh regenerates the fuzz corpus seeds.', technology: 'bash, awk, ps', sourceIds: ['src-seed', 'src-example', 'src-make'] }),

  // components of lib
  el({ id: 'c-db', kind: 'component', parentId: 'lib', name: 'db', description: 'I/O shell. Db paths and name validation, parse_statements and split_script, create (schema in rollback mode, then WAL), list, rename, drop_db, exec_with (history lock, live lock, recovery capture, authorizer, VACUUM path), log, status, clone_at, summarize. Returns structs; never prints.', technology: 'Rust module, rusqlite, std::fs', tags: ['io'], docIds: ['doc-c-db'], sourceIds: ['src-db'] }),
  el({ id: 'c-store', kind: 'component', parentId: 'lib', name: 'store', description: 'I/O shell around meta.db. Store::open creates the schema; open_existing does not. lock takes the history write lock (BEGIN IMMEDIATE); capture reads only the new WAL tail from the stored resume state, parses it and inserts new commits in a savepoint. Contains the pure function commits_seen.', technology: 'Rust module, rusqlite, std::fs', tags: ['io'], docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  el({ id: 'c-wal', kind: 'component', parentId: 'lib', name: 'wal', description: 'Pure WAL parser: header, byte order, salts, chained checksums, commit grouping, and resumable parsing (Resume, parse_from). Borrows pages from the input buffer. Reference encoder behind cfg(test) or feature testing.', technology: 'Rust module, no I/O', tags: ['pure'], docIds: ['doc-wal-format'], sourceIds: ['src-wal'] }),
  el({ id: 'c-replay', kind: 'component', parentId: 'lib', name: 'replay', description: 'Pure page replay onto an in-memory image: validate all frames against max_pages first, overwrite at (pgno-1)*page_size, then resize to db_pages.', technology: 'Rust module, no I/O', tags: ['pure'], sourceIds: ['src-replay'] }),
  // component of cli (shown as supporting in the library view)
  el({ id: 'c-main', kind: 'component', parentId: 'cli', name: 'main.rs (clap layer)', description: 'Cli and Cmd clap types, main dispatch, read_sql (file or stdin) and report. Handles exec SQL, exec - and exec --script, and log --limit/--head/--all. Output formatting only. Shown as a supporting component from the CLI container.', technology: 'Rust, clap 4 derive, anyhow', tags: ['external', 'io'], sourceIds: ['src-main'] }),

  // code: wal
  el({ id: 'k-wal-parse', kind: 'code', parentId: 'c-wal', symbolKind: 'function', name: 'parse', description: 'Returns Ok(None) for a short buffer or bad header checksum, Err for bad magic or a page size that is not a power of two in 512..=65536, else the committed prefix.', members: [{ name: 'parse', signature: "pub fn parse(buf: &[u8]) -> Result<Option<Wal<'_>>>" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-checksum', kind: 'code', parentId: 'c-wal', symbolKind: 'function', name: 'checksum', description: 'SQLite running checksum over 8-byte chunks; byte order chosen by the magic number.', members: [{ name: 'checksum', signature: 'fn checksum(big_endian: bool, data: &[u8], s0: u32, s1: u32) -> (u32, u32)', visibility: 'private' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-wal', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Wal', description: 'Parsed WAL generation.', members: [{ name: 'page_size', signature: 'u32' }, { name: 'salt', signature: '(u32, u32)' }, { name: 'commits', signature: "Vec<Commit<'a>>" }, { name: 'end', signature: 'Resume (where parsing can continue)' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-parse-from', kind: 'code', parentId: 'c-wal', symbolKind: 'function', name: 'parse_from', description: 'Continues a parse from a stored Resume state with only the tail of the file. Returns None when the state does not fit, so the caller parses the whole file.', members: [{ name: 'parse_from', signature: "pub fn parse_from<'a>(header: &[u8], tail: &'a [u8], from: &Resume) -> Option<Wal<'a>>" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-resume', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Resume', description: 'Parser state at a commit boundary: the 32-byte header, file offset, running checksum and commit count. Stored in meta.db under cursor_resume (text form v1).', members: [{ name: 'header', signature: '[u8; 32]' }, { name: 'offset', signature: 'u64' }, { name: 'cksum', signature: '(u32, u32)' }, { name: 'commits', signature: 'u64' }, { name: 'tail_start', signature: 'pub fn tail_start(&self) -> u64' }, { name: 'to_meta / from_meta', signature: 'text round trip' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-commit', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Commit', description: 'Frames up to and including a commit frame.', members: [{ name: 'frames', signature: "Vec<Frame<'a>>" }, { name: 'db_pages', signature: 'u32 (db size after the commit)' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-frame', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Frame', description: 'One page image borrowed from the WAL buffer.', members: [{ name: 'pgno', signature: 'u32' }, { name: 'page', signature: "&'a [u8]" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-encode', kind: 'code', parentId: 'c-wal', symbolKind: 'module', name: 'encode', description: 'Reference WAL encoder used as a test oracle. Compiled only with cfg(test) or feature testing (the fuzz crate enables it).', tags: ['testing-feature'], members: [{ name: 'encode', signature: 'pub fn encode(big_endian: bool, salt: (u32, u32), page_size: u32, commits: &[Commit]) -> Vec<u8>' }], sourceIds: ['src-wal', 'src-cargo'] }),

  // code: store
  el({ id: 'k-store', kind: 'code', parentId: 'c-store', symbolKind: 'class', name: 'Store', description: 'Owns the meta.db connection.', members: [{ name: 'open / open_with_timeout', signature: 'pub fn open(path: &Path) -> Result<Self>  (creates the schema)' }, { name: 'open_existing', signature: 'pub fn open_existing(path: &Path) -> Result<Self>  (no create, no schema)' }, { name: 'get / set', signature: 'meta key-value access' }, { name: 'page_size', signature: 'pub fn page_size(&self) -> Result<Option<u64>>' }, { name: 'commit_count', signature: 'pub fn commit_count(&self) -> Result<u64>' }, { name: 'uncaptured', signature: 'pub fn uncaptured(&self, wal_path: &Path) -> Result<u64>' }, { name: 'lock', signature: 'pub fn lock(&mut self) -> Result<WriteLock<\'_>>' }, { name: 'capture', signature: 'pub fn capture(&mut self, wal_path: &Path, label: &str) -> Result<usize>' }, { name: 'conn', signature: 'pub fn conn(&self) -> &Connection' }], sourceIds: ['src-store'] }),
  el({ id: 'k-writelock', kind: 'code', parentId: 'c-store', symbolKind: 'class', name: 'WriteLock', description: 'The history write lock: an open BEGIN IMMEDIATE on meta.db. capture runs inside it in a savepoint; commit releases it; drop without commit rolls back.', members: [{ name: 'capture', signature: 'pub fn capture(&self, wal_path: &Path, label: &str) -> Result<Captured>' }, { name: 'commit', signature: 'pub fn commit(self) -> Result<()>' }], sourceIds: ['src-store'] }),
  el({ id: 'k-commits-seen', kind: 'code', parentId: 'c-store', symbolKind: 'function', name: 'commits_seen', description: 'Pure cursor rule: same salt returns the stored count; different salt, missing or junk returns 0.', tags: ['pure'], members: [{ name: 'commits_seen', signature: 'pub fn commits_seen(stored_salt: Option<&str>, stored_commits: Option<&str>, salt: (u32, u32)) -> usize' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-meta', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'meta', description: 'Key-value facts. Keys used: page_size, cursor_salt (salt1:salt2), cursor_commits, cursor_resume (Resume text form; used only while it agrees with salt and count).', members: [{ name: 'key', signature: 'TEXT PRIMARY KEY' }, { name: 'value', signature: 'TEXT NOT NULL' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-commits', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'commits', description: 'One row per captured commit, 1-based.', members: [{ name: 'idx', signature: 'INTEGER PRIMARY KEY' }, { name: 'label', signature: 'TEXT NOT NULL (the SQL statement)' }, { name: 'db_pages', signature: 'INTEGER NOT NULL' }, { name: 'committed_at', signature: 'TEXT DEFAULT CURRENT_TIMESTAMP' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-frames', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'frames', description: 'Full page images in commit order.', members: [{ name: 'commit_idx', signature: 'INTEGER NOT NULL REFERENCES commits(idx)' }, { name: 'seq', signature: 'INTEGER NOT NULL' }, { name: 'pgno', signature: 'INTEGER NOT NULL' }, { name: 'page', signature: 'BLOB NOT NULL' }, { name: 'PRIMARY KEY', signature: '(commit_idx, seq)' }], sourceIds: ['src-store'] }),

  // deployment: developer machine
  ...(() => {
    const env = 'developer-machine'
    const dn = (id, name, description, technology, parentId) => el({ id, kind: 'deploymentNode', name, description, technology, environment: env, ...(parentId ? { parentId } : {}), sourceIds: ['src-make', 'src-readme'] })
    const inst = (id, name, description, instanceOf, parentId) => el({ id, kind: 'instance', name, description, instanceOf, parentId, environment: env, sourceIds: ['src-make', 'src-readme'] })
    return [
      dn('dn-host', 'Developer machine', 'Unix-like OS with a Rust toolchain 1.85+ (tests need 1.88+). SQLite is bundled, so no system SQLite is needed.', 'macOS or Linux, cargo'),
      dn('dn-cargo-bin', '~/.cargo/bin', 'Install target of cargo install --path .', 'cargo install', 'dn-host'),
      inst('i-cli-installed', 'walflip (installed)', 'Binary on PATH for everyday use.', 'cli', 'dn-cargo-bin'),
      dn('dn-target', 'target/release', 'Output of cargo build --release (make build). make seed and make demo use this binary.', 'cargo build --release', 'dn-host'),
      inst('i-cli-release', 'walflip (target/release)', 'Binary that the Makefile and seed script run.', 'cli', 'dn-target'),
      el({ id: 'infra-make', kind: 'infrastructureNode', parentId: 'dn-host', name: 'Makefile targets', description: 'build, test, fuzz (FUZZ_SECS, default 30), fuzz-replay, seed, demo, clean, distclean.', technology: 'make', sourceIds: ['src-make'] }),
      dn('dn-data-root', 'walflip-data/', 'Data root. --root or WALFLIP_ROOT; the default is relative to the working directory. Ignored by git.', 'Directory on local disk', 'dn-host'),
      dn('dn-db-folder', 'walflip-data/NAME/', 'One folder per database. make seed creates processes/.', 'Directory', 'dn-data-root'),
      inst('i-live-db', 'live.db', 'Live database file.', 'live-db', 'dn-db-folder'),
      inst('i-live-wal', 'live.db-wal', 'Present only while a connection is open or after an unclean close.', 'live-wal', 'dn-db-folder'),
      inst('i-base-db', 'base.db', 'Base snapshot.', 'base-db', 'dn-db-folder'),
      inst('i-meta-db', 'meta.db', 'History store.', 'meta-db', 'dn-db-folder'),
      dn('dn-tmp', '/tmp', 'make demo writes /tmp/at250.db and /tmp/at500.db; make clean removes them.', 'Directory', 'dn-host'),
      inst('i-clone', 'at250.db / at500.db', 'Clones produced by make demo.', 'clone-file', 'dn-tmp'),
      dn('dn-nightly', 'Rust nightly + cargo-fuzz', 'Needed only for make fuzz and make fuzz-replay. Build output in fuzz/target, crashes in fuzz/artifacts (both ignored by git).', 'cargo +nightly fuzz', 'dn-host'),
      inst('i-fuzz', 'Six fuzz targets', 'make fuzz runs each with -max_total_time=FUZZ_SECS; make fuzz-replay replays the committed corpora once.', 'fuzz', 'dn-nightly'),
    ]
  })(),
  // deployment: CI
  ...(() => {
    const env = 'ci-github'
    const e = (o) => el({ environment: env, sourceIds: ['src-ci'], ...o })
    return [
      e({ id: 'dn-ci', kind: 'deploymentNode', name: 'GitHub Actions runner', description: 'Job test on ubuntu-24.04 (pinned; ubuntu-latest moves to Ubuntu 26 on 2026-10-19). Steps: checkout, stable Rust, rust-cache, cargo test.', technology: 'GitHub Actions, Rust stable' }),
      el({ id: 'infra-ci-trigger', kind: 'infrastructureNode', parentId: 'dn-ci', name: 'ci.yml triggers', description: 'pull_request and push to main. No fuzz job: the nightly fuzz and the PR fuzz build + corpus replay workflows are archived in archive/ci/.', technology: 'GitHub Actions workflow', sourceIds: ['src-ci', 'src-ci-archive', 'src-status'] }),
      e({ id: 'i-ci-tests', kind: 'instance', parentId: 'dn-ci', instanceOf: 'cli-tests', name: 'cargo test --features testing', description: 'Unit, property, round-trip and scenario suites (about 155 tests).' }),
    ]
  })(),
]

// ---------- relationships ----------
const rel = (id, sourceId, targetId, label, technology, extra = {}) => ({ id, sourceId, targetId, label, technology, status: 'existing', plannedNotes: [], docIds: [], sourceIds: [], ...extra })
const relationships = [
  // context
  rel('ctx-op-wf', 'operator', 'walflip', 'Creates dbs, runs SQL, inspects history, clones commits', 'CLI args, env, stdin, stdout', { sourceIds: ['src-main'] }),
  rel('ctx-ci-wf', 'ci-operator', 'walflip', 'Runs tests (CI) and fuzzing (local)', 'cargo test, cargo +nightly fuzz', { sourceIds: ['src-make'] }),
  rel('ctx-wf-fs', 'walflip', 'host-fs', 'Stores live db, snapshot and history; writes clones', 'SQLite via rusqlite, std::fs', { sourceIds: ['src-db'] }),
  rel('ctx-other-fs', 'other-writers', 'host-fs', 'Unsupported: writes live.db directly, not captured', 'SQLite', { sourceIds: ['src-readme'] }),
  rel('ctx-client-fs', 'sqlite-clients', 'host-fs', 'Opens clone files', 'SQLite file format', { sourceIds: ['src-readme'] }),
  // containers runtime
  rel('op-cli', 'operator', 'cli', 'Runs commands', 'argv, WALFLIP_ROOT, WALFLIP_DB, stdin', { sourceIds: ['src-main'] }),
  rel('cli-lib', 'cli', 'lib', 'Calls one db operation per command; prints returned data', 'Rust calls (walflip::db)', { sourceIds: ['src-main'] }),
  rel('lib-live', 'lib', 'live-db', 'Runs each statement as one transaction under the history and live write locks, autocheckpoint off', 'rusqlite Connection', { sourceIds: ['src-db'] }),
  rel('lib-wal', 'lib', 'live-wal', 'Reads the new WAL bytes after each commit', 'std::fs::File read from the stored offset', { sourceIds: ['src-store'] }),
  rel('lib-base', 'lib', 'base-db', 'Copies at create; reads as replay base at clone', 'std::fs::copy, std::fs::read', { sourceIds: ['src-db'] }),
  rel('lib-meta', 'lib', 'meta-db', 'Stores commits, frames and cursor; takes the history write lock; reads them back', 'rusqlite, SQL', { sourceIds: ['src-store', 'src-db'] }),
  rel('lib-clone', 'lib', 'clone-file', 'Writes rebuilt image; checks it read-only', 'std::fs::write, SQLite immutable=1', { sourceIds: ['src-db'] }),
  rel('live-appends-wal', 'live-db', 'live-wal', 'SQLite appends page frames on commit', 'SQLite WAL writer', { sourceIds: ['src-wal'] }),
  rel('other-live', 'other-writers', 'live-db', 'Unsupported direct writes and checkpoints', 'SQLite', { sourceIds: ['src-readme'] }),
  rel('client-clone', 'sqlite-clients', 'clone-file', 'Opens the rebuilt database', 'SQLite file format', { sourceIds: ['src-readme'] }),
  // containers verification
  rel('ci-unit', 'ci-operator', 'unit-tests', 'Runs', 'cargo test (make test)', { sourceIds: ['src-make'] }),
  rel('ci-roundtrip', 'ci-operator', 'roundtrip-tests', 'Runs', 'cargo test', { sourceIds: ['src-make'] }),
  rel('ci-cli-tests', 'ci-operator', 'cli-tests', 'Runs', 'cargo test', { sourceIds: ['src-make'] }),
  rel('ci-fuzz', 'ci-operator', 'fuzz', 'Runs six targets for FUZZ_SECS each', 'cargo +nightly fuzz run (make fuzz, make fuzz-replay)', { sourceIds: ['src-make'] }),
  rel('op-seed', 'operator', 'seed-script', 'Seeds a 500-commit demo database', 'make seed, make demo', { sourceIds: ['src-make'] }),
  rel('unit-lib', 'unit-tests', 'lib', 'Tests module internals in place', '#[cfg(test)], proptest', { sourceIds: ['src-wal', 'src-replay'] }),
  rel('roundtrip-lib', 'roundtrip-tests', 'lib', 'Calls create, exec, clone_at, summarize directly', 'Rust calls, tempfile', { sourceIds: ['src-tests-roundtrip'] }),
  rel('clitests-cli', 'cli-tests', 'cli', 'Drives the compiled binary', 'std::process::Command', { sourceIds: ['src-tests-common'] }),
  rel('clitests-lib', 'cli-tests', 'lib', 'Calls the library; checks invariants via db::list and clone_at; reads meta.db read-only', 'Rust calls, rusqlite immutable=1', { sourceIds: ['src-tests-invariants'] }),
  rel('clitests-seed', 'cli-tests', 'seed-script', 'Runs the seed script', 'bash', { sourceIds: ['src-tests-cli'] }),
  rel('fuzz-lib', 'fuzz', 'lib', 'Feeds bytes and structured programs to wal, replay, store and db', 'libFuzzer, feature testing', { sourceIds: ['src-fuzz'] }),
  rel('seed-cli', 'seed-script', 'cli', 'Runs drop, create --schema, exec -, status', 'bash subprocess', { sourceIds: ['src-seed'] }),
  // components
  rel('op-main', 'operator', 'c-main', 'Runs commands', 'CLI', { sourceIds: ['src-main'] }),
  rel('main-fs', 'c-main', 'host-fs', 'Reads the schema file', 'std::fs::read_to_string or stdin', { sourceIds: ['src-main'] }),
  rel('main-db', 'c-main', 'c-db', 'Calls operations; receives Created, DbSummary, ExecReport, LogEntry, Status, Summary', 'Rust calls', { sourceIds: ['src-main'] }),
  rel('db-store', 'c-db', 'c-store', 'Opens Store; lock; capture; page_size, commit_count, uncaptured; queries through conn()', 'Rust calls', { sourceIds: ['src-db'] }),
  rel('db-replay', 'c-db', 'c-replay', 'apply(image, page_size, frames, db_pages, max_pages)', 'Rust call on Vec<u8>', { sourceIds: ['src-db', 'src-replay'] }),
  rel('store-wal', 'c-store', 'c-wal', 'parse(bytes) or parse_from(header, tail, resume) -> Option<Wal>', 'Rust call on &[u8]', { sourceIds: ['src-store'] }),
  rel('db-sqlite', 'c-db', 'sqlite-engine', 'Opens live.db with an authorizer, runs statements, VACUUM, checkpoint at create, integrity_check', 'rusqlite', { sourceIds: ['src-db'] }),
  rel('store-sqlite', 'c-store', 'sqlite-engine', 'Reads and writes meta.db; one write-lock transaction per statement', 'rusqlite', { sourceIds: ['src-store'] }),
  rel('store-fs', 'c-store', 'host-fs', 'Reads live.db-wal header and tail', 'std::fs::File read', { sourceIds: ['src-store'] }),
  rel('db-fs', 'c-db', 'host-fs', 'Creates, copies, reads, writes, renames and removes files', 'std::fs', { sourceIds: ['src-db'] }),
  // components to datastores (dynamic views)
  rel('db-live', 'c-db', 'live-db', 'Opens and writes live.db', 'rusqlite Connection', { sourceIds: ['src-db'] }),
  rel('db-base', 'c-db', 'base-db', 'Copies or reads base.db', 'std::fs', { sourceIds: ['src-db'] }),
  rel('db-clone', 'c-db', 'clone-file', 'Writes and checks the clone', 'std::fs::write, SQLite immutable=1', { sourceIds: ['src-db'] }),
  rel('db-livewal', 'c-db', 'live-wal', 'Reports WAL size; wal_mark for change detection', 'std::fs::metadata', { sourceIds: ['src-db'] }),
  rel('store-meta', 'c-store', 'meta-db', 'Reads and writes history tables', 'rusqlite, SQL', { sourceIds: ['src-store'] }),
  rel('store-livewal', 'c-store', 'live-wal', 'Reads WAL header and tail', 'std::fs::File read', { sourceIds: ['src-store'] }),
  // code: wal
  rel('k-parse-checksum', 'k-wal-parse', 'k-wal-checksum', 'Verifies header and chained frame checksums', 'function call'),
  rel('k-parse-wal', 'k-wal-parse', 'k-wal-wal', 'Returns', 'Result<Option<Wal>>'),
  rel('k-wal-commits', 'k-wal-wal', 'k-wal-commit', 'Holds commits in order', 'Vec', { kind: 'composition' }),
  rel('k-commit-frames', 'k-wal-commit', 'k-wal-frame', 'Holds frames in order', 'Vec', { kind: 'composition' }),
  rel('k-encode-checksum', 'k-wal-encode', 'k-wal-checksum', 'Shares the checksum primitive', 'function call'),
  rel('k-parsefrom-checksum', 'k-wal-parse-from', 'k-wal-checksum', 'Verifies chained checksums from the stored one', 'function call'),
  rel('k-parsefrom-wal', 'k-wal-parse-from', 'k-wal-wal', 'Returns only commits after the resume point', 'Option<Wal>'),
  rel('k-wal-end', 'k-wal-wal', 'k-wal-resume', 'end: where the next parse can start', 'field', { kind: 'composition' }),
  rel('k-parsefrom-resume', 'k-wal-parse-from', 'k-wal-resume', 'Starts from', 'function argument'),
  // code: store
  rel('k-store-seen', 'k-store', 'k-commits-seen', 'capture computes commits already stored', 'function call'),
  rel('k-store-meta', 'k-store', 'k-t-meta', 'Reads page_size and cursor; writes cursor', 'SQL'),
  rel('k-store-commits', 'k-store', 'k-t-commits', 'Inserts one row per new commit', 'SQL INSERT'),
  rel('k-store-frames', 'k-store', 'k-t-frames', 'Inserts page images', 'SQL INSERT'),
  rel('k-store-lock', 'k-store', 'k-writelock', 'lock() returns it', 'BEGIN IMMEDIATE'),
  rel('k-lock-meta', 'k-writelock', 'k-t-meta', 'Capture advances cursor_salt, cursor_commits, cursor_resume', 'SQL'),
  rel('k-frames-commits', 'k-t-frames', 'k-t-commits', 'commit_idx references idx', 'foreign key', { kind: 'association' }),
  // deployment: developer machine
  rel('d-make-cli', 'infra-make', 'i-cli-release', 'make seed / make demo run this binary', 'bash, WALFLIP=target/release/walflip'),
  rel('d-make-fuzz', 'infra-make', 'i-fuzz', 'make fuzz / make fuzz-replay', 'cargo +nightly fuzz run, each target'),
  rel('d-cli-live', 'i-cli-release', 'i-live-db', 'exec writes', 'rusqlite'),
  rel('d-cli-wal', 'i-cli-release', 'i-live-wal', 'Reads WAL bytes', 'std::fs::read'),
  rel('d-cli-base', 'i-cli-release', 'i-base-db', 'Copies or reads snapshot', 'std::fs'),
  rel('d-cli-meta', 'i-cli-release', 'i-meta-db', 'Writes and reads history', 'rusqlite'),
  rel('d-cli-clone', 'i-cli-release', 'i-clone', 'make demo clones -n 250 and -n 500', 'std::fs::write'),
  rel('d-installed-live', 'i-cli-installed', 'i-live-db', 'Same layout when run from the same working directory', 'rusqlite'),
  // deployment: CI
  rel('d-ci-tests', 'infra-ci-trigger', 'i-ci-tests', 'Runs on every pull request and push to main', 'cargo test --features testing', { sourceIds: ['src-ci'] }),
]

// ---------- legends ----------
const byId = Object.fromEntries(elements.map(e => [e.id, e]))
const relById = Object.fromEntries(relationships.map(r => [r.id, r]))
function legendFor(kind, nodes, edges, groups) {
  const entries = []
  const add = (id, label, description, swatch) => entries.push({ id, label, description, swatch })
  const ns = nodes.map(n => byId[n])
  if (ns.some(e => e.kind === 'person')) add('person', 'Person', 'A human or a job run by a human', { shape: 'person', tone: 'default', stroke: 'solid' })
  if (kind === 'context') add('accent', 'System in focus', 'walflip, the scope of this view', { shape: 'box', tone: 'accent', stroke: 'solid' })
  if (ns.some(e => e.kind !== 'person' && !e.tags.includes('external'))) add('box', 'Element', kind === 'code' ? 'A Rust symbol or a meta.db table' : 'A system, container, component or physical node in scope', { shape: 'box', tone: 'default', stroke: 'solid' })
  if (ns.some(e => e.containerType === 'datastore')) add('database', 'Data store', 'A file that walflip reads or writes', { shape: 'database', tone: 'default', stroke: 'solid' })
  if (ns.some(e => e.tags.includes('external'))) add('external', 'External or supporting element', 'Outside the scope of this view', { shape: 'box', tone: 'external', stroke: 'solid' })
  if (groups.length || kind === 'deployment') add('boundary', 'Boundary', kind === 'deployment' ? 'A physical node such as a machine or directory' : 'A visual grouping, not a separate process', { shape: 'boundary', tone: 'default', stroke: 'solid' })
  const anyPlanned = ns.some(e => e.status !== 'existing') || edges.some(r => relById[r].status !== 'existing') || groups.some(g => g.status !== 'existing')
  if (anyPlanned) add('dashed', 'Planned or partly planned', 'Not fully present in the code today; see the planned notes', { shape: 'box', tone: 'muted', stroke: 'dashed' })
  if (edges.length) add('forward', 'Relationship', 'Arrow points from the caller to the supplier; the label gives intent and mechanism', { shape: 'line', tone: 'default', stroke: 'solid', arrow: 'forward' })
  if (kind === 'dynamic') add('scenario-id', 'Scenario step number', 'Tree position in the scenario; B marks branches with no order between them. Selected interactions are highlighted.', { shape: 'line', tone: 'accent', stroke: 'solid', arrow: 'forward' })
  return { title: 'Diagram key', entries }
}

function diagram(o) {
  const groups = o.groups ?? []
  const nodes = o.nodes.map(n => (typeof n === 'string' ? { elementId: n } : n))
  return {
    id: o.id, kind: o.kind, title: o.title, status: o.status ?? 'existing',
    ...(o.scopeId ? { scopeId: o.scopeId } : {}),
    ...(o.environment ? { environment: o.environment } : {}),
    ...(o.parent ? { parent: o.parent } : {}),
    ...(o.related ? { related: o.related } : {}),
    ...(groups.length ? { groups } : {}),
    nodes,
    edges: o.edges.map(relationshipId => ({ relationshipId })),
    ...(o.scenario ? { scenario: o.scenario } : {}),
    legend: legendFor(o.kind, nodes.map(n => n.elementId), o.edges, groups),
    layout: { mode: 'auto', direction: o.direction ?? 'TB' },
    docIds: o.docIds ?? [], sourceIds: o.sourceIds ?? [],
  }
}

// scenario helpers
const seq = (id, children) => ({ kind: 'sequence', id, children })
const par = (id, branches) => ({ kind: 'parallel', id, branches })
const fwd = (id, relationshipId, label) => ({ kind: 'interaction', id, relationshipId, direction: 'forward', label })
const rev = (id, relationshipId, label) => ({ kind: 'interaction', id, relationshipId, direction: 'reverse', label })
const usedRels = (step, acc = new Set()) => { if (step.kind === 'interaction') acc.add(step.relationshipId); else (step.children ?? step.branches).forEach(s => usedRels(s, acc)); return acc }
const dynamic = (o) => diagram({ ...o, kind: 'dynamic', edges: [...usedRels(o.scenario)], direction: 'LR', parent: { diagramId: 'components' } })

const createScenario = seq('create', [
  fwd('cr-1', 'op-main', 'walflip create NAME --schema FILE'),
  fwd('cr-2', 'main-db', 'Db::new: validate NAME (1-64 of A-Z a-z 0-9 _ -) and build paths'),
  fwd('cr-3', 'main-fs', 'read_sql: read FILE, or stdin when FILE is -'),
  fwd('cr-4', 'main-db', 'db::create(&d, Some(sql))'),
  fwd('cr-5', 'db-fs', 'Fail if the folder exists; create_dir_all'),
  fwd('cr-6', 'db-live', 'Open live.db in rollback mode; run the schema with an authorizer (ATTACH, DETACH, PRAGMA journal_mode denied)'),
  fwd('cr-7', 'db-live', 'Fail if the schema left a transaction open; any error removes the whole folder'),
  fwd('cr-8', 'db-live', 'PRAGMA journal_mode=WAL (must return wal); wal_checkpoint(TRUNCATE); read page_size; close'),
  fwd('cr-9', 'db-base', 'fs::copy live.db to base.db'),
  fwd('cr-10', 'db-store', 'Store::open(meta.db); set page_size'),
  fwd('cr-11', 'store-meta', 'CREATE TABLE IF NOT EXISTS meta, commits, frames; store page_size'),
  rev('cr-12', 'main-db', 'Return Created { page_size }'),
  rev('cr-13', 'op-main', 'Print: created DIR (page size N)'),
])

const execScenario = seq('exec', [
  fwd('ex-1', 'op-main', 'walflip --db NAME exec SQL, exec - (one statement per line) or exec --script FILE (split like the sqlite3 shell)'),
  fwd('ex-2', 'main-db', 'Db::existing: fail unless live.db exists; parse_statements or split_script'),
  fwd('ex-3', 'main-db', 'db::exec_with(&d, &statements, busy timeout)'),
  fwd('ex-4', 'db-store', 'Store::open_with_timeout(meta.db)'),
  fwd('ex-5', 'db-live', 'Open live.db; assert WAL mode; install authorizer; PRAGMA wal_autocheckpoint=0'),
  seq('ex-loop', [
    fwd('ex-6', 'db-store', 'Take the history lock: Store::lock'),
    fwd('ex-7', 'store-meta', 'BEGIN IMMEDIATE on meta.db (waits up to the busy timeout)'),
    fwd('ex-8', 'db-live', 'BEGIN IMMEDIATE on live.db'),
    fwd('ex-9', 'db-store', 'Recovery: if the WAL changed since our last capture, capture commits nobody recorded as (recovered)'),
    fwd('ex-10', 'db-live', 'Run the statement with transaction control denied; COMMIT. A bare VACUUM takes its own path in autocommit'),
    fwd('ex-11', 'live-appends-wal', 'SQLite appends frames; the last frame carries db_size (commit marker)'),
    fwd('ex-12', 'db-store', 'capture(wal_path, label = stmt) inside the open meta.db transaction'),
    fwd('ex-13', 'store-livewal', 'Read the 32-byte header and only the tail from the stored resume offset; full read if the header changed'),
    fwd('ex-14', 'store-wal', 'wal::parse_from(header, tail, resume), or wal::parse(bytes) as the fallback'),
    rev('ex-15', 'store-wal', 'Wal { page_size, salt, commits, end }: stops at a salt or checksum mismatch, drops uncommitted tail'),
    fwd('ex-16', 'store-meta', 'In a savepoint: check page_size; insert commits after the cursor and their frames; set cursor_salt, cursor_commits, cursor_resume'),
    rev('ex-17', 'db-store', 'Return commits added; COMMIT releases the history lock. More than one commit for one statement is an error'),
  ]),
  rev('ex-18', 'main-db', 'Return ExecReport { captured, recovered }'),
  rev('ex-19', 'op-main', 'Print: captured N commit(s); warn on stderr if some were (recovered)'),
])

const cloneScenario = seq('clone', [
  fwd('cl-1', 'op-main', 'walflip --db NAME clone -n N -o FILE'),
  fwd('cl-2', 'main-db', 'db::clone_at(&d, N, FILE)'),
  fwd('cl-3', 'db-clone', 'Fail if FILE already exists'),
  fwd('cl-4', 'db-store', 'Store::open_existing (no create); commit_count; fail if N > count; require page_size'),
  fwd('cl-5', 'store-meta', 'SELECT pgno, page FROM frames WHERE commit_idx <= N ORDER BY commit_idx, seq'),
  fwd('cl-6', 'store-meta', 'db_pages of commit N (none for N = 0) and max(db_pages) over 1..=N'),
  fwd('cl-7', 'db-base', 'fs::read base.db into memory'),
  fwd('cl-8', 'db-replay', 'apply(image, page_size, frames, db_pages, max_pages = max(base pages, max db_pages))'),
  rev('cl-9', 'db-replay', 'Validate every frame first, overwrite at (pgno-1)*page_size, resize to db_pages'),
  fwd('cl-10', 'db-clone', 'Open FILE with create_new (refuse if it exists) and write the image'),
  fwd('cl-11', 'main-db', 'db::summarize(FILE)'),
  fwd('cl-12', 'db-clone', 'Open read-only immutable=1; integrity_check; count rows per table'),
  rev('cl-13', 'op-main', 'Print frames replayed, integrity_check and row counts'),
])

const inspectScenario = par('inspect', [
  seq('ls', [
    fwd('ls-1', 'op-main', 'walflip list'),
    fwd('ls-2', 'main-db', 'db::list(root)'),
    fwd('ls-3', 'db-fs', 'read_dir(root); keep folders with live.db and meta.db; missing root gives an empty list'),
    fwd('ls-4', 'db-store', 'Store::open_existing(NAME/meta.db).commit_count()'),
    fwd('ls-5', 'store-meta', 'SELECT count(*) FROM commits'),
    rev('ls-6', 'op-main', 'Print name and commits, sorted by name'),
  ]),
  seq('st', [
    fwd('st-1', 'op-main', 'walflip --db NAME status'),
    fwd('st-2', 'main-db', 'db::status(&d)'),
    fwd('st-3', 'db-store', 'open_existing; page_size, commit_count, count(frames), uncaptured(wal)'),
    fwd('st-4', 'store-meta', 'Read meta and count rows'),
    fwd('st-5', 'db-livewal', 'fs::metadata(live.db-wal): size, or absent'),
    rev('st-6', 'op-main', 'Print page size, commits, frames, live wal, and uncaptured when WAL commits are not in history'),
  ]),
  seq('lg', [
    fwd('lg-1', 'op-main', 'walflip --db NAME log [--limit N (default 20)] [--head | --all]'),
    fwd('lg-2', 'main-db', 'db::log(&d, N, from_end)'),
    fwd('lg-3', 'db-store', 'Store::open_existing; query through conn()'),
    fwd('lg-4', 'store-meta', 'commits LEFT JOIN frames GROUP BY idx; the last N (or first N), shown oldest first'),
    rev('lg-5', 'op-main', 'Print n, frames, db_pages and the label on one line, cut to 90 characters'),
  ]),
])

const diagrams = [
  diagram({ id: 'context', kind: 'context', title: 'walflip: system context', scopeId: 'walflip',
    nodes: ['operator', 'ci-operator', { elementId: 'walflip', detail: { diagramId: 'containers-runtime' } }, 'host-fs', 'other-writers', 'sqlite-clients'],
    edges: ['ctx-op-wf', 'ctx-ci-wf', 'ctx-wf-fs', 'ctx-other-fs', 'ctx-client-fs'],
    related: [{ diagramId: 'containers-verification', label: 'Containers: tests, fuzzing and tooling' }],
    docIds: ['doc-context'], sourceIds: ['src-readme'] }),
  diagram({ id: 'containers-runtime', kind: 'container', title: 'walflip: runtime containers and data stores', scopeId: 'walflip', parent: { diagramId: 'context', elementId: 'walflip' },
    nodes: ['operator', 'cli', { elementId: 'lib', detail: { diagramId: 'components' } }, 'live-db', 'live-wal', 'base-db', 'meta-db', 'clone-file', 'other-writers', 'sqlite-clients'],
    edges: ['op-cli', 'cli-lib', 'lib-live', 'lib-wal', 'lib-base', 'lib-meta', 'lib-clone', 'live-appends-wal', 'other-live', 'client-clone'],
    groups: [{ id: 'g-db-folder', label: 'Per-database folder: ROOT/NAME/', memberIds: ['live-db', 'live-wal', 'base-db', 'meta-db'], status: 'existing' }],
    related: [{ diagramId: 'containers-verification', label: 'Tests, fuzzing and tooling' }, { diagramId: 'deploy-dev', label: 'Deployment on a developer machine' }],
    docIds: ['doc-containers-runtime'], sourceIds: ['src-db', 'src-store'] }),
  diagram({ id: 'containers-verification', kind: 'container', title: 'walflip: tests, fuzzing and tooling', scopeId: 'walflip', parent: { diagramId: 'context', elementId: 'walflip' },
    nodes: ['ci-operator', 'operator', 'unit-tests', 'roundtrip-tests', 'cli-tests', 'fuzz', 'seed-script', 'cli', 'lib'],
    edges: ['ci-unit', 'ci-roundtrip', 'ci-cli-tests', 'ci-fuzz', 'op-seed', 'unit-lib', 'roundtrip-lib', 'clitests-cli', 'clitests-lib', 'clitests-seed', 'fuzz-lib', 'seed-cli', 'cli-lib'],
    related: [{ diagramId: 'containers-runtime', label: 'Runtime containers and data stores' }, { diagramId: 'deploy-ci', label: 'CI deployment' }],
    docIds: ['doc-containers-verification'], sourceIds: ['src-tests-cli', 'src-tests-scenarios', 'src-fuzz', 'src-fuzz-exec', 'src-plan'] }),
  diagram({ id: 'components', kind: 'component', title: 'walflip library: components', scopeId: 'lib', parent: { diagramId: 'containers-runtime', elementId: 'lib' },
    nodes: ['operator', 'c-main', 'c-db', { elementId: 'c-store', detail: { diagramId: 'code-store' } }, { elementId: 'c-wal', detail: { diagramId: 'code-wal' } }, 'c-replay', 'sqlite-engine', 'host-fs'],
    edges: ['op-main', 'main-fs', 'main-db', 'db-store', 'db-replay', 'store-wal', 'db-sqlite', 'store-sqlite', 'store-fs', 'db-fs'],
    groups: [
      { id: 'g-io', label: 'I/O shell: files and SQLite', memberIds: ['c-db', 'c-store'], status: 'existing' },
      { id: 'g-pure', label: 'Pure core: bytes in, values out, no I/O', memberIds: ['c-wal', 'c-replay'], status: 'existing' },
    ],
    related: [{ diagramId: 'dyn-create', label: 'Dynamic: create --schema' }, { diagramId: 'dyn-exec', label: 'Dynamic: exec' }, { diagramId: 'dyn-clone', label: 'Dynamic: clone -n N' }, { diagramId: 'dyn-inspect', label: 'Dynamic: list, status, log' }],
    docIds: ['doc-components', 'doc-decisions', 'doc-known-gaps'], sourceIds: ['src-lib'] }),
  diagram({ id: 'code-wal', kind: 'code', title: 'wal: parser symbols', scopeId: 'c-wal', parent: { diagramId: 'components', elementId: 'c-wal' },
    nodes: ['k-wal-parse', 'k-wal-parse-from', 'k-wal-checksum', 'k-wal-wal', 'k-wal-commit', 'k-wal-frame', 'k-wal-resume', 'k-wal-encode'],
    edges: ['k-parse-checksum', 'k-parse-wal', 'k-wal-commits', 'k-commit-frames', 'k-encode-checksum', 'k-parsefrom-checksum', 'k-parsefrom-wal', 'k-wal-end', 'k-parsefrom-resume'],
    docIds: ['doc-wal-format'], sourceIds: ['src-wal'] }),
  diagram({ id: 'code-store', kind: 'code', title: 'store: Store, cursor rule and meta.db schema', scopeId: 'c-store', parent: { diagramId: 'components', elementId: 'c-store' },
    nodes: ['k-store', 'k-writelock', 'k-commits-seen', 'k-t-meta', 'k-t-commits', 'k-t-frames'],
    edges: ['k-store-lock', 'k-lock-meta', 'k-store-seen', 'k-store-meta', 'k-store-commits', 'k-store-frames', 'k-frames-commits'],
    docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  dynamic({ id: 'dyn-create', title: 'Dynamic: create NAME --schema FILE', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'host-fs', 'live-db', 'base-db', 'meta-db'], scenario: createScenario, docIds: ['doc-dyn-create'], sourceIds: ['src-main', 'src-db'] }),
  dynamic({ id: 'dyn-exec', title: 'Dynamic: exec with locks, recovery and incremental capture', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'c-wal', 'live-db', 'live-wal', 'meta-db'], scenario: execScenario, docIds: ['doc-dyn-exec'], sourceIds: ['src-db', 'src-store', 'src-wal', 'src-status'] }),
  dynamic({ id: 'dyn-clone', title: 'Dynamic: clone -n N with replay and db_pages resize', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'c-replay', 'meta-db', 'base-db', 'clone-file'], scenario: cloneScenario, docIds: ['doc-dyn-clone'], sourceIds: ['src-db', 'src-replay'] }),
  dynamic({ id: 'dyn-inspect', title: 'Dynamic: list, status and log', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'meta-db', 'live-wal', 'host-fs'], scenario: inspectScenario, docIds: ['doc-dyn-inspect'], sourceIds: ['src-db'] }),
  diagram({ id: 'deploy-dev', kind: 'deployment', title: 'Deployment: developer machine', environment: 'developer-machine', parent: { diagramId: 'containers-runtime' },
    nodes: ['dn-host', 'dn-cargo-bin', 'i-cli-installed', 'dn-target', 'i-cli-release', 'infra-make', 'dn-data-root', 'dn-db-folder', 'i-live-db', 'i-live-wal', 'i-base-db', 'i-meta-db', 'dn-tmp', 'i-clone', 'dn-nightly', 'i-fuzz'],
    edges: ['d-make-cli', 'd-make-fuzz', 'd-cli-live', 'd-cli-wal', 'd-cli-base', 'd-cli-meta', 'd-cli-clone', 'd-installed-live'],
    related: [{ diagramId: 'deploy-ci', label: 'CI deployment' }],
    docIds: ['doc-deploy-dev'], sourceIds: ['src-make', 'src-readme'] }),
  diagram({ id: 'deploy-ci', kind: 'deployment', title: 'Deployment: CI (GitHub Actions)', environment: 'ci-github', parent: { diagramId: 'containers-verification' },
    nodes: ['dn-ci', 'infra-ci-trigger', 'i-ci-tests'],
    edges: ['d-ci-tests'],
    docIds: ['doc-deploy-ci'], sourceIds: ['src-ci', 'src-ci-archive', 'src-status'] }),
]

// ---------- documents ----------
const doc = (id, title, plain, ste, sourceIds) => ({ id, title, plain, ste: { text: ste, issue: 'ASD-STE100 Issue 9', reviewStatus: 'draft' }, termIds: [], sourceIds })
const PLAN = REPO + 'docs/TESTING_PLAN.md'
const documents = [
  doc('doc-context', 'System context',
    'walflip is a local command-line tool. It has no server and no background process; each command is one OS process and all state lives under the data root.\n\nThe operator runs every write through `walflip exec`. A CI job or fuzzer operator is a secondary consumer that runs the test suites and the fuzzers.\n\nOther programs that open `live.db` are **not supported**. Their commits are picked up by the next `exec` as `(recovered)` commits, but a checkpoint they cause can discard WAL frames before walflip reads them (README, Limitations).',
    'walflip is a local command-line tool. Each command is one process. All data is in the data root.\n\nThe operator must send all writes through walflip exec. Other programs must not write live.db. If they write it, walflip does not record the change.',
    ['src-readme']),
  doc('doc-operator', 'Operator',
    'Selects a database with `--db NAME` or `WALFLIP_DB`, and a data root with `--root DIR` or `WALFLIP_ROOT` (default `walflip-data`, relative to the working directory).',
    'Use --db or WALFLIP_DB to select the database. Use --root or WALFLIP_ROOT to select the data root.',
    ['src-main']),
  doc('doc-cli', 'CLI binary',
    'Subcommands: `create NAME [--schema FILE|-]`, `list`, `rename OLD NEW`, `drop NAME`, `exec SQL | - | --script FILE`, `log [--limit N] [--head | --all]`, `status`, `clone -n N -o FILE`. Each arm of the `match` in `main` calls one `walflip::db` function and prints its result. `exec` prints a stderr warning when some commits were `(recovered)`. After `clone`, `report` calls `db::summarize` and prints `integrity_check` and row counts.',
    'The CLI reads the arguments. It calls one library function for each command. It prints the result. It does not contain logic.',
    ['src-main']),
  doc('doc-containers-runtime', 'Runtime containers and data stores',
    'Two code containers (the binary and the library crate) and five data stores. Four stores live in one folder per database, `ROOT/NAME/`: `live.db`, `live.db-wal`, `base.db` and `meta.db`. The fifth is the clone file at the `-o` path.\n\n`live.db-wal` is owned by SQLite. walflip only reads it, as raw bytes, from inside `exec`.',
    'The binary calls the library. The library reads and writes five files. Four files are in the database folder. The fifth file is the clone.\n\nSQLite writes live.db-wal. walflip only reads it.',
    ['src-db', 'src-store']),
  doc('doc-containers-verification', 'Tests, fuzzing and tooling',
    'Each suite is a separately runnable target. In-crate `#[cfg(test)]` modules test internals. `tests/roundtrip.rs` links the library and compares clones with live contents. `tests/cli.rs` and the `tests/scenarios_*.rs` suites run scenarios S1-S20 through `tests/common`: they spawn the compiled binary, use a shadow-SQLite `Model` as oracle and check the invariant catalog (INV-xx) through the library. `tests/library.rs` has library-level scenarios, `tests/perf.rs` a perf smoke test and `tests/fuzz_seed.rs` the corpus generator.\n\nThe fuzz crate has six targets: `wal_parse`, `wal_differential`, `replay_apply`, `store_capture`, `statements_split` and `exec_sql`. The last one runs structured SQL programs through the real `create` and `exec` with a `testing`-feature sandbox. `scripts/seed.sh` builds the 500-commit demo.\n\nCI runs `cargo test --features testing` only. Fuzzing is local (`make fuzz`, `make fuzz-replay`).',
    'Each test suite runs separately. The scenario suites run the binary and compare the result with a shadow database. The fuzz crate has six targets.\n\nCI runs cargo test. Fuzzing is a local task.',
    ['src-tests-cli', 'src-tests-roundtrip', 'src-fuzz', 'src-seed', 'src-plan']),
  doc('doc-components', 'Library components and the pure/I-O boundary',
    'Dependencies point one way: `main.rs` -> `db` -> `store` -> `wal`, and `db` -> `replay`. `wal` and `replay` import only `anyhow`; they take bytes or slices and return values. `store` and `db` own all file and SQLite I/O. `store::commits_seen` is a pure function inside the I/O module so the cursor rule can be property-tested alone.\n\n`main.rs` is shown as a supporting component of the CLI container. Tests and the fuzz crate also call the library directly (see the verification container view).',
    'Dependencies go in one direction: main.rs, then db, then store, then wal. db also uses replay.\n\nwal and replay do not read or write files. store and db do all file and SQLite operations.',
    ['src-lib', 'src-db', 'src-store', 'src-wal', 'src-replay']),
  doc('doc-c-db', 'db module',
    'Names must be 1-64 characters of ASCII letters, digits, `_` or `-`, because they become folder names. `parse_statements` splits stdin into one statement per non-blank line; `split_script` splits `--script` text with `sqlite3_complete`. `exec_with` takes the history and live write locks per statement, captures commits nobody recorded as `(recovered)`, and installs an authorizer that denies `ATTACH`, `DETACH`, `PRAGMA journal_mode` and, inside a statement, transaction control. A bare `VACUUM` takes a separate autocommit path. Every function returns a plain struct (`Created`, `DbSummary`, `ExecReport`, `LogEntry`, `Status`, `Summary`) or a count.',
    'A database name must have 1 to 64 letters, digits, underscores or hyphens. The name becomes a folder name. Each function returns data. It does not print.',
    ['src-db']),
  doc('doc-decisions', 'Architecture decisions and constraints',
    '- **Parse the WAL file directly instead of using SQLite hooks.** `store::capture` reads `live.db-wal` and `wal::parse` validates magic, page size (a power of two, 512 to 65536), salts and the chained checksum. Commits are frames up to one with non-zero `db_size`.\n- **Full-page replay.** Every WAL frame is a whole page, so `replay::apply` is positioned overwrites at `(pgno - 1) * page_size` on a copy of `base.db`, then a resize to commit N\'s `db_pages`. No SQL runs during replay.\n- **Library returns data, CLI prints.** `db` returns structs; `main.rs` only formats output. Tests call the library directly.\n- **Capture runs inside exec with autocheckpoint disabled.** `db::exec_with` sets `PRAGMA wal_autocheckpoint=0` and captures after every commit on the same connection, so SQLite cannot fold frames into `live.db` before they are read.\n- **Two locks, fixed order (Q1).** Per statement: `BEGIN IMMEDIATE` on `meta.db` (history lock), then on `live.db`. Both are SQLite file locks and vanish with the process. Concurrent `exec` processes serialise; a lock not obtained within 10 s is a clean `locked` error.\n- **Recovery (Q3).** With both locks held, commits in the WAL that history lacks are captured first under the label `(recovered)`, never attributed to the statement.\n- **Statements may not control the transaction (Q2); ATTACH, DETACH and `PRAGMA journal_mode` are denied (Q8).** An authorizer enforces this. As a backstop, a failed statement that changed the WAL is still captured, and more than one commit per statement is an error.\n- **VACUUM (Q4).** Exactly `VACUUM` or `VACUUM main` runs in autocommit under the history lock and is captured as one commit. Other forms are refused.\n- **Schema first, then WAL (Q7).** `create` runs the schema in rollback mode so `page_size` and `auto_vacuum` take effect, then enables WAL and checkpoints.\n- **Cursor = salts + commit count + resume state.** `cursor_salt` and `cursor_commits` identify the WAL generation and how many commits are stored. `cursor_resume` adds the header, offset and running checksum so a capture parses only the new tail; it is used only while it agrees with the salt and count, else a full parse runs.\n- **Read paths do not write (Q11).** `list`, `status`, `log` and `clone` use `Store::open_existing`: no `meta.db` is created and no schema runs.\n- **page_size pinned at create.** `capture` rejects a WAL with a different page size because replay offsets depend on it.\n- **Replay allocation bound.** `clone_at` passes `max_pages = max(base pages, max db_pages over 1..=N)`; `apply` rejects any frame or size above it before changing the image.\n- **One statement = one transaction = at most one commit.** A statement that changes nothing writes no frames and adds no commit.',
    '- walflip reads the WAL file directly. It does not use SQLite hooks.\n- Each WAL frame is a full page. Replay writes each page at its offset. Replay does not run SQL.\n- The library returns data. The CLI prints the data.\n- exec stops automatic checkpoints. exec reads the WAL after each commit.\n- exec takes the history lock and then the live lock for each statement. Two exec commands wait for each other.\n- A commit that is in the WAL but not in the history is recorded as (recovered).\n- A statement cannot use BEGIN, COMMIT or ROLLBACK. ATTACH and DETACH are not allowed.\n- The cursor is the WAL salts, the number of commits and a resume state. A new salt sets the number to 0.\n- create records the page size. capture stops with an error if the WAL page size is different.',
    ['src-store', 'src-wal', 'src-replay', 'src-db', 'src-main']),
  doc('doc-known-gaps', 'Known limits and fixed defects',
    'The testing plan ([docs/TESTING_PLAN.md](\' + PLAN + \')) lists defects B1-B10. **All are fixed**, each with a regression test (see docs/WAVE_STATUS.md). The model in this viewer was re-checked against the code on 2026-10-15.\n\n- B1, B10: `summarize` quoted table names and the clone path correctly. B9: `clone` opens the output with `create_new`.\n- B2: transaction control inside a statement is rejected by the authorizer.\n- B3, B4: per-statement history lock; orphan commits are captured as `(recovered)`.\n- B5, B7, B8: schema runs before WAL; `PRAGMA journal_mode` is denied; `page_size` and `auto_vacuum` take effect; an unterminated `BEGIN` has its own error.\n- B6: `replay::apply` has the `max_pages` bound.\n\n**Known limits (by design):**\n- A writer that does not use walflip\'s locks can commit between a statement\'s commit and its capture. `exec` captures everything and fails with "statement produced N commits".\n- If outside commits are checkpointed and the WAL restarted before any `exec` runs, they are in `live.db` but not in history.\n- Incremental capture assumes the WAL is append-only within a generation.\n- Sector-padding frames from a foreign writer appear as `(recovered)` commits (Q6).\n- `exec -` splits on lines, not on `;`; use `exec --script` for multi-line SQL (Q5).\n- `clone` builds the image in memory, so it needs RAM about the size of the database.',
    'The testing plan lists defects B1 to B10. All defects are fixed. Each fix has a test.\n\nKnown limits:\n- A writer that does not use walflip can commit between a statement and its capture. exec then stops with an error.\n- exec - splits on lines. Use exec --script for SQL on many lines.\n- clone needs memory about the size of the database.',
    ['src-plan']),
  doc('doc-wal-format', 'WAL file format as parsed by wal::parse',
    'All header integers are big-endian u32. The magic only selects the byte order of the checksum words.\n\n**Header, 32 bytes:** 0 magic (`0x377f0682` little-endian checksums, `0x377f0683` big-endian); 4 format version (not checked; encoder writes 3007000); 8 page size (a power of two from 512 to 65536); 12 checkpoint sequence (not checked); 16 salt-1; 20 salt-2; 24 checksum-1; 28 checksum-2 (over bytes 0-23).\n\n**Frame, 24 + page_size bytes:** 0 page number; 4 db size in pages if this is a commit frame, else 0; 8 salt-1; 12 salt-2; 16 checksum-1; 20 checksum-2; 24 page image. The checksum covers frame bytes 0-7 then the page, chained from the previous frame (or the header).\n\n**Outcomes:** buffer shorter than 32 bytes or bad header checksum gives `Ok(None)`; bad magic or page size gives `Err`; the frame loop stops at the first salt mismatch (older generation), checksum mismatch or partial frame; frames after the last commit frame are dropped.\n\n**Resuming:** `Wal::end` is a `Resume` (header, offset after the last commit frame, running checksum, commit count). `parse_from` continues from it with only the file tail, starting at the last commit frame, whose header is re-checked against the stored checksum.',
    'The header has 32 bytes. It has the magic number, the page size, two salts and a checksum.\n\nEach frame has a 24-byte header and one page. The frame header has the page number, the database size, the salts and a checksum.\n\nA frame with a database size that is not zero ends a commit. The parser stops at the first bad salt or bad checksum.',
    ['src-wal']),
  doc('doc-meta-schema', 'meta.db schema',
    '- `meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)`: keys `page_size`, `cursor_salt` (`salt1:salt2`), `cursor_commits`, `cursor_resume` (`v1 <header hex> <offset> <cksum0> <cksum1> <commits>`).\n- `commits(idx INTEGER PRIMARY KEY, label TEXT NOT NULL, db_pages INTEGER NOT NULL, committed_at TEXT DEFAULT CURRENT_TIMESTAMP)`: `idx` is 1-based; `label` is the SQL statement or `(recovered)`.\n- `frames(commit_idx INTEGER NOT NULL REFERENCES commits(idx), seq INTEGER NOT NULL, pgno INTEGER NOT NULL, page BLOB NOT NULL, PRIMARY KEY(commit_idx, seq))`.\n\n`Store::open` runs `CREATE TABLE IF NOT EXISTS` and is used by `create` and `exec`. The read paths (`list`, `status`, `log`, `clone`) use `Store::open_existing`, which neither creates the file nor runs the schema; `list` skips folders without `meta.db`. Each capture runs inside the history write lock (`BEGIN IMMEDIATE`) and in a savepoint, so a failed capture leaves history and the cursor unchanged.',
    'meta.db has three tables: meta, commits and frames. meta keeps the page size and the cursor. commits has one row for each commit. frames keeps the page images.\n\nEach capture runs inside the history lock. A failed capture changes nothing.',
    ['src-store']),
  doc('doc-dyn-create', 'create --schema',
    'The name is validated before the schema file is read. The schema runs **first**, in rollback mode, under an authorizer that denies `ATTACH`, `DETACH` and `PRAGMA journal_mode`; a schema that leaves a transaction open is an error. Then WAL mode is switched on and the WAL is checkpointed with `TRUNCATE` so `base.db` is a complete, WAL-free copy. `page_size` and `auto_vacuum` in the schema therefore take effect. If any step fails, `create` removes the whole folder.\n\nFailure cases: folder already exists; schema SQL error; unterminated `BEGIN`; denied statement (folder removed).',
    'create checks the name first. Then it reads the schema and runs it before WAL mode. Then it sets WAL mode and copies live.db to base.db. If a step fails, create removes the folder.',
    ['src-db', 'src-main', 'src-plan']),
  doc('doc-dyn-exec', 'exec: locks, recovery and incremental capture',
    'For each statement: take the history lock (`BEGIN IMMEDIATE` on `meta.db`), then the live lock (`BEGIN IMMEDIATE` on `live.db`). Both are released at the end of the statement, so concurrent `exec` processes interleave statement by statement.\n\n**Recovery:** if the WAL changed since this process\'s last capture, commits the history lacks are captured first as `(recovered)`. They are never attributed to the statement.\n\n**Run and capture:** the statement runs with transaction control denied and commits. Capture runs on the same open connection with automatic checkpointing off, so every committed frame is still in the WAL. The cursor makes capture idempotent: `seen = commits_seen(cursor_salt, cursor_commits, wal.salt)`. When `cursor_resume` agrees with the cursor, only the WAL tail from the stored offset is read and parsed (`parse_from`); otherwise the whole file is parsed. New commits and the new cursor are written in one savepoint, then the history lock is released.\n\n**VACUUM:** exactly `VACUUM` or `VACUUM main` cannot run in a transaction, so after the recovery capture walflip releases its live transaction and runs `VACUUM` in autocommit under the history lock. It is captured as one commit.\n\nWhen `exec` returns the connection closes; SQLite normally checkpoints and removes the WAL, and the next `exec` starts a new generation with new salts, so the count restarts at 0.\n\nFailure cases: unknown `--db`; lock not obtained within the timeout (`locked`, nothing changed); SQL error or denied statement (earlier statements stay committed and captured, and the error names how many); a statement that produced more than one commit; WAL page size differs from `meta.page_size`.',
    'For each statement, exec takes the history lock and then the live lock. First it records commits that are in the WAL but not in the history, as (recovered). Then it runs the statement. Then it reads the new part of the WAL and records the new commits. The cursor tells which commits are already recorded. Thus a second read adds nothing.\n\nTwo exec commands on one database wait for each other.',
    ['src-db', 'src-store', 'src-wal', 'src-plan']),
  doc('doc-dyn-clone', 'clone -n N: replay and db_pages resize',
    '`clone_at` opens `meta.db` with `Store::open_existing`, so a database with no history is an error and nothing is created. It loads frames of commits 1..=N in `(commit_idx, seq)` order, so later writes to a page win. `replay::apply` validates every frame before changing anything (page 0, page above `max_pages`, wrong length, `db_pages` above `max_pages` all give `Err`). After the overwrites it resizes the image to commit N\'s `db_pages`, which shrinks it when a later commit freed pages. For N = 0 no frames apply and the result is `base.db`.\n\nThe output is opened with `create_new`, so a racing clone cannot overwrite another\'s file. The image is built in memory, so clone needs RAM roughly the size of the database. The report opens the clone read-only (`immutable=1`, path percent-encoded).\n\nFailure cases: output exists; N greater than the commit count; no recorded page size.',
    'clone reads base.db into memory. It writes each stored page at its offset, in commit order. Then it sets the size to the size of commit N. Then it writes the file and checks it.',
    ['src-db', 'src-replay', 'src-plan']),
  doc('doc-dyn-inspect', 'list, status and log',
    'These three commands are independent; the scenario shows them as unordered branches. All are read-only: they use `Store::open_existing`, which does not create `meta.db` and does not run the schema. `list` skips folders that lack `meta.db`. `status` reports `live wal: absent (checkpointed)` when `live.db-wal` does not exist, which is the normal state between commands, and `uncaptured: N` when the WAL holds commits that history lacks (a hint, read without the lock). `log` shows the most recent N commits, oldest first; `--head` shows the first N and `--all` shows all. A label with line breaks is shown on one row.',
    'list, status and log only read the history. status shows if live.db-wal exists. Usually it does not exist between commands.',
    ['src-db', 'src-main']),
  doc('doc-deploy-dev', 'Deployment: developer machine',
    'Install with `cargo install --path .` (Rust 1.85+, Unix-like OS; SQLite is bundled; the test suite needs Rust 1.88+). Make targets: `make build` (`cargo build --release`), `make test` (`cargo test --features testing`), `make fuzz` (each of the six fuzz targets for `FUZZ_SECS`, default 30 s, needs nightly and cargo-fuzz), `make fuzz-replay` (replay the committed corpora once), `make seed` (build, then `scripts/seed.sh` with `WALFLIP=target/release/walflip`), `make demo` (seed, then clone at 250 and 500 into `/tmp`), `make clean`, `make distclean`.\n\nThe data root defaults to `walflip-data/` in the working directory, so the installed binary and the release binary share data only when run from the same directory.',
    'Install walflip with cargo install. Use make build, make test, make fuzz, make fuzz-replay, make seed and make demo. make fuzz needs the nightly Rust toolchain and cargo-fuzz.',
    ['src-make', 'src-readme', 'src-seed']),
  doc('doc-deploy-ci', 'Deployment: CI (GitHub Actions)',
    '`.github/workflows/ci.yml` is the only active workflow. It runs on pull requests and pushes to `main`, on `ubuntu-24.04` (pinned), and does `cargo test --features testing`.\n\nThe nightly fuzz workflow and the PR-time fuzz build + corpus replay job were archived on request (`archive/ci/fuzz-nightly.yml`, `archive/ci/fuzz-build-replay.yml`). Fuzzing is local.',
    'CI runs cargo test on each pull request and each push to main. The fuzz workflows are archived. Fuzzing is a local task.',
    ['src-plan']),
]

const bundle = {
  schemaVersion: 1,
  id: 'walflip',
  title: 'walflip architecture',
  revision: '2026-10-15',
  defaultDiagramId: 'context',
  model: { elements, relationships },
  diagrams,
  documents,
  sources,
}
writeFileSync(out, JSON.stringify(bundle, null, 2) + '\n')
console.log('wrote', out, elements.length, 'elements', relationships.length, 'relationships', diagrams.length, 'diagrams')
