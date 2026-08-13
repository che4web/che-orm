# che-orm

Экспериментальный ORM для Rust, вдохновлённый Django ORM. Библиотека даёт
типизированные модели, CRUD, выражения запросов, метаданные схемы и миграции.
Минимальная версия Rust: 1.85.

> Проект находится на стадии MVP. Перед использованием в production изучите
> [ограничения](docs/backends.md#ограничения) и правила работы с
> [миграциями](docs/migrations.md).

## Начало работы

Добавьте SQLite-вариант по умолчанию и Tokio в приложение:

```toml
[dependencies]
che-orm = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use che_orm::{Database, Model};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    email: String,
    name: String,
}

#[tokio::main]
async fn main() -> che_orm::Result<()> {
    let db = Database::connect("sqlite::memory:").await?;
    db.create_table::<User>().await?;

    let user = db
        .create::<User>()
        .set(UserFields::EMAIL, "alice@example.com")
        .set(UserFields::NAME, "Alice")
        .execute()
        .await?;

    let users = db
        .query::<User>()
        .filter(UserFields::EMAIL.contains("@example.com"))
        .all()
        .await?;
    assert_eq!(users[0].id, user.id);
    Ok(())
}
```

Полное пошаговое руководство находится в [docs/getting-started.md](docs/getting-started.md).

## Возможности

| Возможность | SQLite | PostgreSQL |
| --- | --- | --- |
| Модели, CRUD, типизированные фильтры, сортировка, пагинация, `count` | Да | Да |
| Ручные SQLx-миграции и применение миграций | Да | Да |
| Нативное создание миграций из schema snapshot | Да | Нет |
| Atlas для создания миграций (experimental) | Да | Да |
| Relation loading, signals, projections, группировки, numeric aggregates | Да | Нет |
| Foreign-key metadata and DDL | Да | Да |
| `distinct` для запросов моделей | Да | Да |

Backend выбирается во время компиляции. Флаги `sqlite` и `postgres`
взаимоисключающие; по умолчанию включён `sqlite`.

```toml
che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
```

Подробнее: [backends.md](docs/backends.md).

## Документация

- [Быстрый старт](docs/getting-started.md)
- [Модели и поля](docs/models-and-fields.md)
- [Запросы](docs/queries.md)
- [Связи и сигналы](docs/relations-and-signals.md)
- [Миграции](docs/migrations.md)
- [Backend-ы и feature-флаги](docs/backends.md)
- [Runtime manager](docs/manager.md)
- [API на docs.rs](https://docs.rs/che-orm)
- [CLI](crates/che-orm-cli/README.md)
- [Запускаемые примеры](crates/che-orm-examples/README.md)

## Состав workspace

- `che-orm`: runtime API.
- `che-orm-macros`: реализации `#[derive(Model)]` и `#[derive(Choice)]`.
- `che-orm-cli`: бинарник `che-orm` для миграций.
- `che-orm-examples`: запускаемые примеры, не публикуется.

## Проверка

```bash
cargo test --workspace
cargo test -p che-orm --test backend_compile
cargo test -p che-orm --no-default-features --features postgres --test backend_compile
```
