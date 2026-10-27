# Шаблон приложения che-orm

Шаблон в `templates/che-orm` создаёт самостоятельное Rust-приложение через
[`cargo-generate`](https://cargo-generate.github.io/cargo-generate/). Требуется
Rust 1.88 или новее. Приложение требует `che-orm 0.3.2` или новее с crates.io.
До публикации 0.3.2 проверяйте шаблон с локальными patches командой
`python3 scripts/check-template.py`.

## Создание проекта

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
```

Генератор запросит имя проекта, backend (`sqlite` или `postgres`) и структуру:
`single` (по умолчанию, модели в `src/models.rs`) или `modular` (стартовый модуль
`src/apps/tasks`). Для генерации без вопросов укажите параметры явно:

```bash
cargo generate --git https://github.com/che4web/che-orm templates/che-orm \
  --name my-app --define backend=sqlite --define layout=modular --silent
```

Из локального checkout репозитория:

```bash
cargo generate --path templates/che-orm --name my-app --define backend=sqlite --silent --no-workspace
```

`--no-workspace` оставляет приложение отдельным от workspace библиотеки.
Имя проекта с дефисами автоматически преобразуется в Rust-имя с подчёркиваниями
для импортов.

## Запуск

Для SQLite:

```bash
cd my-app
cargo run --bin manage -- schema
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
cargo run
```

База по умолчанию — `app.db`. Переменная `CHE_ORM_DATABASE_URL` переопределяет
путь. CRUD-пример создаёт, читает, обновляет и удаляет одну задачу.

Для PostgreSQL выберите `backend=postgres`, создайте БД и перед запуском задайте
URL, например:

```bash
export CHE_ORM_DATABASE_URL='postgresql://postgres:postgres@localhost/my_app'
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run
```

Backend выбирается feature при компиляции. Для переключения отключите default
features и включите ровно один backend на каждой команде:

```bash
cargo run --no-default-features --features postgres --bin manage -- schema
```

## Содержимое

- `src/models.rs` (`single`) или `src/apps/tasks/models.rs` (`modular`): модель
  `Task` с integer primary key и managed timestamps.
- `src/apps/mod.rs`: реестр модулей приложения.
- `src/lib.rs`: `AppConfig` и `AppRegistry`.
- `src/settings.rs`: подключение к БД.
- `src/migrations/`: пустой реестр compiled migrations. Первую миграцию
  пользователь создаёт через `makemigrations --name initial`.
- `src/bin/manage.rs`: `schema`, `makemigrations`, `migrate`, `migrate status`.
- `src/bin/manage/makemigrations.rs`: генератор compiled migrations приложения.
- `src/bin/manage/createapp.rs`: генератор новых модулей.
- `src/main.rs`: async CRUD-пример.
- `tests/runtime.rs`: сверка модели с историей и SQLite CRUD на мигрированной БД.

Шаблон не содержит готовой `0001_initial`: сначала настройте модели и создайте
начальную миграцию командой из раздела запуска. Приложение не применяет миграции
при старте. При изменении модели создайте новую
compiled migration:

```bash
cargo run --bin manage -- makemigrations --name update_tasks
cargo run --bin manage -- migrate
```

Команда сравнивает модели с результатом replay compiled-истории, создаёт следующий
пронумерованный Rust-модуль и обновляет `src/migrations/mod.rs`. Она не подключается
к БД. Для форматирования нужен `rustfmt`: `rustup component add rustfmt`.
`--dry-run` печатает предложенный Rust-код без записи, а `--check` завершает команду
с ошибкой, если модели требуют новой миграции. Если изменений нет, файлы не создаются.
Всегда запускайте через `cargo run`: устаревший бинарник отклоняет генерацию, если
исходники миграций изменились после его компиляции.

Перед применением проверьте операции: перенос данных и явные переименования могут
потребовать ручной правки. Не изменяйте
идентичности и checksums применённых миграций. Шаблон хранит структурированные
операции, которые компилируются для выбранного backend; `Task::schema()` не
используется для пересчёта первоначальной миграции.
Подробнее: [compiled migrations](atlas.md).

Шаблон сохраняет готовый объект через `Database::save(&model)` и
`Transaction::save(&model)`, доступные с версии 0.3.1.
После `save` пример использует возвращённую модель с актуальными managed timestamps.
Тесты проверяют commit, rollback и возврат `None` при сохранении удалённой строки.

## Создание модуля

```bash
cargo run --bin manage -- createapp blog
```

Команда доступна для обеих структур. Она создаёт `src/apps/blog/mod.rs` с
`APP_NAME`, `AppConfig`, `models.rs` с подсказкой по именованию и автоматически
регистрирует модуль в `src/apps/mod.rs`. Имя должно быть snake_case, например
`user_accounts`.
Для `single` исходные модели остаются в корневом `src/models.rs`; для `modular`
стартовый `Task` находится в `src/apps/tasks/models.rs`.

Добавьте модели с `#[orm(app = super::APP_NAME)]` и зарегистрируйте их в
`AppConfig::schema()` нового модуля, например
`SchemaSet::new().model::<models::Post>()`. Затем выполните `makemigrations` и
`migrate`. Все модули используют общий реестр миграций `src/migrations/`, при этом
операции сохраняют владельца каждой модели. FK-родители должны регистрироваться
раньше детей; новые модули добавляются в конец реестра. Сохраняйте комментарии
`createapp` в `src/apps/mod.rs` для дальнейшего автоматического подключения.
Команда требует `rustfmt`, не подключается к БД и не перезаписывает существующие
модули. Запускайте её через `cargo run`, чтобы пересобрать изменённый реестр.

`createapp blog` создаёт `pub const APP_NAME: &str = "blog"` в `mod.rs`
приложения, а `AppConfig::name()` возвращает эту константу. Для `Post` с
`#[orm(app = super::APP_NAME)]` получится `blog_post` (см. [Модели](models.md#app)).
Стартовый `Task` использует таблицу `tasks_task` в обеих структурах. Его константа
находится в `src/lib.rs` для `single` и `src/apps/tasks/mod.rs` для `modular`.
Явный `#[orm(table = "existing_table")]` позволяет сохранить существующее имя.

## Проверка шаблона

В созданном приложении после генерации начальной миграции:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

PostgreSQL-вариант проверяет историю без подключения к серверу. Дополнительно
можно запустить in-memory CRUD через `cargo test --no-default-features --features sqlite`.

Из корня ORM-репозитория можно проверить генерацию и оба backend:

```bash
python3 scripts/check-template.py
```

Нужен установленный `cargo-generate`; скрипт создаёт временные проекты, проверяет
их с локальными core и macros, создаёт начальную миграцию из пустой истории,
применяет SQLite-миграции и запускает CRUD.
Также он меняет модель, проверяет `--check`/`--dry-run`, генерирует follow-up
миграцию, проверяет защиту от устаревшего бинарника и сохранность существующих строк.
После публикации проверьте зависимость из crates.io через
`python3 scripts/check-template.py --published`.
Флаг `--backend sqlite` или `--backend postgres` ограничивает проверку одним вариантом.
`--layout single` или `--layout modular` ограничивает проверку структуры. По
умолчанию проверяются все четыре комбинации, включая `createapp`, подключение
новой модели с FK и применение общей миграционной истории.
