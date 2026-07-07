# che-orm

`che-orm` is an experimental Rust ORM inspired by Django ORM.

The current MVP focuses on SQLite, model derive macros, basic CRUD, schema metadata, migration SQL generation, and simple foreign-key relations.

## Model Definition

```rust
use che_orm::{Model, SqliteBackend};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,

    #[field(unique, max_length = 255)]
    email: String,

    name: String,

    #[field(default = true)]
    is_active: bool,
}
```

`#[derive(Model)]` generates:

- `impl Model for User`
- `impl SqliteModel for User`
- `UserCreate`
- `UserUpdate` for low-level update calls
- schema metadata used by migrations

## CRUD

```rust
# use che_orm::{Model, SqliteBackend};
# #[derive(Debug, Clone, Model)]
# #[model(table = "users")]
# struct User {
#     #[field(primary_key)]
#     id: i64,
#     email: String,
#     name: String,
#     is_active: bool,
# }
# async fn example() -> che_orm::Result<()> {
let db = SqliteBackend::connect("sqlite::memory:").await?;
db.create_table::<User>().await?;

let user = User::objects(&db)
    .create(UserCreate {
        email: "alice@example.com".to_string(),
        name: "Alice".to_string(),
        is_active: true,
    })
    .await?;

let mut fetched = User::objects(&db).get(user.id).await?;

fetched.name = "Alicia".to_string();
fetched.is_active = false;
let updated = fetched.save(&db).await?;

let updated_without_loading = User::objects(&db)
    .update_fields(user.id)
    .set("name", "Alice Updated")
    .execute()
    .await?;

let users = User::objects(&db).all().await?;

User::objects(&db).delete(user.id).await?;
# Ok(())
# }
```

The external API does not require application code to use `sqlx` directly. `sqlx` is currently an internal SQLite implementation detail.

## Relations

```rust
use che_orm::{Model, SqliteBackend};

#[derive(Debug, Clone, Model)]
#[model(table = "authors")]
struct Author {
    #[field(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Model)]
#[model(table = "posts")]
struct Post {
    #[field(primary_key)]
    id: i64,

    #[field(foreign_key = Author)]
    author_id: i64,

    title: String,
}
```

`#[field(foreign_key = Author)]` generates schema metadata and SQLite `REFERENCES authors(id)`.

```rust
# use che_orm::{Model, SqliteBackend};
# #[derive(Debug, Clone, Model)]
# #[model(table = "authors")]
# struct Author { #[field(primary_key)] id: i64, name: String }
# #[derive(Debug, Clone, Model)]
# #[model(table = "posts")]
# struct Post { #[field(primary_key)] id: i64, #[field(foreign_key = Author)] author_id: i64, title: String }
# async fn example() -> che_orm::Result<()> {
# let db = SqliteBackend::connect("sqlite::memory:").await?;
# db.create_table::<Author>().await?;
# db.create_table::<Post>().await?;
# let author = Author::objects(&db).create(AuthorCreate { name: "Alice".to_string() }).await?;
# let post = Post::objects(&db).create(PostCreate { author_id: author.id, title: "Hello".to_string() }).await?;
let loaded_author = Post::objects(&db)
    .get_related::<Author>(post.author_id)
    .await?;

let author_posts = Post::objects(&db)
    .filter_by_i64("author_id", author.id)
    .await?;
# Ok(())
# }
```

## Schema Snapshots

The ORM can serialize model metadata to a JSON schema snapshot.

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

The CLI uses this snapshot as the current schema input for `makemigrations`.

## Migration API

Create one table directly from a model:

```rust
# use che_orm::{Model, SqliteBackend};
# #[derive(Debug, Clone, Model)]
# #[model(table = "users")]
# struct User { #[field(primary_key)] id: i64, email: String }
# async fn example() -> che_orm::Result<()> {
let db = SqliteBackend::connect("sqlite::memory:").await?;
db.create_table::<User>().await?;
# Ok(())
# }
```

Apply migration files from a directory:

```rust
# use che_orm::SqliteBackend;
# async fn example() -> che_orm::Result<()> {
let db = SqliteBackend::connect("sqlite://app.sqlite").await?;
let applied = db.apply_migrations_dir("migrations").await?;
# Ok(())
# }
```

## Supported Field Attributes

- `#[field(primary_key)]`
- `#[field(auto)]`
- `#[field(unique)]`
- `#[field(max_length = 255)]`
- `#[field(default = true)]`
- `#[field(rename = "db_column")]`
- `#[field(foreign_key = OtherModel)]`

## Current Limitations

- SQLite only.
- QuerySet-style filtering is not implemented yet.
- Relations are minimal and currently use explicit FK ids.
- Migration diff supports create table, drop table, add column, and manual comments for dropped columns.
- Rename detection and safe SQLite table rebuilds are not implemented yet.

## Examples

Runnable examples are in `crates/che-orm-examples`.

```bash
cargo run -p che-orm-examples --bin crud
cargo run -p che-orm-examples --bin relations
cargo run -p che-orm-examples --bin schema_snapshot
```
