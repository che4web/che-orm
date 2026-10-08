# Compile-time Backends

Enable exactly one backend. SQLite is enabled by default:

```toml
che-orm = "0.3"
```

For PostgreSQL, disable defaults:

```toml
che-orm = { version = "0.3", default-features = false, features = ["postgres"] }
```

Enabling both features, or neither, is a compilation error. There is one
`Database`, `Transaction`, and CRUD/query facade; models and serializers do not
have backend parameters. The PostgreSQL build does not depend on `rusqlite`.
Rust 1.88 or later is required.

## Connections

Use `Database::connect(settings.database_url()).await?` for either backend.
SQLite accepts a file path or `sqlite://` path. `:memory:` requires a one-connection
pool, selected automatically by `connect`. PostgreSQL accepts a PostgreSQL URL
or the driver's connection string. The default pool size is four; use
`connect_with_pool_size(url, size).await?` to override it. Acquisition times out
after 30 seconds, and connecting checks out a connection to validate access.

PostgreSQL uses native TLS with platform certificate validation. Use
`sslmode=require` when TLS is required, or `sslmode=disable` for a local test
server. PostgreSQL URLs can set driver options such as `connect_timeout=10`.

## Transactions

```rust
database.transaction(|tx| Box::pin(async move {
    let user = tx.create::<User>().set(User::NAME, "Alice").execute().await?;
    tx.update::<User>(user.id).set(User::NAME, "Updated").execute().await?;
    Ok(user.id)
})).await?;
```

Every operation uses the same connection. Errors roll back, success commits,
and cancellation rolls back unfinished transactions before recycling the
connection. Do not use the outer `Database` for operations inside the callback.

If a callback catches a PostgreSQL statement error, the transaction may still
be aborted. The ORM checks this before committing and returns an error rather
than reporting success for a transaction PostgreSQL rolled back.

The default isolation level is serializable. `transaction_with_isolation`
accepts `ReadCommitted`, `RepeatableRead`, or `Serializable`. PostgreSQL applies
the requested level; SQLite uses its stronger serializable snapshot for all
three. PostgreSQL serialization conflicts are returned to the caller.

For portable constraint handling, use `OrmError::constraint_kind()`. PostgreSQL
errors also expose `sql_state()` and retain the native driver error.

## Types and Keys

`i64`, `f64`, text, binary, booleans, timestamps, UUID, `DbEnum`, `ImageField`
and their optional variants work on both backends. UUIDv7 is generated in Rust
when an INSERT builder omits the UUID key. PostgreSQL stores UUID natively;
SQLite stores its 16 bytes. Integer keys use SQLite rowids or PostgreSQL identity.
Explicit integer imports follow the database's identity/sequence semantics.

Use models returned by `create` / `update` for generated keys and values.
`ExecuteResult::last_insert_rowid` is SQLite-only metadata and is `None` on
PostgreSQL. `TIMESTAMPTZ` values are decoded in UTC with microsecond precision. PostgreSQL timestamp defaults
use transaction time; ORM `auto_now` assignments use the Rust process clock.

## Migrations

```rust
let graph = che_orm::MigrationGraph::new(application_migrations())?;
let applied = database.migrate(graph).await?;
```

`migrate_to(graph, Some(target)).await?` applies the target's forward dependency
closure. `migration_history().await?` reads the recorded identities and checksums.
The migration graph/state are shared. SQLite uses table rebuilds; PostgreSQL
uses native ALTER statements and deterministic, length-bounded object names.
PostgreSQL serializes migration runners with a transaction-scoped advisory lock.
Each migration and its history entry commit atomically. Errors and divergent
checksums stop execution. Forward references among tables created in the same
migration are represented by structured deferred foreign keys. They are applied
as soon as the referenced key is available, follow renames, and are discarded
when their source column/table or reference is removed. Column type changes
drop affected constraints and defaults before conversion, then recreate the
new definitions.

`RunSql`, defaults, checks and schema-object SQL are reviewed, backend-specific
SQL. Type conversions that need `USING` or data remapping must use explicit
application migration SQL with matching state operations. Migration SQL must
not contain transaction-control commands. Keep previous identities/checksums.

The sample keeps separate frozen SQLite and PostgreSQL baselines. Configure it
with `CHE_ORM_DATABASE_URL`, then run:

```bash
cargo run -p che-orm-examples --no-default-features --features postgres --bin manage -- migrate
cargo run -p che-orm-examples --no-default-features --features postgres --bin sqlite_crud
cargo run -p che-orm-examples --no-default-features --features postgres --bin transactions
```

The `sqlite_crud` binary retains its historical name and uses the shared API.

## Upgrade from 0.2

- Await `connect`, `connect_with_pool_size`, and `connect_in_memory`.
- Transaction callbacks return `Box::pin(async move { ... })`; await methods.
- Use SQLite-only `raw_transaction` for old direct-driver callbacks.
- Hand-written models decode `&che_orm::Row` and return `DecodeError`, using
  `FromDatabaseValue` for custom field types. Derives generate this automatically.
- Hand-written models must implement `primary_key_value` for batched queries
  and relation grouping; there is no panic-producing default implementation.
- Direct SQLite `query_row` callers can use `Model::from_sqlite_row`.
- Use `che-orm-macros` 0.3.0 with core 0.3.0; the old SQLite pool accessor was
  removed from the common facade.

## Verification

```bash
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
TEST_POSTGRES_URL='postgresql://postgres:postgres@localhost/che_orm_test' \
  cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
```

The server-free PostgreSQL suite intentionally ignores runtime tests. The
explicit runtime command requires a live server and a role that can create
schemas. It tests the same application scenarios as SQLite, including all field
types, RETURNING, relations, cancellation, concurrent migrations and rollback.
