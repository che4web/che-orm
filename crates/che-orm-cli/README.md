# che-orm CLI

`che-orm-cli` предоставляет migration commands для `che-orm`. Имя бинарника:
`che-orm`.

## Команды

- `makemigrations`: сравнивает schema snapshot с предыдущим состоянием и
  создаёт SQL migration. Нужен feature `migration-native` или `migration-atlas`.
- `migrate`: применяет новые migration files через SQLx.
- `status`: показывает состояние migration files в SQLx.

## SQLite: от моделей до миграции

CLI не читает Rust models. Сначала приложение должно сериализовать реестр
моделей в schema snapshot. В workspace есть готовый пример:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url 'sqlite://example.sqlite?mode=rwc'
cargo run -p che-orm-cli -- status --database-url 'sqlite://example.sqlite?mode=rwc'
```

По умолчанию используются `che_orm_schema.json`, каталог `migrations` и имя
`auto`. После `makemigrations` в `migrations/` появятся SQL-файл и `schema.json`.
Коммитьте оба файла.

## Конфигурация

По умолчанию `migrate` и `status` читают `app.toml`. Явный `--database-url`
имеет приоритет над конфигурацией.

```toml
[database]
url = "sqlite://example.sqlite?mode=rwc"

[migrations]
dir = "migrations"
```

```bash
cargo run -p che-orm-cli -- migrate --config app.toml
cargo run -p che-orm-cli -- status --config app.toml
```

## PostgreSQL и Atlas

Native generator предназначен только для SQLite. PostgreSQL применяет вручную
написанные SQLx migration files из отдельного каталога:

```bash
cargo run -p che-orm-cli --no-default-features --features postgres -- \
  migrate --config app.toml
```

Для создания миграций через Atlas включите `migration-atlas`:

```bash
CHE_ORM_ATLAS_BIN=atlas CHE_ORM_ATLAS_DEV_URL='sqlite://file?mode=memory' \
cargo run -p che-orm-cli --no-default-features --features sqlite,migration-atlas -- \
  makemigrations --schema che_orm_schema.json --name add_posts
```

Для PostgreSQL backend и Atlas используйте PostgreSQL development URL и
соберите CLI с PostgreSQL feature:

```bash
CHE_ORM_ATLAS_BIN=atlas CHE_ORM_ATLAS_DEV_URL='postgres://user:password@localhost/db' \
  cargo run -p che-orm-cli --no-default-features --features postgres,migration-atlas -- \
  makemigrations --schema che_orm_schema.json --name add_posts
```

`CHE_ORM_ATLAS_BIN` по умолчанию равен `atlas`,
`CHE_ORM_ATLAS_DEV_URL` - `sqlite://file?mode=memory`. Само применение миграций всегда выполняет
SQLx и не требует Atlas.

## Правила безопасности

- SQLx хранит применённые migration versions и checksums в `_sqlx_migrations`.
  Не редактируйте уже применённые SQL-файлы.
- Запускайте миграции для одной базы только из одного процесса.
- Просматривайте generated SQL до применения. SQLite rebuild table сохраняет
  общие столбцы, но изменения type и rename требуют ручной data migration.
- Генератор отклоняет новый обязательный столбец и nullable-to-required change
  без default.

Подробный процесс и ограничения:
[docs/migrations.md](https://github.com/che4web/che-orm/blob/v0.1.0/docs/migrations.md).
