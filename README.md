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

## Status

This is an early MVP. SQLite CRUD, simple relations, schema snapshots, and migration SQL generation are implemented. QuerySet-style filtering and production-grade migration operations are still in progress.
