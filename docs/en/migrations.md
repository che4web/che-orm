# Compiled Migrations

Migrations belong to the application and compile with it. The ORM applies
`MigrationOperation` values and records identities and checksums in database
history. Changing a model alone does not update an existing schema.

## Generate Migrations in Your Application

The easiest starting point is the [application template](template.md). It includes
the `manage` CLI source and an empty registry in `src/migrations/`. Adding the
`che-orm` dependency alone does not add a CLI to an existing project.

Register models in `AppConfig::schema()` and application modules in `AppRegistry`,
in dependency order: parents before models with foreign keys. From the generated
application's directory, create the first migration:

```bash
cargo run --bin manage -- makemigrations --name initial
```

The generator compares registered models with the state replayed from previous
migrations, not with the live database. It writes a numbered Rust module in
`src/migrations/` and updates `src/migrations/mod.rs`.
Generation needs no database connection, but requires `rustfmt`.

Review operations before applying them, especially removals, renames and new
required fields on populated tables. Data backfills and explicit renames may
require manual edits. Commit migration files to Git.

```bash
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
```

Create new migrations after subsequent model changes:

```bash
cargo run --bin manage -- makemigrations --name update_models
cargo run --bin manage -- migrate
```

Do not edit applied migrations: preserve their identities, contents and checksums.
Application is a separate deployment step, not part of startup.
Generation is always explicit as well.

## Preview Changes and Check in CI

```bash
cargo run --bin manage -- makemigrations --dry-run
cargo run --bin manage -- makemigrations --check
```

`--dry-run` prints the proposed Rust source without writing files. `--check`
returns a nonzero exit code if models need a new migration.
Neither command needs a database connection. Run through `cargo run` to use
current models and registry: a stale binary refuses generation when migration
sources have changed.

For PostgreSQL, create the database first and set `CHE_ORM_DATABASE_URL`.
The template selects its backend during generation; when switching to PostgreSQL,
use `--no-default-features --features postgres` on every Cargo command.
Structured operations compile for the selected backend, but arbitrary SQL and
data conversions need review for the target database.

## Example Application CLI

Run from the repository root:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

`schema` prints the registered models' schema. `migrate` applies migrations;
`migrate status` shows which identities have been recorded in database history.

`database_url()` in `che-orm-examples/src/lib.rs` reads `CHE_ORM_DATABASE_URL`.
SQLite defaults to `app.db`; PostgreSQL defaults to `postgresql://localhost/che_orm`.
For PostgreSQL, add `--no-default-features --features postgres` before `--bin`.

The example ships separate frozen SQL baselines for both backends and includes
the selected one with `include_str!` in `src/bin/manage.rs`. Its migration
identity is `examples:0001_initial`. The example does not generate migrations;
`makemigrations` and `migrate lint` are unavailable.

## Custom Registry and API

Keep models, `AppRegistry`, settings, and the migration registry outside the core
crate. Construct migrations with `Migration::new`, specifying stable
`MigrationId` values, dependencies, and ordered `MigrationOperation` values.

```rust
let graph = che_orm::MigrationGraph::new(application_migrations())?;
let applied = database.migrate(graph).await?;
```

`application_migrations()` is your application's function returning
`Vec<Migration>`. Use `ProjectState::diff` and `operations_from_changes` to
calculate changes, and `render_migration_rust` to generate a Rust module. Review
operations and SQL before applying them. `RunSql` must use the selected backend's
SQL and include matching `state_operations` when changing the schema.

Preserve the identities, contents, and checksums of applied migrations. Append
new operations in new migrations and plan data backfills explicitly. For
example, a required `DbEnum` column on a populated table needs a valid value
backfilled while retaining its `CHECK` constraint.

Apply migrations as a separate deployment/CI step. Reserve
`Database::create_table` for tests and local experiments. See the
[backend guide](backends.md#migrations) for locking, transaction, and SQL details.
