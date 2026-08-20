# Step-by-Step Tutorial

This tutorial creates two related models, stores data in SQLite, loads relations,
`create_table` is intended only for tests and local experiments.

## 1. Create an Application

```bash
cargo new blog-app
```

Add a local dependency on `che-orm` to `Cargo.toml`. Adjust the path for your
application's location:

```toml
[dependencies]
che-orm = { path = "../che-orm" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
time = { version = "0.3", features = ["formatting", "parsing"] }
```

## 2. Declare Models

Replace `src/main.rs` with this code. `foreign_key = User` creates a foreign key
in DDL, `Post::USER` for `belongs_to`, and `Post::USER.reverse()` for `has_many`.

```rust
use che_orm::{Database, Model};
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "users", index("name"))]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    name: String,
    #[orm(auto_now_add)]
    created_at: OffsetDateTime,
    #[orm(auto_now)]
    updated_at: OffsetDateTime,
}

#[derive(Debug, Model)]
#[orm(table = "posts", index("user_id"))]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}

#[tokio::main]
async fn main() -> Result<(), che_orm::OrmError> {
    let database = Database::connect_in_memory()?;
    database.create_table::<User>().await?;
    database.create_table::<Post>().await?;

    Ok(())
}
```

Models must have exactly one `#[orm(primary_key)]` field of type `i64`. The
`auto_now_add` and `auto_now` fields must be `OffsetDateTime`; the ORM manages

## 3. Create and Read a Row

Add this to `main` after creating the tables:

```rust
let user = database
    .create::<User>()
    .set(User::EMAIL, "alice@example.test")
    .set(User::NAME, "Alice")
    .execute()
    .await?;

let users = database
    .query::<User>()
    .filter(User::NAME.eq("Alice"))
    .order_by(User::EMAIL.asc())
    .all(&database)
    .await?;

let loaded = database.get::<User>(user.id).await?;
println!("{users:?}");
println!("{loaded:?}");
```

`filter` creates a parameterized SQL condition. Queryset terminal methods such
as `all`, `first`, and `count` receive `&Database`:

```rust
let first = database
    .query::<User>()
    .filter(User::EMAIL.eq("alice@example.test"))
    .first(&database)
    .await?;
```

Run the application:

```bash
cargo run
```

## 4. Update a Row

The high-level facade always restricts `update` by primary key:

```rust
let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;

println!("{updated:?}");
```

`update` returns `Option<User>` if the row could have been deleted before the
update. For complex or bulk operations, use the AST builder and add a `filter`;
a call without a filter is rejected unless `allow_all()` is explicitly specified.

## 5. Load Related Data

Create a post for the previously created user:

```rust
let post = database
    .create::<Post>()
    .set(Post::USER_ID, user.id)
    .set(Post::TITLE, "First post")
    .execute()
    .await?;

let posts = database.fetch_by(Post::USER_ID, user.id).await?;
println!("{post:?}");
println!("{posts:?}");
```

For a user list, do not call `fetch_by` in a loop. Use `prefetch_related`: the
ORM performs a user query and one batch query for posts.

```rust
let users_with_posts = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

The result has type `Loaded<User, (LoadedMany<Post, PostUserRelation>,)>`. For
returns `WithOne<Post, User>`:

```rust
let posts_with_user = database
    .query::<Post>()
    .select_related(Post::USER)
    .all(&database)
    .await?;
```

A nullable foreign key (`Option<i64>`) with `on_delete = "set null"` uses a
`LEFT JOIN`; its `select_related` returns `WithOptionalOne`.

Deletion through the high-level facade is also restricted by primary key. In this
example it cascade-deletes the created posts:

```rust
let deleted = database.delete::<User>(user.id).await?;
assert!(deleted);
```

## 6. Move to Atlas Migrations

Do not use `create_table` in production. Split models into application modules,
implement `AppConfig`, and register applications in dependency order: parent
tables must come before tables that declare foreign keys to them.

```rust
pub struct AccountsApp;

impl che_orm::AppConfig for AccountsApp {
    fn name() -> &'static str {
        "accounts"
    }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}

let registry = che_orm::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

In this repository, the example application keeps its registry, database path,
and migrations in `che-orm-examples`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

Atlas must be available in `PATH`. Run these commands from the repository root:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- makemigrations initial_schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

Review the SQL in a new migration before applying it and commit the migration
file together with `che-orm-examples/migrations/atlas.sum`. See [Atlas migrations](atlas.md) for details.

## 7. Explore Runnable Examples

This repository includes examples using the current public API:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin serializers
cargo run -p che-orm-examples --bin transactions
```

Next reference material: [models and schema](models.md),
[SQLite runtime](sqlite.md), and [Atlas migrations](atlas.md).
