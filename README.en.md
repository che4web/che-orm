# che-orm

[Русский](README.md) | **English**

An experimental typed ORM for Rust with a shared async API for SQLite and
PostgreSQL. The backend is selected at compile time. Requires **Rust 1.88+**.

The current published `che-orm` version is **0.3.3**. The project is under
development; check the [changelog](CHANGELOG.md) before upgrading.

## Features

- **Typed models and queries.** `#[derive(Model)]` generates metadata and
  fields for filtering and writes. Build CRUD queries with Rust expressions;
  values are passed to SQL as parameters.
- **One async API for SQLite and PostgreSQL.** Models, CRUD and transactions
  share an interface; select the backend with a feature flag at build time.
- **Application-owned migrations.** Schema changes live in compiled migrations.
  The template includes a CLI to generate and apply them; organize models
  into application modules with `AppConfig` / `AppRegistry`.
- **Explicit relation loading and serialization.** `select_related` and
  `prefetch_related` load related data before serialization.
  `ModelSerializer` works with materialized data and never queries the database.
- **Types for application data.** Integer and UUIDv7 keys, nullable fields,
  enum choices with `CHECK` constraints, JSON, timestamps and `ImageField` keys.

**Offline synchronization is a separate extension.** `che-sync` synchronizes
UUID models between client and server without coupling the ORM to HTTP or
Tauri. See [synchronization and integrations](#synchronization-and-integrations).

## A model and its use in an application

Add the dependencies to `Cargo.toml`. SQLite is enabled by default:

```toml
[dependencies]
che-orm = "0.3.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### Define a model

Declare an ordinary Rust struct in `src/main.rs`. `Model` generates table
metadata and typed fields: `User::EMAIL`, `User::NAME` and `User::ID`.

```rust
use che_orm::Model;

#[derive(Debug, Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    name: String,
}
```

### Use the model

In the same file, add the connection and data operations. This example runs
against a temporary SQLite database without additional setup.
`create_table` is only for the demonstration here; use
[compiled migrations](docs/en/migrations.md) for persistent application schemas.

```rust
use che_orm::Database;

#[tokio::main]
async fn main() -> Result<(), che_orm::OrmError> {
    let database = Database::connect_in_memory().await?;
    database.create_table::<User>().await?;

    let mut user = database
        .create::<User>()
        .set(User::EMAIL, "alice@example.test")
        .set(User::NAME, "Alice")
        .execute()
        .await?;

    let users = database
        .query::<User>()
        .filter(User::NAME.eq("Alice"))
        .all(&database)
        .await?;
    println!("{users:?}");

    user.name = "Alice Smith".into();
    database.save(&user).await?;

    database.delete::<User>(user.id).await?;
    Ok(())
}
```

`database.save(&user)` saves all ordinary fields by primary key, not just
changed fields. It returns `Option<User>`: the updated record, or `None` if
the row has been deleted. The supplied object is not modified;
use `create` to insert a new record.

Instead of changing the object and calling `save`, update only selected fields:

```rust
let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;
```

### Transactions

Operations inside the callback use `tx` and share one connection.
Success commits the changes; an error rolls back the transaction.
For example, to update an existing user:

```rust
database
    .transaction(|tx| Box::pin(async move {
        tx.update::<User>(user.id)
            .set(User::NAME, "Alice Cooper")
            .execute()
            .await?;
        Ok(())
    }))
    .await?;
```

### PostgreSQL

Models and CRUD code stay the same. Replace the dependency in `Cargo.toml`:

```toml
che-orm = { version = "0.3.3", default-features = false, features = ["postgres"] }
```

Create a PostgreSQL database and set the URL with your credentials:

```bash
export CHE_ORM_DATABASE_URL='postgresql://user:password@localhost:5432/my_app'
```

In `main`, replace `connect_in_memory` with a connection by URL:

```rust
let url = std::env::var("CHE_ORM_DATABASE_URL")
    .expect("CHE_ORM_DATABASE_URL must be set");
let database = Database::connect(&url).await?;
```

Enable exactly one feature at build time: `sqlite` or `postgres`.
`default-features = false` disables the default SQLite backend. The application
reads the URL; `connect` opens a pool but does not create the database.
See the [backend guide](docs/en/backends.md) for TLS and backend-specific details.

## Generate an application from a template

Instead of adding the dependency manually, generate a project. The template
offers SQLite or PostgreSQL, a `single` or `modular` layout, a starter model
and a migration CLI:

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
cd my-app
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run
```

For PostgreSQL, create the database and set `CHE_ORM_DATABASE_URL` before migrating.
Create an application module with `cargo run --bin manage -- createapp blog`.
See the [template guide](docs/en/template.md).

## Migrations

Changing a Rust model does not change an existing database.
Schemas evolve through **compiled migrations**: Rust modules containing
operations, stored in your project and compiled with the application.

The template includes a `manage` CLI. It compares registered models with the
state of previous migrations, **not with the live database**. After creating
the initial migration, each subsequent cycle looks like this:

```bash
cargo run --bin manage -- makemigrations --name update_models
# Review the generated operations before applying them.
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
```

Commit the files in `src/migrations/` to Git. Renames and backfilling existing
data may require manual edits to the operations. Do not edit applied migrations:
the database stores their identities and checksums. On deployment, run `migrate`
as a separate step before application startup.

To preview changes without writing files and to check them in CI:

```bash
cargo run --bin manage -- makemigrations --dry-run
cargo run --bin manage -- makemigrations --check
```

Generation needs no database connection; `--check` fails if models need a new
migration. `manage` is part of the template-generated application, not a CLI
installed by the `che-orm` dependency.
See the [migration guide](docs/en/migrations.md) for custom registries,
the migration API and backend-specific details.

## Synchronization and integrations

`che-sync` lets applications work with UUID models locally while offline,
then synchronize changes with the server. It supports retrying pending requests
and field-level merging. Both SQLite and PostgreSQL are supported.

- [Model synchronization](docs/model-sync.md): whole-model authorization
  (protocol 2).
- [Shared objects](https://github.com/che4web/che-rest/blob/main/docs/object-sync.md):
  per-object permissions (protocol 3).
- [Compatibility API](docs/sync.md): the legacy scoped protocol with snapshots.

`che-orm` owns data and transactions; `che-sync` owns synchronization.
HTTP and REST mapping belong to `che-rest`; Tauri commands belong to `che-tauri`.
See the [Todo server and desktop client](https://github.com/che4web/che-tauri/tree/main/examples/todo-sync)
for a two-device example.

## Documentation

- [Step-by-step tutorial](docs/en/tutorial.md)
- [Models, fields, relations and serializers](docs/en/models.md)
- [Connections, CRUD and transactions](docs/en/sqlite.md)
- [Application migrations](docs/en/migrations.md)
- [ImageField: models, uploads and file storage](docs/en/image-field.md)
- [Compile-tested API examples](docs/en/api-examples.md)
- [English guide index](docs/en/README.md)
- [Changelog](CHANGELOG.md), [contributing](CONTRIBUTING.md),
  [security policy](SECURITY.md)

## Examples and verification

From the repository root:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

The example application has frozen migrations and does not support
`makemigrations`. Use the template above for your own application.

```bash
cargo fmt --check
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo doc --workspace --no-deps
```

PostgreSQL runtime tests require a running server and `TEST_POSTGRES_URL`;
run them separately with `--test runtime -- --ignored`.
See [releasing](docs/releasing.md) for the full publication checks.
