# Примеры che-orm

Запускайте команды из корня workspace. Этот crate не публикуется.

| Бинарник | Что демонстрирует | Команда |
| --- | --- | --- |
| `crud` | SQLite модель, CRUD и типизированный запрос | `cargo run -p che-orm-examples --bin crud` |
| `relations` | SQLite foreign key и явная загрузка relation | `cargo run -p che-orm-examples --bin relations` |
| `schema_snapshot` | Реестр моделей в `che_orm_schema.json` | `cargo run -p che-orm-examples --bin schema_snapshot` |
| `manager` | `Application`, `Manager` и собственная CLI-команда | `cargo run -p che-orm-examples --bin manager -- --help` |

`schema_snapshot` записывает `che_orm_schema.json` в текущую директорию.
`manager` использует `example.sqlite` и `migrations` в текущей директории:

```bash
cargo run -p che-orm-examples --bin manager -- ping
cargo run -p che-orm-examples --bin manager -- makemigrations initial
cargo run -p che-orm-examples --bin manager -- migrate
cargo run -p che-orm-examples --bin manager -- status
```

Создать и применить SQLite migration из snapshot можно так:

```bash
cargo run -p che-orm-examples --bin schema_snapshot
cargo run -p che-orm-cli -- makemigrations --schema che_orm_schema.json --name initial
cargo run -p che-orm-cli -- migrate --database-url 'sqlite://example.sqlite?mode=rwc'
```

См. [руководства workspace](https://github.com/che4web/che-orm/blob/main/docs/getting-started.md) для полного
описания API, feature-флагов и миграций.
