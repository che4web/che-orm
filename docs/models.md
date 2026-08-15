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

Foreign key описывается через `references`:

```rust
#[derive(Model)]
#[orm(table = "posts")]
struct Post {
    #[orm(primary_key)]
    id: i64,
    #[orm(references = "users(id)", on_delete = "cascade")]
    user_id: i64,
    title: String,
}
```

`references = "users(id)"` добавляет foreign key в DDL, а
`on_delete = "cascade"` добавляет каскадное удаление. При работе через
`Database` SQLite `PRAGMA foreign_keys = ON` включается на каждом соединении
пула.

Отношение загружается через typed field дочерней модели:

```rust
let posts = database.fetch_by(Post::USER_ID, user_id).await?;
```

Это эквивалентно фильтру `Post::query().filter(Post::USER_ID.eq(user_id))`.
Автоматических relation objects и eager loading пока нет.
