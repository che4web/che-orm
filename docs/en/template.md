# che-orm Application Template

The template in `templates/che-orm` creates a standalone Rust application using
[`cargo-generate`](https://cargo-generate.github.io/cargo-generate/). Requires
Rust 1.88 or later. The application requires `che-orm 0.3.1` or later from crates.io.

## Generate a Project

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
```

Choose `sqlite` (default) or `postgres` at the prompt. To generate non-interactively:

```bash
cargo generate --git https://github.com/che4web/che-orm templates/che-orm \
  --name my-app --define backend=sqlite --silent
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
cargo run --bin manage -- migrate
cargo run
```

The backend is selected at compile time. To switch, disable default features and
enable exactly one backend on every command:

```bash
cargo run --no-default-features --features postgres --bin manage -- schema
```

## Contents

- `src/models.rs`: `Task` with an integer primary key and managed timestamps.
- `src/lib.rs`: `AppConfig` and `AppRegistry`.
- `src/settings.rs`: database settings.
- `src/migrations/`: frozen `tasks:0001_initial` migration and migration registry.
- `src/bin/manage.rs`: `schema`, `migrate`, `migrate status`.
- `src/main.rs`: async CRUD demo.
- `tests/runtime.rs`: schema-history validation and CRUD on migrated SQLite.

Startup does not apply migrations. Add a new compiled migration and register it
in `src/migrations/mod.rs` when changing models. Preserve applied migration
identities and checksums. The template uses structural operations compiled for
the selected backend; it never recalculates the initial migration from
`Task::schema()`. See [compiled migrations](atlas.md).

The demo saves materialized objects with `Database::save(&model)` and
`Transaction::save(&model)`, available since 0.3.1.

## Verify the Template

In the generated application:

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
them against local core and macros, applies SQLite migrations and runs the CRUD demo.
After publishing, verify the crates.io dependency with
`python3 scripts/check-template.py --published`.
Use `--backend sqlite` or `--backend postgres` to check one variant.
