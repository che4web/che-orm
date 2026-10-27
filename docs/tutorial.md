# Пошаговый tutorial

Этот tutorial создаёт две связанные модели, сохраняет данные в SQLite, загружает
relations и показывает переход к compiled migrations. Для первого запуска
используется in-memory SQLite; `create_table` предназначен только для тестов и
локальных экспериментов.

## 1. Создайте приложение

Готовый проект с моделью и compiled migrations можно создать через
[шаблон cargo-generate](template.md). Шаги ниже показывают ручную настройку.

```bash
cargo new blog-app
```

Добавьте локальную зависимость на `che-orm` в `Cargo.toml`. Измените путь под
расположение вашего приложения:

```toml
[dependencies]
che-orm = { path = "../che-orm" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
time = { version = "0.3", features = ["formatting", "parsing"] }
```

## 2. Опишите модели

Замените содержимое `src/main.rs` этим кодом. `foreign_key = User` создаёт
foreign key в DDL, `Post::USER` для `belongs_to` и
`Post::USER.reverse()` для `has_many`.

```rust
use che_orm::{Database, Model};
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "users", index("name"))]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    name: String,
    #[orm(auto_now_add)]
    created_at: OffsetDateTime,
    #[orm(auto_now)]
    updated_at: OffsetDateTime,
}

#[derive(Debug, Model)]
#[orm(table = "posts", index("user_id"))]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}

#[tokio::main]
async fn main() -> Result<(), che_orm::OrmError> {
    let database = Database::connect_in_memory().await?;
    database.create_table::<User>().await?;
    database.create_table::<Post>().await?;

    Ok(())
}
```

Модели должны иметь ровно один `#[orm(primary_key)]` типа `i64` или
`che_orm::Uuid`. Для `Uuid` INSERT-builder автоматически генерирует UUIDv7,
если ключ не передан. Внешние ключи должны совпадать с типом первичного ключа
целевой модели. Подробнее см. [описание моделей](models.md#uuidv7).
Поля `auto_now_add` и `auto_now` должны быть `OffsetDateTime`; ORM управляет
их значениями автоматически.

## 3. Создайте и прочитайте запись

Добавьте в `main` после создания таблиц:

```rust
let user = database
    .create::<User>()
    .set(User::EMAIL, "alice@example.test")
    .set(User::NAME, "Alice")
    .execute()
    .await?;

let users = database
    .query::<User>()
    .filter(User::NAME.eq("Alice"))
    .order_by(User::EMAIL.asc())
    .all(&database)
    .await?;

let loaded = database.get::<User>(user.id).await?;
println!("{users:?}");
println!("{loaded:?}");
```

`filter` создаёт параметризованное SQL-условие. Терминальные методы queryset,
такие как `all`, `first` и `count`, получают `&Database`:

```rust
let first = database
    .query::<User>()
    .filter(User::EMAIL.eq("alice@example.test"))
    .first(&database)
    .await?;
```

Запустите приложение:

```bash
cargo run
```

## 4. Обновите запись

High-level facade всегда ограничивает `update` primary key:

```rust
let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;

println!("{updated:?}");
```

`update` возвращает `Option<User>`, если запись могла быть удалена до update.
Для сложных или массовых операций используйте AST-builder и добавляйте
`filter`; вызов без фильтра отклоняется, пока явно не указан `allow_all()`.

## 5. Загрузите связанные данные

Создайте пост для ранее созданного пользователя:

```rust
let post = database
    .create::<Post>()
    .set(Post::USER_ID, user.id)
    .set(Post::TITLE, "First post")
    .execute()
    .await?;

let posts = database.fetch_by(Post::USER_ID, user.id).await?;
println!("{post:?}");
println!("{posts:?}");
```

Для списка пользователей не вызывайте `fetch_by` в цикле. Используйте
`prefetch_related`: ORM выполнит запрос пользователей и один пакетный запрос
постов.

```rust
let users_with_posts = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

Результат имеет тип `Loaded<User, (LoadedMany<Post, PostUserRelation>,)>`.
Для связи в обратную сторону используйте `select_related`, который формирует
JOIN и возвращает `WithOne<Post, User>`:

```rust
let posts_with_user = database
    .query::<Post>()
    .select_related(Post::USER)
    .all(&database)
    .await?;
```

Nullable foreign key (`Option<i64>`) с `on_delete = "set null"` использует
`LEFT JOIN`; его `select_related` возвращает `WithOptionalOne`.

Удаление через high-level facade также ограничено primary key. В этом примере
оно каскадно удалит созданные posts:

```rust
let deleted = database.delete::<User>(user.id).await?;
assert!(deleted);
```

## 6. Перейдите к compiled migrations

Не используйте `create_table` в production. Разделите модели по application
модулям, реализуйте `AppConfig` и зарегистрируйте приложения в порядке
зависимостей: родительская таблица раньше дочерней.

```rust
pub struct AccountsApp;

impl che_orm::AppConfig for AccountsApp {
    fn name() -> &'static str {
        "accounts"
    }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}

let registry = che_orm::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

В этом репозитории example application хранит registry, путь к базе и migrations
в `che-orm-examples`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

`database_url()` читает `CHE_ORM_DATABASE_URL`, а при его отсутствии использует
настройку по умолчанию для выбранного backend. Выполните команды из корня репозитория:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

Пример содержит замороженный `0001_initial` отдельно для SQLite и PostgreSQL.
Его `manage` не генерирует миграции. В своём приложении храните реестр compiled
операций, добавляйте новые миграции через `Migration::new` и применяйте их через
`Database::migrate`. Не меняйте идентичности и checksums уже применённых миграций.
Подробности: [compiled migrations](atlas.md).

## 7. Изучите runnable examples

В этом репозитории готовы примеры с актуальным публичным API:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin serializers
cargo run -p che-orm-examples --bin transactions
```

Следующие справочные материалы: [модели и схема](models.md),
[SQLite runtime](sqlite.md) и [compiled migrations](atlas.md).
