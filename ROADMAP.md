# che-orm Roadmap

Current baseline:

- SQLite CRUD and model derive macros.
- Schema snapshots, field-property diffs, SQLite table rebuilds, and migration safety checks.
- Django-style `Q` predicates, `IN`, NULL checks, pagination, multi-column ordering, and numeric aggregates.

## 1. Correctness

- Fix `update_one_returning` so it updates only one matching row.
- Select the target primary key with `LIMIT 1`, like `claim_next_returning`.
- Add tests for multiple matches, no matches, ordering, and concurrent claims.

## 2. Migration Runner

- Store a checksum for every applied migration.
- Reject modified historical migration files.
- Replace naive `split(';')` SQL execution.
- Add a CLI `status` command.
- Add preflight checks for uniqueness, foreign keys, choices, max length, and type conversions.
- Mark destructive operations explicitly.

## 3. Schema Evolution

- Add `IndexSchema` and model/field index attributes.
- Generate index create/drop migrations.
- Recreate modeled indexes after SQLite table rebuilds.
- Add explicit `rename_from` support for tables and columns.

## 4. Relations

- Add configurable `on_delete`, `on_update`, and foreign-key target columns.
- Order migrations by foreign-key dependencies.
- Add typed reverse lookups.
- Add `select_related` and batched `prefetch_related`.
- Add many-to-many relations through explicit models.

## 5. Type-Safe Queries

- Evolve `ModelField<M>` to carry the field value type.
- Reject invalid values such as `ID.eq("text")` at compile time.
- Return type-appropriate aggregate results.
- Add `distinct`, projections, `group_by`, `having`, and `annotate`.

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
