# Модели и схема

## Derive Model

`#[derive(Model)]` генерирует реализацию `Model` и константы typed fields.
Каждая модель должна иметь ровно одно поле `#[orm(primary_key)]` типа `i64`:

```rust
#[derive(Model)]
#[orm(table = "users", index("name"))]
struct User {
    #[orm(primary_key)]
    id: i64,
    email: String,
    name: String,
}
```

После derive доступны `User::ID`, `User::EMAIL`, `User::NAME`, которые
используются в фильтрах, сортировке и assignment-ах.
Также `User::primary_key()` используется high-level facade для `get`, `update`
и `delete`.

`Debug` в `#[derive(Debug, Model)]` не относится к ORM. Это стандартный Rust
derive для печати модели через `{:?}`.

## Атрибуты таблицы

### `table`

Задаёт имя таблицы и обязателен:

```rust
#[orm(table = "users")]
```

### `index`

Создаёт индекс после таблицы. Поддерживаются одиночные и составные индексы:

```rust
#[orm(table = "users", index("name"), index("tenant_id", "email"))]
```

### `unique`

Создаёт table-level unique constraint. Для одной колонки проще использовать
`#[orm(unique)]` на поле. Table-level форма нужна для составных ограничений:

```rust
#[orm(table = "memberships", unique("organization_id", "user_id"))]
```

Не задавайте обе формы для одного поля: это создаст две уникальные проверки.

## Атрибуты поля

| Атрибут | Назначение |
| --- | --- |
| `primary_key` | Первичный ключ; `i64` получает identity/rowid поведение backend-а. |
| `unique` | Уникальность одной колонки. |
| `default = "..."` | SQL expression для `DEFAULT`. Значение не параметризуется. |
| `check = "..."` | SQL expression внутри `CHECK (...)`. |
| `references = "roles(id)"` | Foreign key target. |
| `on_delete = "cascade"` | `ON DELETE` действие для foreign key. |
| `auto_now_add` | Managed timestamp при вставке. Требует `OffsetDateTime`. |
| `auto_now` | Managed timestamp при обновлении. Требует `OffsetDateTime`. |

Пример:

```rust
#[derive(Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    #[orm(check = "length(name) > 0")]
    name: String,
    #[orm(default = "true")]
    is_active: bool,
    #[orm(auto_now_add)]
    created_at: time::OffsetDateTime,
    #[orm(auto_now)]
    updated_at: time::OffsetDateTime,
}
```

## Поддерживаемые типы

| Rust | SQL SQLite | Nullable |
| --- | --- | --- |
| `i64` | `INTEGER` | Нет |
| `String` | `TEXT` | Нет |
| `bool` | `INTEGER` (`0`/`1`) | Нет |
| `time::OffsetDateTime` | `TEXT` UTC | Нет |
| `Option<T>` | тип `T` | Да |

`Option<T>` определяется как nullable автоматически. Для `OffsetDateTime`
используется feature `time` у `rusqlite` и UTC-значения.

## Timestamp fields

`auto_now_add` и `auto_now` предназначены для полей, аналогичных Django
`auto_now_add` и `auto_now`.

- оба поля получают SQLite `DEFAULT` на основе `strftime(..., 'now')`;
- managed-поля исключаются из обычного `INSERT` ORM;
- для `auto_now` ORM добавляет assignment в каждый update-запрос;
- raw SQL через `transaction` не обновляет timestamp автоматически;
- при чтении значение декодируется в `OffsetDateTime`.

На PostgreSQL compiler генерирует `TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP`, но
PostgreSQL connection/executor пока не входит в runtime.

## Связи

Модели можно раскладывать по application-модулям. Приложение реализует
`AppConfig` и возвращает собственный `SchemaSet`:

```rust
pub struct AccountsApp;

impl che_orm2::AppConfig for AccountsApp {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm2::SchemaSet {
        che_orm2::SchemaSet::new().model::<User>()
    }
}
```

Затем приложения объединяются через `AppRegistry`:

```rust
let registry = che_orm2::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

Это разделяет ownership моделей и оставляет единый desired schema для Atlas.

Foreign key между ORM-моделями описывается через `foreign_key`:

```rust
#[derive(Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(foreign_key = User, on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

`foreign_key = User` генерирует `Post::USER`, DDL foreign key на `users(id)` и
`Post::USER.reverse()` для обратной связи. Имя обратной связи по умолчанию
`post_set`. `on_delete = "cascade"` добавляет каскадное удаление. Для таблиц
без ORM-модели можно использовать `references = "table(column)"`, но typed
relation для такого поля не создаётся. При работе через
`Database` SQLite `PRAGMA foreign_keys = ON` включается на каждом соединении
пула.

Отношение загружается через typed field дочерней модели:

```rust
let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

Это эквивалентно фильтру `Post::query().filter(Post::USER_ID.eq(user_id))`.
Для коллекции владельцев не вызывайте `fetch_by` в цикле: это создаёт N+1
запросов. Используйте пакетную выборку:

```rust
let users = database.all::<User>().await?;
let user_ids = users.iter().map(|user| user.id);
let posts = database.fetch_by_many(Post::USER_ID, user_ids).await?;
```

`fetch_by_many` выполняет один параметризованный `IN`-запрос и для пустого
набора возвращает пустой список. Затем дочерние строки нужно сгруппировать по
`post.user_id` в памяти.

Для queryset доступны typed eager-loading wrappers:

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::USER)
    .all()
    .await?;

let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all()
    .await?;
```

Они возвращают `WithOne` и `WithMany`. Serializer получает эти
материализованные значения и не имеет доступа к базе.

Несколько `belongs_to` relations можно загружать chainable API одним SQL
запросом. Каждая relation получает alias из имени FK (`author_id` -> `author`):

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::AUTHOR)
    .select_related(Post::EDITOR)
    .all()
    .await?;
```

Для двух FK в одну таблицу generated relation markers различаются, поэтому
`author` и `editor` нельзя перепутать на этапе компиляции.

После `select_related` поля joined model доступны через relation descriptor:

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::USER)
    .filter(Post::USER.related_field(User::NAME).eq("Alice"))
    .order_by(Post::USER.related_field(User::NAME).asc())
    .all()
    .await?;
```

До `select_related` такой вызов не используется: alias joined table появляется
только в materializing query.

Serializer описывает JSON-поля отдельно от ORM-модели:

```rust
#[derive(che_orm2::ModelSerializer)]
#[serializer(model = User)]
struct UserSerializer {
    #[serializer(read_only)]
    id: i64,
    email: String,
    name: String,
    #[serializer(many = Post, relation = PostUserRelation)]
    posts: Vec<PostSerializer>,
}
```

`PostUserRelation` генерируется рядом с моделью `Post`; его нужно импортировать
из модуля модели. Это намеренная compile-time привязка serializer к конкретной
foreign key relation.

`UserSerializer` с nested-полем принимает только `WithMany<User, Post>`, то
есть queryset обязан заранее вызвать `prefetch_related`. Serializer не
принимает `Database`, не выполняет запросы и не может создать N+1.
Несколько reverse relations загружаются цепочкой и сериализуются как typed
tuple:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .prefetch_related(Audit::USER.reverse())
    .all()
    .await?;

let response = UserWithPostsAndAuditsSerializer::many(users);
```

Для нескольких nested-полей используются `LoadedMany` и relation markers;
порядок `prefetch_related` должен соответствовать materialized graph.
Для множества materialized объектов используется `UserSerializer::many(...)`:

```rust
let response = UserSerializer::many(users);
```

Для nested serializer `many` принимает `WithMany`/`WithOne`, поэтому
непредзагруженная relation не может попасть в вызов случайно.
Полный runnable-пример находится в `che-orm2-examples`:

```bash
cargo run -p che-orm2-examples --bin serializers
```
