# SQLite Runtime

## Connection

```rust
let database = che_orm::Database::connect("app.db").await?;
```

`Database` contains a `deadpool-sqlite::Pool`. The default pool size is 4.
To configure it:

```rust
let database = che_orm::Database::connect_with_pool_size("app.db", 8).await?;
```

For tests:

```rust
let database = che_orm::Database::connect_in_memory().await?;
```

An in-memory database must use a pool size of 1, otherwise every connection
receives a separate `:memory:` database.

## Timestamp comparisons

`OffsetDateTime` and `Option<OffsetDateTime>` remain RFC3339 `TEXT` columns.
Derived timestamp fields use `COLLATE che_datetime` in filters and ordering,
including joined fields. Each ORM connection registers this collation, which
compares instants at nanosecond precision. Existing values with different
fractional precision or UTC offsets work without rewriting data or migrations.
PostgreSQL continues to use native timestamp comparisons.

For hand-written ASTs, use `ColumnRef::datetime(table, column)`; manually declared
timestamp fields use `ModelField::datetime(table, column)`. `ColumnRef::new` and
`ModelField::new` retain ordinary SQL comparison semantics. Raw SQL must request
`COLLATE che_datetime` explicitly. External SQLite connections executing that SQL
must also register the collation. Existing binary text indexes cannot serve
chronological comparisons; declare a matching `COLLATE che_datetime` index in an
application-owned migration when needed.

An offset without a limit compiles to `LIMIT -1 OFFSET ...` on SQLite.

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

`create`, `update` and `save` use SQLite `RETURNING` and return values obtained directly
from the ORM operation. The ORM does not create `AFTER INSERT` or `AFTER UPDATE`
triggers, so when the database is used only through the ORM, the returned model
matches the stored values. If such triggers are added manually, through raw SQL,
or in a migration, they can change the row after SQLite produces the `RETURNING`
result; that value does not reflect later trigger changes.

## Saving a Materialized Model

`save` updates every ordinary field of an existing row by primary key:

```rust
let mut user = database.get::<User>(user_id).await?.unwrap();
user.name = "Alice Cooper".into();
let saved = database.save(&user).await?; // Option<User>
```

It never inserts a new row: a missing row returns `None`. The stored primary key
and `auto_now_add` fields are preserved, while the ORM refreshes `auto_now`.
Ordinary fields, including nullable values, JSON and trusted `ImageField` keys,
come from the object; `db_column` mappings are respected. The result is the
`RETURNING` model; the supplied object is not modified. Models without ordinary
fields or managed updates return `EmptyUpdate`. For individual field changes,
use `update().set(...)`: `save` writes all ordinary fields and can overwrite
changes made since the object was loaded.

Inside a transaction use `tx.save(&user).await?`. For synchronized models, use
the configured `client.database()` / `server.database()` handle to commit the
write and sync metadata together.

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

Integer `primary_key`, `auto_now_add`, and `auto_now` fields are omitted from
`database.insert(&model)`. UUID keys are supplied from the model as 16-byte
BLOBs. For UUID models, `database.create::<User>()` generates UUIDv7 when the
key is not supplied through `.set(User::ID, uuid)`.
The `auto_now` field is added automatically to an ORM-generated update.
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
in memory. The ORM automatically chunks keys to respect SQLite's bind-parameter
limit and combines results without duplicate primary keys.

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
    .transaction(|tx| Box::pin(async move {
        tx.update::<User>(1).set(User::IS_ACTIVE, false).execute().await?;
        Ok(())
    }))
    .await?;
```

The closure receives the common `Transaction`. Every ORM operation is async;
SQLite executes it in the pool's worker thread. Errors roll back and success
commits. Direct driver access is available through SQLite-only `raw_transaction`.

## Errors and Limitations

`OrmError` includes configuration, pool, driver, decoding, query and migration
errors. `constraint_kind()` classifies constraint violations on both backends.
Use `Database::migrate` for compiled migrations. See the
[backend guide](backends.md) for PostgreSQL configuration.
