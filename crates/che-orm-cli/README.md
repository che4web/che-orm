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
cargo run -p che-orm-cli -- migrate --database-url sqlite://app.sqlite
```

With explicit migration directory:

```bash
cargo run -p che-orm-cli -- migrate \
  --database-url sqlite://app.sqlite \
  --migrations-dir migrations
```

The CLI creates and uses this bookkeeping table:

```sql
CREATE TABLE IF NOT EXISTS _che_orm_migrations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

## Migration Diff Support

Currently supported:

- new table -> `CREATE TABLE IF NOT EXISTS ...`
- removed table -> `DROP TABLE IF EXISTS ...`
- new column -> `ALTER TABLE ... ADD COLUMN ...`
- removed column -> generated comment requiring manual review

SQLite column drops are intentionally not executed automatically yet because safe support requires table rebuild logic.

## End-To-End Example

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url sqlite://example.sqlite
```

## Notes

- The CLI is SQLite-focused in the current MVP.
- SQL execution is hidden behind the `che-orm` runtime API; application code does not need to call `sqlx` directly.
- Keep generated migration files under version control.
