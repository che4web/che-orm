# AGENTS.md

## Project Overview

`che-orm` is an experimental Rust ORM workspace.

Workspace members:

- `che-orm`: core ORM library, SQLite runtime, SQL AST, and schema compiler.
- `che-orm-macros`: procedural macros `#[derive(Model)]`, `#[derive(DbEnum)]`
  and `#[derive(ModelSerializer)]`.
- `che-orm-examples`: downstream examples, sample application models, and the `manage` CLI.

## Architecture

- `src/types.rs`: database values, expressions, typed model fields.
- `src/query.rs`: query AST, model trait, SELECT/INSERT/UPDATE/DELETE builders.
- `src/schema.rs`: column metadata, `SchemaSet`, `AppConfig`, and `AppRegistry`.
- `src/sql.rs`: SQLite/PostgreSQL dialects and SQL compilation.
- `src/connection.rs`: async SQLite pool, CRUD execution, row decoding, and transactions.
- `che-orm-macros/src/lib.rs`: `Model` derive implementation.
- `che-orm-examples/src/lib.rs`: sample application models, registry, and database path.
- `che-orm-examples/src/bin/manage.rs`: Atlas migration CLI for the sample application.
- `che-orm-examples/migrations/`: sample Atlas migrations and `atlas.sum`.

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

- Use `#[derive(Debug, Model)]` for application models.
- Always specify `#[orm(table = "...")]`.
- Mark generated integer keys with `#[orm(primary_key)]`.
- Use `#[orm(foreign_key = User, on_delete = "cascade")]` for foreign keys to
  ORM models. This generates `Post::USER` and `Post::USER.reverse()`.
- Use `#[orm(references = "table(column)")]` only for tables without an ORM
  model; it does not generate typed relations.
- `foreign_key` supports `i64` and `Option<i64>`. Use `on_delete = "set null"`
  only with `Option<i64>`.
- Use `#[orm(auto_now_add)]` and `#[orm(auto_now)]` only with `OffsetDateTime`.
- `auto_now` is updated by ORM-generated UPDATE statements. Raw SQL does not update it.
- `Option<T>` maps to a nullable SQL column.
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

Atlas is the source of applied schema changes. Do not use `Database::create_table`
for production deployment; it is intended for tests and local setup.

The sample application configuration lives in `che-orm-examples/src/lib.rs`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

Use the application CLI:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- makemigrations
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
cargo run -p che-orm-examples --bin manage -- migrate lint
```

`makemigrations` writes the desired schema to a temporary file and invokes
Atlas with `--to file://...`. The temporary file must not be committed.

`migrate` and `migrate status` use the sample application's `DATABASE_PATH`;
do not add a second database URL source to the CLI.

Atlas must be installed and available as `atlas` in `PATH`. Set `ATLAS_BIN`
when a non-default executable path is needed. Some Atlas versions require
`atlas login` or Pro for `migrate lint`.

Migration files and `atlas.sum` are committed to the repository. Review every
generated migration before applying it.

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

For the PostgreSQL SQL compiler without the SQLite runtime:

```bash
cargo test -p che-orm --no-default-features --features postgres
```

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
