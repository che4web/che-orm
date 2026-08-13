# Модели и поля

Модель должна быть struct с именованными полями, `Debug`, `Clone` и ровно одним
`#[field(primary_key)]`. Для автоматически генерируемого идентификатора
используйте `id: i64`: такой primary key получает `auto` по умолчанию.

```rust
use che_orm::{Choice, FilePath, Model, NaiveDateTime};
use serde_json::Value;

#[derive(Debug, Clone, Choice)]
enum Status { Draft, Published }

#[derive(Debug, Clone, Model)]
#[model(table = "posts")]
struct Post {
    #[field(primary_key)]
    id: i64,
    #[field(unique, max_length = 255)]
    slug: String,
    #[field(default = false)]
    featured: bool,
    status: Status,
    metadata: Option<Value>,
    attachment: Option<FilePath>,
    #[field(auto_now_add)]
    created_at: NaiveDateTime,
    #[field(auto_now)]
    updated_at: NaiveDateTime,
}
```

## Сгенерированный API

`#[derive(Model)]` реализует `Model`, добавляет метаданные схемы и генерирует
`PostFields`. Константы вроде `PostFields::SLUG` типизированы и используются в
`create`, `update` и `query`. Для foreign key также генерируется `PostRelations`.

`#[derive(Choice)]` работает только с enum из unit-вариантов. В базу сохраняется
snake_case значение варианта: `Status::Draft` становится `"draft"`.

## Поддерживаемые типы

- `i64`, `i32`, `u32`
- `String`
- `bool`
- `f64`, `f32`
- `chrono::NaiveDateTime`
- `serde_json::Value`
- `FilePath`
- `Option<T>` от перечисленных типов
- enum с `#[derive(Choice)]`

JSON хранится как `TEXT` в SQLite и `JSONB` в PostgreSQL. `FilePath` хранит
проверенный относительный путь, а не содержимое файла.

## Атрибуты полей

| Атрибут | Назначение |
| --- | --- |
| `primary_key` | Единственный primary key модели. |
| `auto` | Значение генерирует база данных. |
| `auto_now_add` | `NaiveDateTime`, выставляется при вставке. |
| `auto_now` | `NaiveDateTime`, выставляется при вставке и обновлении. |
| `unique` | Добавляет ограничение уникальности. |
| `index` | Добавляет моделируемый индекс. |
| `max_length = 255` | Метаданные максимальной длины строки. |
| `default = ...` | Значение по умолчанию для схемы. |
| `rename = "db_column"` | Имя столбца в базе. |
| `foreign_key = Other` | Внешний ключ на `Other.id`, только `i64` или `Option<i64>`. |
| `on_delete = Cascade` | Действие FK: `NoAction`, `Restrict`, `Cascade`, `SetNull`, `SetDefault`. |

`SetNull` требует `Option<i64>`, а `SetDefault` требует `default`. Связи сейчас
доступны только для SQLite.

## Файловое хранилище

`FilePath::new` принимает только непустые относительные пути без `..`, `.`,
обратных слешей и абсолютных путей. `LocalFileStorage` сохраняет файлы ниже
переданного root directory; операции `store` и `delete` меняют файловую систему.
