# che-orm Documentation

`che-orm` is an experimental typed ORM for Rust. SQLite and PostgreSQL each
provide a runtime; exactly one backend is selected at compile time. Both expose
the same async high-level API.

## Guides

- [Backends and PostgreSQL](backends.md): feature selection, transactions, migrations and the 0.3 upgrade.
- [Tutorial](tutorial.md): create related models, use SQLite, and adopt compiled migrations.
- [Models and schema](models.md): model derives, fields, relations, and serializers.
- [ImageField](image-field.md): a complete model-to-upload example, storage keys, URLs and cleanup.
- [SQLite runtime](sqlite.md): connections, CRUD, queries, relations, and transactions.
- [Compiled migrations](atlas.md): application-owned migration registries and the example CLI.
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
