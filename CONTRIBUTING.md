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

## Required Checks

Run these commands before opening a pull request:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
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

Russian guides in `docs/` are the original documentation. Keep their English
counterparts in `docs/en/` consistent when changing public behavior. Update
`README.md` and `README.en.md` when installation, backend status, or the main
workflow changes.

## Pull Requests

Describe the user-visible behavior, tests run, schema or migration impact, and
any compatibility concerns. Do not include `target/`, local SQLite databases,
or temporary schema files.
