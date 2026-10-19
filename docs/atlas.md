# Compiled migrations

Миграции принадлежат приложению и компилируются вместе с ним. ORM применяет
`MigrationOperation` и записывает идентичности и checksums в историю базы.
Отдельный Atlas CLI для этого не требуется. Этот адрес руководства сохранён
для совместимости с существующими ссылками.

## CLI приложения-примера

Из корня репозитория:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

`schema` печатает схему зарегистрированных моделей. `migrate` применяет
миграции, а `migrate status` показывает, какие из них записаны в историю.

`database_url()` в `che-orm-examples/src/lib.rs` читает `CHE_ORM_DATABASE_URL`.
По умолчанию SQLite использует `app.db`, PostgreSQL - `postgresql://localhost/che_orm`.
Для PostgreSQL добавьте `--no-default-features --features postgres` перед `--bin`.

Пример содержит отдельные замороженные SQL baseline для обоих backend и
подключает выбранный через `include_str!` в `src/bin/manage.rs`. Идентичность
миграции - `examples:0001_initial`. Пример не поддерживает генерацию новых
миграций: команды `makemigrations` и `migrate lint` недоступны.

## Миграции своего приложения

Храните модели, `AppRegistry`, настройки и реестр миграций вне core-крейта.
Создавайте миграции через `Migration::new`, указывая стабильные `MigrationId`,
зависимости и последовательность `MigrationOperation`.

```rust
let graph = che_orm::MigrationGraph::new(application_migrations())?;
let applied = database.migrate(graph).await?;
```

`application_migrations()` - функция вашего приложения, возвращающая
`Vec<Migration>`. Для расчёта изменений используйте `ProjectState::diff` и
`operations_from_changes`, а для генерации Rust-модуля - `render_migration_rust`.
Проверяйте полученные операции и SQL до применения. `RunSql` должен содержать
SQL выбранного backend и соответствующие `state_operations`, если меняет схему.

Не изменяйте идентичности, содержимое и checksums уже применённых миграций.
Добавляйте новые операции в новые миграции; backfill существующих данных
планируйте явно. Например, обязательному `DbEnum` в непустой таблице нужно
заполнить допустимое значение, сохранив его `CHECK` constraint.

Применение миграций - отдельный шаг deploy/CI. `Database::create_table` оставьте
для тестов и локальных экспериментов. Особенности блокировок, транзакций и SQL
обоих backend описаны в [руководстве backend](backends.md#миграции).
