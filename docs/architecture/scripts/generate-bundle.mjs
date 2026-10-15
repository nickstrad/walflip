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
  'src-tests-roundtrip': ['tests/roundtrip.rs', 'Round-trip property tests'],
  'src-fuzz': ['fuzz/fuzz_targets/wal_parse.rs', 'wal_parse fuzz target'],
  'src-fuzz-cargo': ['fuzz/Cargo.toml', 'Fuzz crate manifest'],
  'src-seed': ['scripts/seed.sh', 'Seed script'],
  'src-example': ['examples/processes.sql', 'Example schema'],
  'src-make': ['Makefile', 'Make targets'],
  'src-plan': ['docs/TESTING_PLAN.md', 'Testing plan (bug IDs B1-B8, invariants INV-xx)'],
}
const sources = Object.entries(srcFiles).map(([id, [path, title]]) => ({
  id, title, url: REPO + path, path,
  notes: 'Read from the local checkout on 2026-10-14. The GitHub URL assumes the repository is pushed to main.',
}))

// ---------- elements ----------
const el = (o) => ({ status: 'existing', tags: [], plannedNotes: [], docIds: [], sourceIds: [], ...o })
const elements = [
  // people
  el({ id: 'operator', kind: 'person', name: 'Operator / developer', description: 'Creates databases, runs every write through walflip exec, inspects history and clones past states. May set WALFLIP_ROOT and WALFLIP_DB.', docIds: ['doc-operator'], sourceIds: ['src-readme', 'src-main'] }),
  el({ id: 'ci-operator', kind: 'person', name: 'CI job or fuzzer operator', description: 'Secondary consumer. Runs cargo test and cargo +nightly fuzz. No CI workflow file exists in the repo yet.', sourceIds: ['src-make', 'src-plan'] }),
  // systems
  el({ id: 'walflip', kind: 'softwareSystem', name: 'walflip', description: 'Captures every committed SQLite transaction as WAL page images and rebuilds a database as of any commit.', technology: 'Rust 2021 CLI + library', docIds: ['doc-decisions', 'doc-known-gaps'], sourceIds: ['src-readme', 'src-cargo', 'src-lib'] }),
  el({ id: 'host-fs', kind: 'softwareSystem', name: 'Local disk (SQLite files)', description: 'The host filesystem. Holds the data root (default walflip-data/ relative to the working directory) and the clone files.', technology: 'POSIX filesystem, std::fs', tags: ['external'], sourceIds: ['src-db', 'src-main'] }),
  el({ id: 'other-writers', kind: 'softwareSystem', name: 'Other programs that open live.db', description: 'Unsupported. Their writes are not captured, and their checkpoints can fold WAL frames into live.db before walflip reads them.', technology: 'Any SQLite writer', tags: ['external', 'unsupported'], sourceIds: ['src-readme', 'src-plan'] }),
  el({ id: 'sqlite-clients', kind: 'softwareSystem', name: 'Any SQLite client', description: 'Opens a clone file. A clone is a standalone SQLite database with no WAL.', technology: 'SQLite file format', tags: ['external'], sourceIds: ['src-readme'] }),
  el({ id: 'sqlite-engine', kind: 'softwareSystem', name: 'SQLite engine (bundled)', description: 'Third-party SQL engine compiled into the binary through rusqlite feature bundled. Owns journal_mode, checkpoints and the WAL writer.', technology: 'rusqlite 0.32, bundled SQLite', tags: ['external'], sourceIds: ['src-cargo'] }),

  // containers: runtime
  el({ id: 'cli', kind: 'container', parentId: 'walflip', name: 'walflip CLI binary', description: 'Parses arguments and environment, calls one library operation per command and prints the returned data. No logic of its own.', technology: 'Rust, clap 4 (derive, env)', docIds: ['doc-cli'], sourceIds: ['src-main'] }),
  el({ id: 'lib', kind: 'container', parentId: 'walflip', name: 'walflip library crate', description: 'Modules wal, replay, store and db. Returns plain data; never prints. Also linked directly by the test suites and the fuzz crate.', technology: 'Rust, rusqlite 0.32 (bundled), anyhow', docIds: ['doc-decisions'], sourceIds: ['src-lib'] }),
  el({ id: 'live-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'live.db', description: 'The database you write to, in WAL journal mode. Only walflip exec may write it.', technology: 'SQLite, journal_mode=WAL', tags: ['per-database-folder'], sourceIds: ['src-db'] }),
  el({ id: 'live-wal', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'live.db-wal', description: 'SQLite write-ahead log beside live.db. walflip reads it as raw bytes after each commit. Absent after a checkpoint on close.', technology: 'SQLite WAL file format', tags: ['per-database-folder', 'transient'], docIds: ['doc-wal-format'], sourceIds: ['src-wal', 'src-store'] }),
  el({ id: 'base-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'base.db', description: 'Snapshot copied from live.db at create, after the schema ran and the WAL was checkpointed. clone -n 0 returns it.', technology: 'SQLite file (byte copy)', tags: ['per-database-folder'], sourceIds: ['src-db'] }),
  el({ id: 'meta-db', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'meta.db', description: 'History store: tables meta (page_size, cursor_salt, cursor_commits), commits and frames (full page images).', technology: 'SQLite: meta, commits, frames', tags: ['per-database-folder'], docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  el({ id: 'clone-file', kind: 'container', containerType: 'datastore', parentId: 'walflip', name: 'Clone output file', description: 'Database as of commit N at the -o path. walflip refuses to overwrite an existing file.', technology: 'SQLite file, std::fs::write', sourceIds: ['src-db'] }),

  // containers: verification and tooling
  el({ id: 'unit-tests', kind: 'container', parentId: 'walflip', name: 'In-crate unit and property tests', description: '#[cfg(test)] modules in wal, replay, store and db: WAL round-trip, truncation, corruption, no-panic; replay page-map model, composition, idempotence, max_pages bound; cursor cases; name validation.', technology: 'cargo test, proptest 1', sourceIds: ['src-wal', 'src-replay', 'src-store', 'src-db'] }),
  el({ id: 'roundtrip-tests', kind: 'container', parentId: 'walflip', name: 'tests/roundtrip.rs', description: 'Random insert/update/delete sequences on a real database; asserts clone -n k equals live contents after step k, single-WAL batches and idempotent capture.', technology: 'proptest (24 cases), tempfile; links the library', sourceIds: ['src-tests-roundtrip'] }),
  el({ id: 'cli-tests', kind: 'container', parentId: 'walflip', name: 'tests/cli.rs + tests/common', description: 'End-to-end scenarios that spawn the compiled binary through Harness, compare against a shadow SQLite Model and check the invariant catalog through the library.', technology: 'std::process::Command, CARGO_BIN_EXE_walflip, rusqlite', sourceIds: ['src-tests-cli', 'src-tests-common', 'src-tests-invariants', 'src-tests-model'] }),
  el({ id: 'fuzz', kind: 'container', parentId: 'walflip', name: 'walflip-fuzz crate', description: 'cargo-fuzz crate with one target, wal_parse: raw bytes into wal::parse, asserting no panic and internal consistency. Seeded corpus in fuzz/corpus/wal_parse.', technology: 'libfuzzer-sys 0.4, Rust nightly, walflip feature testing', status: 'mixed', plannedNotes: ['Planned by TESTING_PLAN.md section 2.1, not present: wal_differential, replay_apply, store_capture and statements_split targets, plus fuzz/wal.dict.'], sourceIds: ['src-fuzz', 'src-fuzz-cargo', 'src-plan'] }),
  el({ id: 'seed-script', kind: 'container', parentId: 'walflip', name: 'scripts/seed.sh + examples/processes.sql', description: 'Demo tooling. Samples ps twice, generates 250 INSERT, 150 UPDATE and 100 DELETE statements, and feeds them to the binary. Fails unless status reports 500 commits.', technology: 'bash, awk, ps', sourceIds: ['src-seed', 'src-example', 'src-make'] }),

  // components of lib
  el({ id: 'c-db', kind: 'component', parentId: 'lib', name: 'db', description: 'I/O shell. Db paths and name validation, parse_statements, create, list, rename, drop_db, exec, log, status, clone_at, summarize. Returns structs; never prints.', technology: 'Rust module, rusqlite, std::fs', tags: ['io'], docIds: ['doc-c-db'], sourceIds: ['src-db'] }),
  el({ id: 'c-store', kind: 'component', parentId: 'lib', name: 'store', description: 'I/O shell around meta.db. Store::open creates the schema; capture reads the WAL file, parses it and inserts new commits atomically. Contains the pure function commits_seen.', technology: 'Rust module, rusqlite, std::fs', tags: ['io'], docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  el({ id: 'c-wal', kind: 'component', parentId: 'lib', name: 'wal', description: 'Pure WAL parser: header, byte order, salts, chained checksums, commit grouping. Borrows pages from the input buffer. Reference encoder behind cfg(test) or feature testing.', technology: 'Rust module, no I/O', tags: ['pure'], docIds: ['doc-wal-format'], sourceIds: ['src-wal'] }),
  el({ id: 'c-replay', kind: 'component', parentId: 'lib', name: 'replay', description: 'Pure page replay onto an in-memory image: validate all frames against max_pages first, overwrite at (pgno-1)*page_size, then resize to db_pages.', technology: 'Rust module, no I/O', tags: ['pure'], sourceIds: ['src-replay'] }),
  // component of cli (shown as supporting in the library view)
  el({ id: 'c-main', kind: 'component', parentId: 'cli', name: 'main.rs (clap layer)', description: 'Cli and Cmd clap types, main dispatch, read_schema (file or stdin) and report. Output formatting only. Shown as a supporting component from the CLI container.', technology: 'Rust, clap 4 derive, anyhow', tags: ['external', 'io'], sourceIds: ['src-main'] }),

  // code: wal
  el({ id: 'k-wal-parse', kind: 'code', parentId: 'c-wal', symbolKind: 'function', name: 'parse', description: 'Returns Ok(None) for a short buffer or bad header checksum, Err for bad magic or page size, else the committed prefix.', members: [{ name: 'parse', signature: "pub fn parse(buf: &[u8]) -> Result<Option<Wal<'_>>>" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-checksum', kind: 'code', parentId: 'c-wal', symbolKind: 'function', name: 'checksum', description: 'SQLite running checksum over 8-byte chunks; byte order chosen by the magic number.', members: [{ name: 'checksum', signature: 'fn checksum(big_endian: bool, data: &[u8], s0: u32, s1: u32) -> (u32, u32)', visibility: 'private' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-wal', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Wal', description: 'Parsed WAL generation.', members: [{ name: 'page_size', signature: 'u32' }, { name: 'salt', signature: '(u32, u32)' }, { name: 'commits', signature: "Vec<Commit<'a>>" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-commit', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Commit', description: 'Frames up to and including a commit frame.', members: [{ name: 'frames', signature: "Vec<Frame<'a>>" }, { name: 'db_pages', signature: 'u32 (db size after the commit)' }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-frame', kind: 'code', parentId: 'c-wal', symbolKind: 'type', name: 'Frame', description: 'One page image borrowed from the WAL buffer.', members: [{ name: 'pgno', signature: 'u32' }, { name: 'page', signature: "&'a [u8]" }], sourceIds: ['src-wal'] }),
  el({ id: 'k-wal-encode', kind: 'code', parentId: 'c-wal', symbolKind: 'module', name: 'encode', description: 'Reference WAL encoder used as a test oracle. Compiled only with cfg(test) or feature testing (the fuzz crate enables it).', tags: ['testing-feature'], members: [{ name: 'encode', signature: 'pub fn encode(big_endian: bool, salt: (u32, u32), page_size: u32, commits: &[Commit]) -> Vec<u8>' }], sourceIds: ['src-wal', 'src-cargo'] }),

  // code: store
  el({ id: 'k-store', kind: 'code', parentId: 'c-store', symbolKind: 'class', name: 'Store', description: 'Owns the meta.db connection.', members: [{ name: 'open', signature: 'pub fn open(path: &Path) -> Result<Self>' }, { name: 'get / set', signature: 'meta key-value access' }, { name: 'page_size', signature: 'pub fn page_size(&self) -> Result<Option<u64>>' }, { name: 'commit_count', signature: 'pub fn commit_count(&self) -> Result<u64>' }, { name: 'capture', signature: 'pub fn capture(&mut self, wal_path: &Path, label: &str) -> Result<usize>' }, { name: 'conn', signature: 'pub fn conn(&self) -> &Connection' }], sourceIds: ['src-store'] }),
  el({ id: 'k-commits-seen', kind: 'code', parentId: 'c-store', symbolKind: 'function', name: 'commits_seen', description: 'Pure cursor rule: same salt returns the stored count; different salt, missing or junk returns 0.', tags: ['pure'], members: [{ name: 'commits_seen', signature: 'pub fn commits_seen(stored_salt: Option<&str>, stored_commits: Option<&str>, salt: (u32, u32)) -> usize' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-meta', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'meta', description: 'Key-value facts. Keys used: page_size, cursor_salt (salt1:salt2), cursor_commits.', members: [{ name: 'key', signature: 'TEXT PRIMARY KEY' }, { name: 'value', signature: 'TEXT NOT NULL' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-commits', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'commits', description: 'One row per captured commit, 1-based.', members: [{ name: 'idx', signature: 'INTEGER PRIMARY KEY' }, { name: 'label', signature: 'TEXT NOT NULL (the SQL statement)' }, { name: 'db_pages', signature: 'INTEGER NOT NULL' }, { name: 'committed_at', signature: 'TEXT DEFAULT CURRENT_TIMESTAMP' }], sourceIds: ['src-store'] }),
  el({ id: 'k-t-frames', kind: 'code', parentId: 'c-store', symbolKind: 'table', name: 'frames', description: 'Full page images in commit order.', members: [{ name: 'commit_idx', signature: 'INTEGER NOT NULL REFERENCES commits(idx)' }, { name: 'seq', signature: 'INTEGER NOT NULL' }, { name: 'pgno', signature: 'INTEGER NOT NULL' }, { name: 'page', signature: 'BLOB NOT NULL' }, { name: 'PRIMARY KEY', signature: '(commit_idx, seq)' }], sourceIds: ['src-store'] }),

  // deployment: developer machine
  ...(() => {
    const env = 'developer-machine'
    const dn = (id, name, description, technology, parentId) => el({ id, kind: 'deploymentNode', name, description, technology, environment: env, ...(parentId ? { parentId } : {}), sourceIds: ['src-make', 'src-readme'] })
    const inst = (id, name, description, instanceOf, parentId) => el({ id, kind: 'instance', name, description, instanceOf, parentId, environment: env, sourceIds: ['src-make', 'src-readme'] })
    return [
      dn('dn-host', 'Developer machine', 'Unix-like OS with a Rust toolchain 1.75+. SQLite is bundled, so no system SQLite is needed.', 'macOS or Linux, cargo'),
      dn('dn-cargo-bin', '~/.cargo/bin', 'Install target of cargo install --path .', 'cargo install', 'dn-host'),
      inst('i-cli-installed', 'walflip (installed)', 'Binary on PATH for everyday use.', 'cli', 'dn-cargo-bin'),
      dn('dn-target', 'target/release', 'Output of cargo build --release (make build). make seed and make demo use this binary.', 'cargo build --release', 'dn-host'),
      inst('i-cli-release', 'walflip (target/release)', 'Binary that the Makefile and seed script run.', 'cli', 'dn-target'),
      el({ id: 'infra-make', kind: 'infrastructureNode', parentId: 'dn-host', name: 'Makefile targets', description: 'build, test, fuzz (FUZZ_SECS, default 30), seed, demo, clean, distclean.', technology: 'make', sourceIds: ['src-make'] }),
      dn('dn-data-root', 'walflip-data/', 'Data root. --root or WALFLIP_ROOT; the default is relative to the working directory. Ignored by git.', 'Directory on local disk', 'dn-host'),
      dn('dn-db-folder', 'walflip-data/NAME/', 'One folder per database. make seed creates processes/.', 'Directory', 'dn-data-root'),
      inst('i-live-db', 'live.db', 'Live database file.', 'live-db', 'dn-db-folder'),
      inst('i-live-wal', 'live.db-wal', 'Present only while a connection is open or after an unclean close.', 'live-wal', 'dn-db-folder'),
      inst('i-base-db', 'base.db', 'Base snapshot.', 'base-db', 'dn-db-folder'),
      inst('i-meta-db', 'meta.db', 'History store.', 'meta-db', 'dn-db-folder'),
      dn('dn-tmp', '/tmp', 'make demo writes /tmp/at250.db and /tmp/at500.db; make clean removes them.', 'Directory', 'dn-host'),
      inst('i-clone', 'at250.db / at500.db', 'Clones produced by make demo.', 'clone-file', 'dn-tmp'),
      dn('dn-nightly', 'Rust nightly + cargo-fuzz', 'Needed only for make fuzz. Build output in fuzz/target, crashes in fuzz/artifacts (both ignored by git).', 'cargo +nightly fuzz', 'dn-host'),
      inst('i-fuzz', 'wal_parse fuzz target', 'Runs with -max_total_time=FUZZ_SECS.', 'fuzz', 'dn-nightly'),
    ]
  })(),
  // deployment: planned CI
  ...(() => {
    const env = 'ci-planned'
    const note = ['Planned in TESTING_PLAN.md section 2.4. The repository has no CI workflow file yet.']
    return [
      el({ id: 'dn-ci', kind: 'deploymentNode', name: 'CI runner (planned)', description: 'Planned runner for PR and nightly jobs.', technology: 'Unspecified CI, Rust stable + nightly', environment: env, status: 'planned', plannedNotes: note, sourceIds: ['src-plan'] }),
      el({ id: 'infra-ci-schedule', kind: 'infrastructureNode', parentId: 'dn-ci', name: 'Nightly and PR triggers (planned)', description: 'Nightly: each fuzz target for 300 s with -rss_limit_mb=1024. PR: cargo fuzz build and corpus replay with -runs=0.', technology: 'CI scheduler', status: 'planned', plannedNotes: note, sourceIds: ['src-plan'] }),
      el({ id: 'i-ci-fuzz', kind: 'instance', parentId: 'dn-ci', instanceOf: 'fuzz', environment: env, name: 'Fuzz targets (planned job)', description: 'wal_parse today; more targets planned.', status: 'planned', plannedNotes: note, sourceIds: ['src-plan'] }),
      el({ id: 'i-ci-tests', kind: 'instance', parentId: 'dn-ci', instanceOf: 'cli-tests', environment: env, name: 'cargo test (planned job)', description: 'Unit, property, round-trip and end-to-end suites.', status: 'planned', plannedNotes: note, sourceIds: ['src-plan'] }),
    ]
  })(),
]

// ---------- relationships ----------
const rel = (id, sourceId, targetId, label, technology, extra = {}) => ({ id, sourceId, targetId, label, technology, status: 'existing', plannedNotes: [], docIds: [], sourceIds: [], ...extra })
const relationships = [
  // context
  rel('ctx-op-wf', 'operator', 'walflip', 'Creates dbs, runs SQL, inspects history, clones commits', 'CLI args, env, stdin, stdout', { sourceIds: ['src-main'] }),
  rel('ctx-ci-wf', 'ci-operator', 'walflip', 'Runs tests and fuzzing', 'cargo test, cargo +nightly fuzz', { sourceIds: ['src-make'] }),
  rel('ctx-wf-fs', 'walflip', 'host-fs', 'Stores live db, snapshot and history; writes clones', 'SQLite via rusqlite, std::fs', { sourceIds: ['src-db'] }),
  rel('ctx-other-fs', 'other-writers', 'host-fs', 'Unsupported: writes live.db directly, not captured', 'SQLite', { sourceIds: ['src-readme'] }),
  rel('ctx-client-fs', 'sqlite-clients', 'host-fs', 'Opens clone files', 'SQLite file format', { sourceIds: ['src-readme'] }),
  // containers runtime
  rel('op-cli', 'operator', 'cli', 'Runs commands', 'argv, WALFLIP_ROOT, WALFLIP_DB, stdin', { sourceIds: ['src-main'] }),
  rel('cli-lib', 'cli', 'lib', 'Calls one db operation per command; prints returned data', 'Rust calls (walflip::db)', { sourceIds: ['src-main'] }),
  rel('lib-live', 'lib', 'live-db', 'Runs each statement as one transaction, autocheckpoint off', 'rusqlite Connection', { sourceIds: ['src-db'] }),
  rel('lib-wal', 'lib', 'live-wal', 'Reads raw WAL bytes after each commit', 'std::fs::read', { sourceIds: ['src-store'] }),
  rel('lib-base', 'lib', 'base-db', 'Copies at create; reads as replay base at clone', 'std::fs::copy, std::fs::read', { sourceIds: ['src-db'] }),
  rel('lib-meta', 'lib', 'meta-db', 'Stores commits, frames and cursor; reads them back', 'rusqlite, SQL', { sourceIds: ['src-store', 'src-db'] }),
  rel('lib-clone', 'lib', 'clone-file', 'Writes rebuilt image; checks it read-only', 'std::fs::write, SQLite immutable=1', { sourceIds: ['src-db'] }),
  rel('live-appends-wal', 'live-db', 'live-wal', 'SQLite appends page frames on commit', 'SQLite WAL writer', { sourceIds: ['src-wal'] }),
  rel('other-live', 'other-writers', 'live-db', 'Unsupported direct writes and checkpoints', 'SQLite', { sourceIds: ['src-readme'] }),
  rel('client-clone', 'sqlite-clients', 'clone-file', 'Opens the rebuilt database', 'SQLite file format', { sourceIds: ['src-readme'] }),
  // containers verification
  rel('ci-unit', 'ci-operator', 'unit-tests', 'Runs', 'cargo test (make test)', { sourceIds: ['src-make'] }),
  rel('ci-roundtrip', 'ci-operator', 'roundtrip-tests', 'Runs', 'cargo test', { sourceIds: ['src-make'] }),
  rel('ci-cli-tests', 'ci-operator', 'cli-tests', 'Runs', 'cargo test', { sourceIds: ['src-make'] }),
  rel('ci-fuzz', 'ci-operator', 'fuzz', 'Runs wal_parse for FUZZ_SECS', 'cargo +nightly fuzz run (make fuzz)', { sourceIds: ['src-make'] }),
  rel('op-seed', 'operator', 'seed-script', 'Seeds a 500-commit demo database', 'make seed, make demo', { sourceIds: ['src-make'] }),
  rel('unit-lib', 'unit-tests', 'lib', 'Tests module internals in place', '#[cfg(test)], proptest', { sourceIds: ['src-wal', 'src-replay'] }),
  rel('roundtrip-lib', 'roundtrip-tests', 'lib', 'Calls create, exec, clone_at, summarize directly', 'Rust calls, tempfile', { sourceIds: ['src-tests-roundtrip'] }),
  rel('clitests-cli', 'cli-tests', 'cli', 'Drives the compiled binary', 'std::process::Command', { sourceIds: ['src-tests-common'] }),
  rel('clitests-lib', 'cli-tests', 'lib', 'Checks invariants via db::list and clone_at; reads meta.db read-only', 'Rust calls, rusqlite immutable=1', { sourceIds: ['src-tests-invariants'] }),
  rel('clitests-seed', 'cli-tests', 'seed-script', 'Runs the seed script (scenario 11)', 'bash', { sourceIds: ['src-tests-cli'] }),
  rel('fuzz-lib', 'fuzz', 'lib', 'Feeds raw bytes to wal::parse', 'libFuzzer, feature testing', { sourceIds: ['src-fuzz'] }),
  rel('seed-cli', 'seed-script', 'cli', 'Runs drop, create --schema, exec -, status', 'bash subprocess', { sourceIds: ['src-seed'] }),
  // components
  rel('op-main', 'operator', 'c-main', 'Runs commands', 'CLI', { sourceIds: ['src-main'] }),
  rel('main-fs', 'c-main', 'host-fs', 'Reads the schema file', 'std::fs::read_to_string or stdin', { sourceIds: ['src-main'] }),
  rel('main-db', 'c-main', 'c-db', 'Calls operations; receives Created, DbSummary, LogEntry, Status, Summary', 'Rust calls', { sourceIds: ['src-main'] }),
  rel('db-store', 'c-db', 'c-store', 'Opens Store; capture; page_size, commit_count; queries through conn()', 'Rust calls', { sourceIds: ['src-db'] }),
  rel('db-replay', 'c-db', 'c-replay', 'apply(image, page_size, frames, db_pages, max_pages)', 'Rust call on Vec<u8>', { sourceIds: ['src-db', 'src-replay'] }),
  rel('store-wal', 'c-store', 'c-wal', 'parse(bytes) -> Option<Wal>', 'Rust call on &[u8]', { sourceIds: ['src-store'] }),
  rel('db-sqlite', 'c-db', 'sqlite-engine', 'Opens live.db, runs statements, checkpoint at create, integrity_check', 'rusqlite', { sourceIds: ['src-db'] }),
  rel('store-sqlite', 'c-store', 'sqlite-engine', 'Reads and writes meta.db in one transaction per capture', 'rusqlite', { sourceIds: ['src-store'] }),
  rel('store-fs', 'c-store', 'host-fs', 'Reads live.db-wal bytes', 'std::fs::read', { sourceIds: ['src-store'] }),
  rel('db-fs', 'c-db', 'host-fs', 'Creates, copies, reads, writes, renames and removes files', 'std::fs', { sourceIds: ['src-db'] }),
  // components to datastores (dynamic views)
  rel('db-live', 'c-db', 'live-db', 'Opens and writes live.db', 'rusqlite Connection', { sourceIds: ['src-db'] }),
  rel('db-base', 'c-db', 'base-db', 'Copies or reads base.db', 'std::fs', { sourceIds: ['src-db'] }),
  rel('db-clone', 'c-db', 'clone-file', 'Writes and checks the clone', 'std::fs::write, SQLite immutable=1', { sourceIds: ['src-db'] }),
  rel('db-livewal', 'c-db', 'live-wal', 'Reports WAL size', 'std::fs::metadata', { sourceIds: ['src-db'] }),
  rel('store-meta', 'c-store', 'meta-db', 'Reads and writes history tables', 'rusqlite, SQL', { sourceIds: ['src-store'] }),
  rel('store-livewal', 'c-store', 'live-wal', 'Reads WAL bytes', 'std::fs::read', { sourceIds: ['src-store'] }),
  // code: wal
  rel('k-parse-checksum', 'k-wal-parse', 'k-wal-checksum', 'Verifies header and chained frame checksums', 'function call'),
  rel('k-parse-wal', 'k-wal-parse', 'k-wal-wal', 'Returns', 'Result<Option<Wal>>'),
  rel('k-wal-commits', 'k-wal-wal', 'k-wal-commit', 'Holds commits in order', 'Vec', { kind: 'composition' }),
  rel('k-commit-frames', 'k-wal-commit', 'k-wal-frame', 'Holds frames in order', 'Vec', { kind: 'composition' }),
  rel('k-encode-checksum', 'k-wal-encode', 'k-wal-checksum', 'Shares the checksum primitive', 'function call'),
  // code: store
  rel('k-store-seen', 'k-store', 'k-commits-seen', 'capture computes commits already stored', 'function call'),
  rel('k-store-meta', 'k-store', 'k-t-meta', 'Reads page_size and cursor; writes cursor', 'SQL'),
  rel('k-store-commits', 'k-store', 'k-t-commits', 'Inserts one row per new commit', 'SQL INSERT'),
  rel('k-store-frames', 'k-store', 'k-t-frames', 'Inserts page images', 'SQL INSERT'),
  rel('k-frames-commits', 'k-t-frames', 'k-t-commits', 'commit_idx references idx', 'foreign key', { kind: 'association' }),
  // deployment: developer machine
  rel('d-make-cli', 'infra-make', 'i-cli-release', 'make seed / make demo run this binary', 'bash, WALFLIP=target/release/walflip'),
  rel('d-make-fuzz', 'infra-make', 'i-fuzz', 'make fuzz', 'cargo +nightly fuzz run wal_parse'),
  rel('d-cli-live', 'i-cli-release', 'i-live-db', 'exec writes', 'rusqlite'),
  rel('d-cli-wal', 'i-cli-release', 'i-live-wal', 'Reads WAL bytes', 'std::fs::read'),
  rel('d-cli-base', 'i-cli-release', 'i-base-db', 'Copies or reads snapshot', 'std::fs'),
  rel('d-cli-meta', 'i-cli-release', 'i-meta-db', 'Writes and reads history', 'rusqlite'),
  rel('d-cli-clone', 'i-cli-release', 'i-clone', 'make demo clones -n 250 and -n 500', 'std::fs::write'),
  rel('d-installed-live', 'i-cli-installed', 'i-live-db', 'Same layout when run from the same working directory', 'rusqlite'),
  // deployment: planned CI
  rel('d-ci-fuzz', 'infra-ci-schedule', 'i-ci-fuzz', 'Nightly fuzz run (planned)', 'cargo +nightly fuzz run, 300 s per target', { status: 'planned', plannedNotes: ['TESTING_PLAN.md section 2.4'], sourceIds: ['src-plan'] }),
  rel('d-ci-tests', 'infra-ci-schedule', 'i-ci-tests', 'PR test run (planned)', 'cargo test', { status: 'planned', plannedNotes: ['TESTING_PLAN.md section 2.4'], sourceIds: ['src-plan'] }),
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
  fwd('cr-3', 'main-fs', 'read_schema: read FILE, or stdin when FILE is -'),
  fwd('cr-4', 'main-db', 'db::create(&d, Some(sql))'),
  fwd('cr-5', 'db-fs', 'Fail if the folder exists; create_dir_all'),
  fwd('cr-6', 'db-live', 'Open live.db; PRAGMA journal_mode=WAL; fail unless it returns wal'),
  fwd('cr-7', 'db-live', 'execute_batch(schema); any error removes the whole folder'),
  fwd('cr-8', 'db-live', 'PRAGMA wal_checkpoint(TRUNCATE); read PRAGMA page_size; close'),
  fwd('cr-9', 'db-base', 'fs::copy live.db to base.db'),
  fwd('cr-10', 'db-store', 'Store::open(meta.db); set page_size'),
  fwd('cr-11', 'store-meta', 'CREATE TABLE IF NOT EXISTS meta, commits, frames; store page_size'),
  rev('cr-12', 'main-db', 'Return Created { page_size }'),
  rev('cr-13', 'op-main', 'Print: created DIR (page size N)'),
])

const execScenario = seq('exec', [
  fwd('ex-1', 'op-main', 'walflip --db NAME exec SQL, or exec - with one statement per line'),
  fwd('ex-2', 'main-db', 'Db::existing: fail unless live.db exists; parse_statements for stdin'),
  fwd('ex-3', 'main-db', 'db::exec(&d, &statements)'),
  fwd('ex-4', 'db-store', 'Store::open(meta.db)'),
  fwd('ex-5', 'db-live', 'Open live.db; PRAGMA wal_autocheckpoint=0'),
  seq('ex-loop', [
    fwd('ex-6', 'db-live', 'For each statement: BEGIN; execute_batch(stmt); COMMIT'),
    fwd('ex-7', 'live-appends-wal', 'SQLite appends frames; the last frame carries db_size (commit marker)'),
    fwd('ex-8', 'db-store', 'capture(wal_path, label = stmt)'),
    fwd('ex-9', 'store-livewal', 'fs::read the whole WAL; a missing file adds 0 commits'),
    fwd('ex-10', 'store-wal', 'wal::parse(bytes)'),
    rev('ex-11', 'store-wal', 'Wal { page_size, salt, commits }: stops at a salt or checksum mismatch, drops uncommitted tail'),
    fwd('ex-12', 'store-meta', 'Read page_size, cursor_salt, cursor_commits, count(commits); fail if page sizes differ'),
    fwd('ex-13', 'store-meta', 'One transaction: insert commits after the cursor and their frames; set cursor to salt and total'),
    rev('ex-14', 'db-store', 'Return commits added = total - seen'),
  ]),
  rev('ex-15', 'main-db', 'Return total captured'),
  rev('ex-16', 'op-main', 'Print: captured N commit(s)'),
])

const cloneScenario = seq('clone', [
  fwd('cl-1', 'op-main', 'walflip --db NAME clone -n N -o FILE'),
  fwd('cl-2', 'main-db', 'db::clone_at(&d, N, FILE)'),
  fwd('cl-3', 'db-clone', 'Fail if FILE already exists'),
  fwd('cl-4', 'db-store', 'Store::open; commit_count; fail if N > count; require page_size'),
  fwd('cl-5', 'store-meta', 'SELECT pgno, page FROM frames WHERE commit_idx <= N ORDER BY commit_idx, seq'),
  fwd('cl-6', 'store-meta', 'db_pages of commit N (none for N = 0) and max(db_pages) over 1..=N'),
  fwd('cl-7', 'db-base', 'fs::read base.db into memory'),
  fwd('cl-8', 'db-replay', 'apply(image, page_size, frames, db_pages, max_pages = max(base pages, max db_pages))'),
  rev('cl-9', 'db-replay', 'Validate every frame first, overwrite at (pgno-1)*page_size, resize to db_pages'),
  fwd('cl-10', 'db-clone', 'fs::write image to FILE'),
  fwd('cl-11', 'main-db', 'db::summarize(FILE)'),
  fwd('cl-12', 'db-clone', 'Open read-only immutable=1; integrity_check; count rows per table'),
  rev('cl-13', 'op-main', 'Print frames replayed, integrity_check and row counts'),
])

const inspectScenario = par('inspect', [
  seq('ls', [
    fwd('ls-1', 'op-main', 'walflip list'),
    fwd('ls-2', 'main-db', 'db::list(root)'),
    fwd('ls-3', 'db-fs', 'read_dir(root); keep folders with live.db; missing root gives an empty list'),
    fwd('ls-4', 'db-store', 'Store::open(NAME/meta.db).commit_count()'),
    fwd('ls-5', 'store-meta', 'SELECT count(*) FROM commits'),
    rev('ls-6', 'op-main', 'Print name and commits, sorted by name'),
  ]),
  seq('st', [
    fwd('st-1', 'op-main', 'walflip --db NAME status'),
    fwd('st-2', 'main-db', 'db::status(&d)'),
    fwd('st-3', 'db-store', 'page_size, commit_count, count(frames)'),
    fwd('st-4', 'store-meta', 'Read meta and count rows'),
    fwd('st-5', 'db-livewal', 'fs::metadata(live.db-wal): size, or absent'),
    rev('st-6', 'op-main', 'Print page size, commits, frames, live wal'),
  ]),
  seq('lg', [
    fwd('lg-1', 'op-main', 'walflip --db NAME log --limit N (default 20)'),
    fwd('lg-2', 'main-db', 'db::log(&d, N)'),
    fwd('lg-3', 'db-store', 'Query through Store::conn()'),
    fwd('lg-4', 'store-meta', 'commits LEFT JOIN frames GROUP BY idx ORDER BY idx LIMIT N'),
    rev('lg-5', 'op-main', 'Print n, frames, db_pages and the first 90 characters of the SQL'),
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
    related: [{ diagramId: 'containers-runtime', label: 'Runtime containers and data stores' }, { diagramId: 'deploy-ci', label: 'Planned CI deployment' }],
    docIds: ['doc-containers-verification'], sourceIds: ['src-tests-cli', 'src-fuzz', 'src-plan'] }),
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
    nodes: ['k-wal-parse', 'k-wal-checksum', 'k-wal-wal', 'k-wal-commit', 'k-wal-frame', 'k-wal-encode'],
    edges: ['k-parse-checksum', 'k-parse-wal', 'k-wal-commits', 'k-commit-frames', 'k-encode-checksum'],
    docIds: ['doc-wal-format'], sourceIds: ['src-wal'] }),
  diagram({ id: 'code-store', kind: 'code', title: 'store: Store, cursor rule and meta.db schema', scopeId: 'c-store', parent: { diagramId: 'components', elementId: 'c-store' },
    nodes: ['k-store', 'k-commits-seen', 'k-t-meta', 'k-t-commits', 'k-t-frames'],
    edges: ['k-store-seen', 'k-store-meta', 'k-store-commits', 'k-store-frames', 'k-frames-commits'],
    docIds: ['doc-meta-schema'], sourceIds: ['src-store'] }),
  dynamic({ id: 'dyn-create', title: 'Dynamic: create NAME --schema FILE', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'host-fs', 'live-db', 'base-db', 'meta-db'], scenario: createScenario, docIds: ['doc-dyn-create'], sourceIds: ['src-main', 'src-db'] }),
  dynamic({ id: 'dyn-exec', title: 'Dynamic: exec with WAL parse and idempotent capture', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'c-wal', 'live-db', 'live-wal', 'meta-db'], scenario: execScenario, docIds: ['doc-dyn-exec'], sourceIds: ['src-db', 'src-store', 'src-wal'] }),
  dynamic({ id: 'dyn-clone', title: 'Dynamic: clone -n N with replay and db_pages resize', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'c-replay', 'meta-db', 'base-db', 'clone-file'], scenario: cloneScenario, docIds: ['doc-dyn-clone'], sourceIds: ['src-db', 'src-replay'] }),
  dynamic({ id: 'dyn-inspect', title: 'Dynamic: list, status and log', nodes: ['operator', 'c-main', 'c-db', 'c-store', 'meta-db', 'live-wal', 'host-fs'], scenario: inspectScenario, docIds: ['doc-dyn-inspect'], sourceIds: ['src-db'] }),
  diagram({ id: 'deploy-dev', kind: 'deployment', title: 'Deployment: developer machine', environment: 'developer-machine', parent: { diagramId: 'containers-runtime' },
    nodes: ['dn-host', 'dn-cargo-bin', 'i-cli-installed', 'dn-target', 'i-cli-release', 'infra-make', 'dn-data-root', 'dn-db-folder', 'i-live-db', 'i-live-wal', 'i-base-db', 'i-meta-db', 'dn-tmp', 'i-clone', 'dn-nightly', 'i-fuzz'],
    edges: ['d-make-cli', 'd-make-fuzz', 'd-cli-live', 'd-cli-wal', 'd-cli-base', 'd-cli-meta', 'd-cli-clone', 'd-installed-live'],
    related: [{ diagramId: 'deploy-ci', label: 'Planned CI deployment' }],
    docIds: ['doc-deploy-dev'], sourceIds: ['src-make', 'src-readme'] }),
  diagram({ id: 'deploy-ci', kind: 'deployment', title: 'Deployment: CI (planned)', environment: 'ci-planned', status: 'planned', parent: { diagramId: 'containers-verification' },
    nodes: ['dn-ci', 'infra-ci-schedule', 'i-ci-fuzz', 'i-ci-tests'],
    edges: ['d-ci-fuzz', 'd-ci-tests'],
    docIds: ['doc-deploy-ci'], sourceIds: ['src-plan'] }),
]

// ---------- documents ----------
const doc = (id, title, plain, ste, sourceIds) => ({ id, title, plain, ste: { text: ste, issue: 'ASD-STE100 Issue 9', reviewStatus: 'draft' }, termIds: [], sourceIds })
const PLAN = REPO + 'docs/TESTING_PLAN.md'
const documents = [
  doc('doc-context', 'System context',
    'walflip is a local command-line tool. It has no server and no background process; each command is one OS process and all state lives under the data root.\n\nThe operator runs every write through `walflip exec`. A CI job or fuzzer operator is a secondary consumer that runs the test suites and the WAL fuzzer.\n\nOther programs that open `live.db` are **not supported**: their writes are not captured, and a checkpoint they cause can discard WAL frames before walflip reads them (README, Limitations).',
    'walflip is a local command-line tool. Each command is one process. All data is in the data root.\n\nThe operator must send all writes through walflip exec. Other programs must not write live.db. If they write it, walflip does not record the change.',
    ['src-readme']),
  doc('doc-operator', 'Operator',
    'Selects a database with `--db NAME` or `WALFLIP_DB`, and a data root with `--root DIR` or `WALFLIP_ROOT` (default `walflip-data`, relative to the working directory).',
    'Use --db or WALFLIP_DB to select the database. Use --root or WALFLIP_ROOT to select the data root.',
    ['src-main']),
  doc('doc-cli', 'CLI binary',
    'Subcommands: `create NAME [--schema FILE|-]`, `list`, `rename OLD NEW`, `drop NAME`, `exec SQL|-`, `log [--limit N]`, `status`, `clone -n N -o FILE`. Each arm of the `match` in `main` calls one `walflip::db` function and prints its result. After `clone`, `report` calls `db::summarize` and prints `integrity_check` and row counts.',
    'The CLI reads the arguments. It calls one library function for each command. It prints the result. It does not contain logic.',
    ['src-main']),
  doc('doc-containers-runtime', 'Runtime containers and data stores',
    'Two code containers (the binary and the library crate) and five data stores. Four stores live in one folder per database, `ROOT/NAME/`: `live.db`, `live.db-wal`, `base.db` and `meta.db`. The fifth is the clone file at the `-o` path.\n\n`live.db-wal` is owned by SQLite. walflip only reads it, as raw bytes, from inside `exec`.',
    'The binary calls the library. The library reads and writes five files. Four files are in the database folder. The fifth file is the clone.\n\nSQLite writes live.db-wal. walflip only reads it.',
    ['src-db', 'src-store']),
  doc('doc-containers-verification', 'Tests, fuzzing and tooling',
    'Each suite is a separately runnable target. In-crate `#[cfg(test)]` modules test internals. `tests/roundtrip.rs` links the library and compares clones with live contents. `tests/cli.rs` spawns the compiled binary through `tests/common::Harness`, uses a shadow-SQLite `Model` as oracle and checks the invariant catalog through the library. The fuzz crate depends on walflip with feature `testing` and calls only `wal::parse`. `scripts/seed.sh` drives the binary to build the 500-commit demo; `tests/cli.rs` also runs it.\n\nNo CI workflow exists in the repo. Planned fuzz targets and CI jobs are described in [TESTING_PLAN.md](' + PLAN + ') sections 2.1 and 2.4.',
    'Each test suite runs separately. The fuzz crate sends random bytes to wal::parse. The seed script runs the binary to make a demo database.\n\nThe repository has no CI file. The testing plan gives the CI jobs that are planned.',
    ['src-tests-cli', 'src-tests-roundtrip', 'src-fuzz', 'src-seed', 'src-plan']),
  doc('doc-components', 'Library components and the pure/I-O boundary',
    'Dependencies point one way: `main.rs` -> `db` -> `store` -> `wal`, and `db` -> `replay`. `wal` and `replay` import only `anyhow`; they take bytes or slices and return values. `store` and `db` own all file and SQLite I/O. `store::commits_seen` is a pure function inside the I/O module so the cursor rule can be property-tested alone.\n\n`main.rs` is shown as a supporting component of the CLI container. Tests and the fuzz crate also call the library directly (see the verification container view).\n\nNote: the doc comment in `src/lib.rs` still lists a `schema` module as pure. That module no longer exists; the doc comment is stale.',
    'Dependencies go in one direction: main.rs, then db, then store, then wal. db also uses replay.\n\nwal and replay do not read or write files. store and db do all file and SQLite operations.',
    ['src-lib', 'src-db', 'src-store', 'src-wal', 'src-replay']),
  doc('doc-c-db', 'db module',
    'Names must be 1-64 characters of ASCII letters, digits, `_` or `-`, because they become folder names. `parse_statements` splits stdin into one statement per non-blank line. Every function returns a plain struct (`Created`, `DbSummary`, `LogEntry`, `Status`, `Summary`) or a count.',
    'A database name must have 1 to 64 letters, digits, underscores or hyphens. The name becomes a folder name. Each function returns data. It does not print.',
    ['src-db']),
  doc('doc-decisions', 'Architecture decisions and constraints',
    '- **Parse the WAL file directly instead of using SQLite hooks.** `store::capture` reads `live.db-wal` with `fs::read` and `wal::parse` validates magic, page size, salts and the chained checksum. Commits are frames up to one with non-zero `db_size`.\n- **Full-page replay.** Every WAL frame is a whole page, so `replay::apply` is positioned overwrites at `(pgno - 1) * page_size` on a copy of `base.db`, then a resize to commit N\'s `db_pages`. No SQL runs during replay.\n- **Library returns data, CLI prints.** `db` returns structs; `main.rs` only formats output. Tests call the library directly.\n- **Capture runs inside exec with autocheckpoint disabled.** `db::exec` sets `PRAGMA wal_autocheckpoint=0` and calls `capture` after every commit on the same open connection, so SQLite cannot fold frames into `live.db` before they are read.\n- **Cursor = salts + commit count.** `meta.cursor_salt` and `meta.cursor_commits` record the WAL generation and how many of its commits are stored. A new salt (WAL restarted after a checkpoint) resets the count to 0, so re-reading the same WAL adds nothing.\n- **page_size pinned at create.** `create` stores `PRAGMA page_size` in `meta`; `capture` rejects a WAL with a different page size because replay offsets depend on it.\n- **Replay allocation bound.** `clone_at` passes `max_pages = max(base pages, max db_pages over 1..=N)`; `apply` rejects any frame or size above it before changing the image.\n- **One statement = one transaction = at most one commit.** A statement that changes nothing writes no frames and adds no commit.',
    '- walflip reads the WAL file directly. It does not use SQLite hooks.\n- Each WAL frame is a full page. Replay writes each page at its offset. Replay does not run SQL.\n- The library returns data. The CLI prints the data.\n- exec stops automatic checkpoints. exec reads the WAL after each commit.\n- The cursor is the WAL salts and the number of commits. A new salt sets the number to 0.\n- create records the page size. capture stops with an error if the WAL page size is different.',
    ['src-store', 'src-wal', 'src-replay', 'src-db', 'src-main']),
  doc('doc-known-gaps', 'Known gaps (reported by TESTING_PLAN.md, not re-verified)',
    'These defects are **reported by** [docs/TESTING_PLAN.md](' + PLAN + '). They were not re-run for this documentation.\n\n- **B1** (medium): `summarize` does not double `"` in table names, so `clone` writes the file and then fails on a name such as `a"b`. Affects the clone report step.\n- **B2** (high): a statement that contains `COMMIT` commits to `live.db`, then `tx.commit()` fails and capture never runs; the WAL is checkpointed on close and history is lost. Affects the exec loop.\n- **B3** (medium): commits already in `live.db-wal` when `exec` starts (crashed or external writer) are captured under the next statement\'s label.\n- **B4** (high): concurrent `exec` on one database fails with `UNIQUE constraint failed: commits.idx` and records extra commits; the cursor is read outside the `meta.db` transaction.\n- **B5** (high): a schema with `PRAGMA journal_mode=DELETE` is accepted; afterwards every `exec` captures 0 commits.\n- **B6** (medium): `replay::apply` could be forced into a huge allocation by a large `pgno` or `db_pages`. Observation: the current code has a `max_pages` parameter and a test `huge_pgno_is_rejected_without_allocating`, so this looks addressed, but the plan still lists it.\n- **B7** (low/UX): `PRAGMA page_size` and `PRAGMA auto_vacuum` in a schema are silently ignored because WAL mode is enabled first; `create` reports 4096.\n- **B8** (low): an unterminated `BEGIN` in a schema fails with `database table is locked` instead of an "applying schema" message.',
    'The testing plan reports these defects. We did not test them again.\n\n- B1: Table names with a quote character cause the clone report to fail.\n- B2: A statement with COMMIT can cause a loss of history.\n- B3: Old commits in the WAL get the label of the next statement.\n- B4: Two exec processes at the same time can corrupt the history.\n- B5: A schema can turn off WAL mode. Then exec records no commits.\n- B6: A large page number could cause a very large allocation. The current code has a limit for this.\n- B7: create ignores page_size and auto_vacuum in the schema.\n- B8: An open BEGIN in the schema gives an unclear error.',
    ['src-plan']),
  doc('doc-wal-format', 'WAL file format as parsed by wal::parse',
    'All header integers are big-endian u32. The magic only selects the byte order of the checksum words.\n\n**Header, 32 bytes:** 0 magic (`0x377f0682` little-endian checksums, `0x377f0683` big-endian); 4 format version (not checked; encoder writes 3007000); 8 page size (must be a power of two and at least 512); 12 checkpoint sequence (not checked); 16 salt-1; 20 salt-2; 24 checksum-1; 28 checksum-2 (over bytes 0-23).\n\n**Frame, 24 + page_size bytes:** 0 page number; 4 db size in pages if this is a commit frame, else 0; 8 salt-1; 12 salt-2; 16 checksum-1; 20 checksum-2; 24 page image. The checksum covers frame bytes 0-7 then the page, chained from the previous frame (or the header).\n\n**Outcomes:** buffer shorter than 32 bytes or bad header checksum gives `Ok(None)`; bad magic or page size gives `Err`; the frame loop stops at the first salt mismatch (older generation), checksum mismatch or partial frame; frames after the last commit frame are dropped.',
    'The header has 32 bytes. It has the magic number, the page size, two salts and a checksum.\n\nEach frame has a 24-byte header and one page. The frame header has the page number, the database size, the salts and a checksum.\n\nA frame with a database size that is not zero ends a commit. The parser stops at the first bad salt or bad checksum.',
    ['src-wal']),
  doc('doc-meta-schema', 'meta.db schema',
    '- `meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)`: keys `page_size`, `cursor_salt` (`salt1:salt2`), `cursor_commits`.\n- `commits(idx INTEGER PRIMARY KEY, label TEXT NOT NULL, db_pages INTEGER NOT NULL, committed_at TEXT DEFAULT CURRENT_TIMESTAMP)`: `idx` is 1-based; `label` is the SQL statement.\n- `frames(commit_idx INTEGER NOT NULL REFERENCES commits(idx), seq INTEGER NOT NULL, pgno INTEGER NOT NULL, page BLOB NOT NULL, PRIMARY KEY(commit_idx, seq))`.\n\n`Store::open` runs `CREATE TABLE IF NOT EXISTS` on every open, so `list`, `status` and `log` create an empty `meta.db` if it is missing. Each `capture` writes its commits, frames and cursor in one transaction.',
    'meta.db has three tables: meta, commits and frames. meta keeps the page size and the cursor. commits has one row for each commit. frames keeps the page images.\n\nEach capture writes all rows in one transaction.',
    ['src-store']),
  doc('doc-dyn-create', 'create --schema',
    'The name is validated before the schema file is read. WAL mode is switched on **before** the schema runs, then the WAL is checkpointed with `TRUNCATE` so `base.db` is a complete, WAL-free copy. If any step in `init_files` fails, `create` removes the whole folder.\n\nFailure cases: folder already exists; schema SQL error (folder removed). Reported gaps: B5 (schema can switch journal mode away from WAL), B7 (`page_size`/`auto_vacuum` in the schema ignored), B8 (unterminated `BEGIN` gives a locking error).',
    'create checks the name first. Then it reads the schema. It sets WAL mode before it runs the schema. Then it copies live.db to base.db. If a step fails, create removes the folder.',
    ['src-db', 'src-main', 'src-plan']),
  doc('doc-dyn-exec', 'exec: WAL parse and idempotent capture',
    'Invariant: capture runs on the same open connection, after each commit, with automatic checkpointing off, so every committed frame is still in the WAL when it is read.\n\nThe cursor makes capture idempotent: `seen = commits_seen(cursor_salt, cursor_commits, wal.salt)`; only commits after `seen` are inserted, and the cursor is set to the WAL\'s total. When `exec` returns the connection closes; SQLite then normally checkpoints and removes the WAL, and the next `exec` starts a new generation with new salts, so the count restarts at 0.\n\nFailure cases: `--db` missing or unknown; SQL error (earlier statements stay committed and captured); WAL page size differs from `meta.page_size`. Reported gaps: B2 (`COMMIT` inside a statement), B3 (pre-existing WAL commits mislabelled), B4 (concurrent exec; cursor read outside the transaction).',
    'exec stops automatic checkpoints. After each commit, exec reads the WAL and records the new commits. The cursor tells which commits are already recorded. Thus a second read adds nothing.\n\nDo not run two exec commands on one database at the same time.',
    ['src-db', 'src-store', 'src-wal', 'src-plan']),
  doc('doc-dyn-clone', 'clone -n N: replay and db_pages resize',
    '`clone_at` loads frames of commits 1..=N in `(commit_idx, seq)` order, so later writes to a page win. `replay::apply` validates every frame before changing anything (page 0, page above `max_pages`, wrong length, `db_pages` above `max_pages` all give `Err`). After the overwrites it resizes the image to commit N\'s `db_pages`, which shrinks it when a later commit freed pages (for example after `DROP TABLE` with `auto_vacuum`). For N = 0 no frames apply and the result is `base.db`.\n\nThe image is built in memory, so clone needs RAM roughly the size of the database. Failure cases: output exists; N greater than the commit count; no recorded page size. Reported gap: B1 (table names containing `"` break the report after the file is written).',
    'clone reads base.db into memory. It writes each stored page at its offset, in commit order. Then it sets the size to the size of commit N. Then it writes the file and checks it.',
    ['src-db', 'src-replay', 'src-plan']),
  doc('doc-dyn-inspect', 'list, status and log',
    'These three commands are independent; the scenario shows them as unordered branches. All are read-only on history, but `Store::open` runs `CREATE TABLE IF NOT EXISTS`, so it creates `meta.db` if it is missing. `status` reports `live wal: absent (checkpointed)` when `live.db-wal` does not exist, which is the normal state between commands.',
    'list, status and log only read the history. status shows if live.db-wal exists. Usually it does not exist between commands.',
    ['src-db', 'src-main']),
  doc('doc-deploy-dev', 'Deployment: developer machine',
    'Install with `cargo install --path .` (Rust 1.75+, Unix-like OS; SQLite is bundled). Make targets: `make build` (`cargo build --release`), `make test` (`cargo test`), `make fuzz` (`cargo +nightly fuzz run wal_parse -- -max_total_time=$(FUZZ_SECS)`, default 30 s, needs nightly and cargo-fuzz), `make seed` (build, then `scripts/seed.sh` with `WALFLIP=target/release/walflip`), `make demo` (seed, then clone at 250 and 500 into `/tmp`), `make clean`, `make distclean`.\n\nThe data root defaults to `walflip-data/` in the working directory, so the installed binary and the release binary share data only when run from the same directory.',
    'Install walflip with cargo install. Use make build, make test, make fuzz, make seed and make demo. make fuzz needs the nightly Rust toolchain and cargo-fuzz.',
    ['src-make', 'src-readme', 'src-seed']),
  doc('doc-deploy-ci', 'Deployment: CI (planned)',
    'Not implemented. [TESTING_PLAN.md](' + PLAN + ') section 2.4 plans a nightly job (each fuzz target for 300 s with `-rss_limit_mb=1024` and a dictionary) and a PR job (`cargo +nightly fuzz build` plus corpus replay with `-runs=0`).',
    'This deployment is planned. It does not exist now.',
    ['src-plan']),
]

const bundle = {
  schemaVersion: 1,
  id: 'walflip',
  title: 'walflip architecture',
  revision: '2026-10-14',
  defaultDiagramId: 'context',
  model: { elements, relationships },
  diagrams,
  documents,
  sources,
}
writeFileSync(out, JSON.stringify(bundle, null, 2) + '\n')
console.log('wrote', out, elements.length, 'elements', relationships.length, 'relationships', diagrams.length, 'diagrams')
