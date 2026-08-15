# SQLite runtime

## Подключение

```rust
let database = che_orm2::Database::connect("app.db")?;
```

`Database` содержит `deadpool-sqlite::Pool`. По умолчанию pool size равен 4.
Для настройки:

```rust
let database = che_orm2::Database::connect_with_pool_size("app.db", 8)?;
```

Для тестов:

```rust
let database = che_orm2::Database::connect_in_memory()?;
```

In-memory database должна использовать pool size 1, иначе каждое соединение
получит отдельную `:memory:` базу.

## Создание схемы

```rust
database.create_table::<User>().await?;
```

Операция выполняет:

1. `CREATE TABLE`;
2. все `CREATE INDEX` из `#[orm(index(...))]`;
3. все generated indexes.

Для просмотра SQL без подключения:

```rust
let compiled = che_orm2::SqlCompiler::<che_orm2::SqliteDialect>
    ::compile(&User::create_table().into_ast());
println!("{}", compiled.sql);
```

## Insert

```rust
let user = User {
    id: 0,
    email: "alice@example.test".into(),
    name: "Alice".into(),
    is_active: true,
    created_at: time::OffsetDateTime::now_utc(),
    updated_at: time::OffsetDateTime::now_utc(),
};

let result = database.insert(&user).await?;
println!("{}", result.rows_affected);
println!("{:?}", result.last_insert_rowid);
```

`primary_key`, `auto_now_add` и `auto_now` не включаются в обычный insert.
Поле `auto_now` автоматически добавляется в ORM-generated update.
Остальные поля передаются как SQL parameters.

## Select

```rust
use che_orm2::Model;

let users = database
    .fetch_all(
        User::query()
            .filter(User::IS_ACTIVE.eq(true))
            .order_by(User::NAME.asc())
            .limit(20),
    )
    .await?;

let first = database
    .fetch_one(User::query().filter(User::EMAIL.eq("alice@example.test")))
    .await?;
```

Значения фильтра параметризуются. Имена таблиц и колонок берутся из derive
metadata и не должны приходить от пользователя.

## Update и delete

```rust
let update = User::update()
    .set(User::NAME, "Alice Cooper")
    .filter(User::ID.eq(1))
    .into_ast()?;
database.execute(update).await?;

let delete = User::delete()
    .filter(User::ID.eq(1))
    .into_ast()?;
database.execute(delete).await?;
```

Без `filter` update/delete возвращают `QueryBuildError::MissingFilter`.
Если массовая операция действительно нужна:

```rust
let query = User::update()
    .set(User::IS_ACTIVE, false)
    .allow_all()
    .into_ast()?;
```

## Связанные строки

Опишите дочернюю модель с foreign key:

```rust
#[derive(Debug, che_orm2::Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(references = "users(id)", on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

Создайте таблицы в порядке зависимости и загрузите дочерние строки:

```rust
database.create_table::<User>().await?;
database.create_table::<Post>().await?;

let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

`Database` включает `PRAGMA foreign_keys = ON` при каждом получении
соединения, поэтому несуществующий `user_id` завершится ошибкой SQLite, а
удаление пользователя каскадно удалит его posts.

## Транзакции

```rust
database
    .transaction(|connection| {
        connection.execute(
            "UPDATE users SET is_active = ?1 WHERE id = ?2",
            (false, 1),
        )?;
        Ok(())
    })
    .await?;
```

Closure получает blocking `rusqlite::Connection`, но вызывается внутри
worker-потока `deadpool-sqlite`, поэтому SQLite-операция не выполняется на
Tokio executor thread. Ошибка из closure вызывает rollback, успех вызывает
commit.

## Ошибки и ограничения

`OrmError` объединяет pool error, SQLite error, interaction error и ошибки
построения query.

Текущий runtime не содержит:

- миграций;
- автоматического diff схемы;
- relations и joins на уровне ORM-моделей;
- PostgreSQL connection pool;
- typed update API для каждого backend-а.
