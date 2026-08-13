# Backend-ы и feature-флаги

Нужно включить ровно один backend. Значение по умолчанию: `sqlite` вместе с
`migration-native`.

```toml
# SQLite, значения по умолчанию
che-orm = "0.1"

# PostgreSQL
che-orm = { version = "0.1", default-features = false, features = ["postgres"] }
```

Одна программа не может переключаться между SQLite и PostgreSQL во время
выполнения. Каждый feature подключает только соответствующий SQLx driver.

| API | SQLite | PostgreSQL |
| --- | --- | --- |
| `Database`, модели, `FilePath`, CRUD | Да | Да |
| Фильтры, `Q`, ordering, limit/offset, `count` | Да | Да |
| Ручные миграции SQLx | Да | Да |
| `create_table` и native schema diff | Да | Нет |
| Foreign-key metadata and DDL | Да | Да |
| Relation loading API, signals | Да | Нет |
| Projections, grouping, annotations, numeric aggregates | Да | Нет |

## Создание миграций

`migration-native` работает только с SQLite и включён по умолчанию.
`migration-atlas` может создавать миграции для обоих backend-ов через Atlas.
Это experimental integration: проверяйте generated SQL и Atlas environment
отдельно перед production use.
Оба feature одновременно включать нельзя. Без feature создания миграций можно
применять вручную написанные SQLx migration files.

PostgreSQL migration directories должны быть отделены от SQLite. См.
[migrations.md](migrations.md) для процесса и правил безопасности.

## Ограничения

- Нет автоматического определения переименований таблиц и столбцов.
- Нет rollback migrations.
- Изменение типа требует явной data migration.
- Schema snapshots не имеют гарантии обратной совместимости между изменениями
  формата ORM.
- Применяйте миграции к одной базе из одного процесса за раз.
