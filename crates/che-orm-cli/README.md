# che-orm CLI

`che-orm-cli` provides migration commands for `che-orm`.

The binary name is `che-orm`.

## Commands

- `makemigrations`: compare the previous schema snapshot with the current schema snapshot and generate a SQLite `.sql` migration file.
- `migrate`: apply unapplied migration files to a SQLite database.

## Generate Current Schema

The CLI does not inspect Rust source code directly. Your application generates the current schema snapshot using `che-orm` runtime metadata.

Example:

```rust
use che_orm::{ModelSchema, Schema};

# use che_orm::Model;
# #[derive(Debug, Clone, Model)]
# #[model(table = "users")]
# struct User { #[field(primary_key)] id: i64, email: String }
let schema = Schema::from_models(vec![
    ModelSchema::from_model::<User>(),
]);

schema.save("che_orm_schema.json")?;
# Ok::<(), che_orm::Error>(())
```

The examples crate has a runnable snapshot generator:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
```

## Create Migrations

From the workspace root:

```bash
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
```

Defaults:

- `--schema che_orm_schema.json`
- `--migrations-dir migrations`
- `--name auto`

Generated files:

```text
migrations/
  0001_initial.sql
  schema.json
```

`migrations/schema.json` is the last committed schema snapshot used for the next diff.

## Apply Migrations

```bash
cargo run -p che-orm-cli -- migrate --config app.toml
```

With explicit migration directory:

```bash
cargo run -p che-orm-cli -- migrate \
  --config app.toml \
  --migrations-dir migrations
```

The config file must contain:

```toml
[database]
url = "sqlite://app.sqlite?mode=rwc"
```

You can still override the config with an explicit database URL:

```bash
cargo run -p che-orm-cli -- migrate --database-url sqlite://app.sqlite?mode=rwc
```

The CLI creates and uses this bookkeeping table:

```sql
CREATE TABLE IF NOT EXISTS _che_orm_migrations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    checksum TEXT,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

## Migration Diff Support

Currently supported:

- new table -> `CREATE TABLE IF NOT EXISTS ...`
- removed table -> `DROP TABLE IF EXISTS ...`
- new column -> `ALTER TABLE ... ADD COLUMN ...`
- changed column properties -> SQLite table rebuild
- removed column -> SQLite table rebuild
- added/removed modeled indexes -> `CREATE INDEX` / `DROP INDEX`

The generated rebuild preserves values in columns shared by the old and new
schemas. Before generating a migration, `makemigrations` rejects a new required
column without a default and a nullable-to-required change without a default.
Type changes are rejected and require an explicit data migration. Column and
table renames are currently treated as remove/add operations.

When a generated rebuild changes a table referenced by another table, the
runtime uses a dedicated SQLite connection, temporarily disables FK checks,
runs `PRAGMA foreign_key_check`, and restores enforcement before completing the
migration. This prevents SQLite `ON DELETE` actions from mutating child rows
during the temporary table drop.

Run `migrate` from exactly one process at a time for a database. Concurrent
migration runners are not supported.

Use `status` to inspect migration files and their applied state:

```bash
cargo run -p che-orm-cli -- status --config app.toml
```

Applied migration files are checksum-validated. Editing a migration after it
has been applied causes `migrate` to fail instead of silently accepting drift.

## End-To-End Example

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --config app.toml
```

## Notes

- The CLI is SQLite-focused in the current MVP.
- SQL execution is hidden behind the `che-orm` runtime API; application code does not need to call `sqlx` directly.
- Keep generated migration files under version control.
- Run `migrate` from one process at a time per database.
