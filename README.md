# che-orm

Experimental Rust ORM inspired by Django ORM.

Current workspace crates:

- `che-orm`: runtime ORM API, SQLite backend, schema metadata, migration helpers.
- `che-orm-macros`: `#[derive(Model)]` implementation.
- `che-orm-cli`: migration CLI binary named `che-orm`.
- `che-orm-examples`: runnable examples, not published.

## Quick Example

```rust
use che_orm::{Model, SqliteBackend};

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
let db = SqliteBackend::connect("sqlite::memory:").await?;
db.create_table::<User>().await?;

let mut user = User::objects(&db)
    .create()
    .set("email", "alice@example.com")
    .set("name", "Alice")
    .execute()
    .await?;

user.name = "Alicia".to_string();
let user = user.save(&db).await?;
# Ok(())
# }
```

## Query Example

The generated `UserFields` constants support Django-style filters and `Q`
expressions:

```rust
use che_orm::Q;

let user = User::objects(&db)
    .query()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(
        Q::from(UserFields::NAME.contains("Ali"))
            .or(UserFields::ID.in_values([1_i64, 2, 3])),
    )
    .order_by_desc(UserFields::ID)
    .first()
    .await?;
```

Queries also support NULL checks, pagination, multiple ordering expressions,
`count`, and numeric `sum`/`avg`/`min`/`max` aggregates.

## Examples

```bash
cargo run -p che-orm-examples --bin crud
cargo run -p che-orm-examples --bin relations
cargo run -p che-orm-examples --bin schema_snapshot
```

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

Schema changes to field properties are detected. SQLite migrations rebuild a
table when a column must be altered or removed, preserving shared column data.
The CLI rejects required columns without defaults when existing rows may not be
populated safely.

## Status

This is an early MVP. SQLite CRUD, Django-style query expressions, simple relations, schema snapshots, and migration SQL generation are implemented. Query joins, custom indexes, rename detection, and rollback migrations are still in progress.
