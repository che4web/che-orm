# Связи и сигналы

Relation loading API и сигналы доступны только с feature `sqlite`. Foreign-key
metadata и соответствующий DDL поддерживаются обоими backend-ами.

## Внешние ключи

Поле `#[field(foreign_key = Author)] author_id: i64` хранит id связанной модели
и создаёт `REFERENCES authors(id)` в SQLite или PostgreSQL. Сначала создайте таблицу цели, затем
таблицу со внешним ключом. SQLite backend включает `PRAGMA foreign_keys = ON`.

```rust
let author = PostRelations::AUTHOR
    .get(db.as_sqlite(), post.author_id)
    .await?;
let posts = PostRelations::AUTHOR
    .reverse()
    .query(db.as_sqlite(), author_id)
    .all()
    .await?;
```

Для batched eager loading доступны `select_related` для прямой связи и
`prefetch_related` для обратной. Они выполняют дополнительный пакетный запрос,
а не SQL JOIN.

## Сигналы

`db.as_sqlite().signals().subscribe::<User>()` возвращает Tokio broadcast
receiver с `PostSave` и `PostUpdate`. Событие содержит имя таблицы и JSON-снимок
модели. `PostSave.created` отличает insert от update.

Успешное создание отправляет `PostSave { created: true }`. Успешное обновление
отправляет `PostSave { created: false }`, затем `PostUpdate`.

Доставка best-effort и at-most-once. Очередь каждого подписчика ограничена 1024
событиями; при отставании `recv()` возвращает `RecvError::Lagged`. CRUD не ждёт
подписчиков. Raw SQL и изменения вне ORM не отправляют сигналы.
