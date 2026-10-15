# walflip architecture viewer

This folder holds the C4 architecture docs for walflip. They are a small Vite + React + TypeScript app
that renders the diagrams with [c4-sdk](https://github.com/nickstrad/c4-sdk). The model lives in
one validated bundle, `src/bundle.json`. Each element and relationship in it was checked against
the code on 2026-10-14 and carries source links (`src/*.rs`, `tests/`, `fuzz/`, `Makefile`,
`docs/TESTING_PLAN.md`).

## Diagrams

| Id | Kind | Question it answers |
|---|---|---|
| `context` | context | Who uses walflip, and which outside things does it touch? (operator, CI/fuzzer, local disk, other writers, SQLite clients) |
| `containers-runtime` | container | Which parts run, and which data stores do they use? (CLI binary, library crate, `live.db`, `live.db-wal`, `base.db`, `meta.db` in the per-database folder, clone file) |
| `containers-verification` | container | What do the tests and the fuzzer run? (in-crate tests, `tests/roundtrip.rs`, the scenario suites + `tests/common`, the `walflip-fuzz` crate with six targets, the seed script) |
| `components` | component | What modules make up the library? Where is the line between pure code and I/O, and which way do the dependencies point? |
| `code-wal` | code | Which symbols make up the WAL parser (`parse`, `parse_from`, `Resume`), and what is behind the `testing` feature? |
| `code-store` | code | What are `Store`, `WriteLock`, the cursor rule and the `meta.db` tables (`meta`, `commits`, `frames`)? |
| `dyn-create` | dynamic | What happens during `create NAME --schema FILE`? |
| `dyn-exec` | dynamic | What happens during `exec`: the two write locks, `(recovered)` capture, the VACUUM path and incremental capture from the resume state? |
| `dyn-clone` | dynamic | What happens during `clone -n N`: the replay, the `max_pages` bound and the `db_pages` resize? |
| `dyn-inspect` | dynamic | What do `list`, `status` and `log` read? |
| `deploy-dev` | deployment | How is walflip built and run on a developer machine? (`cargo install`, make targets, local fuzzing) |
| `deploy-ci` | deployment | What does CI run? `ci.yml` on GitHub Actions: `cargo test --features testing` on PRs and pushes to `main`. The fuzz workflows are archived. |

Suggested reading order: context, then runtime containers, components, the four dynamic views,
and last the code and deployment views. The explanation panel holds a plain text and a draft
STE100 text for each diagram. The design decisions and the known gaps are in the documents
"Architecture decisions and constraints" and "Known limits and fixed defects". Open them from the context or
components diagram. The defects B1-B10 are all fixed (see `docs/WAVE_STATUS.md`); the "Known limits" document lists what remains by design.

## Published site

The viewer is built and published to GitHub Pages by `.github/workflows/pages.yml` on every push to
`main` that touches `docs/architecture/`: <https://nickstrad.github.io/walflip/>. In the repository
settings, Pages must use the source "GitHub Actions".

## Run

Use Node 22.23.2, the version that c4-sdk pins (for example
`export PATH="$HOME/.nvm/versions/node/v22.23.2/bin:$PATH"`).

```sh
cd docs/architecture
npm install          # installs c4-sdk from npm
npm run dev          # local viewer with hot reload
npm run validate     # decode src/bundle.json with c4-sdk/core; exits 1 on any error (--strict also fails on warnings)
npm run build        # tsc --noEmit, then vite build into dist/ (base './', works from any sub-path)
npm run preview      # serve dist/
```

Deep links use the hash route, for example `#/architectures/walflip/diagrams/dyn-exec`.

## Update the model when the code changes

1. Edit `scripts/generate-bundle.mjs`. It defines the elements, relationships, scenarios, documents
   and source links, and it builds each diagram's legend from what that diagram shows.
2. Run `npm run generate`. This rewrites `src/bundle.json`. Do not edit the JSON by hand, because the
   next generate run will overwrite it.
3. Run `npm run validate`, then `npm run build`.

Rules that the SDK enforces and that bite most often:

- A component in a component view must belong to the scoped container. If it does not, tag it
  `external` (this is why `main.rs` is tagged `external` in the library view).
- Containers and systems in a component view must be tagged `external`.
- A dynamic step can only use a relationship that the diagram lists in `edges`. The generator
  derives `edges` from the scenario.
- Deployment edges connect only instances or infrastructure nodes in the same environment.
- Each relationship needs a `technology`. Without one, validate gives a `missing-protocol`
  warning.
- Source URLs must be HTTPS. They point at `github.com/nickstrad/walflip/blob/main/...`, so they
  work only after that branch is pushed.

## About the c4-sdk package

c4-sdk is installed from npm (`c4-sdk@0.1.0`, published from `nickstrad/c4-sdk`). Earlier revisions
vendored a beta tarball; that is no longer needed.

The SDK's peer dependencies are pinned to `react` and `react-dom` 19.3.0. Keep this app on those
exact versions.
