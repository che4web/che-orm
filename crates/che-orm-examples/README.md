# che-orm examples

Run examples from the workspace root.

CRUD:

```bash
cargo run -p che-orm-examples --bin crud
```

Relations:

```bash
cargo run -p che-orm-examples --bin relations
```

Generate a schema snapshot used by the CLI migration generator:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
```

Run the runtime database manager example:

```bash
cargo run -p che-orm-examples --bin manager
```

The manager owns the application model registry and adds a custom `ping`
command. Backend and migration authoring are selected at compile time: the
default is SQLite with native migration generation; use `migration-atlas` for
Atlas generation. PostgreSQL uses manual SQLx migrations unless Atlas is
enabled.

Then create and apply migrations:

```bash
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url sqlite://example.sqlite
```

Run the PostgreSQL migration CLI with:

```bash
cargo run -p che-orm-cli --no-default-features --features postgres -- migrate --config app.toml
```
