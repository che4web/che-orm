# che-orm

`che-orm` is an experimental Rust ORM inspired by Django ORM.

The current MVP focuses on SQLite, model derive macros, CRUD, Django-style queries, schema metadata, migration SQL generation, and simple foreign-key relations.

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
- `UserUpdate` for low-level update calls
- `UserFields` typed query fields
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
    .create()
    .set("email", "alice@example.com")
    .set("name", "Alice")
    .set("is_active", true)
    .execute()
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

Use `#[field(foreign_key = OtherModel)]` on an integer id field to declare a
SQLite foreign key. The field still stores the related model id; relation loading
is explicit.

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
Create the referenced table before the table that contains the foreign key, or
generate/apply migrations in dependency order.

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
let author = Author::objects(&db)
    .create()
    .set("name", "Alice")
    .execute()
    .await?;

let post = Post::objects(&db)
    .create()
    .set("author_id", author.id)
    .set("title", "Hello")
    .execute()
    .await?;

let loaded_author = Post::objects(&db)
    .get_related::<Author>(post.author_id)
    .await?;
assert_eq!(loaded_author.name, "Alice");

let author_posts = Post::objects(&db)
    .query()
    .filter(PostFields::AUTHOR_ID.eq(author.id))
    .all()
    .await?;
assert_eq!(author_posts.len(), 1);
# Ok(())
# }
```

## Queries

Use generated `ModelFields` constants with the Django-style query builder:

```rust
let users = User::objects(&db)
    .query()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(UserFields::NAME.contains("Ali"))
    .order_by("-name")
    .order_by(UserFields::ID)
    .limit(20)
    .all()
    .await?;
```

Repeated `filter` calls are combined with `AND`. Use `Q` for grouped boolean
expressions:

```rust
use che_orm::Q;

let user = User::objects(&db)
    .query()
    .filter(
        Q::from(UserFields::NAME.contains("Ali"))
            .or(UserFields::ID.in_values([1_i64, 2, 3]))
            .and(UserFields::EMAIL.is_not_null()),
    )
    .first()
    .await?;
```

Supported predicates include `eq`, `contains`, `gt`, `gte`, `lt`, `lte`,
`in_values`, `is_null`, and `is_not_null`. An empty `in_values` list matches no
rows. `first()` returns `Option<User>` and preserves ordering and offset.

The query builder also supports `count()` and numeric aggregates:

```rust
let active_count = User::objects(&db)
    .query()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .count()
    .await?;
let highest_id = User::objects(&db)
    .query()
    .max(UserFields::ID)
    .await?;
```

`sum`, `avg`, `min`, and `max` return `Option<f64>` and accept integer and
real fields. An empty result returns `None`.

For REST serializers, keep the foreign key id as a normal writable field and add
a read-only related field when you want nested output:

```rust
# use che_orm::Model;
# use che_rest::{Field, ModelSerializer, RelatedModel};
# #[derive(Debug, Clone, Model)]
# #[model(table = "authors")]
# struct Author { #[field(primary_key)] id: i64, name: String }
# #[derive(Debug, Clone, Model)]
# #[model(table = "posts")]
# struct Post { #[field(primary_key)] id: i64, #[field(foreign_key = Author)] author_id: i64, title: String }
static AUTHOR_RELATION: RelatedModel<Author> = RelatedModel::new(author_serializer);

static POST_FIELDS: &[Field] = &[
    Field::new("id").read_only(),
    Field::new("author_id"),
    Field::related("author", "author_id", &AUTHOR_RELATION),
    Field::new("title"),
];

fn author_serializer() -> ModelSerializer<Author> {
    ModelSerializer::new(&[
        Field::new("id").read_only(),
        Field::new("name"),
    ])
}

fn post_serializer() -> ModelSerializer<Post> {
    ModelSerializer::new(POST_FIELDS)
}
```

## Timestamp Fields

Use `NaiveDateTime` fields for ORM-managed create/update timestamps:

```rust
use che_orm::{Model, NaiveDateTime};

#[derive(Debug, Clone, Model)]
#[model(table = "tasks")]
struct Task {
    #[field(primary_key)]
    id: i64,

    title: String,

    #[field(auto_now_add)]
    created_at: NaiveDateTime,

    #[field(auto_now)]
    updated_at: NaiveDateTime,
}
```

`auto_now_add` is set on insert. `auto_now` is set on insert and updated on each
`update`, `update_fields(...).execute()`, and `save()`. Both fields are read-only
for create/update builders and are stored in SQLite as `TEXT DEFAULT CURRENT_TIMESTAMP`.

## JSON Fields

Use `serde_json::Value` for JSON data. Values are stored in SQLite as `TEXT` and
decoded back to JSON when rows are loaded.

```rust
use che_orm::Model;
use serde_json::Value;

#[derive(Debug, Clone, Model)]
#[model(table = "tasks")]
struct Task {
    #[field(primary_key)]
    id: i64,

    title: String,
    metadata: Value,
    optional_metadata: Option<Value>,
}
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

Schema diffs detect changes to field properties such as type, nullability,
defaults, uniqueness, timestamps, foreign keys, choices, and `max_length`.
SQLite table rebuilds are generated when a column must be altered or removed,
and values in shared columns are preserved.

`makemigrations` rejects a new required column without a default and a change
from nullable to required without a default. Add a default or write a manual
data migration before applying such a schema change.

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
- `#[field(auto_now_add)]`
- `#[field(auto_now)]`
- `#[field(unique)]`
- `#[field(max_length = 255)]`
- `#[field(default = true)]`
- `#[field(rename = "db_column")]`
- `#[field(foreign_key = OtherModel)]`

## Current Limitations

- SQLite only.
- Query expressions support Django-style `filter`, `Q` composition, `IN`, NULL checks, ordering, pagination, and numeric aggregates.
- Relations are minimal and currently use explicit FK ids.
- Migration diff supports field-property changes and SQLite table rebuilds, but not rename detection, custom indexes, or rollback migrations.

## Examples

Runnable examples are in `crates/che-orm-examples`.

```bash
cargo run -p che-orm-examples --bin crud
cargo run -p che-orm-examples --bin relations
cargo run -p che-orm-examples --bin schema_snapshot
```
