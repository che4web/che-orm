# Changelog

## 0.1.0 - 2026-08-13

Первый публичный релиз `che-orm`.

### Поддерживается

- SQLite и PostgreSQL, выбираемые Cargo feature-флагом во время компиляции.
- `#[derive(Model)]`, `#[derive(Choice)]`, CRUD, типизированные filters, `Q`,
  ordering, pagination, `count` и `distinct` для обоих backend-ов.
- SQLite relations, signals, typed projections, grouping и numeric aggregates.
- Schema snapshots, ручные SQLx migrations и native SQLite migration diff.
- Atlas migration authoring доступен как experimental integration; перед
  production use проверяйте generated SQL и Atlas environment отдельно.
- Проверенные `FilePath` и capability-based `LocalFileStorage`.

### Ограничения

- Нет automatic rename detection, rollback migrations и type conversion.
- SQLite table rebuild не сохраняет unmanaged triggers, views и custom indexes.
- Migrations и schema snapshots являются trusted, version-controlled inputs;
  создавайте и применяйте миграции одним процессом.
- PostgreSQL не поддерживает relation loading API, signals, projections, grouping,
  annotations, numeric aggregates и native schema diff.
- Foreign-key metadata и DDL поддерживаются обоими backend-ами.
