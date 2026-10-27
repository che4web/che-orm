# Changelog

## Initial sync publications: 0.1.0

- Prepare the first crates.io releases of `che-sync-protocol` and `che-sync`,
  depending on the already published che-orm 0.3.2.
- Include package-local README, changelog and MIT license files, with backend
  selection, protocol compatibility and publication-order documentation.
- Cover scoped compatibility, per-model protocol 2 and explicit object protocol
  3 without conflating wire versions with Cargo package versions.

## 0.3.2

- Use shared `APP_NAME` constants and inferred app-prefixed table names in both
  template layouts and `createapp` guidance; the starter table is `tasks_task`.
- Add `#[orm(app = "...")]` table-name inference with an application prefix;
  accept paths to `const &str` prefixes shared with `AppConfig::name()`, with
  compile-time name assembly and validation. Explicit `table` remains an exact
  override.
- Add single/modular layout selection and `manage createapp` to the application
  template, with automatic schema registration and shared project migrations.
- Add `manage makemigrations` to the cargo-generate template, with compiled
  history diffs, automatic registry updates, `--check` and `--dry-run`.
- Start generated applications with an empty migration registry; users create
  the initial migration explicitly after defining their models.

## 0.3.1

- Fix SQLite timestamp filters, joins and ordering with the `che_datetime`
  collation, preserving nanosecond precision and supporting historical RFC3339
  values with different fractional precision or UTC offsets without rewriting rows.
- Fix SQLite queries and counts with an offset but no limit.
- Require `che-orm-macros` 0.3.1 for generated timestamp field metadata.
- Add a cargo-generate application template with backend selection, compiled
  migrations, a management CLI and standalone generation checks.
- Add `Database::save(&model)` and `Transaction::save(&model)` to update existing
  materialized models by primary key, preserving managed creation timestamps and
  capturing writes through configured sync runtimes.

## 0.3.0

- Add shared `#[orm(sync)]` models with explicit client/server registration,
  stable wire field names and infrastructure-owned service columns.
- Capture ordinary ORM writes through configured sync database handles and
  generate role-specific migrations; servers do not create shadow tables.
- Add independent `pull()`, `flush()` and explicit `full_load()` operations.
  Retain `sync_once()` for compatibility and preserve pending request identities
  when journal cursors expire. Server replacement/history rewind is unsupported.
- Enforce scope authorization and scope/tombstone filtering on configured server
  reads, including `execute(SELECT)` and queries inside existing transactions.
- Introduce `che-sync` and `che-sync-protocol` 0.1.0; support either database on
  both clients and servers and ordinary REST delivery through `che-rest`.
- Add release checks for package archives, Rust 1.88 and live PostgreSQL.
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
