# AGENTS.md

## Project Overview

`che-orm` is an experimental Rust ORM workspace.

Workspace members:

- `che-orm`: core ORM library, compile-time SQLite/PostgreSQL runtimes, SQL AST, and schema compiler.
- `che-orm-macros`: procedural macros `#[derive(Model)]`, `#[derive(DbEnum)]`
  and `#[derive(ModelSerializer)]`.
- `che-orm-examples`: downstream examples, sample application models, and the `manage` CLI.

## Architecture

- `src/types.rs`: database values, expressions, typed model fields.
- `src/image.rs`: `ImageField`, a TEXT-backed relative file key; filesystem/HTTP handling belongs in `che-rest::media`.
- `src/query.rs`: query AST, model trait, SELECT/INSERT/UPDATE/DELETE builders.
- `src/schema.rs`: column metadata, `SchemaSet`, `AppConfig`, and `AppRegistry`.
- `src/sql.rs`: SQLite/PostgreSQL dialects and SQL compilation.
- `src/connection.rs`: shared async Database, CRUD, transactions and relation loading.
- `src/backend/`: the feature-selected driver, pool, binding and row adapters.
- `src/row.rs`: backend-neutral Row, DecodeError and FromDatabaseValue.
- `src/migration/postgres_schema.rs`: native PostgreSQL schema operations.
- `src/migration/postgres_executor.rs`: PostgreSQL migration locking and execution.
- `che-orm-macros/src/lib.rs`: `Model` derive implementation.
- `che-orm-examples/src/lib.rs`: sample application models, registry, and database path.
- `che-orm-examples/src/bin/manage.rs`: compiled-migration CLI for the sample application.
- `che-orm-examples/migrations/`: the sample's frozen compiled baseline SQL.

## Application Modules

Keep application models, registry, settings, binaries, and migrations outside
the core crate. The sample application uses `che-orm-examples`.

Each application should expose an `App` type implementing `AppConfig`:

```rust
pub struct App;

impl che_orm::AppConfig for App {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}
```

Register apps with `AppRegistry`. Register parent tables before applications
that define foreign keys to them.

## Model Rules

- Enable exactly one backend: `sqlite` (default) or `postgres` with defaults
  disabled. Upper-level application code uses the same Database/Transaction API.
- Connection methods are async. Use `Database::connect(url).await?` and async
  transaction callbacks `|tx| Box::pin(async move { ... })` with awaited methods.
- Models decode the common `Row`; custom scalar types implement
  `FromDatabaseValue`. Keep driver types in backend-specific adapters.
- Use `#[derive(Debug, Model)]` for application models.
- Always specify `#[orm(table = "...")]`.
- Mark generated integer keys with `#[orm(primary_key)]`.
- UUID keys use `#[orm(primary_key)] id: che_orm::Uuid`. INSERT builders generate
  UUIDv7 in Rust when the key is omitted; explicitly supplied keys are preserved.
  SQLite stores UUIDs as 16-byte BLOBs, PostgreSQL compiles them as UUID, and JSON
  uses UUID strings. `Database::insert(&model)` uses the model's supplied UUID.
- Use `#[orm(db_column = "existing_name")]` when a Rust field must use a
  different name from its physical SQL column. Queries, indexes and generated
  schema use the physical name while Rust code keeps the field-based constant.
  Historical migrations retain both names; changing only the Rust name creates
  no schema operation when `db_column` remains unchanged.
- Use `#[orm(foreign_key = User, on_delete = "cascade")]` for foreign keys to
  ORM models. This generates `Post::USER` and `Post::USER.reverse()`.
- Use `#[orm(references = "table(column)")]` only for tables without an ORM
  model; it does not generate typed relations.
- `foreign_key` supports `i64`, `Uuid`, `Option<i64>` and `Option<Uuid>`; its
  underlying type must match the target model's primary key. Use
  `on_delete = "set null"` only with optional keys.
- Use `#[orm(auto_now_add)]` and `#[orm(auto_now)]` only with `OffsetDateTime`.
- `auto_now` is updated by ORM-generated UPDATE statements. Raw SQL does not update it.
- `Option<T>` maps to a nullable SQL column.
- `ImageField` stores a relative key in SQLite `TEXT`, not bytes or an HTTP URL. `Option<ImageField>` is nullable. `src/image.rs` implements `ColumnTypeOf`, typed query binding, SQLite read/write and string JSON serialization; do not add filesystem operations to the ORM.
- `ModelSerializer` makes `ImageField` and `Option<ImageField>` read-only in JSON writes even without an explicit annotation. Server code sets them after a validated upload; do not accept client-supplied storage keys through CRUD.
- Keep the user-facing walkthrough in `docs/image-field.md` and `docs/en/image-field.md` aligned with the actual `che-rest::media` APIs. Ordinary models need just `Option<ImageField>`; show `#[orm(db_column = "...")]` only for an existing SQL column with a different name. `ImageField::new` wraps a trusted key but does not validate or store a file.
- Avoid declaring both field-level `#[orm(unique)]` and a duplicate table-level
  `unique("field")` constraint.

`#[derive(ModelSerializer)]` generates a JSON serializer for materialized model
data. It must not access `Database`. Use queryset `select_related` for
`belongs_to` and `prefetch_related` for reverse relations before converting a
result to a serializer. Nested fields use `#[serializer(many = Post, relation =
PostUserRelation)]` or `#[serializer(one = User, relation = PostUserRelation)]` and accept
only the matching `WithMany`/`WithOne`/`LoadedMany` relation marker. Optional
relations use `WithOptionalOne`.
Scalar writable foreign-key serializer fields use `#[serializer(foreign_key = User, relation = TaskAssigneeRelation)]`; this exposes the FK id as write metadata without requiring a nested queryset relation.

## Migrations

Application-owned compiled migration operations are the source of persistent schema
changes. Do not use `Database::create_table` for production deployment; it is
intended for tests and local setup.

The sample application configuration lives in `che-orm-examples/src/lib.rs`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

Use the application CLI:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

The example rejects `makemigrations`: its `0001_initial` history is frozen.
`migrate` and `migrate status` use its `database_url()` (`CHE_ORM_DATABASE_URL`,
or the configured default). Each backend has its own frozen baseline. Update the application's
compiled migration registry when adding a model. `che-rest` applications have
their own compiled `makemigrations` flow. Review generated operations and
backfill existing file keys separately if changing their storage format:
`#[orm(db_column = "existing_name")]` keeps the SQL column name but does not
rewrite stored values. Preserve previous migration identities and checksums.

The ORM high-level `create` and `update` APIs use SQLite `RETURNING`. The ORM
does not generate `AFTER INSERT` or `AFTER UPDATE` triggers. Do not add such
triggers for ORM models without explicitly reviewing the returned-value
semantics: triggers created by migrations or raw SQL may change a row after
SQLite has produced the `RETURNING` result, so the returned model will not
include those post-trigger changes.

## Verification

Run these commands after code changes:

```bash
cargo fmt --check
cargo test --workspace
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
```

For PostgreSQL without SQLite:

```bash
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
```

The explicit runtime test requires TEST_POSTGRES_URL and a live PostgreSQL
server; absence of the variable is an error. The sample PostgreSQL build uses
`-p che-orm-examples --no-default-features --features postgres`.

Run examples after applying the configured migration:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin schema
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

## Editing Rules

- Preserve the public re-exports in `src/lib.rs` unless an API change is intentional.
- Keep SQL values parameterized; never interpolate user values into SQL.
- Keep identifiers model-controlled and review any new raw SQL identifier path.
- Add or update tests for compiler output, schema changes, and runtime behavior.
- Use ASCII in source and documentation unless existing content requires otherwise.
- Do not commit generated `target/` artifacts or local database files.
