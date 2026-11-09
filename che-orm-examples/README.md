# ORM API examples

Run these commands from the che-orm repository root. The examples use the async
che-orm 0.3.2 API and a database selected by `CHE_ORM_DATABASE_URL` (default:
`app.db`). Apply the frozen compiled migration before running database examples:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
cargo run -p che-orm-examples --bin schema
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

- `sqlite_crud` creates users and a post, edits a materialized user using
  `database.save(&user).await?`, and shows that saving a deleted user returns
  `None` rather than inserting a row.
- `transactions` uses `tx.save(&user).await?` and verifies both committed and
  rolled-back writes.
- `serializers` loads relations before serializing them; serializers do not query
  the database.
- `playground` prints compiled SQL without opening a database.

`Database::save` and `Transaction::save` update all ordinary fields of an existing
row. They preserve its primary key and `auto_now_add`, refresh `auto_now`, and
return `Option<Model>` without mutating the supplied object. Keep the returned
model when continuing to edit it. Use `update().set(...)` for a selected-field
update when the rest of a materialized object may be stale.

The synchronization examples use the same API through their configured handles;
those handles also capture outgoing work transactionally:

```bash
cargo run -p che-sync --example model_log
cargo run -p che-sync --example offline
```

For PostgreSQL, select a disposable database with `CHE_ORM_DATABASE_URL` and add
`--no-default-features --features postgres` to the ORM example commands. The
`offline` sync example accepts `CHE_SYNC_DATABASE_URL` and the same backend flags;
`model_log` uses in-memory SQLite.
