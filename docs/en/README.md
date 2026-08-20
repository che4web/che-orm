# che-orm Documentation

`che-orm` is an experimental typed ORM for Rust. The current runtime supports
SQLite through `deadpool-sqlite`; the PostgreSQL SQL compiler is available, but
a PostgreSQL connection pool and executor are not yet implemented.

## Guides

- [Tutorial](tutorial.md): create related models, use SQLite, and move to Atlas migrations.
- [Models and schema](models.md): model derives, fields, relations, and serializers.
- [SQLite runtime](sqlite.md): connections, CRUD, queries, relations, and transactions.
- [Atlas migrations](atlas.md): desired schema generation and the `manage` CLI.
- [Compile-tested API examples](api-examples.md): models, relations, serializers, and queries.

## Project resources

- [English README](../../README.en.md)
- [Repository README in Russian](../../README.md)
- [Russian documentation](../models.md)
- [Runnable examples](../../che-orm-examples/)
- [Contributing](../../CONTRIBUTING.md)
- [Security policy](../../SECURITY.md)
