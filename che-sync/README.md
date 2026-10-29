# che-sync

Native offline-first synchronization for che-orm: current/shadow client tables,
durable delivery, server revisions and journal, and three-way field merging.

Requires **Rust 1.88+**, **che-orm 0.3.2** and **che-sync-protocol 0.1.0**.
Licensed under MIT. See the [changelog](CHANGELOG.md) and
[Rust API documentation](https://docs.rs/che-sync).

## Install

SQLite is enabled by default:

```toml
[dependencies]
che-sync = "0.1.0"
che-orm = "0.3.2"
```

For PostgreSQL, disable defaults on both crates and select exactly one backend:

```toml
[dependencies]
che-sync = { version = "0.1.0", default-features = false, features = ["postgres"] }
che-orm = { version = "0.3.2", default-features = false, features = ["postgres"] }
```

When developing with local checkouts, use the same local ORM dependency for all
consumers; local and registry copies are distinct Rust crates even at the same
version. The Cargo package version is independent of wire protocol versions.

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

Inspect local state through `ModelLogClient::has_initial_pull(model)`,
`pending_count()` and `pending_ids(model)` rather than querying internal sync
tables. An accepted empty first page counts as an initial pull even if its
sequence is zero. Pending counts/IDs describe dirty entities (including local
deletions), not transport requests or application-owned upload queues. Model
arguments must name a registered model.

`ModelLogTransport` has a stable format ID, one-time `prepare_write`, saved-payload
`write`, and model journal `changes`. The engine treats payloads as opaque data.
HTTP routes/headers/bodies and old REST request compatibility belong to the
`che-rest` adapter, not to this crate or `che-sync-protocol`.

See [the protocol-2 guide](https://github.com/che4web/che-orm/blob/main/docs/model-sync.md),
[REST integration](https://github.com/che4web/che-rest/blob/main/docs/model-sync.md) and the runnable
[Todo server / Tauri client](https://github.com/che4web/che-tauri/blob/main/examples/todo-sync/README.md).

Run the native journal replay and offline field-merge example (SQLite):

```bash
cargo run -p che-sync --example model_log
```

It uses the journal application API directly; the Todo application demonstrates
delivery through a `ModelLogTransport` adapter.

## Protocol 3: shared objects

Per-object permissions use explicit `ModelLogClient::open_objects` and
`SyncServer::object_changes` APIs. Protocol 2 keeps whole-model authorization;
upgrades are explicit and require submitted writes to settle first. See the
[object-sync contract](https://github.com/che4web/che-rest/blob/main/docs/object-sync.md).

## Scoped compatibility protocol

`SyncClient` / `SyncEngine` retain scoped feeds. Their `pull` reports explicit
snapshot loading requirements, `full_load` performs that loading, and the
compatibility `sync_once` can combine it with delivery. These loading rules do
not apply to `ModelLogClient`.

See [the scoped walkthrough](https://github.com/che4web/che-orm/blob/main/docs/sync.md)
and run its native example from a repository checkout:

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
