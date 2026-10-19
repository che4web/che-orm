# Changelog

## 0.3.0

- Add a PostgreSQL runtime with TLS, pooled connections, typed CRUD, relations,
  transactions, and compiled migration execution.
- Select exactly one backend at compile time with `sqlite` (default) or
  `postgres`. Both expose the same `Database`, builders, and `Transaction` API.
- Make database connection methods async. Transaction callbacks return boxed
  async futures and all transaction operations are awaited.
- Decode models through backend-neutral `Row` / `FromDatabaseValue`, replacing
  the SQLite row contract. `Model::from_sqlite_row` and `raw_transaction` are
  SQLite-specific adapters for direct driver operations.
- Add native PostgreSQL schema operations, stable constraint names, migration
  locking, checksum validation, and rollback on failure.
- Preserve existing SQLite migration identities and checksums.
- Deprecate `Database::typed_transaction` in favor of `Database::transaction`.
- Remove the unused `QueryBuildError::EmptyInsert` variant; default-only inserts
  remain supported.
- Replace retired Atlas CLI instructions with the compiled migration workflow
  and remove the unused example Atlas checksum file.
- Require `che-orm-macros` 0.3.0 and Rust 1.88 (the current `time` dependency's
  minimum supported version).

## 0.2.3

- Add UUIDv7 primary keys, 16-byte SQLite BLOB storage, UUID PostgreSQL DDL,
  typed CRUD, relations, serializers, and migration metadata.
- Add `Model::PrimaryKey` and `WriteMode<Key>`; hand-written model and write
  serializer implementations must use the model's key type.
- Require `che-orm-macros` 0.2.3 for the updated derive implementations.
- Support generic `select_related` execution with required and nullable keys.
- Add nullable `ImageField` storage keys with SQLite round-trip support and read-only JSON writes.
- Document image-field models, uploads, storage paths, and cleanup in Russian and English.

## 0.2.1

- Add typed transaction execution and support inserts with only database defaults.
- Encode model timestamps as RFC 3339 values in serializers.
- Add binary column round-trip support.

## 0.2.0

- Replace the legacy implementation with the typed SQLite ORM rewrite.
- Publish application binaries, sample models, and migrations only through
  `che-orm-examples`.
- Add English documentation, contribution and security policies, and a release
  checklist.

## 0.1.0

- Legacy release.
