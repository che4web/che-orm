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

Then create and apply migrations:

```bash
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url sqlite://example.sqlite
```
