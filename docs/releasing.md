# Release procedure

## Release versions

Workspace package versions (core/macros 0.3.2 are already published; the initial
sync publications are 0.1.0):

| Package | Version |
| --- | --- |
| che-orm-macros | 0.3.2 |
| che-orm | 0.3.2 |
| che-sync-protocol | 0.1.0 |
| che-sync | 0.1.0 |

`che-orm-examples` is private (`publish = false`). Package the four public
crates explicitly rather than using `cargo package --workspace`.

The 0.3.2 patch releases core and macros together. It adds app-prefixed table-name
inference from literals or shared constants, and updates the template with
`createapp`, single/modular layouts and explicit migration generation.
The template requires the published core 0.3.2. Sync package versions remain
0.1.0 for their initial publication; they have not previously been published.

## Initial sync publication: 0.1.0

`che-sync-protocol` is transport/backend independent. `che-sync` depends on
`che-sync-protocol 0.1.0` and `che-orm 0.3.2`. Both require Rust 1.88.
Each sync archive includes its own README, changelog and MIT license. Review the
package file lists and preserve existing uncommitted application/example work
when preparing a release commit.

The remaining publication order is:

1. `che-sync-protocol 0.1.0`.
2. Wait for its crates.io index entry, then `che-sync 0.1.0`.
3. The downstream `che-rest 0.3.1` can then pass its normal registry dry run.

Before publication, validate both unpublished packages together using Cargo's
temporary package registry:

```bash
cargo publish --dry-run -p che-sync-protocol --allow-dirty
cargo package -p che-sync-protocol -p che-sync --allow-dirty
```

An ordinary `cargo publish --dry-run -p che-sync` cannot resolve the unpublished
protocol dependency. After protocol publication, repeat it without substituting
local dependencies. The joint package check is not a registry publication.

For extracted-archive Rust 1.88 verification, use the sibling REST helper:

```bash
TMPDIR=/tmp/opencode python3 ../che-rest/scripts/verify_sync_packages.py \
  --output /absolute/path/outside/source/repos/sync-0.1.0-verification
```

The output directory must be new and have room for five crates and their build
artifacts. It retains logs, archives and a SHA-256 report. The helper checks both
SQLite/HTTP and PostgreSQL against extracted sources; it requires cached optional
dependencies for its offline checks.

Publish only after reviewing and committing the intended release files:

```bash
cargo publish -p che-sync-protocol
# Wait for the protocol package to be indexed.
cargo publish --dry-run -p che-sync
cargo publish -p che-sync
```

Use distinct sync tags such as `che-sync-protocol-v0.1.0` and `che-sync-v0.1.0`
for this initial release; do not move the published core tag `v0.3.2`.

### Verified candidate (2026-10-27)

- Protocol `publish --dry-run --allow-dirty --locked`: passed.
- Joint sync packaging with `--allow-dirty --locked`: passed, using the published
  crates.io ORM 0.3.2 rather than a substituted ORM version.
- Sync tests, both backend builds, Clippy, API docs and both native examples:
  passed. Live PostgreSQL tests remain ignored without `TEST_POSTGRES_URL`.
- Extracted sync archives: Rust 1.88 all-target checks for SQLite and PostgreSQL,
  and SQLite protocol/engine tests passed against the published ORM.
- Audit findings were addressed in lock files: Tokio 1.54.1, ctutils 0.4.4, and
  matching downstream REST TLS dependencies. Current ORM/REST/fullstack lock files
  have no reported vulnerabilities. Auditing the sync archive lock cannot check
  registry yanked status for the as-yet unpublished protocol package; repeat
  the audit after it is indexed.

Final extracted-archive report and logs are retained at
`/home/che/web/rust/che-sync-010-final-2552aaa0/result.json`.

| Archive | SHA-256 |
| --- | --- |
| `che-sync-protocol-0.1.0.crate` | `cf3afbcad85d21683a4e4252af1589ad9ce19daf38126b55ef11769e14e1c8c8` |
| `che-sync-0.1.0.crate` | `452dff20c5b9dec6f9436d5d42dc1d7d320083e653168550eb3e69c85b809d19` |

These hashes identify the reviewed local candidates; packaging again after a
release commit can change VCS metadata and therefore the archive hashes.

The 0.3 ORM release introduces async database access with feature-selected
SQLite/PostgreSQL backends and role-aware synchronization. Shared models use
`#[orm(sync)]`, with `SchemaSet::client_model` or `server_model` selecting their
database role. Prefer [protocol-2 model journals](model-sync.md) for new clients;
their independent `pull`/`flush` methods have no snapshot phase. The
[scoped compatibility API](sync.md) retains explicit `full_load` and its own
history/upgrade rules. Both contracts require persistent server history;
replacement/history rewind is outside them.

## Verification

The `Release checks` workflow verifies SQLite, live PostgreSQL, Rust 1.88 and
the contents of the publishable archives. Run the equivalent checks locally
before tagging. SQLite and PostgreSQL are mutually exclusive features.

```bash
cargo fmt --check
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
cargo run -p che-orm-examples --bin manage --locked -- schema
cargo run -p che-sync --example offline --locked
python3 scripts/check-template.py
cargo +1.88.0 check --workspace --all-targets --locked
cargo +1.88.0 check -p che-orm -p che-sync --all-targets --locked --no-default-features --features postgres
cargo package -p che-orm-macros -p che-orm -p che-sync-protocol -p che-sync --locked
```

Set `TEST_POSTGRES_URL` to a disposable PostgreSQL database, then run the four
PostgreSQL commands in the workflow, including ignored runtime/model-role and
`model_log_postgres` tests. The latter verifies protocol-2 journals, concurrent
commits, no-op suppression, deferred FK projection and SET NULL.
For a local working tree review, packaging can use `--allow-dirty`; the release
must include the reviewed new source and test files in its commit.

Validate the matching downstream `che-rest` checkout with:

```bash
cargo test --test sync --test model_sync --test sync_lifecycle
cargo test --features sync-http
```

The matching che-tauri client and Todo application are separate downstream
workspaces. When changing sync contracts, also run their checks listed in the
[protocol-2 guide](model-sync.md#verification). Desktop compilation and the
two-client HTTP test do not replace manual GUI or deployment migration review.

For an existing installation, rehearse its compiled follow-up migration on a
copy of its database. Verify revisions, scope ownership, UUIDs, tombstones,
receipts, cursors and pending edits as described in [sync.md](sync.md) and the
[protocol-2 migration guidance](../../che-rest/docs/model-sync.md#rehearsing-an-existing-installations-migration).
Fresh-database tests do not validate this application-specific upgrade.

## Publication

Commit the verified release, then publish in dependency order. Wait for each
dependency to become available in the registry before publishing its consumers:

```bash
cargo publish -p che-orm-macros
cargo publish -p che-orm
python3 scripts/check-template.py --published
```

The published core tag is `v0.3.2`. For the separate initial sync release, follow
the 0.1.0 publication section above.

`cargo publish --dry-run -p PACKAGE` can be used before each step once its
dependencies are available. The multi-package `cargo package` check above
validates the release together using Cargo's temporary package registry even
before those dependencies have been published.
