# Single-layout template example

Runnable task app based on `layout=single` from [`templates/che-orm`](../../templates/che-orm).
Models and their `AppConfig` live at the crate root rather than under a starter
app module. See [`template-modular`](../template-modular/) for comparison.
Requires Rust 1.89 or newer. The default backend is SQLite.

## Run

From the repository root, apply the included `tasks:0001_initial` migration
before running the CRUD demo:

```bash
cargo run -p template-single --bin template-single-manage -- schema
cargo run -p template-single --bin template-single-manage -- migrate
cargo run -p template-single --bin template-single-manage -- migrate status
cargo run -p template-single
```

The demo creates, reads, saves with `Database::save`, and deletes one task.
It uses the model returned by `save` to get refreshed managed timestamps, then
checks that saving a deleted row returns `None` instead of inserting it again.
Migrations are applied explicitly before startup.

SQLite uses `template-single.db` in the working directory by default. Set
`CHE_ORM_DATABASE_URL` to use a different file. Use a fresh database for this
example; the migration CLI requires a persistent database, while tests use
in-memory SQLite. To reuse a database created with che-orm 0.3.x, preserve its
migration history and follow the [SQLite datetime upgrade guide](../../docs/en/sqlite.md)
before running the updated app.

Exactly one backend feature must be enabled. To switch to the other backend,
disable defaults and enable that feature on **every** command, for example:

```bash
cargo run -p template-single --no-default-features --features postgres --bin template-single-manage -- schema
```

## Project layout

- `src/models.rs`: application models in one module.
- `src/apps/mod.rs`: app registration, extended by `template-single-manage createapp`.
- `src/lib.rs`: `AppConfig` and application registry.
- `src/settings.rs`: database connection settings.
- `src/migrations/`: compiled migration registry and the initial task migration.
- `src/bin/manage.rs`: schema, migration generation, application and status commands.
- `src/bin/manage/makemigrations.rs`: application-owned migration generator.
- `src/bin/manage/createapp.rs`: app module generator.
- `src/main.rs`: async CRUD demo.
- `tests/runtime.rs`: schema-history validation, SQLite CRUD and transactional
  save commit/rollback coverage.

## Create an App Module

```bash
cargo run -p template-single --bin template-single-manage -- createapp blog
```

This creates `src/apps/blog/mod.rs` with `APP_NAME` and an `AppConfig`, a
`src/apps/blog/models.rs` with naming guidance, and registers the app in
`src/apps/mod.rs`.
Names use snake_case, such as `blog` or `user_accounts`.

The app's `APP_NAME` is shared by `AppConfig::name()` and model table names:

```rust
// src/apps/blog/models.rs
#[derive(Debug, che_orm::Model)]
#[orm(app = super::APP_NAME)]
pub struct Post {
    #[orm(primary_key)]
    pub id: i64,
    pub title: String,
}
```

This produces `blog_post`. Add models to the app's `AppConfig::schema`, for example
`SchemaSet::new().model::<models::Post>()`. Register FK parents before children;
new apps are appended to the app registry. Keep the `createapp` marker comments
in `src/apps/mod.rs` so future commands can extend registration.

```bash
cargo run -p template-single --bin template-single-manage -- makemigrations --name add_blog
cargo run -p template-single --bin template-single-manage -- migrate
```

All apps share the project's compiled migration registry in `src/migrations/`.
Migration operations preserve each model's app ownership. App creation requires
`rustfmt` and does not connect to the database. Existing app directories are not
overwritten. Run through `cargo run` so registry changes are compiled before
creating the next app.

The starter `Task` uses the same pattern and produces `tasks_task`. Its
`APP_NAME` is in `src/lib.rs`.
An explicit `#[orm(table = "existing_table")]` preserves an existing table name.

## Change the schema

After changing `Task`, generate a follow-up migration:

```bash
cargo run -p template-single --bin template-single-manage -- makemigrations --name update_tasks
cargo run -p template-single --bin template-single-manage -- migrate
```

`makemigrations` compares registered models with the replayed compiled history,
writes `src/migrations/m0002_update_tasks.rs` and updates `src/migrations/mod.rs`.
It records a dependency on the previous migration without modifying its source.
Review generated operations before applying them, especially removals and changes
to existing non-null fields. Data backfills and explicit rename operations may
need manual edits. Subsequent commands rebuild the management binary automatically.

```bash
cargo run -p template-single --bin template-single-manage -- makemigrations --dry-run
cargo run -p template-single --bin template-single-manage -- makemigrations --check
```

`--dry-run` prints the proposed Rust migration without changing history. `--check`
returns a nonzero exit code when changes need a migration, for use in CI. Neither
requires a database connection. Generation requires `rustfmt` (`rustup component
add rustfmt`). Run generation through `cargo run`: a stale prebuilt management
binary refuses to generate from changed migration sources.

The schema-history test detects model changes without matching migrations.
Preserve the identities and checksums of applied migrations. Use compiled
migrations for persistent schemas; `create_table` is for tests and local setup.
Migration generation is explicit; application startup never generates migrations.

## Verify

```bash
cargo fmt -p template-single --check
cargo test -p template-single
cargo clippy -p template-single --all-targets -- -D warnings
```

`cargo test -p template-single --no-default-features --features postgres` validates the
compiled history without connecting to a server. Running PostgreSQL migration
and CRUD commands requires a live database.

See the [che-orm documentation](https://github.com/che4web/che-orm) for relations,
serializers, UUID models and synchronization.
