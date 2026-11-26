# Modular-layout template example

Runnable task app based on `layout=modular` from [`templates/che-orm`](../../templates/che-orm).
Unlike [`template-single`](../template-single/), it keeps the model and its
`AppConfig` and migrations together in the `apps/tasks/` Cargo crate. The root
application registers that crate in `src/apps/mod.rs` and combines migration
histories through `src/migrations.rs`.

```text
template-modular/
|-- Cargo.toml                     # path dependency on the tasks crate
|-- apps/tasks/
|   |-- Cargo.toml
|   `-- src/
|       |-- lib.rs                # APP_NAME and AppConfig
|       |-- models.rs
|       `-- migrations/
|           |-- mod.rs            # tasks migration registry
|           `-- m0001_initial.rs
`-- src/
    |-- apps/mod.rs               # registered app crates
    |-- migrations.rs             # combined graph, no migration definitions
    `-- bin/manage.rs              # root migration CLI
```

## Run

From the repository root, apply the included migration and run the CRUD demo:

```bash
cargo run -p template-modular --bin template-modular-manage -- migrate
cargo run -p template-modular --bin template-modular-manage -- migrate status
cargo run -p template-modular
```

SQLite uses `template-modular.db` by default. To use PostgreSQL, set
`CHE_ORM_DATABASE_URL` to a disposable PostgreSQL database and add
`--no-default-features --features postgres` to each Cargo command.

App crates do not enable a backend by default. To test the tasks crate on its
own, select exactly one backend:

```bash
cargo test -p template-modular-app-tasks --features sqlite
cargo test -p template-modular-app-tasks --features postgres
```

## Extend the app

`apps/tasks/src/models.rs` defines `Task`. `apps/tasks/src/lib.rs` shares
`APP_NAME` between the model's `#[orm(app = super::APP_NAME)]` and `AppConfig`,
so the model maps to `tasks_task`. Its initial migration is in
`apps/tasks/src/migrations/m0001_initial.rs`.

To add a second app module:

```bash
cargo run -p template-modular --bin template-modular-manage -- createapp blog
```

The command creates `apps/blog/` and registers it in `Cargo.toml` and
`src/apps/mod.rs`. After adding a model and registering it in the new crate's
`AppConfig::schema`, generate `apps/blog/src/migrations/m0001_add_blog.rs` and
apply it through the root CLI:

```bash
cargo run -p template-modular --bin template-modular-manage -- makemigrations --name add_blog
cargo run -p template-modular --bin template-modular-manage -- migrate
```

Migrations are numbered per crate: `tasks:0001_initial` and
`blog:0001_add_blog` can coexist. If a blog model has a foreign key to `Task`,
add this under `[dependencies]` in `apps/blog/Cargo.toml`:

```toml
app_tasks = { package = "template-modular-app-tasks", path = "../tasks" }
```

Import `app_tasks::models::Task` in the blog model. Cargo resolves the model
dependency at build time; the migration graph determines the order in which
the CLI creates the tables.

Run `cargo test -p template-modular` to check the compiled history and SQLite
CRUD. See the [template guide](../../docs/en/template.md) for migration and
module-generation details.
