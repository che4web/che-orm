# AGENTS.md

## Project Overview

`che-orm` is an experimental Rust ORM workspace.

Workspace members:

- `che-orm`: core ORM library, compile-time SQLite/PostgreSQL runtimes, SQL AST, and schema compiler.
- `che-orm-macros`: procedural macros `#[derive(Model)]`, `#[derive(DbEnum)]`
  and `#[derive(ModelSerializer)]`.
- `che-orm-examples`: downstream examples, sample application models, and the `manage` CLI.
- `che-sync-protocol`: backend-independent synchronization messages and tagged values.
- `che-sync`: native current/shadow synchronization engine, migrations, transport trait and offline example.

## Architecture

- `src/types.rs`: database values, expressions, typed model fields.
- `src/image.rs`: `ImageField`, a TEXT-backed relative file key; filesystem/HTTP handling belongs in `che-rest::media`.
- `src/query.rs`: query AST, model trait, SELECT/INSERT/UPDATE/DELETE builders.
- `src/schema.rs`: column metadata, `SchemaSet`, `AppConfig`, and `AppRegistry`.
- `src/sql.rs`: SQLite/PostgreSQL dialects and SQL compilation.
- `src/connection.rs`: shared async Database, CRUD, transactions and relation loading.
- `src/sync.rs`: shared `SyncModel` metadata, registration roles and infrastructure migrations.
- `src/backend/`: the feature-selected driver, pool, binding and row adapters.
- `src/row.rs`: backend-neutral Row, DecodeError and FromDatabaseValue.
- `src/migration/postgres_schema.rs`: native PostgreSQL schema operations.
- `src/migration/postgres_executor.rs`: PostgreSQL migration locking and execution.
- `che-orm-macros/src/lib.rs`: `Model` derive implementation.
- `che-orm-examples/src/lib.rs`: sample application models, registry, and database path.
- `che-orm-examples/src/bin/manage.rs`: compiled-migration CLI for the sample application.
- `che-orm-examples/migrations/`: the sample's frozen compiled baseline SQL.
- `che-sync/src/runtime.rs`: configured client ORM capture and role-aware read filters.
- `che-sync/src/server.rs`: server capture, access policy, journal and operation receipts.
- `che-sync/src/engine.rs`: independent pull/flush/full-load and compatibility sync cycle.
- `che-sync/src/model_client.rs`: protocol-2 `ModelLogClient`, bounded model-log engine and opaque transport payload persistence.
- `che-sync/src/model_log.rs`: protocol-2 journal/state application and model-log capture.
- `docs/model-sync.md`: current per-model protocol, layer boundaries and downstream Tauri integration; `docs/sync.md` documents the scoped compatibility API.
- `docs/releasing.md` and `.github/workflows/release-check.yml`: release procedure and checks.

## Application Modules

Keep application models, registry, settings, binaries, and migrations outside
the core crate. The sample application uses `che-orm-examples`.

Each application should expose an `App` type implementing `AppConfig`:

```rust
pub struct App;

impl che_orm::AppConfig for App {
    fn name() -> &'static str { "accounts" }

    fn schema() -> che_orm::SchemaSet {
        che_orm::SchemaSet::new().model::<User>()
    }
}
```

Register apps with `AppRegistry`. Register parent tables before applications
that define foreign keys to them.

## Model Rules

- Enable exactly one backend: `sqlite` (default) or `postgres` with defaults
  disabled. Upper-level application code uses the same Database/Transaction API.
- Connection methods are async. Use `Database::connect(url).await?` and async
  transaction callbacks `|tx| Box::pin(async move { ... })` with awaited methods.
- Models decode the common `Row`; custom scalar types implement
  `FromDatabaseValue`. Keep driver types in backend-specific adapters.
- Use `#[derive(Debug, Model)]` for application models.
- Always specify `#[orm(table = "...")]`.
- Mark generated integer keys with `#[orm(primary_key)]`.
- UUID keys use `#[orm(primary_key)] id: che_orm::Uuid`. INSERT builders generate
  UUIDv7 in Rust when the key is omitted; explicitly supplied keys are preserved.
  SQLite stores UUIDs as 16-byte BLOBs, PostgreSQL compiles them as UUID, and JSON
  uses UUID strings. `Database::insert(&model)` uses the model's supplied UUID.
- Use `#[orm(db_column = "existing_name")]` when a Rust field must use a
  different name from its physical SQL column. Queries, indexes and generated
  schema use the physical name while Rust code keeps the field-based constant.
  Historical migrations retain both names; changing only the Rust name creates
  no schema operation when `db_column` remains unchanged.
- Use `#[orm(foreign_key = User, on_delete = "cascade")]` for foreign keys to
  ORM models. This generates `Post::USER` and `Post::USER.reverse()`.
- Use `#[orm(references = "table(column)")]` only for tables without an ORM
  model; it does not generate typed relations.
- `foreign_key` supports `i64`, `Uuid`, `Option<i64>` and `Option<Uuid>`; its
  underlying type must match the target model's primary key. Use
  `on_delete = "set null"` only with optional keys.
- Use `#[orm(auto_now_add)]` and `#[orm(auto_now)]` only with `OffsetDateTime`.
- `auto_now` is updated by ORM-generated UPDATE statements. Raw SQL does not update it.
- `Option<T>` maps to a nullable SQL column.
- `JsonField(Value)` stores JSON as SQL TEXT and serializes as a JSON value.
  It supports typed writes and decoding; application validation owns JSON shape.
- `ImageField` stores a relative key in SQLite `TEXT`, not bytes or an HTTP URL. `Option<ImageField>` is nullable. `src/image.rs` implements `ColumnTypeOf`, typed query binding, SQLite read/write and string JSON serialization; do not add filesystem operations to the ORM.
- `ModelSerializer` makes `ImageField` and `Option<ImageField>` read-only in JSON writes even without an explicit annotation. Server code sets them after a validated upload; do not accept client-supplied storage keys through CRUD.
- Keep the user-facing walkthrough in `docs/image-field.md` and `docs/en/image-field.md` aligned with the actual `che-rest::media` APIs. Ordinary models need just `Option<ImageField>`; show `#[orm(db_column = "...")]` only for an existing SQL column with a different name. `ImageField::new` wraps a trusted key but does not validate or store a file.
- Avoid declaring both field-level `#[orm(unique)]` and a duplicate table-level
  `unique("field")` constraint.

`#[derive(ModelSerializer)]` generates a JSON serializer for materialized model
data. It must not access `Database`. Use queryset `select_related` for
`belongs_to` and `prefetch_related` for reverse relations before converting a
result to a serializer. Nested fields use `#[serializer(many = Post, relation =
PostUserRelation)]` or `#[serializer(one = User, relation = PostUserRelation)]` and accept
only the matching `WithMany`/`WithOne`/`LoadedMany` relation marker. Optional
relations use `WithOptionalOne`.
Scalar writable foreign-key serializer fields use `#[serializer(foreign_key = User, relation = TaskAssigneeRelation)]`; this exposes the FK id as write metadata without requiring a nested queryset relation.

## Migrations

Application-owned compiled migration operations are the source of persistent schema
changes. Do not use `Database::create_table` for production deployment; it is
intended for tests and local setup.

The sample application configuration lives in `che-orm-examples/src/lib.rs`:

```rust
pub const DATABASE_PATH: &str = "app.db";
```

Use the application CLI:

```bash
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin manage -- migrate status
```

The example rejects `makemigrations`: its `0001_initial` history is frozen.
`migrate` and `migrate status` use its `database_url()` (`CHE_ORM_DATABASE_URL`,
or the configured default). Each backend has its own frozen baseline. Update the application's
compiled migration registry when adding a model. `che-rest` applications have
their own compiled `makemigrations` flow. Review generated operations and
backfill existing file keys separately if changing their storage format:
`#[orm(db_column = "existing_name")]` keeps the SQL column name but does not
rewrite stored values. Preserve previous migration identities and checksums.

The ORM high-level `create` and `update` APIs use SQLite `RETURNING`. The ORM
does not generate `AFTER INSERT` or `AFTER UPDATE` triggers. Do not add such
triggers for ORM models without explicitly reviewing the returned-value
semantics: triggers created by migrations or raw SQL may change a row after
SQLite has produced the `RETURNING` result, so the returned model will not
include those post-trigger changes.

## Synchronization

Sync models use UUID keys. Prefer `#[orm(sync, sync_name = "app.model")]` and
`SchemaSet::client_model` / `server_model` for a shared business model with
infrastructure-owned service columns. Preserve wire field names with
`#[orm(sync_name = "stable_name")]` when renaming Rust fields. Register FK parents
before children. ProjectState::from_app_schemas includes role-specific sync tables
automatically; servers have no shadows. All writes to synchronized tables use
the configured `SyncClient::database()` / `SyncServer::database()` handle or the
explicit sync facade. Original unconfigured handles and raw SQL bypass capture.
Configured server reads recheck scope authorization and filter scope/tombstones,
including counts, joins, `execute(SELECT)` and reads in existing transactions.
Preserve this behavior in every AST read path. Non-sync tables use ordinary ORM rules.
The following snapshot/reset rules belong to the scoped compatibility API:
`SyncEngine::pull()` downloads only and reports initial/reset load requirements;
`full_load()` is explicit. `flush()` sends only, through the supplied transport.
Clients target one persistent server history. Server replacement/history rewind
is outside the sync contract; existing epoch fields are compatibility identity
checks, not a recovery API. Journal pruning preserves revisions, tombstones and
receipts. An expired cursor blocks new operations until full loading, but pending
requests still retry with their original IDs. Do not retire or discard outbox
operations as part of cursor recovery.
`sync_once()` is a compatibility API that can load snapshots automatically;
the explicit loading rule applies to `pull()` and `flush()`. Both client and
server support either backend. Clone one SyncClient to share locks; concurrent
independent clients/processes on the same client database are unsupported.
Keep client writes/outbox and server writes/journal/receipts transactional.
ImageField mappings must be server-owned; the Model derive exposes protected
columns to sync registration. See `docs/sync.md` for ownership and scope contracts.

HTTP dependencies, resource paths, request bodies, headers and HTTP DTOs belong
to che-rest. che-sync-protocol exposes transport-neutral `ModelWrite` and model
states/results. che-sync may persist opaque adapter-owned payloads and expose a
generic `ModelLogTransport`; it must not encode or interpret HTTP requests.
Adapters own their stable format IDs and legacy payload-key compatibility.
Prepared server APIs use `request_receipt`, `write_prepared_request`,
`reject_request` and `prepare_conditional_write`; keep receipt storage compatible.

`ModelLogClient` and `SyncServer::open_model_logs` implement protocol 2 with
independent model journals and whole-model authorization. Protocol ownership,
scope and replica metadata initialize atomically; failed opens must leave legacy
metadata/outbox payloads unchanged. Legacy `SyncClient` must reject a protocol-2
database. Existing records require explicit `backfill_model_log`; never backfill
at startup or GET. `che-sync/tests/model_log.rs` and `model_log_postgres.rs` cover
the journal contract; che-rest owns HTTP, effect workers and migration guidance.

Protocol 2 has no snapshot/full-load step: `pull` downloads one page per model
from cursor zero; `flush` sends at most one coalesced record per model and retries
already submitted operations first. `sync_once` settles pending requests, pulls
journals, then prepares new work when caught up. Keep UI/worker cycles bounded;
stop on `blocked`, conflicts and rejections. Never apply the legacy cursor-expiry
and automatic full-load rules to `ModelLogEngine`.

`ModelLogTransport` supplies a stable `id`, one-time `prepare_write`, immutable
payload `write`, and journal `changes`. Validate embedded request identity in the
adapter before side effects. Preserve pending payload namespaces and legacy
compatibility keys; do not re-encode saved requests with new routes or codecs.

The downstream `che-tauri/sync` feature wraps this engine as `TauriSync` and
`che_model_sync`; ordinary i64 `che_api` resources remain separate. UUID CRUD
commands write through its configured client handle. Do not add Tauri, HTTP or
UI dependencies to this workspace. Sync database ownership includes server and
account identity; auth/cache changes must not clear pending sync data. Cache/sync
table overlap is currently prohibited by the downstream application contract,
not enforced by a registration guard.

The runnable server/client example is in the sibling
`../che-tauri/examples/todo-sync` workspace, with shared models and frozen client
and server migrations. Keep existing initial migrations unchanged; startup does
not apply them. See its README for two-device launch and manual GUI checks.

## Verification

Run these commands after code changes:

```bash
cargo fmt --check
cargo test --workspace
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-sync --example offline
```

For PostgreSQL without SQLite:

```bash
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
cargo test -p che-sync --no-default-features --features postgres
cargo test -p che-sync --no-default-features --features postgres --test runtime -- --ignored
cargo test -p che-sync --no-default-features --features postgres --test model_roles --test model_log_postgres -- --ignored
```

The explicit runtime test requires TEST_POSTGRES_URL and a live PostgreSQL
server; absence of the variable is an error. The sample PostgreSQL build uses
`-p che-orm-examples --no-default-features --features postgres`.

For release changes, follow `docs/releasing.md`: verify Rust 1.88, both backend
builds and all four public package archives. `che-orm-examples` is not published;
use explicit package selection rather than `cargo package --workspace`.

When changing the sync/model/serializer contracts, also verify matching sibling
checkouts (these are not included in `cargo test --workspace`):

```bash
cargo test --manifest-path ../che-rest/Cargo.toml --features sync-http
cargo test --manifest-path ../che-tauri/Cargo.toml --features sync
cargo check --manifest-path ../che-tauri/examples/todo-sync/Cargo.toml --workspace
cargo test --manifest-path ../che-tauri/examples/todo-sync/Cargo.toml -p todo-sync-server -p todo-sync-client --no-default-features
```

HTTP tests bind ephemeral loopback ports. The Todo integration uses disposable
file databases and tests offline reopen, two-device merges, conflicts, deletion
and invalid credentials. Desktop compilation does not imply GUI inspection.

Run examples after applying the configured migration:

```bash
cargo run -p che-orm-examples --bin manage -- migrate
cargo run -p che-orm-examples --bin schema
cargo run -p che-orm-examples --bin sqlite_crud
cargo run -p che-orm-examples --bin transactions
cargo run -p che-orm-examples --bin serializers
```

## Editing Rules

- Preserve the public re-exports in `src/lib.rs` unless an API change is intentional.
- Keep SQL values parameterized; never interpolate user values into SQL.
- Keep identifiers model-controlled and review any new raw SQL identifier path.
- Add or update tests for compiler output, schema changes, and runtime behavior.
- Use ASCII in source and documentation unless existing content requires otherwise.
- Do not commit generated `target/` artifacts or local database files.
