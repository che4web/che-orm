# che-orm Documentation

`che-orm` is an experimental typed ORM for Rust. The current runtime supports
SQLite through `deadpool-sqlite`; the PostgreSQL SQL compiler is available, but
a PostgreSQL connection pool and executor are not yet implemented.

## Guides

- [Tutorial](tutorial.md): create related models and use SQLite (the Atlas section is historical).
- [Models and schema](models.md): model derives, fields, relations, and serializers.
- [ImageField](image-field.md): a complete model-to-upload example, storage keys, URLs and cleanup.
- [SQLite runtime](sqlite.md): connections, CRUD, queries, relations, and transactions.
- [Historical Atlas migration guide](atlas.md): retained for reference; the example now uses compiled migrations.
- [Compile-tested API examples](api-examples.md): models, relations, serializers, and queries.

## Project resources

- [English README](../../README.en.md)
- [Repository README in Russian](../../README.md)
- [Russian documentation](../models.md)
- [Runnable examples](../../che-orm-examples/)
- [Contributing](../../CONTRIBUTING.md)
- [Security policy](../../SECURITY.md)
