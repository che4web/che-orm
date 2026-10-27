# Backend, выбираемый при компиляции

Включается ровно один backend. SQLite выбран по умолчанию:

```toml
che-orm = "0.3.2"
```

Для PostgreSQL:

```toml
che-orm = { version = "0.3.2", default-features = false, features = ["postgres"] }
```

Оба features одновременно или отсутствие обоих — ошибка компиляции. Типы
`Database`, `Transaction`, CRUD-builders, модели и сериализаторы одинаковые.
PostgreSQL-сборка не зависит от `rusqlite`. Требуется Rust 1.88 или новее.

## Подключение

Приложение вызывает `Database::connect(settings.database_url()).await?`.
Для SQLite настройка содержит путь к файлу или `sqlite://` путь. Для PostgreSQL —
URL или строку подключения драйвера. Пул по умолчанию содержит четыре соединения;
`connect_with_pool_size(url, size).await?` задаёт размер явно. Ожидание соединения
ограничено 30 секундами. `connect` проверяет доступ, получая соединение из пула.

SQLite `:memory:` требует одного соединения; `connect` выбирает этот размер
автоматически. PostgreSQL использует native TLS с проверкой сертификата через
системное хранилище. Для обязательного TLS задайте `sslmode=require`; для
локального тестового сервера допустим `sslmode=disable`. URL также принимает
опции драйвера, например `connect_timeout=10`.

## Транзакции

```rust
database.transaction(|tx| Box::pin(async move {
    let user = tx.create::<User>().set(User::NAME, "Alice").execute().await?;
    tx.update::<User>(user.id).set(User::NAME, "Updated").execute().await?;
    Ok(user.id)
})).await?;
```

Все операции используют одно соединение. Ошибка вызывает rollback, успех —
commit. При отмене незавершённая транзакция откатывается до возврата соединения
в пул. Внутри callback используйте `tx`, а не внешний `Database`.

Перехваченная SQL-ошибка PostgreSQL может оставить транзакцию в aborted-состоянии.
ORM проверяет это перед commit и возвращает ошибку вместо ложного успеха после
фактического rollback.

По умолчанию используется serializable. `transaction_with_isolation` принимает
`ReadCommitted`, `RepeatableRead` или `Serializable`. PostgreSQL применяет
указанный уровень; SQLite во всех случаях использует serializable snapshot.
Ошибки сериализации PostgreSQL возвращаются вызывающему коду.

`OrmError::constraint_kind()` классифицирует ограничения одинаково для обеих
БД. Для PostgreSQL доступны `sql_state()` и исходная ошибка драйвера.

## Типы и ключи

Обе БД поддерживают `i64`, `f64`, строки, бинарные данные, bool, timestamps,
UUID, `DbEnum`, `ImageField` и nullable-варианты. Пропущенный UUID-ключ в
INSERT-builder генерируется как UUIDv7 в Rust. PostgreSQL хранит нативный UUID,
SQLite — 16-байтовый BLOB. Целочисленные ключи используют rowid / identity.
Явный импорт целочисленных ключей следует правилам sequence выбранной БД.

Для переносимых ключей используйте модель из `create` / `update` с `RETURNING`.
`ExecuteResult::last_insert_rowid` — SQLite-метаданные; PostgreSQL возвращает
`None`. Timestamps PostgreSQL декодируются в UTC с точностью до микросекунд.
PostgreSQL default `CURRENT_TIMESTAMP`
использует время начала транзакции, ORM `auto_now` — часы Rust-процесса.

## Миграции

`database.migrate(graph).await?` применяет compiled migrations.
`migrate_to(graph, Some(target)).await?` ограничивает применение forward-цепочкой
зависимостей цели. `migration_history().await?` читает историю и checksums.

Граф и состояние общие. SQLite использует rebuild таблиц; PostgreSQL — нативный
ALTER и детерминированные имена ограничений/индексов длиной до 63 байт.
PostgreSQL синхронизирует параллельные запуски advisory lock. Каждая миграция
и запись истории фиксируются атомарно. Ошибка или несовпадение checksum
останавливает применение. FK к таблицам, созданным позже в той же миграции,
хранятся как структурированные отложенные FK. Они добавляются, когда целевой
ключ становится доступен, следуют за rename и удаляются из очереди при удалении
исходной колонки/таблицы или самой ссылки. При смене типа зависимые ограничения
и defaults снимаются до преобразования, затем создаются новые определения.

`RunSql`, defaults, checks и SQL schema objects зависят от backend. Сложные
преобразования типов с `USING` и перенос значений задаются явным SQL приложения
с соответствующими state operations. В SQL миграции запрещены команды управления
транзакцией. Сохраняйте прежние identities и checksums.

Пример приложения содержит отдельные frozen baselines SQLite и PostgreSQL.
Подключение задаётся через `CHE_ORM_DATABASE_URL`:

```bash
cargo run -p che-orm-examples --no-default-features --features postgres --bin manage -- migrate
cargo run -p che-orm-examples --no-default-features --features postgres --bin sqlite_crud
cargo run -p che-orm-examples --no-default-features --features postgres --bin transactions
```

Бинарник `sqlite_crud` сохранил историческое имя, но использует общий API.

## Переход с 0.2

- Добавьте `.await` к методам подключения.
- Callback транзакции возвращает `Box::pin(async move { ... })`; операции
  внутри также требуют `.await`.
- Старые SQLite callback с `rusqlite::Connection` перенесите в `raw_transaction`.
- Ручной `Model` декодирует `&che_orm::Row` и возвращает `DecodeError`;
  пользовательские поля реализуют `FromDatabaseValue`. Derive делает это сам.
- Ручной `Model` также реализует `primary_key_value` для пакетных запросов и
  группировки связей; реализации по умолчанию с panic больше нет.
- Для прямого SQLite `query_row` есть `Model::from_sqlite_row`.
- Core 0.3.1 требует макросы 0.3.1 для timestamp metadata. SQLite pool accessor убран из общего фасада.

## Проверка

```bash
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
TEST_POSTGRES_URL='postgresql://postgres:postgres@localhost/che_orm_test' \
  cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
```

Без сервера runtime-тесты PostgreSQL отмечены ignored. Явный запуск требует
настоящего сервера и роли с правом создавать тестовые схемы. Проверяются те же
прикладные сценарии, что для SQLite, включая типы, CRUD, связи, отмену транзакции,
параллельные миграции и rollback.
