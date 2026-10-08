# Changelog

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
