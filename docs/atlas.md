# Миграции через Atlas

CLI миграций является частью приложения и запускается через бинарник
`manage`:

```bash
cargo run --bin manage -- schema
cargo run --bin manage -- makemigrations
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
cargo run --bin manage -- migrate lint
```

## Временная schema-файл

Команда `makemigrations` не оставляет сгенерированный desired schema в рабочем
дереве:

1. `manage` собирает `SchemaSet` из моделей приложения.
2. Записывает SQL во временный файл в системном temp directory.
3. Запускает Atlas с `--to file://<temporary-schema>`.
4. Atlas сравнивает его с migration directory и создаёт versioned migration.
5. `manage` удаляет временный файл независимо от результата Atlas.

Это предотвращает рассинхронизацию постоянного `schema.sql` и Rust-моделей.

Модели для Atlas регистрируются в `src/apps/mod.rs`:

```rust
pub fn registry() -> AppRegistry {
    AppRegistry::new()
        .register::<accounts::App>()
        .register::<content::App>()
}
```

Каждый `AppConfig` владеет собственным набором моделей. Порядок регистрации
определяет порядок SQL DDL и должен учитывать foreign keys.

Путь к базе находится в `src/settings.rs` и используется и runtime, и
`manage`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

В application code для этого пути можно использовать
`Database::connect_configured()`.

Порядок вызова `.model::<...>()` является порядком DDL. Сначала добавляйте
таблицы-родители, затем таблицы с foreign keys.

## Команды

### `schema`

Печатает полный desired schema в stdout. Эта команда полезна для проверки:

```bash
cargo run --bin manage -- schema > /tmp/schema.sql
```

### `makemigrations`

Генерирует новую migration. Если имя не указано, оно создаётся автоматически
в формате `auto_<unix_timestamp>`:

```bash
cargo run --bin manage -- makemigrations
cargo run --bin manage -- makemigrations add_user_status
```

Команда использует `migrations/` и SQLite dev database
`sqlite://dev?mode=memory`.

### `migrate`

Применяет pending migrations к базе:

```bash
cargo run --bin manage -- migrate
```

Приложение не применяет migrations автоматически при старте. Это делается
отдельным шагом deploy/CI.

### `migrate status` и `migrate lint`

```bash
cargo run --bin manage -- migrate status
cargo run --bin manage -- migrate lint
```

В старых и canary-версиях Atlas команда `migrate lint` может требовать
`atlas login` или Atlas Pro. Это ограничение самого Atlas, а не wrapper-а.
`makemigrations`, `migrate` и `migrate status` работают без этой команды.

## Atlas executable

По умолчанию wrapper ищет исполняемый файл `atlas` в `PATH`. Другой путь можно
указать через `ATLAS_BIN`:

```bash
ATLAS_BIN=/opt/atlas/atlas cargo run --bin manage -- makemigrations add_posts
```

`manage` не интерполирует аргументы через shell и возвращает ненулевой exit code
Atlas как ошибку процесса.

## Required enum columns

Adding a required `DbEnum` field to a table that already has rows needs a data backfill. Review
the generated Atlas migration and set a valid enum value while copying the old rows, for example:

```sql
INSERT INTO new_tasks_task (id, name, status)
SELECT id, name, 'draft' FROM tasks_task;
```

The replacement table must keep the enum `CHECK` constraint. Test the migration against a copy of
an existing database before deployment.

## Production workflow

1. Изменить Rust-модель.
2. Запустить `makemigrations`.
3. Проверить сгенерированный SQL.
4. Запустить `migrate lint` в CI.
5. Закоммитить migration files и `atlas.sum`.
6. На deploy выполнить `migrate`.

После перехода на Atlas не используйте `Database::create_table` для production
инициализации схемы: этот метод предназначен для тестов и локальных сценариев.
