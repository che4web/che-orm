# Модели и схема

## Derive Model

`#[derive(Model)]` генерирует реализацию `Model` и константы typed fields.
Каждая модель должна иметь ровно одно поле `#[orm(primary_key)]` типа `i64`
или `che_orm::Uuid`:

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

### UUIDv7

```rust
use che_orm::{Model, Uuid};

#[derive(Debug, Model)]
#[orm(table = "uuid_users")]
struct User {
    #[orm(primary_key)]
    id: Uuid,
    name: String,
}
```

`database.create::<User>()` и `User::insert()` генерируют UUIDv7 в Rust при
сборке INSERT, если ключ не задан через `.set(User::ID, uuid)`.
`database.insert(&model)` сохраняет UUID из `model.id`. Явно переданный UUID
сохраняется независимо от версии. SQLite хранит 16-байтовый `BLOB`, PostgreSQL
DDL использует `UUID`, JSON-сериализаторы возвращают UUID-строку.

Derive задаёт `Model::PrimaryKey` по типу поля. В ручной реализации `Model`
укажите `type PrimaryKey = i64` или `Uuid`; методы `primary_key` и
`primary_key_value` используют этот тип. Режим записи сериализатора использует
тип первичного ключа модели; например, `WriteMode<Uuid>`.

## Атрибуты таблицы

### `table`

Задаёт точное имя таблицы. Обязателен, если не указан `app`:

```rust
#[orm(table = "users")]
```

### `app`

Если `table` не указан, формирует имя `<app>_<имя модели в snake_case>`:

```rust
#[derive(Debug, che_orm::Model)]
#[orm(app = "blog")]
pub struct Post {
    #[orm(primary_key)]
    pub id: i64,
}
```

Для `Post` получится `blog_post`, для `PostComment` — `blog_post_comment`.
Множественное число не добавляется. Явный
`#[orm(app = "blog", table = "legacy_posts")]` задаёт точное имя `legacy_posts`.
Префикс должен быть допустимым SQL-идентификатором.

`app` также принимает путь к `const &str`: модели и `AppConfig::name()` могут
использовать одно определение имени приложения:

```rust
pub const APP_NAME: &str = "blog";

#[derive(Debug, che_orm::Model)]
#[orm(app = APP_NAME)]
pub struct Post {
    #[orm(primary_key)]
    pub id: i64,
}
```

В дочернем модуле моделей укажите `#[orm(app = super::APP_NAME)]` или
импортируйте константу. Полный путь `crate::apps::blog::APP_NAME` тоже
поддерживается. Из `AppConfig::name()` возвращайте эту же константу.
Имя таблицы собирается и проверяется при компиляции, без выделения памяти во
время выполнения. Произвольные выражения и вызовы функций не принимаются.
Явный `table` переопределяет префикс, в том числе заданный константой.

`app` отвечает только за имя таблицы: модель по-прежнему нужно зарегистрировать
в `AppConfig::schema()`. Derive не определяет владельца из реестра или пути модуля.
Изменение `app` меняет вычисленное имя таблицы; проверяйте diff миграции.
Чтобы сохранить существующее имя, задайте его явно через `table`.

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
| `primary_key` | Первичный ключ; `i64` получает identity/rowid поведение backend-а, `Uuid` генерируется как UUIDv7 в Rust при пропущенном ключе. |
| `db_column = "old_name"` | Физическая SQL-колонка при другом Rust-имени поля. |
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
| `f64` | `REAL` | Нет |
| `Uuid` | `BLOB` (16 байт) | Нет |
| `String` | `TEXT` | Нет |
| `ImageField` | `TEXT` (относительный ключ файла) | Нет |
| `JsonField` | `TEXT` (JSON) | Нет |
| enum с `DbEnum` | `TEXT` с `CHECK` допустимых значений | Нет |
| `Vec<u8>` | `BLOB` | Нет |
| `bool` | `INTEGER` (`0`/`1`) | Нет |
| `time::OffsetDateTime` | `TEXT` UTC | Нет |
| `Option<T>` | тип `T` | Да |

`Option<T>` определяется как nullable автоматически. ORM декодирует поля через
общие `Row` и `FromDatabaseValue`. В SQLite `OffsetDateTime` хранится как RFC3339
`TEXT` и декодируется в UTC; подробнее см. [SQLite runtime](sqlite.md).
Типы PostgreSQL описаны в [руководстве backend](backends.md#типы-и-ключи).

### Rust-имя и SQL-колонка

Если существующая колонка называется `caption`, Rust-поле можно назвать `title`:

```rust
#[derive(Debug, che_orm::Model)]
#[orm(table = "articles")]
struct Article {
    #[orm(primary_key)]
    id: i64,
    #[orm(db_column = "caption")]
    title: String,
}
```

В запросах используется `Article::TITLE`, а SQL обращается к `caption`.
Переименование только Rust-поля при сохранении `db_column` не создаёт операции
изменения схемы и не переписывает данные. Для новых колонок с совпадающими именами
этот атрибут не нужен. В sync-моделях стабильное wire-имя задаётся отдельно через
`#[orm(sync_name = "...")]`.

`Option<ImageField>` хранит относительный ключ файла либо `NULL`, например
`avatars/photo.png`. Он не содержит байты изображения или HTTP-адрес. В
`#[derive(ModelSerializer)]` поле автоматически доступно только для чтения:
обычный JSON CRUD не принимает от клиента путь к файлу. Загрузка и проверка
изображения настраиваются в `che-rest::media`.

Начните с [пошагового примера ImageField](image-field.md): там показаны модель,
настройка папки, регистрация маршрутов, результат в JSON и перенос старых
ключей. `#[orm(db_column = "old_name")]` нужен только если физическая SQL-колонка
уже существует под другим именем; новым моделям он обычно не нужен.

## Timestamp fields

`auto_now_add` и `auto_now` предназначены для полей, аналогичных Django
`auto_now_add` и `auto_now`.

- оба поля получают SQLite `DEFAULT` на основе `strftime(..., 'now')`;
- managed-поля исключаются из обычного `INSERT` ORM;
- для `auto_now` ORM добавляет assignment в каждый update-запрос;
- raw SQL через `execute_sql` не обновляет timestamp автоматически;
- при чтении значение декодируется в `OffsetDateTime`.

На PostgreSQL используются `TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP` и нативное
декодирование `OffsetDateTime`. `CURRENT_TIMESTAMP` фиксируется в начале
транзакции; `auto_now` в ORM UPDATE задаётся временем Rust-процесса.

## Связи

Модели можно раскладывать по application-модулям. Приложение реализует
`AppConfig` и возвращает собственный `SchemaSet`:

```rust
pub struct AccountsApp;

impl che_orm::AppConfig for AccountsApp {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}
```

Затем приложения объединяются через `AppRegistry`:

```rust
let registry = che_orm::AppRegistry::new()
    .register::<AccountsApp>()
    .register::<ContentApp>();
```

Это разделяет ownership моделей и оставляет единое состояние схемы для
compiled migrations приложения.

Foreign key между ORM-моделями описывается через `foreign_key`.
Тип ключа должен совпадать с первичным ключом целевой модели: `i64` / `Uuid`
или их nullable-варианты `Option<i64>` / `Option<Uuid>`.
`on_delete = "set null"` используется только с nullable-ключами:

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

`fetch_by_many` выполняет параметризованные `IN`-запросы, автоматически разбивая
ключи на порции по лимиту backend. Пустой набор возвращает пустой список без
запроса; результаты объединяются без повторов по primary key. Затем дочерние
строки нужно сгруппировать по `post.user_id` в памяти.

Для queryset доступны typed eager-loading wrappers:

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::USER)
    .all(&database)
    .await?;

let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .all(&database)
    .await?;
```

`select_related` возвращает `WithOne`, а `prefetch_related` возвращает
`Loaded` с `LoadedMany`. Serializer получает эти материализованные значения и
не имеет доступа к базе.

Несколько `belongs_to` relations можно загружать chainable API одним SQL
запросом. Каждая relation получает alias из имени FK (`author_id` -> `author`):

```rust
let posts = database
    .query::<Post>()
    .select_related(Post::AUTHOR)
    .select_related(Post::EDITOR)
    .all(&database)
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
    .all(&database)
    .await?;
```

До `select_related` такой вызов не используется: alias joined table появляется
только в materializing query.

Serializer описывает JSON-поля отдельно от ORM-модели:

```rust
#[derive(che_orm::ModelSerializer)]
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

`UserSerializer` с nested-полем `many` принимает результат
`prefetch_related`: `Loaded<User, (LoadedMany<Post, PostUserRelation>,)>`.
Queryset обязан заранее вызвать `prefetch_related`. Serializer не принимает
`Database`, не выполняет запросы и не может создать N+1.
Несколько reverse relations загружаются цепочкой и сериализуются как typed
tuple:

```rust
let users = database
    .query::<User>()
    .prefetch_related(Post::USER.reverse())
    .prefetch_related(Audit::USER.reverse())
    .all(&database)
    .await?;

let response = UserWithPostsAndAuditsSerializer::many(users);
```

Для нескольких nested-полей используются `LoadedMany` и relation markers;
порядок `prefetch_related` должен соответствовать materialized graph.
Для множества materialized объектов используется `UserSerializer::many(...)`:

```rust
let response = UserSerializer::many(users);
```

Для nested serializer `many` принимает `Loaded` с соответствующим `LoadedMany`,
поэтому непредзагруженная relation не может попасть в вызов случайно.
Полный runnable-пример находится в `che-orm-examples`:

```bash
cargo run -p che-orm-examples --bin serializers
```
