# che-orm

Offline-first synchronization: [per-model protocol 2](docs/model-sync.md) and
[scoped compatibility protocol](docs/sync.md).

**English** | [Russian](README.md)

`che-orm` is an experimental typed ORM for Rust. Choose SQLite or PostgreSQL
at compile time; both use the same models and high-level async API. Requires
Rust 1.88 or later.

## Status

Both backends provide an async pool, CRUD facade, transactions, relations,
serializers, and compiled migrations. SQLite uses `deadpool-sqlite`; PostgreSQL
uses `tokio-postgres` / `deadpool-postgres` with native TLS.

Model fields support `f64` and `Option<f64>` as SQLite `REAL` and PostgreSQL
`DOUBLE PRECISION`, including typed numeric filters and parameterized writes.

`che-sync` shares `#[orm(sync)]` UUID models between client and server, selecting
the role through `SchemaSet::client_model` / `server_model`. Configured database
handles capture ordinary ORM writes. Clients have current/shadow tables; servers
have authoritative rows and journals without shadows. Prefer `ModelLogClient`
with independent model journals/cursors and whole-model authorization for new
clients. Its `pull`/`flush` API has no snapshot/full-load phase. The scoped
`SyncClient` / `SyncEngine` API retains its separate snapshot workflow.

Both roles support either backend. The ORM and sync crates own data and
transport-independent delivery; HTTP belongs to `che-rest` and Tauri invoke to
`che-tauri`. See the [protocol-2 guide](docs/model-sync.md) and the complete
[Todo server and desktop client](../che-tauri/examples/todo-sync/README.md).

## Quick Start

Generate an application with SQLite/PostgreSQL selection, a model and compiled
migrations using `cargo-generate`:

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
cd my-app
cargo run --bin manage -- migrate
cargo run
```

For PostgreSQL, create the database and set `CHE_ORM_DATABASE_URL` before migrating.
See the [template guide](docs/en/template.md) for local generation and project layout.

```toml
[dependencies]
che-orm = "0.3.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
time = { version = "0.3", features = ["formatting", "parsing"] }
```

```rust
use che_orm::{Database, Model};
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "users")]
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

#[tokio::main]
async fn main() -> Result<(), che_orm::OrmError> {
    let database = Database::connect("app.db").await?;
    database.create_table::<User>().await?;

    let user = database
        .create::<User>()
        .set(User::EMAIL, "alice@example.test")
        .set(User::NAME, "Alice")
        .execute()
        .await?;
    let loaded = database.get::<User>(user.id).await?;
    println!("{loaded:?}");
    Ok(())
}
```

`create_table` is suitable for tests and local experiments. Use compiled
migrations for persistent application schemas.

## Backend selection

Enable exactly one backend. SQLite is the default. For PostgreSQL:

```toml
che-orm = { version = "0.3.1", default-features = false, features = ["postgres"] }
```

The same `Database::connect(settings.database_url()).await?` call accepts a
SQLite path or a PostgreSQL URL for the selected backend. Models, CRUD, relation
queries and serializers are unchanged. Neither backend is selected at runtime.

Transactions also use one async API:

```rust
database.transaction(|tx| Box::pin(async move {
    tx.update::<User>(user.id).set(User::NAME, "Updated").execute().await?;
    Ok(())
})).await?;
```

Save all ordinary fields of an existing materialized model with `save`:

```rust
let mut user = database.get::<User>(user_id).await?.unwrap();
user.name = "Alice Cooper".into();
let saved = database.save(&user).await?; // Option<User>
```

`save` updates by primary key, returning `None` if the row is missing. It preserves
`auto_now_add`, refreshes `auto_now`, and returns the `RETURNING` model without
modifying the supplied object. Use `create` or `insert` for new rows.
Transactions also support `tx.save(&user).await?`.

See [the backend and PostgreSQL guide](docs/en/backends.md) for configuration,
migrations, testing and upgrading from 0.2.

## UUIDv7 primary keys

Use `Uuid` instead of `i64` for a UUID primary key:

```rust
use che_orm::{Model, Uuid};

#[derive(Debug, Model)]
#[orm(table = "uuid_users")]
struct User {
    #[orm(primary_key)]
    id: Uuid,
    name: String,
}
```

`database.create::<User>().set(User::NAME, "Alice").execute().await?`
generates UUIDv7 in Rust when the key is omitted. `User::insert()` does the
same when converted into an AST. To supply a key explicitly, use
`.set(User::ID, uuid)`; `database.insert(&model)` preserves `model.id`.
Explicit UUIDs are preserved regardless of version.

SQLite stores UUIDs as 16-byte `BLOB`s, PostgreSQL DDL uses `UUID`, and JSON
serializers expose UUID strings. `get`, `update`, `delete`, typed transactions,
filters and related queries accept the model's key type. Foreign keys use
`Uuid` or `Option<Uuid>` when the target model has a UUID primary key; use
`on_delete = "set null"` only with optional keys.

`Model` has an associated `PrimaryKey` type, generated by the derive macro.
Hand-written implementations must set `type PrimaryKey = i64` or `Uuid`.
`WriteMode` defaults to integer keys; explicitly typed UUID write modes use
`WriteMode<Uuid>`. Calls such as `WriteMode::Patch { id: user.id }` infer the
key type from the serializer.

Add UUID models through application migrations. Changing an existing integer
key to UUID requires an explicit data migration and updating its foreign keys;
changing the Rust field type does not convert stored keys automatically.

## ImageField

Use `Option<ImageField>` when a record can have an image:

```rust
use che_orm::{ImageField, Model};

#[derive(Debug, Model)]
#[orm(table = "boards")]
struct Board {
    #[orm(primary_key)]
    id: i64,
    preview: Option<ImageField>,
}
```

The nullable SQLite `TEXT` column stores a relative key such as
`board-previews/a1b2.png`, **not** image bytes, an absolute filesystem path, or
the browser URL. `che-orm` reads and writes this key; `che-rest::media` handles
uploads, validation, storage and serving files. Image fields are read-only in
JSON writes. `ImageField::new(...)` only wraps a trusted string: it does **not**
validate or create a file.

See the [step-by-step ImageField guide](docs/en/image-field.md) for the complete
model, ViewSet, JSON response, URL, deletion and migration workflow.

The repository migration CLI is an example application command:

```bash
cargo run -p che-orm-examples --bin manage -- schema
```

## Documentation

- [English guides](docs/en/README.md)
- [API examples](docs/en/api-examples.md)
- [Russian documentation](README.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Release checklist](docs/release-checklist.md)

## Verification

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo doc --workspace --no-deps
```
