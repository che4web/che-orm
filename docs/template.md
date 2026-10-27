# Шаблон приложения che-orm

Шаблон в `templates/che-orm` создаёт самостоятельное Rust-приложение через
[`cargo-generate`](https://cargo-generate.github.io/cargo-generate/). Требуется
Rust 1.88 или новее. Приложение требует `che-orm 0.3.1` или новее с crates.io.

## Создание проекта

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
```

Генератор предложит выбрать `sqlite` (по умолчанию) или `postgres`. Для генерации
без вопросов укажите backend явно:

```bash
cargo generate --git https://github.com/che4web/che-orm templates/che-orm \
  --name my-app --define backend=sqlite --silent
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
cargo run --bin manage -- migrate
cargo run
```

Backend выбирается feature при компиляции. Для переключения отключите default
features и включите ровно один backend на каждой команде:

```bash
cargo run --no-default-features --features postgres --bin manage -- schema
```

## Содержимое

- `src/models.rs`: модель `Task` с integer primary key и managed timestamps.
- `src/lib.rs`: `AppConfig` и `AppRegistry`.
- `src/settings.rs`: подключение к БД.
- `src/migrations/`: независимая от текущей модели compiled migration
  `tasks:0001_initial` и реестр миграций.
- `src/bin/manage.rs`: `schema`, `migrate`, `migrate status`.
- `src/main.rs`: async CRUD-пример.
- `tests/runtime.rs`: сверка модели с историей и SQLite CRUD на мигрированной БД.

Приложение не применяет миграции при старте. При изменении модели добавляйте новую
compiled migration и регистрируйте её в `src/migrations/mod.rs`. Не изменяйте
идентичности и checksums применённых миграций. Шаблон хранит структурированные
операции, которые компилируются для выбранного backend; `Task::schema()` не
используется для пересчёта первоначальной миграции.
Подробнее: [compiled migrations](atlas.md).

Шаблон сохраняет готовый объект через `Database::save(&model)` и
`Transaction::save(&model)`, доступные с версии 0.3.1.

## Проверка шаблона

В созданном приложении:

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
их с локальными core и macros, применяет SQLite-миграции и запускает CRUD.
После публикации проверьте зависимость из crates.io через
`python3 scripts/check-template.py --published`.
Флаг `--backend sqlite` или `--backend postgres` ограничивает проверку одним вариантом.
