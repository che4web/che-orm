# che-orm2

Экспериментальная типизированная ORM для Rust. Сейчас рабочий runtime-backend —
SQLite через `deadpool-sqlite`, а строки декодируются обратно в модели,
сгенерированные procedural macro.

## Возможности

- `#[derive(Model)]` для генерации метаданных модели и typed fields.
- SQL AST и параметризованная генерация `SELECT`, `INSERT`, `UPDATE`, `DELETE`.
- Генерация `CREATE TABLE`, индексов и ограничений.
- Async SQLite pool через `deadpool-sqlite` и Tokio.
- High-level `get`, `all`, `create`, `update`, `delete` и typed `query` facade.
- Низкоуровневые `insert`, `fetch_all`, `fetch_one` и произвольный `QueryAst`.
- Транзакции с commit/rollback.
- Foreign keys с каскадным удалением и typed `fetch_by` для связанных строк.
- Versioned migrations через Atlas и встроенный `manage` CLI.
- Разделение моделей по application-модулям через `AppConfig` и `AppRegistry`.
- `OffsetDateTime` и managed-поля `auto_now_add` / `auto_now`.
- Защита от пустых mutation-запросов и случайных массовых update/delete.

Проект находится в разработке. Automatic relations, joins и PostgreSQL
executor пока не реализованы.

## Быстрый старт

Добавьте зависимость:

```toml
[dependencies]
che-orm2 = { path = "../che-orm2" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
time = { version = "0.3", features = ["formatting", "parsing"] }
```

Опишите модель:

```rust
use che_orm2::Model;
use time::OffsetDateTime;

#[derive(Debug, Model)]
#[orm(table = "users", index("name"))]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    name: String,
    is_active: bool,
    #[orm(auto_now_add)]
    created_at: OffsetDateTime,
    #[orm(auto_now)]
    updated_at: OffsetDateTime,
}
```

Используйте модель через async SQLite database:

```rust
use che_orm2::Database;

#[tokio::main]
async fn main() -> Result<(), che_orm2::OrmError> {
    let database = Database::connect("app.db")?;
    database.create_table::<User>().await?;

    let user = database
        .create::<User>()
        .set(User::EMAIL, "alice@example.test")
        .set(User::NAME, "Alice")
        .set(User::IS_ACTIVE, true)
        .execute()
        .await?;

    let users = database
        .query::<User>()
        .filter(User::NAME.eq("Alice"))
        .all()
        .await?;
    let loaded = database.get::<User>(user.id).await?;
    println!("{users:?}");
    println!("{loaded:?}");
    Ok(())
}
```

Managed timestamp-поля не передаются в `INSERT`: их значения создаёт SQLite.
`updated_at` автоматически добавляется ORM в каждый update-запрос.

## Пул

По умолчанию создаётся пул размером 4:

```rust
let database = Database::connect("app.db")?;
```

В application binary можно использовать единый путь из settings:

```rust
let database = Database::connect_configured()?;
```

Размер можно задать явно:

```rust
let database = Database::connect_with_pool_size("app.db", 8)?;
```

Для in-memory SQLite используется пул размера 1:

```rust
let database = Database::connect_in_memory()?;
```

Это важно: разные SQLite-соединения с `:memory:` имеют разные базы данных.

## CRUD и транзакции

Основной facade автоматически ограничивает update/delete primary key:

```rust
let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;

let deleted = database.delete::<User>(user.id).await?;
assert!(deleted);
```

Для сложных условий и массовых операций используйте низкоуровневый AST.
`UPDATE` и `DELETE` должны иметь фильтр. Массовую операцию нужно разрешить
явно через `.allow_all()`:

```rust
let query = User::update()
    .set(User::NAME, "New name")
    .filter(User::ID.eq(1))
    .into_ast()?;

database.execute(query).await?;
```

Транзакция выполняется в worker-потоке SQLite pool:

```rust
database
    .transaction(|connection| {
        connection.execute("DELETE FROM users WHERE id = ?1", [1])?;
        Ok(())
    })
    .await?;
```

Ошибка closure вызывает rollback, успешное выполнение вызывает commit.

## Связанные модели

Foreign key задаётся на поле дочерней модели:

```rust
#[derive(Debug, Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

Загрузить записи `has_many` можно через typed helper:

```rust
let user = database
    .create::<User>()
    .set(User::EMAIL, "alice@example.test")
    .set(User::NAME, "Alice")
    .execute()
    .await?;
let user_id = user.id;
let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

Для нескольких пользователей используйте `fetch_by_many`, а не вызывайте
`fetch_by` в цикле: это устраняет N+1 запросов.

```rust
let users = database.all::<User>().await?;
let user_ids = users.iter().map(|user| user.id);
let posts = database.fetch_by_many(Post::USER_ID, user_ids).await?;
```

Дочерние строки затем группируются по `post.user_id` в памяти. Пустой набор
возвращает пустой список; для очень больших наборов ключей используйте
несколько порций из-за лимита SQLite на bind-параметры.

`fetch_by` строит обычный параметризованный `SELECT`. SQLite foreign keys
включаются для каждого соединения пула автоматически. Поэтому
`on_delete = "cascade"` будет работать и при удалении пользователя.

`Post::USER` описывает forward relation, а `Post::USER.reverse()` имеет
reverse name `post_set` по умолчанию. Связанные модели загружаются через typed
queryset:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all()
    .await?;
```

Результат `WithMany<User, Post>` можно передать в `ModelSerializer`; serializer
получает только материализованные данные и не имеет доступа к `Database`.

## Примеры

В workspace есть crate `che-orm2-examples`:

```bash
cargo run -p che-orm2-examples --bin schema
cargo run --bin manage -- migrate
cargo run -p che-orm2-examples --bin sqlite_crud
cargo run -p che-orm2-examples --bin transactions
cargo run -p che-orm2-examples --bin serializers
```

## Миграции

CLI миграций является частью приложения:

```bash
cargo run --bin manage -- schema
cargo run --bin manage -- makemigrations
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
cargo run --bin manage -- migrate lint
```

`makemigrations` собирает schema из Rust-моделей, записывает её во временный
файл, передаёт Atlas через `--to file://...` и удаляет файл после завершения.
Миграции хранятся в `migrations/` и применяются только отдельной командой
deploy, а не при старте приложения. Подробности: [`docs/atlas.md`](docs/atlas.md).
В некоторых версиях Atlas `migrate lint` требует `atlas login`/Pro; это
ограничение Atlas, а не приложения.

## Applications

Модели можно группировать по приложениям, как в Django:

```rust
pub struct AccountsApp;

impl che_orm2::AppConfig for AccountsApp {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm2::SchemaSet {
        che_orm2::SchemaSet::new().model::<User>()
    }
}

let registry = che_orm2::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

Каждое приложение хранит свои модели и возвращает свой `SchemaSet`. Общий
registry используется `manage schema` и `makemigrations`. Регистрируйте
приложения в порядке зависимостей: сначала таблицы-родители, затем модели с
foreign keys.

## Backend status

| Backend | SQL compiler | Pool/executor |
| --- | --- | --- |
| SQLite | Да | Да, async `deadpool-sqlite` |
| PostgreSQL | Да, placeholders и DDL dialect | Нет |

Для проверки PostgreSQL compiler без SQLite runtime:

```bash
cargo test -p che-orm2 --no-default-features --features postgres
```

Features `sqlite` и `postgres` взаимоисключающие.

## Проверка проекта

```bash
cargo fmt --check
cargo test --workspace
cargo doc --workspace --no-deps
```

Подробности находятся в [`docs/models.md`](docs/models.md) и
[`docs/sqlite.md`](docs/sqlite.md).
