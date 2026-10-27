# che-orm Application Template

The template in `templates/che-orm` creates a standalone Rust application using
[`cargo-generate`](https://cargo-generate.github.io/cargo-generate/). Requires
Rust 1.88 or later. The application requires `che-orm 0.3.2` or later from crates.io.
Until 0.3.2 is published, verify the template with local patches using
`python3 scripts/check-template.py`.

## Generate a Project

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
```

The prompts ask for the project name, backend (`sqlite` or `postgres`) and layout:
`single` (default, models in `src/models.rs`) or `modular` (starter app in
`src/apps/tasks`). To generate non-interactively:

```bash
cargo generate --git https://github.com/che4web/che-orm templates/che-orm \
  --name my-app --define backend=sqlite --define layout=modular --silent
```

From a local checkout:

```bash
cargo generate --path templates/che-orm --name my-app --define backend=sqlite --silent --no-workspace
```

`--no-workspace` keeps the application separate from the library workspace.
Hyphenated project names automatically become underscored Rust crate imports.

## Run

For SQLite:

```bash
cd my-app
cargo run --bin manage -- schema
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
cargo run
```

The default database is `app.db`; override its path with `CHE_ORM_DATABASE_URL`.
The CRUD demo creates, reads, updates and deletes one task.

For PostgreSQL, generate with `backend=postgres`, create a database and configure
its URL before running:

```bash
export CHE_ORM_DATABASE_URL='postgresql://postgres:postgres@localhost/my_app'
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run
```

The backend is selected at compile time. To switch, disable default features and
enable exactly one backend on every command:

```bash
cargo run --no-default-features --features postgres --bin manage -- schema
```

## Contents

- `src/models.rs` (`single`) or `src/apps/tasks/models.rs` (`modular`): `Task`
  with an integer primary key and managed timestamps.
- `src/apps/mod.rs`: app module registration.
- `src/lib.rs`: `AppConfig` and `AppRegistry`.
- `src/settings.rs`: database settings.
- `src/migrations/`: an empty compiled migration registry. Generate the first
  migration yourself with `makemigrations --name initial`.
- `src/bin/manage.rs`: `schema`, `makemigrations`, `migrate`, `migrate status`.
- `src/bin/manage/makemigrations.rs`: application-owned compiled migration generator.
- `src/bin/manage/createapp.rs`: app module generator.
- `src/main.rs`: async CRUD demo.
- `tests/runtime.rs`: schema-history validation and CRUD on migrated SQLite.

The template has no prebuilt `0001_initial`: customize your models, then create
the initial migration using the Run instructions. Startup does not apply
migrations. After changing models, generate a follow-up:

```bash
cargo run --bin manage -- makemigrations --name update_tasks
cargo run --bin manage -- migrate
```

The command compares models with the replayed compiled history, writes the next
numbered Rust module and updates `src/migrations/mod.rs` without connecting to the
database. Generation requires `rustfmt`: `rustup component add rustfmt`.
`--dry-run` prints the proposed Rust source without writing it; `--check` exits
with an error when model changes need a migration. No changes means no new files.
Use `cargo run` so the binary is rebuilt: stale prebuilt management binaries
refuse to generate when migration sources have changed since compilation.

Review operations before applying them. Data backfills and explicit renames may
need manual edits. Preserve applied migration
identities and checksums. The template uses structural operations compiled for
the selected backend; it never recalculates the initial migration from
`Task::schema()`. See [compiled migrations](atlas.md).

The demo saves materialized objects with `Database::save(&model)` and
`Transaction::save(&model)`, available since 0.3.1.
The demo uses the returned model for refreshed managed timestamps. Tests cover
commit, rollback and saving a deleted row returning `None`.

## Create an App Module

```bash
cargo run --bin manage -- createapp blog
```

Available with either layout, this creates `src/apps/blog/mod.rs` with an
`APP_NAME`, an `AppConfig`, a `models.rs` with naming guidance, and registers the
app in `src/apps/mod.rs`.
Use snake_case names such as `user_accounts`. With `single`, the starter models
stay in `src/models.rs`; with `modular`, `Task` lives in `src/apps/tasks/models.rs`.

Define models with `#[orm(app = super::APP_NAME)]` and register them in
the new app's `AppConfig::schema()`, for example
`SchemaSet::new().model::<models::Post>()`. Then run `makemigrations` and `migrate`.
All apps share `src/migrations/`; operations preserve each model's app ownership.
Register FK parents before children: new apps are appended to the registry.
Keep the `createapp` marker comments in `src/apps/mod.rs` for future automatic
registration. Creation requires `rustfmt`, never connects to the database and
does not overwrite existing app directories. Use `cargo run` to rebuild registry
changes before creating another app.

`createapp blog` defines `pub const APP_NAME: &str = "blog"` in the app's
`mod.rs`, and `AppConfig::name()` returns this constant. `Post` with
`#[orm(app = super::APP_NAME)]` uses `blog_post` (see [Models](models.md#app)).
The starter `Task` uses `tasks_task` in both layouts; its constant lives in
`src/lib.rs` for `single` and `src/apps/tasks/mod.rs` for `modular`.
Use an explicit `#[orm(table = "existing_table")]` to preserve an existing name.

## Verify the Template

In the generated application, after generating the initial migration:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

The PostgreSQL variant validates history without connecting to a server. Also
run the in-memory CRUD test with `cargo test --no-default-features --features sqlite`.

From the ORM repository root, verify generation and both backend variants:

```bash
python3 scripts/check-template.py
```

Install `cargo-generate` first. The script creates temporary projects, checks
them against local core and macros, generates an initial migration from empty
history, applies SQLite migrations and runs the CRUD demo.
It also changes a model, exercises `--check`/`--dry-run`, generates a follow-up
migration, rejects a stale binary and verifies that existing rows survive migration.
After publishing, verify the crates.io dependency with
`python3 scripts/check-template.py --published`.
Use `--backend sqlite` or `--backend postgres` to check one variant.
Use `--layout single` or `--layout modular` to check one layout. By default all
four combinations exercise `createapp`, new model registration with an FK and
the shared compiled migration history.
