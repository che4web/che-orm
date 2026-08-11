# che-orm

Experimental Rust ORM inspired by Django ORM.

Current workspace crates:

- `che-orm`: runtime ORM API, compile-time SQLite or PostgreSQL backend, schema metadata, migration helpers.
- `che-orm-macros`: `#[derive(Model)]` implementation.
- `che-orm-cli`: migration CLI binary named `che-orm`.
- `che-orm-examples`: runnable examples, not published.

## Quick Example

```rust
use che_orm::{Database, Model};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
    name: String,
    #[field(default = false)]
    is_active: bool,
}

# async fn example() -> che_orm::Result<()> {
let db = Database::connect("sqlite::memory:").await?;
db.create_table::<User>().await?;

let mut user = db
    .create::<User>()
    .set("email", "alice@example.com")
    .set("name", "Alice")
    .execute()
    .await?;

user.name = "Alicia".to_string();
let user = db.save(&user).await?;
# Ok(())
# }
```

## Query Example

The generated `UserFields` constants support Django-style filters and `Q`
expressions:

```rust
use che_orm::Q;

let user = db
    .query::<User>()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(
        UserFields::NAME.contains("Ali")
            .or(UserFields::ID.in_values([1_i64, 2, 3])),
    )
    .order_by_desc(UserFields::ID)
    .first()
    .await?;
```

Queries support NULL checks, pagination, multiple ordering expressions, and
`count`. Aggregates, projections, grouped queries, and relations are not part
of the backend-neutral builder yet.

## Examples

```bash
cargo run -p che-orm-examples --bin crud
cargo run -p che-orm-examples --bin relations
cargo run -p che-orm-examples --bin schema_snapshot
```

## Features

`che-orm` selects exactly one backend when compiling an application. The default
is `sqlite`; use PostgreSQL with:

```toml
che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
```

The selected feature enables only its matching SQLx driver. SQLite query
builders, relations, `QueryValue`, and `AggregateValue` are SQLite-only.
`DatabaseValue`, model metadata, `FilePath`, and `#[derive(Model)]` work with
both backends.

Migration authoring is also compile-time selected. `migration-native` is the
default and supports SQLite schema diffs. `migration-atlas` supports Atlas
generation for either backend. With neither authoring feature, use manually
authored SQLx migration files.

## Migrations

Generate a schema snapshot:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
```

Create and apply migrations:

```bash
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url sqlite://example.sqlite
```

Migration execution is provided by SQLx and is tracked in `_sqlx_migrations`.
Existing development databases using the old `_che_orm_migrations` table must
be recreated.

Schema changes to field properties are detected. SQLite migrations rebuild a
table when a column must be altered or removed, preserving shared column data.
The CLI rejects required columns without defaults when existing rows may not be
populated safely.

PostgreSQL uses SQLx to apply manual migrations. Build the CLI with
`--no-default-features --features postgres` for `migrate` and `status`; add
`migration-atlas` when using Atlas to author PostgreSQL migrations.

## Status

This is an early MVP. SQLite CRUD, Django-style query expressions, simple relations, schema snapshots, and migration SQL generation are implemented. Query joins, custom indexes, rename detection, and rollback migrations are still in progress.
