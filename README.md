# che-orm

**Русский** | [English](README.en.md)

Экспериментальная типизированная ORM для Rust с единым async API для SQLite
и PostgreSQL. Backend выбирается при компиляции. Требуется **Rust 1.88+**.

Текущая опубликованная версия `che-orm` — **0.3.3**. Проект находится
в разработке; перед обновлением см. [CHANGELOG](CHANGELOG.md).

Этот checkout готовит **0.4.0** (ещё не опубликована): bound-query API и формат
SQLite datetime изменились несовместимо. До публикации новых ORM/Sync/REST
используйте согласованные локальные зависимости. См. [план выпуска](docs/releasing.md).

## Возможности

- **Типизированные модели и запросы.** `#[derive(Model)]` создаёт метаданные
  и поля для фильтрации и записи. CRUD строится из Rust-выражений;
  значения передаются в SQL как параметры.
- **Один async API для SQLite и PostgreSQL.** Модели, CRUD и транзакции
  используют общий интерфейс; backend выбирается feature-флагом при сборке.
- **Миграции как часть приложения.** Изменения схемы хранятся в компилируемых
  миграциях. Шаблон включает CLI для их генерации и применения, а модели
  группируются в модули приложения через `AppConfig` / `AppRegistry`.
- **Явная загрузка связей и сериализация.** `select_related` и
  `prefetch_related` загружают связанные данные до сериализации.
  `ModelSerializer` работает с готовыми данными и не обращается к базе.
- **Типы для прикладных данных.** Целочисленные и UUIDv7-ключи, необязательные поля,
  перечисления с `CHECK`, JSON, временные метки и ключи изображений `ImageField`.

**Офлайн-синхронизация — отдельное расширение.** `che-sync` добавляет
синхронизацию UUID-моделей между клиентом и сервером, не привязывая ORM
к HTTP или Tauri. Подробнее — [ниже](#синхронизация-и-интеграции).

## Модель и использование в приложении

Добавьте зависимости в `Cargo.toml`. SQLite включён по умолчанию:

```toml
[dependencies]
che-orm = "0.4.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### Опишите модель

В `src/main.rs` объявите обычную Rust-структуру. `Model` создаст метаданные
таблицы и типизированные поля `User::EMAIL`, `User::NAME` и `User::ID`:

```rust
use che_orm::Model;

#[derive(Debug, Model)]
#[orm(table = "users")]
struct User {
    #[orm(primary_key)]
    id: i64,
    #[orm(unique)]
    email: String,
    name: String,
}
```

### Используйте модель

В том же файле добавьте подключение и операции с данными. Этот пример
запускается на временной SQLite-базе без дополнительной настройки.
`create_table` здесь нужен только для демонстрации; для постоянной схемы
приложения используйте [миграции](docs/migrations.md).

```rust
use che_orm::Database;

#[tokio::main]
async fn main() -> Result<(), che_orm::OrmError> {
    let database = Database::connect_in_memory().await?;
    database.create_table::<User>().await?;

    let mut user = database
        .create::<User>()
        .set(User::EMAIL, "alice@example.test")
        .set(User::NAME, "Alice")
        .execute()
        .await?;

    let users = database
        .query::<User>()
        .filter(User::NAME.eq("Alice"))
        .all()
        .await?;
    println!("{users:?}");

    user.name = "Alice Smith".into();
    database.save(&user).await?;

    database.delete::<User>(user.id).await?;
    Ok(())
}
```

`database.save(&user)` сохраняет все обычные поля объекта по первичному ключу,
а не только изменённые. Метод возвращает `Option<User>`: обновлённую запись
или `None`, если строка уже удалена. Исходный объект не изменяется;
для создания новой записи используйте `create`.

Вместо изменения объекта и `save` можно обновить только выбранные поля:

```rust
let updated = database
    .update::<User>(user.id)
    .set(User::NAME, "Alice Cooper")
    .execute()
    .await?;
```

### Транзакции

Операции внутри callback выполняются через `tx`, на одном соединении.
Успешный результат фиксирует изменения; ошибка откатывает транзакцию.
Например, обновление существующего пользователя:

```rust
database
    .transaction(|tx| Box::pin(async move {
        tx.update::<User>(user.id)
            .set(User::NAME, "Alice Cooper")
            .execute()
            .await?;
        Ok(())
    }))
    .await?;
```

### PostgreSQL

Модели и CRUD-код остаются прежними. Замените зависимость в `Cargo.toml`:

```toml
che-orm = { version = "0.4.0", default-features = false, features = ["postgres"] }
```

Создайте базу PostgreSQL и задайте URL со своими реквизитами:

```bash
export CHE_ORM_DATABASE_URL='postgresql://user:password@localhost:5432/my_app'
```

В `main` замените `connect_in_memory` подключением по URL:

```rust
let url = std::env::var("CHE_ORM_DATABASE_URL")
    .expect("CHE_ORM_DATABASE_URL must be set");
let database = Database::connect(&url).await?;
```

При сборке должен быть включён ровно один флаг: `sqlite` или `postgres`.
`default-features = false` отключает SQLite по умолчанию. Приложение само
читает URL; `connect` открывает пул, но не создаёт базу.
Подробнее о TLS и особенностях backend — [в руководстве](docs/backends.md).

## Готовое приложение из шаблона

Вместо ручного подключения можно сгенерировать проект. Шаблон предлагает
SQLite или PostgreSQL, структуру `single` или `modular`, стартовую модель
и CLI миграций:

```bash
cargo install cargo-generate --locked
cargo generate --git https://github.com/che4web/che-orm templates/che-orm --name my-app
cd my-app
cargo run --bin manage -- makemigrations --name initial
cargo run --bin manage -- migrate
cargo run
```

Для PostgreSQL перед миграцией создайте базу и задайте `CHE_ORM_DATABASE_URL`.
Новый модуль приложения: `cargo run --bin manage -- createapp blog`.
Подробнее — [руководство по шаблону](docs/template.md).

## Миграции

Изменение Rust-модели само по себе не меняет существующую базу.
Схема развивается через **компилируемые миграции** — Rust-модули с операциями,
которые хранятся в проекте и компилируются вместе с приложением.

В шаблоне уже есть CLI `manage`. Он сравнивает зарегистрированные модели
с состоянием предыдущих миграций, **а не с живой базой**. После создания
начальной миграции каждый следующий цикл выглядит так:

```bash
cargo run --bin manage -- makemigrations --name update_models
# Проверьте сгенерированные операции перед применением.
cargo run --bin manage -- migrate
cargo run --bin manage -- migrate status
```

Добавляйте файлы из `src/migrations/` в Git. Переименования и заполнение
существующих данных могут требовать ручной правки операций. Применённые
миграции не редактируйте: БД хранит их идентификаторы и контрольные суммы.
На деплое `migrate` запускается отдельным шагом, до старта приложения.

Для просмотра изменений без записи файлов и проверки в CI:

```bash
cargo run --bin manage -- makemigrations --dry-run
cargo run --bin manage -- makemigrations --check
```

Генерация не требует подключения к БД; `--check` завершается с ошибкой,
если модели требуют новой миграции. `manage` — часть приложения из шаблона,
а не CLI, устанавливаемый зависимостью `che-orm`.
Свой реестр, API применения и особенности backend описаны
в [руководстве по миграциям](docs/migrations.md).

## Синхронизация и интеграции

`che-sync` позволяет работать с UUID-моделями локально без сети, а затем
синхронизировать изменения с сервером. Поддерживает повторную отправку
ожидающих запросов и слияние полей. Работает с SQLite и PostgreSQL.

- [Синхронизация моделей](docs/model-sync.md) — доступ ко всей модели
  (протокол 2).
- [Общие объекты](https://github.com/che4web/che-rest/blob/main/docs/object-sync.md)
  — права на отдельные объекты (протокол 3).
- [API совместимости](docs/sync.md) — прежний scoped-протокол со снимками данных.

`che-orm` отвечает за данные и транзакции, `che-sync` — за синхронизацию.
HTTP и REST-маппинг принадлежат `che-rest`, Tauri-команды — `che-tauri`.
См. [Todo-сервер и desktop-клиент](https://github.com/che4web/che-tauri/tree/main/examples/todo-sync)
для примера работы двух устройств.

## Документация

- [Результаты бенчмарков и условия измерений](benchmarks/README.md)

- [Пошаговое руководство](docs/tutorial.md)
- [Модели, поля, связи и сериализаторы](docs/models.md)
- [Подключения, CRUD и транзакции](docs/sqlite.md)
- [Миграции приложения](docs/migrations.md)
- [ImageField: модель, загрузка и хранение файлов](docs/image-field.md)
- [Примеры API, проверяемые компиляцией (English)](docs/en/api-examples.md)
- [Английские руководства](docs/en/README.md)
- [Список изменений](CHANGELOG.md), [участие в разработке](CONTRIBUTING.md),
  [безопасность](SECURITY.md)

## Примеры и проверка

Из корня репозитория:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

Приложение-пример содержит замороженные миграции и не поддерживает
`makemigrations`. Для своего приложения используйте шаблон выше.

```bash
cargo fmt --check
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo doc --workspace --no-deps
```

Runtime-тестам PostgreSQL нужен работающий сервер и `TEST_POSTGRES_URL`;
они запускаются отдельно с `--test runtime -- --ignored`.
Полные проверки публикации описаны в [подготовке релиза](docs/releasing.md).
