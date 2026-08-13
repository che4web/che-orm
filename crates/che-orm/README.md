# che-orm

`che-orm` is an experimental Rust ORM inspired by Django ORM.

The current MVP focuses on SQLite, model derive macros, CRUD, Django-style queries, schema metadata, migration SQL generation, and simple foreign-key relations.

## Model Definition

```rust
use che_orm::{Database, Model};

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
# use che_orm::{Database, Model};
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
let db = Database::connect("sqlite::memory:").await?;
db.create_table::<User>().await?;

let user = db
    .create::<User>()
    .set("email", "alice@example.com")
    .set("name", "Alice")
    .set("is_active", true)
    .execute()
    .await?;

let mut fetched = db.get::<User>(user.id).await?;

fetched.name = "Alicia".to_string();
fetched.is_active = false;
let updated = db.save(&fetched).await?;

let updated_without_loading = db
    .update::<User>(
        user.id,
        UserUpdate { name: Some("Alice Updated".to_string()), ..Default::default() },
    )
    .await?;

let users = db.all::<User>().await?;

db.delete::<User>(user.id).await?;
# Ok(())
# }
```

The external API does not require application code to use `sqlx` directly.

## Signals

Subscribe to model events on a backend and process them in your own task:

```rust
use che_orm::{Model, ModelEvent, SqliteBackend};

let db = SqliteBackend::connect("sqlite::memory:").await?;
let mut events = db.signals().subscribe::<User>();

tokio::spawn(async move {
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        match event {
            ModelEvent::PostSave(event) => {
                println!("saved {}: {}", event.table, event.object);
            }
            ModelEvent::PostUpdate(event) => {
                println!("updated {}: {}", event.table, event.object);
            }
        }
    }
});
```

`PostSave` is emitted after successful create and update operations; its `created` flag
distinguishes them. `PostUpdate` is emitted after each successful update. Each `subscribe::<Model>()`
receiver only receives events from its specified Rust model, including when multiple models map to
the same table.

Events are best-effort and at-most-once. Each subscriber has a per-model bounded queue of 1024
events. CRUD calls do not wait for subscribers. A subscriber that falls behind receives
`tokio::sync::broadcast::error::RecvError::Lagged` and should decide whether to resync or continue.
Dropping the final `SqliteBackend` clone closes all event receivers.

Events contain the table name and a JSON snapshot of the saved model. Raw SQL and changes made
outside this ORM do not emit signals.

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

`#[field(foreign_key = Author)]` generates schema metadata, static relation
descriptors, and SQLite `REFERENCES authors(id)`.
Create the referenced table before the table that contains the foreign key, or
generate/apply migrations in dependency order.

```rust
# use che_orm::{Database, HasMany, Model};
# #[derive(Debug, Clone, Model)]
# #[model(table = "authors")]
# struct Author { #[field(primary_key)] id: i64, name: String }
# #[derive(Debug, Clone, Model)]
# #[model(table = "posts")]
# struct Post { #[field(primary_key)] id: i64, #[field(foreign_key = Author)] author_id: i64, title: String }
# async fn example() -> che_orm::Result<()> {
# let db = Database::connect("sqlite::memory:").await?;
# db.create_table::<Author>().await?;
# db.create_table::<Post>().await?;
let author = db
    .create::<Author>()
    .set("name", "Alice")
    .execute()
    .await?;

let post = db
    .create::<Post>()
    .set("author_id", author.id)
    .set("title", "Hello")
    .execute()
    .await?;

let loaded_author = PostRelations::AUTHOR
    .get(db.as_sqlite(), post.author_id)
    .await?;
assert_eq!(loaded_author.unwrap().name, "Alice");

let author_posts = PostRelations::AUTHOR.reverse()
    .query(db.as_sqlite(), author.id)
    .all()
    .await?;
assert_eq!(author_posts.len(), 1);
# Ok(())
# }
```

For eager loading, use the generated descriptors. `select_related` loads a
forward relation in a second batched query, while `prefetch_related` loads a
reverse relation without an N+1 query:

```rust
let posts: Vec<(Post, Option<Author>)> = db
    .query::<Post>()
    .select_related(PostRelations::AUTHOR)
    .all()
    .await?;

let authors: che_orm::Prefetched<Author, Post> = db
    .query::<Author>()
    .prefetch_related(PostRelations::AUTHOR.reverse())
    .all()
    .await?;
```

## Queries

Generated fields are `ModelField<M, T>`. Direct `ModelField::new` construction
is unsafe; use generated field constants.

Use generated `ModelFields` constants with the Django-style query builder:

```rust
let users = db
    .query::<User>()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(UserFields::NAME.contains("Ali"))
    .order_by_desc(UserFields::NAME)
    .order_by(UserFields::ID)
    .limit(20)
    .all()
    .await?;
```

Repeated `filter` calls are combined with `AND`. Use `Q` for grouped boolean
expressions:

```rust
use che_orm::Q;

let user = db
    .query::<User>()
    .filter(
        UserFields::NAME.contains("Ali")
            .or(UserFields::ID.in_values([1_i64, 2, 3]))
            .and(UserFields::EMAIL.is_not_null()),
    )
    .first()
    .await?;
```

Typed projections are currently SQLite-only and can be combined with `distinct`:

```rust
let names = db
    .query::<User>()
    .values([UserFields::NAME])?
    .distinct()
    .all()
    .await?;
```

For statically typed tuple results, use `select`:

```rust
let rows: Vec<(i64, String)> = db
    .query::<User>()
    .select((UserFields::ID, UserFields::NAME))?
    .all()
    .await?;
```

Nullable fields can be projected with `UserFields::OPTIONAL_FIELD.optional()`;
Choice and `FilePath` fields are decoded into their Rust types as well.

Grouped queries support typed `having` predicates and aggregate annotations:

```rust
let total = AnnotationField::<i64>::new("total");
let repeated = AnnotationField::<i64>::new("repeated");
let grouped: Vec<(bool, i64, i64)> = db
    .query::<User>()
    .values([UserFields::IS_ACTIVE])?
    .group_by()
    .annotate_count_field(&total, UserFields::ID)?
    .annotate_count_field(&repeated, UserFields::ID)?
    .having(UserFields::IS_ACTIVE.eq(true))
    .having_annotation_field(total.clone().gte(2_i64))
    .all_typed((UserFields::IS_ACTIVE, total, repeated))
    .await?;
```

Supported predicates include `eq`, `contains`, `gt`, `gte`, `lt`, `lte`,
`in_values`, `is_null`, and `is_not_null`. An empty `in_values` list matches no
rows. `first()` returns `Option<User>` and preserves ordering and offset.

The query builder also supports `count()` and numeric aggregates:

```rust
let active_count = db
    .query::<User>()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .count()
    .await?;
let highest_id = db
    .query::<User>()
    .max(UserFields::ID)
    .await?;
```

`avg` returns `Option<f64>`. `sum`, `min`, and `max` return `Option<T>` for
the field's typed value `T`; an empty result returns `None`.

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
Schema snapshots are tied to the current ORM schema format and are not backward-compatible.

Schema diffs detect changes to field properties such as type, nullability,
defaults, uniqueness, timestamps, foreign keys, choices, and `max_length`.
SQLite table rebuilds are generated when a column must be altered or removed,
and values in shared columns are preserved.
When a rebuilt table has inbound foreign keys, the SQLite runner uses a
dedicated connection with checks temporarily disabled, verifies the result with
`PRAGMA foreign_key_check`, and restores foreign-key enforcement before return.

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

The directory is executed by SQLx's migration runner. Applied migrations and
their checksums are stored in `_sqlx_migrations`.

When independent applications share a database and each has its own migration
directory, apply each directory with a stable namespace. Local migration
versions may then each begin at `0001`:

```rust
# use che_orm::Database;
# async fn example() -> che_orm::Result<()> {
# let db = Database::connect("sqlite::memory:").await?;
db.apply_migrations_dir_with_namespace("auth", "src/apps/auth/migrations".as_ref())
    .await?;
db.apply_migrations_dir_with_namespace("tasks", "src/apps/tasks/migrations".as_ref())
    .await?;
# Ok(())
# }
```

Namespaces must remain stable after deployment. The SQLite backend records the
namespace hash and rejects collisions; use `migration_status_with_namespace`
with the same namespace and directory to inspect one application's history.

## Runtime Database Facade

Applications can run schema generation and migration application without the
CLI by implementing `Application` and defining a small manager binary:

```rust
let db = Database::connect("sqlite://app.sqlite?mode=rwc")
    .await?
    .with_migrations_dir("migrations");
let schema = Schema::from_models(vec![ModelSchema::from_model::<User>()]);

db.makemigrations(&schema, MigrationOptions::new("migrations").named("auto"))?;
db.migrate().await?;
```

`Application::schema` is the explicit model registry. Settings can be loaded
from TOML:

```toml
[database]
url = "sqlite://app.sqlite?mode=rwc" # selected by the sqlite Cargo feature

[migrations]
dir = "migrations"
```

The generic `Manager<App>` provides `migrate`, `status`, and `connect`.
`makemigrations` is available only when compiling with `migration-native` or
`migration-atlas`. The application owns command parsing, so it can add commands
such as `seed` or `create-admin` and use `manager.connect()` for them.

`Database` is an enum over SQLite and PostgreSQL. Backend-specific work can use
`database.as_sqlite()` or `database.as_postgres()` while the common migration
methods remain available directly on `Database`.

Atlas is selected with the `migration-atlas` Cargo feature. Its executable and
development database are read from `CHE_ORM_ATLAS_BIN` and `CHE_ORM_ATLAS_DEV_URL`:

```rust
db.makemigrations(&schema, MigrationOptions::new("migrations").named("add_posts"))?;
```

The defaults are `atlas` and `sqlite://file?mode=memory`.

## PostgreSQL Backend

PostgreSQL uses SQLx for applying manually authored migrations. Native schema
diff generation is intentionally rejected for PostgreSQL. Configure it with:

```toml
[database]
url = "postgres://app:secret@localhost:5432/app"

[migrations]
dir = "migrations-postgres"
```

Use the `migration-atlas` feature and set `CHE_ORM_ATLAS_DEV_URL` to a PostgreSQL Atlas
development URL when generating migrations with Atlas. Keep SQLite and
PostgreSQL migration directories separate.

Select PostgreSQL when compiling the application:

```toml
che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
```

The default feature is `sqlite`; the two backend features are mutually
exclusive and a binary cannot switch between them at runtime. Each backend
feature enables only its matching SQLx driver. `DatabaseValue`, `FilePath`, and
the `Model` derive are backend-neutral.

The `Database` facade provides `create`, `get`, `all`, `update`, `save`,
`delete`, and typed `query` operations for both backends. JSON model fields use
`JSONB` in PostgreSQL. Relations, projections, annotations, grouped queries,
and numeric aggregates remain SQLite-only.

The runnable `manager` example can be started with:

```bash
cargo run -p che-orm-examples --bin manager
```

Run migration application from exactly one process at a time for a database.

## Supported Field Attributes

- `#[field(primary_key)]`
- `#[field(auto)]`
- `#[field(auto_now_add)]`
- `#[field(auto_now)]`
- `#[field(unique)]`
- `#[field(index)]`
- `#[field(max_length = 255)]`
- `#[field(default = true)]`
- `#[field(rename = "db_column")]`
- `#[field(foreign_key = OtherModel)]`
- `#[field(foreign_key = OtherModel, on_delete = Cascade)]`

Foreign keys support `NoAction`, `Restrict`, `Cascade`, `SetNull`, and
`SetDefault`. All foreign keys target the immutable `id: i64` primary key.

## Current Limitations

- SQLite only.
- Query expressions support Django-style `filter`, `Q` composition, `IN`, NULL checks, ordering, pagination, and numeric aggregates.
- Relations are minimal and currently use explicit FK ids.
- Migration diff supports field-property changes, modeled indexes, and SQLite table rebuilds, but not rename detection or rollback migrations.
- Schema snapshots are not backward-compatible between ORM schema format changes.
- Run migrations from one process at a time per database.

## Examples

Runnable examples are in `crates/che-orm-examples`.

```bash
cargo run -p che-orm-examples --bin crud
cargo run -p che-orm-examples --bin relations
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-examples --bin manager -- --help
```
