# Compiled Migrations

Migrations belong to the application and compile with it. The ORM applies
`MigrationOperation` values and records identities and checksums in database
history. No separate Atlas CLI is required. This guide's URL is retained for
existing links.

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

## Your Application's Migrations

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
