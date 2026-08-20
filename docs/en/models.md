# Models and Schema

## Derive Model

`#[derive(Model)]` generates the `Model` implementation and typed field constants.
Every model must have exactly one `#[orm(primary_key)]` field of type `i64`:

```rust
#[derive(Model)]
#[orm(table = "users", index("name"))]
struct User {
    #[orm(primary_key)]
    id: i64,
    email: String,
    name: String,
}
```

After the derive, `User::ID`, `User::EMAIL`, and `User::NAME` are available for
filters, ordering, and assignments. `User::primary_key()` is also used by the
high-level facade for `get`, `update`, and `delete`.

`Debug` in `#[derive(Debug, Model)]` is unrelated to the ORM. It is the standard
Rust derive for printing a model with `{:?}`.

## Table Attributes

### `table`

Sets the table name and is required:

```rust
#[orm(table = "users")]
```

### `index`

Creates an index after the table. Single-column and composite indexes are supported:

```rust
#[orm(table = "users", index("name"), index("tenant_id", "email"))]
```

### `unique`

Creates a table-level unique constraint. For one column, use `#[orm(unique)]` on
the field instead. The table-level form is for composite constraints:

```rust
#[orm(table = "memberships", unique("organization_id", "user_id"))]
```

Do not specify both forms for one field: this creates two uniqueness checks.

## Field Attributes

| Attribute | Purpose |
| --- | --- |
| `primary_key` | Primary key; `i64` receives backend identity/rowid behavior. |
| `unique` | Uniqueness of one column. |
| `default = "..."` | SQL expression for `DEFAULT`. The value is not parameterized. |
| `check = "..."` | SQL expression inside `CHECK (...)`. |
| `references = "roles(id)"` | Foreign key target. |
| `on_delete = "cascade"` | `ON DELETE` action for a foreign key. |
| `auto_now_add` | Managed timestamp on insertion. Requires `OffsetDateTime`. |
| `auto_now` | Managed timestamp on update. Requires `OffsetDateTime`. |

Example:

```rust
#[derive(Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    #[orm(check = "length(name) > 0")]
    name: String,
    #[orm(default = "true")]
    is_active: bool,
    #[orm(auto_now_add)]
    created_at: time::OffsetDateTime,
    #[orm(auto_now)]
    updated_at: time::OffsetDateTime,
}
```

## Supported Types

| Rust | SQLite SQL | Nullable |
| --- | --- | --- |
| `i64` | `INTEGER` | No |
| `String` | `TEXT` | No |
| `bool` | `INTEGER` (`0`/`1`) | No |
| `time::OffsetDateTime` | UTC `TEXT` | No |
| `Option<T>` | `T` type | Yes |

`Option<T>` is automatically considered nullable. `OffsetDateTime` uses the
`time` feature of `rusqlite` and UTC values.

## Timestamp Fields

`auto_now_add` and `auto_now` are intended for fields analogous to Django's
`auto_now_add` and `auto_now`.

- Both fields receive a SQLite `DEFAULT` based on `strftime(..., 'now')`.
- Managed fields are omitted from regular ORM `INSERT` statements.
- For `auto_now`, the ORM adds an assignment to every update query.
- Raw SQL through `transaction` does not update timestamps automatically.
- On reads, the value is decoded into `OffsetDateTime`.

The PostgreSQL compiler generates `TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP`, but
the PostgreSQL connection/executor is not yet included in the runtime.

## Relations

Models can be organized into application modules. An application implements
`AppConfig` and returns its own `SchemaSet`:

```rust
pub struct AccountsApp;

impl che_orm::AppConfig for AccountsApp {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}
```

Applications are then combined through `AppRegistry`:

```rust
let registry = che_orm::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

This separates model ownership while retaining one desired schema for Atlas.

A foreign key between ORM models is declared through `foreign_key`:

```rust
#[derive(Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

`foreign_key = User` generates `Post::USER`, a DDL foreign key to `users(id)`,
and `Post::USER.reverse()` for the reverse relation. The default reverse
relation name is `post_set`. `on_delete = "cascade"` adds cascade deletion. For
relation is generated for that field. When using `Database`, SQLite
`PRAGMA foreign_keys = ON` is enabled on every pool connection.

Load the relation through the typed field of the child model:

```rust
let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

This is equivalent to the filter `Post::query().filter(Post::USER_ID.eq(user_id))`.
For a collection of owners, do not call `fetch_by` in a loop: that creates N+1
queries. Use a batch query:

```rust
let users = database.all::<User>().await?;
let user_ids = users.iter().map(|user| user.id);
let posts = database.fetch_by_many(Post::USER_ID, user_ids).await?;
```

`fetch_by_many` performs one parameterized `IN` query and returns an empty list
for an empty set. The child rows must then be grouped by `post.user_id` in memory.

Typed eager-loading wrappers are available on querysets:

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::USER)
    .all(&database)
    .await?;

let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

`select_related` returns `WithOne`, while `prefetch_related` returns `Loaded`
with `LoadedMany`. A serializer receives these materialized values and has no
database access.

Multiple `belongs_to` relations can be loaded with a chainable API in one SQL
query. Each relation receives an alias from its FK name (`author_id` -> `author`):

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::AUTHOR)
    .select_related(Post::EDITOR)
    .all(&database)
    .await?;
```

For two FKs to one table, the generated relation markers differ, so `author` and
`editor` cannot be confused at compile time.

After `select_related`, joined model fields are available through the relation

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::USER)
    .filter(Post::USER.related_field(User::NAME).eq("Alice"))
    .order_by(Post::USER.related_field(User::NAME).asc())
    .all(&database)
    .await?;
```

Do not use such a call before `select_related`: the joined table alias exists
only in a materializing query.

A serializer declares JSON fields separately from the ORM model:

```rust
#[derive(che_orm::ModelSerializer)]
#[serializer(model = User)]
struct UserSerializer {
    #[serializer(read_only)]
    id: i64,
    email: String,
    name: String,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostSerializer>,
}
```

`PostUserRelation` is generated alongside the `Post` model; import it from the
model module. This intentionally binds the serializer at compile time to a
specific foreign key relation.

`UserSerializer` with a nested `many` field accepts the result of
`prefetch_related`: `Loaded<User, (LoadedMany<Post, PostUserRelation>,)>`.
The queryset must call `prefetch_related` first. The serializer does not accept
a `Database`, perform queries, or create N+1 queries.

Multiple reverse relations are loaded in a chain and serialized as a typed tuple:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .prefetch_related(Audit::USER.reverse())
    .all(&database)
    .await?;

let response = UserWithPostsAndAuditsSerializer::many(users);
```

For multiple nested fields, use `LoadedMany` and relation markers; the order of
`prefetch_related` must match the materialized graph. Use
`UserSerializer::many(...)` for multiple materialized objects:

```rust
let response = UserSerializer::many(users);
```

For a nested serializer, `many` accepts `Loaded` with the matching `LoadedMany`,
so an unloaded relation cannot accidentally be passed to the call. A complete
runnable example is in `che-orm-examples`:

```bash
cargo run -p che-orm-examples --bin serializers
```
