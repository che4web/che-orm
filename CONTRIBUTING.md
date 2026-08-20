# Contributing to che-orm

## Scope

The current runtime target is SQLite. PostgreSQL support is limited to SQL and
DDL compilation until an executor is intentionally designed and implemented.
Keep public APIs backend-neutral where possible, but do not claim runtime
support that does not exist.

## Development Workflow

1. Keep sample application models, registry, settings, and migrations in
   `che-orm-examples`; the core crate must remain application-independent.
2. Use `#[derive(Debug, Model)]`, an explicit `#[orm(table = "...")]`, and one
   `#[orm(primary_key)]` `i64` field per model.
3. Use Atlas migrations for persisted schema changes. Review generated SQL and
   commit migration files with `che-orm-examples/migrations/atlas.sum`.
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
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
```

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
