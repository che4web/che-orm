# Миграции

CLI не анализирует Rust-код. Приложение сначала формирует JSON snapshot из
зарегистрированных моделей, затем `makemigrations` сравнивает его с
`migrations/schema.json`.

Schema snapshot, каталог migrations и Rust model attributes являются trusted
build inputs. Храните их в version control и разрешайте изменять только
доверенным разработчикам и CI. Запускайте `makemigrations`, как и `migrate`,
одним процессом для одного каталога.

## SQLite: полный сценарий

Сгенерируйте текущую схему из примера или собственного бинарника:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url 'sqlite://example.sqlite?mode=rwc'
cargo run -p che-orm-cli -- status --database-url 'sqlite://example.sqlite?mode=rwc'
```

Вместо `--database-url` можно использовать `app.toml`:

```toml
[database]
url = "sqlite://example.sqlite?mode=rwc"

[migrations]
dir = "migrations"
```

```bash
cargo run -p che-orm-cli -- migrate --config app.toml
```

Храните в version control SQL-файлы миграций и `migrations/schema.json`.
SQLx записывает применённые миграции и checksums в `_sqlx_migrations`: не
редактируйте миграцию после применения.

## Поддерживаемые diff

Native SQLite generator создаёт таблицы, добавляет столбцы, создаёт/удаляет
моделируемые индексы. Изменённый или удалённый столбец требует rebuild table;
значения общих столбцов сохраняются.

Генератор отклоняет обязательный новый столбец без default и переход
nullable-to-required без default. Изменение типа, rename table и rename column
нужно оформить вручную как data migration. Просматривайте generated SQL перед
применением, особенно для rebuild таблиц с inbound foreign keys.

SQLite rebuild сохраняет только общие столбцы и индексы из schema snapshot.
Triggers, views, вручную созданные индексы и другие unmanaged objects нужно
восстановить вручную в migration или не использовать native rebuild.

## PostgreSQL и Atlas

Для PostgreSQL вручную создавайте SQLx migration files и применяйте их отдельным
каталогом:

```bash
cargo run -p che-orm-cli --no-default-features --features postgres -- \
  migrate --config app.toml
```

Для генерации через Atlas включите `migration-atlas` и задайте development URL:

```bash
CHE_ORM_ATLAS_BIN=atlas CHE_ORM_ATLAS_DEV_URL='sqlite://file?mode=memory' \
cargo run -p che-orm-cli --no-default-features --features sqlite,migration-atlas -- \
  makemigrations --schema che_orm_schema.json --name add_posts
```

Для PostgreSQL укажите PostgreSQL Atlas development URL. Применение миграций
всегда выполняет SQLx и не требует Atlas.
