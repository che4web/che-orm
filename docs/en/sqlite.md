# SQLite Runtime

## Connection

```rust
let database = che_orm::Database::connect("app.db")?;
```

`Database` contains a `deadpool-sqlite::Pool`. The default pool size is 4.
To configure it:

```rust
let database = che_orm::Database::connect_with_pool_size("app.db", 8)?;
```

For tests:

```rust
let database = che_orm::Database::connect_in_memory()?;
```

An in-memory database must use a pool size of 1, otherwise every connection
receives a separate `:memory:` database.

## Creating the Schema

```rust
database.create_table::<User>().await?;
```

The operation runs:

1. `CREATE TABLE`;
2. every `CREATE INDEX` declared in the model metadata.

To inspect SQL without a connection:

```rust
let compiled = che_orm::SqlCompiler::<che_orm::SqliteDialect>
    ::compile(&User::create_table().into_ast());
println!("{}", compiled.sql);
```

## High-Level CRUD

The facade API returns models including the generated primary key and managed

```rust
let user = database
    .create::<User>()
    .set(User::EMAIL, "alice@example.test")
    .set(User::NAME, "Alice")
    .set(User::IS_ACTIVE, true)
    .execute()
    .await?;

let loaded = database.get::<User>(user.id).await?;
let users = database
    .query::<User>()
    .filter(User::IS_ACTIVE.eq(true))
    .order_by(User::NAME.asc())
    .limit(20)
    .all(&database)
    .await?;

let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;

let deleted = database.delete::<User>(user.id).await?;
```

`get` and `first` return `Option` when the row is absent. `update` also returns
`Option`; `delete` returns `bool`.

`create` and `update` use SQLite `RETURNING` and return values obtained directly
from the ORM operation. The ORM does not create `AFTER INSERT` or `AFTER UPDATE`
triggers, so when the database is used only through the ORM, the returned model
matches the stored values. If such triggers are added manually, through raw SQL,
or in a migration, they can change the row after SQLite produces the `RETURNING`
result; that value does not reflect later trigger changes.

## Low-Level Insert

```rust
let user = User {
    id: 0,
    email: "alice@example.test".into(),
    name: "Alice".into(),
    is_active: true,
    created_at: time::OffsetDateTime::now_utc(),
    updated_at: time::OffsetDateTime::now_utc(),
};

let result = database.insert(&user).await?;
println!("{}", result.rows_affected);
println!("{:?}", result.last_insert_rowid);
```

`primary_key`, `auto_now_add`, and `auto_now` are not included in a regular
insert. The `auto_now` field is added automatically to an ORM-generated update.
All other fields are passed as SQL parameters.

For normal creation, use `database.create::<User>()`. The low-level `insert`
preserves `ExecuteResult` and is for cases where the created model does not need

## Typed Select Facade

```rust
use che_orm::Model;

let users = database
    .query::<User>()
    .filter(User::IS_ACTIVE.eq(true))
    .order_by(User::NAME.asc())
    .limit(20)
    .all(&database)
    .await?;

let first = database
    .query::<User>()
    .filter(User::EMAIL.eq("alice@example.test"))
    .first(&database)
    .await?;
```

Filter values are parameterized. Table and column names come from derive
metadata and must not be supplied by the user.

## Low-Level Update and Delete

```rust
let update = User::update()
    .set(User::NAME, "Alice Cooper")
    .filter(User::ID.eq(1))
    .into_ast()?;
database.execute(update).await?;

let delete = User::delete()
    .filter(User::ID.eq(1))
    .into_ast()?;
database.execute(delete).await?;
```

The `update::<User>(id)` and `delete::<User>(id)` facades automatically add a
primary key filter. When working with the AST directly, update/delete without a
`filter` return `QueryBuildError::MissingFilter`. If a bulk operation is truly
needed:

```rust
let query = User::update()
    .set(User::IS_ACTIVE, false)
    .allow_all()
    .into_ast()?;
```

## Related Rows

Declare the child model with a foreign key:

```rust
#[derive(Debug, che_orm::Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

Create tables in dependency order and load child rows:

```rust
database.create_table::<User>().await?;
database.create_table::<Post>().await?;

let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

For multiple users, use batch loading to avoid N+1 queries:

```rust
let users = database.all::<User>().await?;
let user_ids = users.iter().map(|user| user.id);
let posts = database.fetch_by_many(Post::USER_ID, user_ids).await?;
```

`fetch_by_many` builds a parameterized `IN` query. An empty set returns an empty
list without accessing the database. Results should be grouped by `post.user_id`
in memory. For many keys, split them into chunks to stay within SQLite's limit on
bind parameters.

`Database` enables `PRAGMA foreign_keys = ON` whenever it obtains a connection,
so a nonexistent `user_id` fails with a SQLite error, and deleting a user
cascades to its posts.

`foreign_key = User` also generates the typed relation `Post::USER`. Use
`select_related(Post::USER)` for `belongs_to`, and for `has_many`:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

`select_related` returns `WithOne<Post, User>`, while `prefetch_related` returns
`Loaded<User, (LoadedMany<Post, _>,)>`. Both results can be passed to a matching
serializer. The serializer does not receive `Database` or execute additional
queries.

A nullable foreign key supports `LEFT JOIN`:

```rust
#[orm(foreign_key = User, on_delete = "set null")]
user_id: Option<i64>,
```

Such a `select_related` returns `WithOptionalOne<Post, User>`, where `related`
may be `None`.

## Transactions

```rust
database
    .transaction(|connection| {
        connection.execute(
            "UPDATE users SET is_active = ?1 WHERE id = ?2",
            (false, 1),
        )?;
        Ok(())
    })
    .await?;
```

The closure receives a blocking `rusqlite::Connection`, but it runs in a
`deadpool-sqlite` worker thread, so the SQLite operation does not run on a Tokio
executor thread. An error from the closure rolls back; success commits.

## Errors and Limitations

`OrmError` combines pool errors, SQLite errors, interaction errors, and query
build errors.

The current runtime does not include:

- automatic schema diffing;
- a PostgreSQL connection pool;
