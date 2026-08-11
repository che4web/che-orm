# che-orm CLI

`che-orm-cli` provides migration commands for `che-orm`.

The binary name is `che-orm`.

## Commands

- `makemigrations`: compare schema snapshots and generate migration SQL. It is
  available only with `migration-native` or `migration-atlas`.
- `migrate`: apply unapplied migration files through SQLx.
- `status`: show migration files and their SQLx application state.

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
  20260810120000_initial.sql
  schema.json
```

`migrations/schema.json` is the last committed schema snapshot used for the next diff.

Use Atlas for migration generation:

```bash
CHE_ORM_ATLAS_BIN=atlas \
CHE_ORM_ATLAS_DEV_URL='sqlite://file?mode=memory' \
cargo run -p che-orm-cli --no-default-features --features sqlite,migration-atlas -- makemigrations --name add_posts
```

`CHE_ORM_ATLAS_BIN` defaults to `atlas`, and `CHE_ORM_ATLAS_DEV_URL` defaults
to `sqlite://file?mode=memory`. These variables are only needed for
the `migration-atlas` feature; applying migrations uses SQLx directly.

PostgreSQL migration application uses SQLx and manual migration files. Native
generation is SQLite-only. Build the CLI with the `postgres` feature and use a
separate migration directory:

```bash
cargo run -p che-orm-cli --no-default-features --features postgres -- migrate --config app.toml
```

Add `migration-atlas` to author PostgreSQL migrations with Atlas. A CLI built
without either migration authoring feature exposes only `migrate` and `status`.

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

The CLI delegates migration execution and bookkeeping to SQLx. It creates and
uses this table:

```sql
CREATE TABLE IF NOT EXISTS _sqlx_migrations (
    version BIGINT PRIMARY KEY NOT NULL,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
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
generated SQL currently contains che-orm-specific handling that is intended for
`apply_sql`. Such migrations need to be reviewed before being run through SQLx.

SQLx validates checksums of previously applied migrations and fails if a
migration file is edited after application. Existing development databases that
use `_che_orm_migrations` must be recreated in this breaking release.

Use `status` to inspect migration files and their applied state:

```bash
cargo run -p che-orm-cli -- status --config app.toml
```

Applied migration files are checksum-validated by SQLx. Editing a migration
after it has been applied causes `migrate` to fail instead of silently
accepting drift.

## End-To-End Example

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --config app.toml
```

## Notes

- The default CLI build targets SQLite. PostgreSQL builds support SQLx migration
  application and Atlas-based migration authoring.
- SQL execution is hidden behind the `che-orm` runtime API; application code does not need to call `sqlx` directly.
- Keep generated migration files under version control.
- Run `migrate` from one process at a time per database.
