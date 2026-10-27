# Contributing to che-orm

## Scope

SQLite and PostgreSQL are selected by mutually exclusive features. Keep the
high-level API and model decoding backend-neutral; only driver adapters and
schema execution belong to backend-specific modules.

## Development Workflow

1. Keep sample application models, registry, settings, and migrations in
   `che-orm-examples`; the core crate must remain application-independent.
2. Use `#[derive(Debug, Model)]`, an explicit `#[orm(table = "...")]`, and one
   `#[orm(primary_key)]` field of type `i64` or `che_orm::Uuid` per model.
3. Use application-owned compiled migrations for persisted schema changes.
   Preserve historical identities and checksums; review backend-specific SQL.
4. Add a regression test for every behavior change. Macro behavior used by
   downstream crates belongs in an integration or trybuild test.
5. Keep SQL values parameterized. Identifiers must remain model-controlled.
6. For synchronized UUID models, use shared `#[orm(sync)]` definitions and select
   client/server roles at registration. Use configured sync database handles for
   captured writes. See [the synchronization contract](docs/sync.md).

## Required Checks

Run these commands before opening a pull request:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
cargo test -p che-sync --no-default-features --features postgres
cargo test -p che-sync --no-default-features --features postgres --test runtime --test model_roles --test model_log_postgres -- --ignored
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-sync --example offline
```

The PostgreSQL runtime command requires `TEST_POSTGRES_URL` pointing at a live
server and a role allowed to create test schemas. CI provisions a service for it.

Run the examples after applying the configured migration:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin schema
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

## Documentation

The standalone application template lives in `templates/che-orm`. When changing
it, install `cargo-generate` and run `python3 scripts/check-template.py`. This
generates both backend variants outside the workspace, verifies their local
dependencies and compiled history, and runs SQLite migrations and CRUD.
After releasing, use `--published` to verify dependencies from crates.io.
The template check also changes a model, generates and applies a follow-up
migration, and verifies that compiled history and existing rows are preserved.
It covers both `single` and `modular` layouts, including `createapp` and
cross-app foreign keys. Use `--layout` and `--backend` to select one combination.

Russian guides in `docs/` are the original documentation. Keep their English
counterparts in `docs/en/` consistent when changing public behavior. Update
`README.md` and `README.en.md` when installation, backend status, or the main
workflow changes.

For package/archive and minimum Rust version checks, follow
[the release procedure](docs/releasing.md).

## Pull Requests

Describe the user-visible behavior, tests run, schema or migration impact, and
any compatibility concerns. Do not include `target/`, local SQLite databases,
or temporary schema files.
