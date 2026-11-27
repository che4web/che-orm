# Compiled Migrations

Migrations belong to the application and compile with it. The ORM applies
`MigrationOperation` values and records identities and checksums in database
history. Changing a model alone does not update an existing schema.

Physical table names must match `[a-z_][a-z0-9_]*`. The rule applies to
models, registered SQL tables, foreign key targets in metadata, and rename
operations. Write the unquoted name in metadata. SQL may quote that name.

## Generate Migrations in Your Application

The easiest starting point is the [application template](template.md). It includes
the `manage` CLI source and an empty migration registry. Adding the
`che-orm` dependency alone does not add a CLI to an existing project.

Register models in `AppConfig::schema()` and application modules in `AppRegistry`,
in dependency order: parents before models with foreign keys. From the generated
application's directory, create the first migration:

```bash
cargo run --bin manage -- makemigrations --name initial
```

The generator compares registered models with the state replayed from previous
migrations, not with the live database. It writes a numbered Rust module and
updates `mod.rs` in the corresponding migration directory.
Generation needs no database connection, but requires `rustfmt`.

Initial generation orders individual operations by their dependencies,
including references to registered SQL tables and views. If new models in
different apps form a cycle, the generator can add their foreign keys in a
second phase after creating the tables. Views and triggers temporarily dropped
for a table rebuild are restored in the same migration. Each registered SQL
object uses one `CREATE` statement. A SQLite trigger body can contain several
commands.

A view that reads a column depends on its SQL name and type. Changing its
`DEFAULT` matters to a trigger that omits it from an `INSERT`. A star reads the
columns of its source, including the output of a subquery. PostgreSQL
`TABLE items` reads the same columns as `SELECT * FROM items`. If the generator
cannot resolve a SQL reference across a column removal, it reports
`NeedsManualMigration`. Use explicit source columns or provide a reviewed migration.

The planner records which migration supplies each required column, key, default or constraint.
The SQLite guard, sequence and generated-key builders provide those inputs from their typed
model fields. For reviewed SQL, pass the complete set of inputs to
`SchemaSet::declared_object(object, inputs)`. For example, a report reading only
`details.id` can declare `[SchemaRequirement::Column("details".into(), "id".into())]`.
A compatible append to a simple view projection keeps existing named outputs
available from the earlier migration. New outputs and wildcard consumers require
the expanded projection. PostgreSQL updates compatible definitions in place.

Use `ColumnList` for a projection that consumes all columns. Use `Relation` for the
existence of a relation. The ordinary SQL registration continues to infer inputs.
If an old definition has identical SQL, the generator can add its contract with a
state-only `SetSchemaRequirements` operation. This preserves the published checksum.

A report that needs populated values can declare `Data(table, column)`. This includes
all independent `BackfillColumn` migrations that contributed to the column.
A `CREATE TABLE ... AS SELECT` snapshot also requires backfills when it reads
through a view with declared input columns. Both `Default` and `WriteConstraint`
require the column itself. A composite `UNIQUE` affects writes to each member column.
For arbitrary data conversions, declare the required file dependency explicitly. During later column edits,
the backend uses the applied history to temporarily remove and restore compatible managed
views and triggers in dependency order inside the transaction. An incompatible definition
causes the transaction to roll back. Offline SQL preview uses the target's ancestors.

| Template layout | Where migrations live | How they are applied |
| --- | --- | --- |
| `single` | Shared `src/migrations/` directory. | The CLI reads the shared registry. |
| `modular` | `apps/<app>/src/migrations/` in the crate that owns the model. Each crate has its own numbering. | Root `src/migrations.rs` builds one graph from all registered crates. |

For example, `tasks:0001_initial` and `blog:0001_add_blog` are distinct
migrations even though both start at `0001`. If `blog` references a `tasks`
table, a graph dependency lets the CLI apply the `tasks` migration first.
A single `makemigrations` run can write files to multiple crates.

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

## Generation policy

For an intentional drop, pass its physical name, e.g.
`makemigrations --name cleanup --drop blog_post.obsolete`. Repeat `--drop` for
multiple resources. Editing a registered SQL table's definition, converting a
column type or adding a required column without a default requires a manual
migration (`NeedsManualMigration`). Use a reviewed `RunSql` with a physical
`ALTER TABLE` and state-only operations to replace the SQL definition without
dropping rows.

Adding a required column with a single-column unique constraint and a default
to an existing table also returns `NeedsManualMigration`. A constant default
would give existing rows the same value. The generator requires manual review
for SQL default expressions and managed timestamp defaults in this case.

1. Add the field as nullable without a default or unique constraint. Generate
   the migration.
2. Use `makemigrations --empty` to add a data migration. Fill each existing
   row with a distinct value, for example with `BackfillColumn` and the primary
   key if it suits the new field. Coordinate writes so new rows receive values
   before the final step.
3. Make the field required and unique in the model and generate the final
   migration. Ensure future inserts supply distinct values.

`generate_with_policy` accepts a `GenerationPolicy`. Its `sql_tables` map
declares each `SqlTableContract` (provided keys and required table/column pairs);
`external_keys` declares keys outside the project. Even without a contract,
`SchemaObjectKind::Table` has a known owner; the backend checks the physical
key. Use `strict_external: true` to reject undeclared targets.

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

`database_url()` in `example/orm-api/src/lib.rs` reads `CHE_ORM_DATABASE_URL`.
SQLite defaults to `app.db`; PostgreSQL defaults to `postgresql://localhost/che_orm`.
For PostgreSQL, add `--no-default-features --features postgres` before `--bin`.

The initial Rust migration `examples:0001_initial_models` in
`example/orm-api/src/migrations/` is shared by SQLite and PostgreSQL. If you
already applied the old SQL migration, set `CHE_ORM_DATABASE_URL` to a new
database. `makemigrations` and `migrate lint` are unavailable.

## Custom Registry and API

Keep models, `AppRegistry`, settings, and the migration registry outside the core
crate. Construct migrations with `Migration::new`, specifying stable
`MigrationId` values, dependencies, and ordered `MigrationOperation` values.

```rust
let graph = che_orm::MigrationGraph::new(application_migrations())?;
let applied = database.migrate(graph).await?;
```

`application_migrations()` is your application's function returning
`Vec<Migration>`. To generate new files, call
`graph.generate(&desired, MigrationLayout::PerApp, "update_models")` (or use
`MigrationLayout::Single("tasks")`) and pass each returned migration to
`render_migration_rust`. `desired` is the `ProjectState` built from the app
registry. Both template CLIs use this API; `ProjectState::diff` alone is not
an execution plan. The planner checks each proposed file against its ancestors
and the selected backend, without opening the database. A change that cannot
be scheduled reports the blocked operations before any files are written. Review
operations and SQL before applying them. `RunSql` must use the selected backend's
SQL and include matching `state_operations` when changing the schema.

Preserve the identities, contents, and checksums of applied migrations. Append
new operations in new migrations and plan data backfills explicitly. For
example, a required `DbEnum` column on a populated table needs a valid value
backfilled while retaining its `CHECK` constraint.

Apply migrations as a separate deployment/CI step. Reserve
`Database::create_table` for tests and local experiments. See the
[backend guide](backends.md#migrations) for locking, transaction, and SQL details.

## Renaming, manual migrations and branches

Confirm model identity changes and physical column renames explicitly. A Rust
field rename that retains its SQL name through `db_column` needs no migration.

```bash
cargo run --bin manage -- makemigrations --rename-model tasks.Task WorkItem --name rename_task
cargo run --bin manage -- makemigrations --rename-field tasks_task.notes description --name rename_notes
cargo run --bin manage -- makemigrations --empty tasks --name backfill_notes
cargo run --bin manage -- makemigrations --merge tasks --dry-run
cargo run --bin manage -- makemigrations --merge tasks
```

A model rename updates the identity in migration history. A changed table name
also generates a table rename. A column rename preserves its values. Update view and trigger definitions that
reference the old column. The generator drops dependent objects before renaming
the column and restores the registered definitions in dependency order. Opaque
CHECK expressions may require a manual migration. Use `--drop` for intentional
removal. An empty migration is registered with the current head as a dependency.
Fill in its operations before deploying, e.g. add a nullable column, run
`BackfillColumn`, then make it required using `AlterColumn`.

For single layouts use the shared migration owner. Modular projects store
the `che_sync` infrastructure owner's files in `src/che_sync_migrations/`.
After a Git merge, include both branches in the compiled registry before
running `--merge`. It joins independent heads and can combine distinct simple
nullable additions on the same table. Conflicting schema changes and opaque SQL
that may affect the other branch require manual review. This includes
`BackfillColumn` expressions that can read data modified by the other branch.

## Inspecting a deployment

```bash
cargo run --bin manage -- migrate status
cargo run --bin manage -- migrate tasks 0002_rename_task --plan
cargo run --bin manage -- sqlmigrate tasks 0002_rename_task
cargo run --bin manage -- migrate tasks 0002_rename_task
```

Status validates checksums and dependencies. Plan reads the recorded history and
lists pending operations. Use the complete app and migration name to apply a
forward target and its prerequisites. SQL preview renders the selected backend's
operation SQL offline. For a SQLite table rebuild, the preview includes dependent
views and triggers from the target's prerequisites. Execution also accounts for
objects from other migrations already applied to that database. Data checks,
locks and history writes occur during execution.

SQLite compares physical columns with history using `pragma_table_xinfo` before
rebuilding a table. A mismatch aborts the migration. Preserve data and reconcile
the schema with a reviewed operation before retrying.

SQLite may retain old index names after a historical table rename. When a table,
view or index needs such a name, the generator schedules its release first. SQLite
verifies the owner and definition of each index and recreates the index without
copying table rows. PostgreSQL uses native index renames when renaming a table.

Generated applications own their management CLI and registries. When updating
an existing application, port the corresponding template changes to its CLI.
For modular layouts also register `src/che_sync_migrations/mod.rs` among its
migration sources. Preserve existing migration files and dependencies.

### When a target deletes another application's data

The executor applies each migration file in full, including every pending file
the target depends on directly or indirectly. In this example, each file was
published in a separate release.

| Migration | Depends on | Operation |
| --- | --- | --- |
| `stock:0002_retire_archive` | `stock:0001_initial` | Drops the `archive` table |
| `stock:0003_create_products` | `stock:0002_retire_archive` | Creates the `products` table |
| `reports:0001_initial` | `stock:0003_create_products` | Creates a report on `products` |

If only `stock:0001_initial` has been applied, installing the report **drops
`archive` and its data**, even though the report only reads `products`.
On that database, `--plan` lists both `stock` migrations and the `reports` migration.

```bash
cargo run --bin manage -- migrate reports 0001_initial --plan
```

Before publishing the history, you can create `products` first, put the archive
drop in a later file, and make the report depend on the file that creates
`products`. Once the chain shown above is published, the drop is a prerequisite
of that target. Adding a later `reports` migration keeps that dependency.
