# che-orm Documentation

`che-orm` is an experimental typed ORM for Rust. SQLite and PostgreSQL each
provide a runtime; exactly one backend is selected at compile time. Both expose
the same async high-level API.

## Guides

- [Backends and PostgreSQL](backends.md): feature selection, transactions, migrations and the 0.3 upgrade.
- [Tutorial](tutorial.md): create related models and use SQLite (the Atlas section is historical).
- [Models and schema](models.md): model derives, fields, relations, and serializers.
- [ImageField](image-field.md): a complete model-to-upload example, storage keys, URLs and cleanup.
- [SQLite runtime](sqlite.md): connections, CRUD, queries, relations, and transactions.
- [Historical Atlas migration guide](atlas.md): retained for reference; the example now uses compiled migrations.
- [Compile-tested API examples](api-examples.md): models, relations, serializers, and queries.
- [Offline-first synchronization](../sync.md): shared models, client/server roles,
  independent pull/flush, snapshots and REST delivery (English).

## Project resources

- [English README](../../README.en.md)
- [Repository README in Russian](../../README.md)
- [Russian documentation](../models.md)
- [Runnable examples](../../che-orm-examples/)
- [Contributing](../../CONTRIBUTING.md)
- [Security policy](../../SECURITY.md)
- [Release procedure](../releasing.md) and [release checklist](../release-checklist.md)
