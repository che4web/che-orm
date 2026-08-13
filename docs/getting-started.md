# Быстрый старт

`che-orm` требует Rust 1.85 и асинхронный runtime Tokio. По умолчанию crate
собирается с SQLite и нативным генератором миграций.

## Зависимости

```toml
[dependencies]
che-orm = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Для PostgreSQL отключите default features и выберите `postgres`. Backend нельзя
переключить во время выполнения: см. [backend-ы](backends.md).

## Первая модель и CRUD

```rust
use che_orm::{Database, Model};

#[derive(Debug, Clone, Model)]
#[model(table = "users")]
struct User {
    #[field(primary_key)]
    id: i64,
    #[field(unique, max_length = 255)]
    email: String,
    name: String,
    #[field(default = true)]
    is_active: bool,
}

#[tokio::main]
async fn main() -> che_orm::Result<()> {
    let db = Database::connect("sqlite::memory:").await?;
    db.create_table::<User>().await?;

    let user = db
        .create::<User>()
        .set(UserFields::EMAIL, "alice@example.com")
        .set(UserFields::NAME, "Alice")
        .set(UserFields::IS_ACTIVE, true)
        .execute()
        .await?;

    let mut user = db.get::<User>(user.id).await?;
    user.name = "Alicia".to_owned();
    let user = db.save(&user).await?;

    db.update::<User>(user.id)
        .set(UserFields::IS_ACTIVE, false)
        .execute()
        .await?;
    db.delete::<User>(user.id).await?;
    Ok(())
}
```

`Database::create_table` удобен для экспериментов и тестов. В приложении со
сохраняемыми данными применяйте миграции, а не создавайте таблицы при запуске.

## Первый запрос

`derive(Model)` генерирует `UserFields`: безопасные дескрипторы полей для
создания, обновления и запросов.

```rust
use che_orm::Q;

let users = db
    .query::<User>()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(
        UserFields::NAME.contains("Ali")
            .or(UserFields::ID.in_values([1_i64, 2, 3])),
    )
    .order_by_desc(UserFields::ID)
    .limit(20)
    .all()
    .await?;
```

Вызовы `filter` объединяются через `AND`; `Q::or` и `Q::and` задают
группировку. Полный список операторов находится в [queries.md](queries.md).

## Что дальше

1. Опишите [модели и поля](models-and-fields.md).
2. Настройте [миграции](migrations.md) для постоянной базы данных.
3. Выберите нужный [backend](backends.md).
