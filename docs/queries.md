# Запросы

Общий для SQLite и PostgreSQL API начинается с `Database::query::<Model>()`.
Используйте только сгенерированные `ModelFields` константы: вручную создавать
`ModelField` небезопасно.

```rust
use che_orm::Q;

let users = db
    .query::<User>()
    .filter(UserFields::IS_ACTIVE.eq(true))
    .filter(
        UserFields::NAME.contains("Ali")
            .or(UserFields::ID.in_values([1_i64, 2, 3]))
            .and(UserFields::EMAIL.is_not_null()),
    )
    .order_by_desc(UserFields::NAME)
    .order_by(UserFields::ID)
    .limit(20)
    .offset(40)
    .all()
    .await?;
```

Поддерживаются `eq`, `contains`, `gt`, `gte`, `lt`, `lte`, `in_values`,
`is_null` и `is_not_null`. Пустой `in_values` не возвращает строк. `first()`
возвращает `Option<Model>`, а `count()` возвращает количество строк.

## SQLite-only возможности

Проекции, `select`, `group_by`, `having`, annotations и числовые агрегаты
(`sum`, `avg`, `min`, `max`) доступны только с feature `sqlite`. `distinct`
доступен в обоих backend-ах.

```rust
let names = db
    .query::<User>()
    .values([UserFields::NAME])?
    .distinct()
    .all()
    .await?;

let highest_id = db.query::<User>().max(UserFields::ID).await?;
```

Не используйте эти методы в коде, который собирается с feature `postgres`.
