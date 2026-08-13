# che-orm Roadmap

## 0.1 Support Contract

- SQLite and PostgreSQL CRUD, model derive macros, typed `Q` predicates,
  pagination, ordering, `count`, and `distinct`.
- SQLite schema snapshots, field-property diffs, table rebuilds, relations,
  signals, projections, grouping, and numeric aggregates.
- SQLx migration application for both backends; native migration generation for
  SQLite and Atlas authoring for both.

Known 0.1 limitations:

- No automatic table or column rename detection, rollback migrations, or type
  conversions.
- SQLite table rebuild preserves shared columns and modeled indexes only;
  triggers, views, and unmanaged indexes must be recreated manually.
- Migration generation and application must have one writer per database and
  migration directory.

## 1. Correctness

- Fix `update_one_returning` so it updates only one matching row. (implemented)
- Select the target primary key with `LIMIT 1`, like `claim_next_returning`. (implemented)
- Add tests for multiple matches, no matches, ordering, and concurrent claims. (implemented)

## 2. Migration Runner

- Store a checksum for every applied migration. (implemented)
- Reject modified historical migration files. (implemented)
- Replace naive `split(';')` SQL execution. (implemented)
- Add a CLI `status` command. (implemented)
- Add preflight checks for uniqueness, foreign keys, choices, and max length. (implemented)
- Reject type conversions that require an explicit manual data migration. (implemented)
- Mark destructive operations explicitly. (implemented)

## 3. Schema Evolution

- Add `IndexSchema` and model/field index attributes. (implemented)
- Generate index create/drop migrations. (implemented)
- Recreate modeled indexes after SQLite table rebuilds. (implemented)
- Add explicit `rename_from` support for tables and columns.

## 4. Relations

- Add configurable `on_delete` for immutable `id: i64` foreign keys. (implemented)
- Order migrations by foreign-key dependencies. (implemented)
- Add static typed relation descriptors and reverse lookups. (implemented)
- Add `select_related` and batched `prefetch_related`. (implemented)
- Add many-to-many relations through explicit models.

## 5. Type-Safe Queries

- Evolve `ModelField<M>` to carry the field value type. (implemented)
- Reject invalid values such as `ID.eq("text")` at compile time. (implemented)
- Return type-appropriate aggregate results. (implemented)
- Add `distinct` and typed field projections. (implemented)
- Add `group_by`, `having`, and `annotate`. (implemented)

## 6. Data Operations

- Add `bulk_create`, bulk update, and bulk delete.
- Add `upsert` and conflict targets.
- Add `get_or_create`.
- Add optimistic locking/version fields.
- Add soft delete support.
- Add an ORM-level transaction API.

## 7. Types and Backends

- Add UUID, decimal, BLOB, date-only, and timezone-aware datetime fields.
- Introduce a custom field codec trait.
- Decouple the public query/model API from SQLite.
- Add additional database backends after the abstraction is stable.

## 8. Quality and Developer Experience

- Add `trybuild` tests for derive macros.
- Add CLI end-to-end tests.
- Test migrations with indexes, foreign keys, triggers, and checksums.
- Add runnable examples for `Q`, aggregates, atomic claims, and migrations.

Recommended order: **1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8**.
