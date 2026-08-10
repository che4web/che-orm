# AGENTS.md

## Repo Shape
- Rust 2024 workspace, MSRV `1.85`, with resolver `3` in the root `Cargo.toml`.
- Workspace crates: `che-orm` runtime API/SQLite backend/migrations, `che-orm-macros` derive macro, `che-orm-cli` migration binary, `che-orm-examples` unpublished runnable examples.
- Public API is re-exported from `crates/che-orm/src/lib.rs`; macro internals use `che_orm::__private` for `chrono`, `serde_json`, and `sqlx` paths.

## Commands
- Full verification: `cargo test --workspace`.
- Focus one crate: `cargo test -p che-orm` or `cargo test -p che-orm-macros`.
- Focus one test by name: `cargo test -p che-orm sqlite_crud_flow`.
- Run examples from workspace root: `cargo run -p che-orm-examples --bin crud`, `cargo run -p che-orm-examples --bin relations`, `cargo run -p che-orm-examples --bin schema_snapshot`.
- Run the CLI via the package name, not the binary name package: `cargo run -p che-orm-cli -- makemigrations ...` and `cargo run -p che-orm-cli -- migrate ...`.

## Migrations
- The CLI does not inspect Rust models; a program must write `che_orm_schema.json` using `Schema`/`ModelSchema` first. The example generator is `cargo run -p che-orm-examples --bin schema_snapshot`.
- `makemigrations` compares `--schema` against `migrations/schema.json`, writes numbered `*.sql`, then updates `migrations/schema.json` as the next diff baseline.
- `migrate` uses SQLx's migration runner and `_sqlx_migrations`; it defaults to `--config app.toml` with `[database].url`; `--database-url` overrides config. Default migrations dir is `migrations`.
- SQLite column drops are executed through table rebuilds that preserve shared columns; direct SQLite drop-column SQL is not generated.

## Testing Notes
- Tests use `sqlite::memory:` and temp files; no external database service is required.
- Timestamp tests sleep for about one second to observe `CURRENT_TIMESTAMP` changes, so `cargo test -p che-orm` is not instant.
- CRUD API expectations are best verified from tests in `crates/che-orm/tests/`; some README examples may lag current builder signatures.

## Implementation Gotchas
- `SqliteBackend::connect` enables `PRAGMA foreign_keys = ON`; keep relation tests in mind when changing connection setup.
- Migration application uses a quote/comment-aware SQL statement parser and ignores comment-only statements; avoid adding migration SQL syntax that depends on unsupported dialect constructs without extending that parser.
- The derive macro requires named structs and exactly relies on a `#[field(primary_key)]`; `i64` primary keys are auto-increment by default.
