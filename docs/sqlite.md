# SQLite runtime

## Подключение

```rust
let database = che_orm::Database::connect("app.db")?;
```

`Database` содержит `deadpool-sqlite::Pool`. По умолчанию pool size равен 4.
Для настройки:

```rust
let database = che_orm::Database::connect_with_pool_size("app.db", 8)?;
```

Для тестов:

```rust
let database = che_orm::Database::connect_in_memory()?;
```

In-memory database должна использовать pool size 1, иначе каждое соединение
получит отдельную `:memory:` базу.

## Создание схемы

```rust
database.create_table::<User>().await?;
```

Операция выполняет:

1. `CREATE TABLE`;
2. все `CREATE INDEX`, описанные в metadata модели.

Для просмотра SQL без подключения:

```rust
let compiled = che_orm::SqlCompiler::<che_orm::SqliteDialect>
    ::compile(&User::create_table().into_ast());
println!("{}", compiled.sql);
```

## High-level CRUD

Facade API возвращает модели, включая generated primary key и значения managed
timestamp-полей:

```rust
let user = database
    .create::<User>()
    .set(User::EMAIL, "alice@example.test")
    .set(User::NAME, "Alice")
    .set(User::IS_ACTIVE, true)
    .execute()
    .await?;

let loaded = database.get::<User>(user.id).await?;
let users = database
    .query::<User>()
    .filter(User::IS_ACTIVE.eq(true))
    .order_by(User::NAME.asc())
    .limit(20)
    .all(&database)
    .await?;

let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;

let deleted = database.delete::<User>(user.id).await?;
```

`get` и `first` возвращают `Option`, если строка отсутствует. `update` также
возвращает `Option`; `delete` возвращает `bool`.

`create` и `update` используют SQLite `RETURNING` и возвращают значения,
полученные непосредственно из ORM-операции. ORM не создаёт `AFTER INSERT` или
`AFTER UPDATE` triggers, поэтому при работе с базой только через ORM
возвращаемая модель соответствует записанным значениям. Если такие triggers
добавлены вручную, через raw SQL или миграцию, они могут изменить строку после
того, как SQLite сформировал результат `RETURNING`; это значение не отражает
последующие trigger-изменения.

## Низкоуровневый insert

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

Для обычного создания используйте `database.create::<User>()`. Низкоуровневый
`insert` сохраняет `ExecuteResult` и нужен для случаев, когда не требуется
перечитывать созданную модель.

## Typed select facade

```rust
use che_orm::Model;

let users = database
    .query::<User>()
    .filter(User::IS_ACTIVE.eq(true))
    .order_by(User::NAME.asc())
    .limit(20)
    .all(&database)
    .await?;

let first = database
    .query::<User>()
    .filter(User::EMAIL.eq("alice@example.test"))
    .first(&database)
    .await?;
```

Значения фильтра параметризуются. Имена таблиц и колонок берутся из derive
metadata и не должны приходить от пользователя.

## Низкоуровневые update и delete

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

Facade `update::<User>(id)` и `delete::<User>(id)` добавляют filter по primary
key автоматически. При прямой работе с AST без `filter` update/delete
возвращают `QueryBuildError::MissingFilter`.
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
#[derive(Debug, che_orm::Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
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

Для нескольких пользователей используйте batch-загрузку, чтобы не получить
N+1 запросов:

```rust
let users = database.all::<User>().await?;
let user_ids = users.iter().map(|user| user.id);
let posts = database.fetch_by_many(Post::USER_ID, user_ids).await?;
```

`fetch_by_many` строит параметризованный `IN`-запрос. Пустой набор возвращает
пустой список без обращения к базе. Результаты следует сгруппировать по
`post.user_id` в памяти. При большом количестве ключей разделяйте их на
порции, чтобы не превысить лимит SQLite на bind-параметры.

`Database` включает `PRAGMA foreign_keys = ON` при каждом получении
соединения, поэтому несуществующий `user_id` завершится ошибкой SQLite, а
удаление пользователя каскадно удалит его posts.

`foreign_key = User` также генерирует typed relation `Post::USER`. Для
`belongs_to` используйте `select_related(Post::USER)`, а для `has_many`:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

`select_related` возвращает `WithOne<Post, User>`, а `prefetch_related` -
`Loaded<User, (LoadedMany<Post, _>,)>`. Оба результата можно передать в
подходящий serializer. Serializer не получает `Database` и не выполняет
дополнительные запросы.

Nullable foreign key поддерживает `LEFT JOIN`:

```rust
#[orm(foreign_key = User, on_delete = "set null")]
user_id: Option<i64>,
```

Такой `select_related` возвращает `WithOptionalOne<Post, User>`, где `related`
может быть `None`.

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

- автоматического diff схемы;
- PostgreSQL connection pool;
