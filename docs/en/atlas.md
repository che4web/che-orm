# Migrations with Atlas

The migration CLI is part of the example application and runs from
`che-orm-examples`:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- makemigrations
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
cargo run -p che-orm-examples --bin manage -- migrate lint
```

## Temporary Schema File

The `makemigrations` command does not leave the generated desired schema in the
working tree:

1. `manage` builds a `SchemaSet` from application models.
2. It writes SQL to a temporary file in the system temporary directory.
3. It invokes Atlas with `--to file://<temporary-schema>`.
4. Atlas compares it with the migration directory and creates a versioned migration.
5. `manage` removes the temporary file regardless of the Atlas result.

This prevents a persistent `schema.sql` from falling out of sync with Rust models.

Models for Atlas are registered in `che-orm-examples/src/lib.rs`:

```rust
pub fn registry() -> AppRegistry {
    AppRegistry::new()
        .register::<accounts::App>()
        .register::<content::App>()
}
```

Each `AppConfig` owns its own set of models. Registration order determines the
SQL DDL order and must account for foreign keys.

The database path is in `che-orm-examples/src/lib.rs` and is used by the
example runtime and `manage`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

Application code passes this path to `Database::connect(...)`.

The order of `.model::<...>()` calls is the DDL order. Add parent tables first,
then tables with foreign keys.

## Commands

### `schema`

Prints the complete desired schema to stdout. This is useful for review:

```bash
cargo run -p che-orm-examples --bin manage -- schema > /tmp/schema.sql
```

### `makemigrations`

Generates a new migration. If no name is provided, one is generated
automatically in the form `auto_<unix_timestamp>`:

```bash
cargo run -p che-orm-examples --bin manage -- makemigrations
cargo run -p che-orm-examples --bin manage -- makemigrations add_user_status
```

The command uses `che-orm-examples/migrations/` and the SQLite development database
`sqlite://dev?mode=memory`.

### `migrate`

Applies pending migrations to the database:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
```

The application does not apply migrations automatically at startup. This is a
separate deployment/CI step.

### `migrate status` and `migrate lint`

```bash
cargo run -p che-orm-examples --bin manage -- migrate status
cargo run -p che-orm-examples --bin manage -- migrate lint
```

In older and canary Atlas versions, `migrate lint` may require `atlas login` or
Atlas Pro. This is an Atlas limitation, not one of the wrapper.
`makemigrations`, `migrate`, and `migrate status` work without this command.

## Atlas Executable

By default, the wrapper searches for the `atlas` executable in `PATH`. Set
`ATLAS_BIN` to use a different path:

```bash
ATLAS_BIN=/opt/atlas/atlas cargo run -p che-orm-examples --bin manage -- makemigrations add_posts
```

`manage` does not interpolate arguments through a shell and reports a nonzero
Atlas exit code as a process error.

## Required Enum Columns

Adding a required `DbEnum` field to a table that already has rows needs a data backfill. Review
the generated Atlas migration and set a valid enum value while copying the old rows, for example:

```sql
INSERT INTO new_tasks_task (id, name, status)
SELECT id, name, 'draft' FROM tasks_task;
```

The replacement table must keep the enum `CHECK` constraint. Test the migration against a copy of
an existing database before deployment.

## Production Workflow

1. Change the Rust model.
2. Run `makemigrations`.
3. Review the generated SQL.
4. Run `migrate lint` in CI.
5. Commit migration files and `atlas.sum`.
6. Run `migrate` during deployment.

After moving to Atlas, do not use `Database::create_table` for production schema
initialization: this method is intended for tests and local scenarios.
