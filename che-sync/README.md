# che-sync

Native offline-first synchronization for che-orm: current/shadow client tables,
durable delivery, server revisions and journal, and three-way field merging.

Shared `#[orm(sync)]` models are registered with `SchemaSet::client_model` or
`server_model`. Only clients create shadow tables. Use the configured
`SyncClient::database()` or `SyncServer::database()` handle to capture ordinary
ORM writes transactionally.

## Protocol 2: per-model journals

Prefer `ModelLogClient` and `SyncServer::open_model_logs` for new applications.
Each model has an independent append-only journal/cursor and whole-model access.
`pull` downloads one page per model from cursor zero; `flush` sends at most one
coalesced record per model, retrying submitted operations first. `sync_once`
settles submissions, pulls journals and delivers new work after catching up.
There is no snapshot/full-load phase. Stop bounded application cycles on
conflicts or blocked results and resolve them explicitly.

The immutable outbox preserves submitted identity and adapter payload across
restarts; unsubmitted edits coalesce. Use `ModelLogClient::database()` or its
facade to capture writes. Apply compiled role-specific migrations before opening;
existing server rows require explicit journal backfill. Never switch legacy
databases or pending requests implicitly.

`ModelLogTransport` has a stable format ID, one-time `prepare_write`, saved-payload
`write`, and model journal `changes`. The engine treats payloads as opaque data.
HTTP routes/headers/bodies and old REST request compatibility belong to the
`che-rest` adapter, not to this crate or `che-sync-protocol`.

See [the protocol-2 guide](../docs/model-sync.md),
[REST integration](../../che-rest/docs/model-sync.md) and the runnable
[Todo server / Tauri client](../../che-tauri/examples/todo-sync/README.md).

## Scoped compatibility protocol

`SyncClient` / `SyncEngine` retain scoped feeds. Their `pull` reports explicit
snapshot loading requirements, `full_load` performs that loading, and the
compatibility `sync_once` can combine it with delivery. These loading rules do
not apply to `ModelLogClient`.

See [the scoped walkthrough](../docs/sync.md) and run its native example:

```bash
cargo run -p che-sync --example offline
```

## Database ownership

The engine is a library with transport traits. Both clients and servers support
SQLite or PostgreSQL, selecting exactly one backend per binary. The sibling
`che-rest` integration delivers client changes through ordinary REST writes.
Clients target one persistent server history; server replacement/history rewind
is outside the contract. Clone one client to share locks; independently opened
concurrent clients/processes against one client database are unsupported. Keep
durable current/shadow/outbox data separate from disposable remote response
caches. Credential renewal does not authorize deleting pending edits or rebinding
them to another server/account.
