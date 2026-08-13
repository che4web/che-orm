# che-orm

`che-orm` - экспериментальный ORM для Rust, вдохновлённый Django ORM. Он
предоставляет типизированные модели, CRUD, запросы, schema snapshots и
применение SQLx migrations. Минимальная версия Rust: 1.85.

## Быстрый пример

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

    let fetched = db.get::<User>(user.id).await?;
    assert_eq!(fetched.email, "alice@example.com");
    Ok(())
}
```

## Backend-ы

По умолчанию выбран SQLite:

```toml
che-orm = "0.1"
```

Для PostgreSQL отключите default features. Backend выбирается при компиляции;
`sqlite` и `postgres` нельзя включать вместе.

```toml
che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
```

Оба backend-а поддерживают модели, CRUD, типизированные predicates, ordering,
pagination, `count`, `distinct`, foreign-key metadata и применение ручных SQLx
migrations. API загрузки relations, signals, projections, grouped queries,
annotations и numeric aggregates доступны только для SQLite. Нативное создание
migration diff также доступно только для SQLite; Atlas может генерировать миграции
для обоих backend-ов.

Страница API на docs.rs собирается с SQLite-конфигурацией по умолчанию
(`sqlite` и `migration-native`). Для PostgreSQL используйте конфигурацию
`default-features = false, features = ["postgres"]`.

## Руководства

Полная документация workspace находится в корне репозитория:

- [Быстрый старт](https://github.com/che4web/che-orm/blob/v0.1.0/docs/getting-started.md)
- [Модели и поля](https://github.com/che4web/che-orm/blob/v0.1.0/docs/models-and-fields.md)
- [Запросы](https://github.com/che4web/che-orm/blob/v0.1.0/docs/queries.md)
- [Связи и сигналы](https://github.com/che4web/che-orm/blob/v0.1.0/docs/relations-and-signals.md)
- [Миграции](https://github.com/che4web/che-orm/blob/v0.1.0/docs/migrations.md)
- [Backend-ы и feature-флаги](https://github.com/che4web/che-orm/blob/v0.1.0/docs/backends.md)
- [Runtime manager](https://github.com/che4web/che-orm/blob/v0.1.0/docs/manager.md)

На docs.rs подробные описания типов и методов доступны в API documentation.

## Ограничения MVP

- Не поддерживаются автоматическое определение rename и rollback migrations.
- Изменение типа требует явной data migration.
- Schema snapshots не обратно совместимы между изменениями формата ORM.
- Миграции для одной базы запускайте из одного процесса за раз.
