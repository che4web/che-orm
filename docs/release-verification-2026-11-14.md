# SQLite security update for the 0.4.0 candidates

Candidates: che-orm 0.4.0, che-sync 0.2.0 and che-rest 0.4.0.
These are working-tree preparation checks, not a publication or deployment.
The earlier Rust 1.88 verification does not cover this dependency update.

## Changes

- Raise core, sync, REST and generated application MSRV to Rust 1.89.
- Require rusqlite 0.41.1 and resolve libsqlite3-sys 0.39.1, bundling patched
  SQLite 3.54.1 (RUSTSEC-2026-0459).
- Replace deadpool-sqlite with a small backend-private manager on deadpool 0.13
  and deadpool-sync 0.2. Published deadpool-sqlite releases do not support the
  patched driver; they are not a dependency of this candidate anymore.
- Preserve blocking-thread execution, lazy opening, pool sizing, URI handling,
  rollback before reuse and foreign-key initialization. Test connection reuse,
  replacement after panic and the minimum safe bundled SQLite version.
- Propagate errors from the new fallible rusqlite authorizer API, including
  authorizer removal after an unsuccessful migration SQL statement.
- Resolve rustix 1.1.6 in the ORM lock file (RUSTSEC-2026-0486).
- Refresh the REST fullstack example lock file, including quinn-proto 0.11.19
  and quinn-udp 0.5.16 for RUSTSEC-2026-0523/0524/0538.
- Keep macros 0.3.2 and protocol 0.1.0 unchanged; do not republish them.

## Verification

- ORM, REST and fullstack-example RustSec audits passed without vulnerabilities
  or warnings.
- Workspace SQLite tests passed, including the strengthened connection-reuse test.
- Rust 1.89 workspace/all-target checks passed for SQLite and PostgreSQL.
- Workspace strict Clippy and documentation passed.
- Live PostgreSQL 18.6 core runtime and sync runtime/model-role/model-journal/
  object-delivery tests passed in an isolated temporary cluster.
- Schema export and the offline example passed.
- REST Rust 1.89 all-target checks and strict Clippy passed with sync-http,push
  for both SQLite and PostgreSQL.
- REST SQLite tests and the complete PostgreSQL suite with --include-ignored
  passed, including management, auth, ACL, media and sync HTTP.
- Tauri sync tests, Todo desktop compilation and Todo server/client HTTP tests
  passed with updated lock files. This is not a manual GUI inspection.
- The fullstack smoke test and the ignored generated-project build/migration
  test passed.
- Both ORM template layouts passed checks for SQLite and PostgreSQL. The ORM
  script exercises PostgreSQL template CRUD via its SQLite feature switch.
- All five public archives packaged and passed extracted-source all-target
  checks on Rust 1.89.0: SQLite/HTTP for the combined workspace and PostgreSQL
  for ORM/sync. Packaged core unit/regression tests passed (84 + 26 tests).
- Core registry publication dry run passed with --allow-dirty; Cargo aborted
  the upload as intended. A clean-tree dry run remains a post-commit gate.
- The PostgreSQL dependency graph contains no SQLite driver or sync-pool adapter.
- Complete REST scaffold matrices passed on Rust 1.89 for SQLite and live
  PostgreSQL: cargo-generate and CLI, single/modular layouts, with/without auth
  (16 combinations), including initial/repeated migrations, createapp, foreign
  keys, formatting, Clippy and HTTP CRUD for public projects.

Archive hashes and detailed package/MSRV logs are retained at
`/tmp/opencode/che-release-040-sqlite-security-189-final/result.json` and adjacent
log files. These identify the working-source snapshot before final documentation
updates, not a published artifact; repackage after the release commit.

An initial set of parallel checks exhausted disk space. Only generated build
artifacts were cleaned; interrupted checks are not counted as successful.
The first combined package attempt also hit Cargo's temporary-registry
"no hash listed" error; the repeat in a fresh staging directory passed. REST
scaffold runs that hit temporary-storage exhaustion were repeated sequentially
after cleaning obsolete verification caches; both complete matrices passed.
The temporary PostgreSQL cluster was stopped after verification.

## Publication and deployment gates

Repackage the reviewed commits after committing the release sources. The
publication order remains core -> sync -> REST; repeat ordinary registry dry
runs for dependent crates after dependency indexing. Actual publication requires
separate authorization. No tags are moved and no application schema is migrated
by this preparation. Existing SQLite applications still require their explicit
canonical-datetime migrations before startup; the driver update does not replace
those migrations or change submitted sync payloads.
