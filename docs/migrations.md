# Compiled migrations

Миграции принадлежат приложению и компилируются вместе с ним. ORM применяет
`MigrationOperation` и записывает идентификаторы и контрольные суммы в историю
базы. Изменение модели само по себе не обновляет существующую схему.

## Генерация в своём приложении

Самый простой путь — [шаблон приложения](template.md). Он включает исходники
CLI `manage` и пустой реестр в `src/migrations/`. Установка зависимости
`che-orm` сама по себе не добавляет CLI в существующий проект.

Зарегистрируйте модели в `AppConfig::schema()`, а модули приложения — в
`AppRegistry`, в порядке зависимостей: родители раньше моделей с внешними
ключами. Затем из каталога сгенерированного приложения создайте первую миграцию:

```bash
cargo run --bin manage -- makemigrations --name initial
```

Генератор сравнивает зарегистрированные модели с состоянием, восстановленным
из предыдущих миграций, а не с живой БД. Он записывает пронумерованный Rust-модуль
в `src/migrations/` и обновляет `src/migrations/mod.rs`.
Для генерации не нужна БД, но нужен `rustfmt`.

Проверьте операции перед применением, особенно удаления, переименования
и новые обязательные поля в непустых таблицах. Перенос данных и явные
переименования могут потребовать ручной правки. Добавьте файлы миграций в Git.

```bash
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
```

После следующих изменений моделей создавайте новые миграции:

```bash
cargo run --bin manage -- makemigrations --name update_models
cargo run --bin manage -- migrate
```

Не меняйте уже применённые миграции: их идентификаторы, содержимое и контрольные
суммы должны сохраняться. Применение — отдельный шаг деплоя, не часть старта
приложения. Генерация тоже всегда выполняется явно.

## Просмотр изменений и CI

```bash
cargo run --bin manage -- makemigrations --dry-run
cargo run --bin manage -- makemigrations --check
```

`--dry-run` печатает предложенный Rust-код без записи файлов. `--check`
завершается с ненулевым кодом, если модели требуют новой миграции.
Обе команды работают без подключения к БД. Запускайте через `cargo run`,
чтобы использовать актуальные модели и реестр: устаревший бинарник отклоняет
генерацию при изменении исходников миграций.

Для PostgreSQL заранее создайте базу и задайте `CHE_ORM_DATABASE_URL`.
В шаблоне backend выбран при генерации; при его переключении используйте
`--no-default-features --features postgres` на каждой Cargo-команде.
Структурированные операции компилируются для выбранного backend, но произвольный
SQL и преобразования данных требуют проверки под конкретную БД.

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

## Собственный реестр и API

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
