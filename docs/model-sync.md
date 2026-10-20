# Per-model synchronization (protocol 2)

`che-sync::ModelLogClient` synchronizes UUID models through independent model
journals. New clients start at cursor zero. Authorization grants access to an
entire model; there is no client-supplied scope or per-record journal filtering.
The scoped `SyncClient` / `SyncEngine` snapshot API remains a separate
[compatibility protocol](sync.md).

## Ownership

| Crate | Responsibility |
| --- | --- |
| `che-orm` | Model metadata, queries, transactions, role schemas and compiled migrations |
| `che-sync-protocol` | Transport-independent `ModelWrite`, journal entries, canonical states and conflicts |
| `che-sync` | Capture, current/shadow data, dirty tracking, immutable outbox, receipts, cursors and merge |
| `che-rest` | HTTP DTOs, routes, serializer mapping, headers and the REST transport |
| `che-tauri` | Managed client state, invoke commands and generated frontend sync client |

The ORM and sync crates have no HTTP dependency. `ModelWrite` describes data
changes and SQL-null metadata. `che_rest::RestModelWrite` owns resource paths and
HTTP bodies. Server APIs `request_receipt`, `write_prepared_request`,
`reject_request` and `prepare_conditional_write` accept data and opaque receipt
identities; HTTP parsing belongs in the adapter.

## Model, schema and migrations

```rust,ignore
use che_orm::{Model, SchemaSet, Uuid};
use che_sync::{ModelLogClient, Registry};

#[derive(Debug, Model)]
#[orm(table = "todos", sync, sync_name = "todo.task")]
struct Todo {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    done: bool,
}

let client_schema = SchemaSet::new().client_model::<Todo>();
let server_schema = SchemaSet::new().server_model::<Todo>();
let registry = Registry::from_schema(&client_schema)?;
// Apply the application's compiled client migrations before opening.
let client = ModelLogClient::open(database, registry).await?;
```

Keep stable model/field sync names when changing Rust names or physical columns.
Register FK parents before children. `ProjectState::from_app_schemas` includes
role-specific service tables: client shadows and metadata, or server per-model
journals and state. Freeze operations in compiled migrations; never recalculate
an applied initial migration from the current model or create tables at startup.

Servers use `SyncServer::open_model_logs`. For existing server rows, apply a
reviewed follow-up migration and explicitly call `backfill_model_log::<Todo>()`
before serving journals/new writes. Startup and journal GET never backfill.
The framework cannot infer ownership conversion from old scopes.

## Local writes and bounded synchronization

```rust,ignore
let todo = Todo { id: Uuid::now_v7(), title: "Offline task".into(), done: false };
client.save(&todo).await?;
let engine = client.engine(transport); // implements ModelLogTransport
let report = engine.sync_once().await?;
let conflicts = client.conflicts().await?;
// Choose explicitly: Resolution::Local, Remote, or Record(...).
client.delete::<Todo>(todo.id).await?;
```

Use the client's configured `database()` handle or facade for every local sync
write. Original unconfigured handles and raw SQL bypass capture. `save` writes a
complete materialized model; application commands own input validation.

- `pull` downloads one page per registered model, in parent-first order. Repeat
  while `has_more` is true. Protocol 2 has no snapshot/full-load step.
- `flush` sends at most one coalesced record per model, retrying submitted
  requests first. Initialize model journals through pull before new delivery.
- `sync_once` settles pending submissions, pulls journals, then sends new work
  after downloads catch up. Keep application loops bounded and stop on
  `blocked`, conflicts or rejections; opening does not start a background task.

Unsubmitted edits coalesce; an unsent create followed by delete sends nothing.
Submitted identity and adapter payload stay immutable across retries/restarts;
later local edits remain separate work. Transport/authentication failures leave
the request pending. Canonical rejection settles delivery and records a conflict.
Independent fields merge; competing fields and delete/update conflicts need
explicit resolution against the latest confirmed state.

Clone one client to share write/cycle locks. Independent concurrent clients or
processes against one client database are unsupported. A client belongs to one
persistent server/account history; switching accounts, restoring a server backup
or replacing history requires explicit application handling, not cursor rewind.
Protocol ownership metadata prevents silent reinterpretation of legacy client
databases and pending requests.

## Transport contract

`ModelLogTransport` provides a stable `id()`, `prepare_write(&ModelWrite)`,
`write(&ModelWrite, &payload)` and `changes(model, after, limit)`.
The engine encodes once and persists the opaque payload with its transport ID.
It reuses the payload after reopening and rejects a different adapter for an
already submitted operation. Validate any embedded identity before side effects.
Adapters own old payload-key compatibility through `legacy_payload_key`.

For REST, reuse `che_rest::sync_http::{RestModelTransport, RestResource}` with
feature `sync-http`; do not add HTTP handling to this workspace. That adapter
uses reqwest 0.12 and preserves old `rest-request:*` payloads. See the
[REST protocol guide](../../che-rest/docs/model-sync.md) for serializer mapping,
wire values, authorization and migration rehearsal. Integers/revisions/cursors
use decimal strings; nullable JSON distinguishes SQL NULL and JSON null.

## Tauri application

Enable `che-tauri/sync`. `ModuleContext::sync_model::<Todo>()` registers the
client schema for migrations; `InstalledApps::sync_registry()` builds its
registry. Manage `TauriSync` after migrations and pass it the existing REST
transport. Register `che_tauri::che_model_sync` with a fully qualified path in
one `tauri::generate_handler!` list alongside application UUID CRUD commands.

The ordinary `che_api` i64 resources remain separate. `AppState` auth changes do
not update sync credentials. Cache invalidation must never delete durable sync
tables: overlap is a documented application restriction, currently without a
registration-time guard. Use a separate client database for each server/account.

The complete [Todo server and desktop client](../../che-tauri/examples/todo-sync/README.md)
uses frozen migrations, offline SQLite, manual synchronization and conflict
selection. Its own Cargo workspace is outside the ORM workspace's test scope.

## Verification

From the ORM root:

```bash
cargo test -p che-sync --test model_log
cargo test -p che-sync --no-default-features --features postgres --test model_log_postgres -- --ignored
```

The PostgreSQL command requires a disposable database in `TEST_POSTGRES_URL`.
For downstream integration, from the matching `che-rest` checkout run
`cargo test --features sync-http`; from `che-tauri` run `cargo test --features sync`
and the separate Todo workspace checks documented in its README. These tests do
not migrate or certify an application's existing deployment database.
