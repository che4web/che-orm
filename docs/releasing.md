# Release procedure

## Release versions

The next release contains:

| Package | Version |
| --- | --- |
| che-orm-macros | 0.3.0 |
| che-orm | 0.3.0 |
| che-sync-protocol | 0.1.0 |
| che-sync | 0.1.0 |

`che-orm-examples` is private (`publish = false`). Package the four public
crates explicitly rather than using `cargo package --workspace`.

The 0.3 ORM release introduces async database access with feature-selected
SQLite/PostgreSQL backends and role-aware synchronization. Shared models use
`#[orm(sync)]`, with `SchemaSet::client_model` or `server_model` selecting their
database role. `pull()`, `flush()` and explicit `full_load()` separate download,
upload and snapshot loading. See [sync.md](sync.md) for the history contract and
application-owned upgrade steps. Server database replacement/history rewind is
outside that contract.

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
cargo +1.88.0 check --workspace --all-targets --locked
cargo +1.88.0 check -p che-orm -p che-sync --all-targets --locked --no-default-features --features postgres
cargo package -p che-orm-macros -p che-orm -p che-sync-protocol -p che-sync --locked
```

Set `TEST_POSTGRES_URL` to a disposable PostgreSQL database, then run the four
PostgreSQL commands in the workflow, including ignored runtime/model-role tests.
For a local working tree review, packaging can use `--allow-dirty`; the release
must include the reviewed new source and test files in its commit.

Validate the matching downstream `che-rest` checkout with:

```bash
cargo test --test sync --test model_sync --test sync_lifecycle
```

For an existing installation, rehearse its compiled follow-up migration on a
copy of its database. Verify revisions, scope ownership, UUIDs, tombstones,
receipts, cursors and pending edits as described in [sync.md](sync.md).
Fresh-database tests do not validate this application-specific upgrade.

## Publication

Commit the verified release, then publish in dependency order. Wait for each
dependency to become available in the registry before publishing its consumers:

```bash
cargo publish -p che-orm-macros
cargo publish -p che-orm
cargo publish -p che-sync-protocol
cargo publish -p che-sync
```

`cargo publish --dry-run -p PACKAGE` can be used before each step once its
dependencies are available. The multi-package `cargo package` check above
validates the release together using Cargo's temporary package registry even
before those dependencies have been published.
