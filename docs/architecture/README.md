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
| `containers-verification` | container | What do the tests and the fuzzer run? (in-crate tests, `tests/roundtrip.rs`, `tests/cli.rs` + `tests/common`, the `walflip-fuzz` crate, the seed script) |
| `components` | component | What modules make up the library? Where is the line between pure code and I/O, and which way do the dependencies point? |
| `code-wal` | code | Which symbols make up the WAL parser, and what is behind the `testing` feature? |
| `code-store` | code | What are `Store`, the cursor rule and the `meta.db` tables (`meta`, `commits`, `frames`)? |
| `dyn-create` | dynamic | What happens during `create NAME --schema FILE`? |
| `dyn-exec` | dynamic | What happens during `exec`: the WAL parse and the idempotent cursor capture? |
| `dyn-clone` | dynamic | What happens during `clone -n N`: the replay, the `max_pages` bound and the `db_pages` resize? |
| `dyn-inspect` | dynamic | What do `list`, `status` and `log` read? |
| `deploy-dev` | deployment | How is walflip built and run on a developer machine? (`cargo install`, make targets, nightly fuzz) |
| `deploy-ci` | deployment (planned) | Which CI jobs does `docs/TESTING_PLAN.md` plan? None exist yet. |

Suggested reading order: context, then runtime containers, components, the four dynamic views,
and last the code and deployment views. The explanation panel holds a plain text and a draft
STE100 text for each diagram. The design decisions and the known gaps are in the documents
"Architecture decisions and constraints" and "Known gaps". Open them from the context or
components diagram. The known gaps B1-B8 are as reported by `docs/TESTING_PLAN.md`. This
documentation did not re-verify them.

## Run

Use Node 22.23.2, the version that c4-sdk pins (for example
`export PATH="$HOME/.nvm/versions/node/v22.23.2/bin:$PATH"`).

```sh
cd docs/architecture
npm install          # installs c4-sdk from vendor/c4-sdk-0.1.0-beta.1.tgz
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

c4-sdk is **not published to npm**, and its `dist-sdk/` is build output, so you cannot install
it straight from GitHub. `vendor/c4-sdk-0.1.0-beta.1.tgz` (about 0.9 MB, sha256
`bfdfaf8c3404e139360d08df07972f33f9e42361d6bc04c034071f98ed704263`) was packed from
`nickstrad/c4-sdk` commit `eb3b7a5` with Node 22.23.2 / npm 10.9.8. To rebuild it:

```sh
git clone https://github.com/nickstrad/c4-sdk.git && cd c4-sdk
npm ci && npm run sdk:build && npm pack
cp c4-sdk-*.tgz <walflip>/docs/architecture/vendor/
# if the version changed, update the file: path in package.json, then npm install
```

The SDK's peer dependencies are pinned to `react` and `react-dom` 19.3.0. Keep this app on those
exact versions.
