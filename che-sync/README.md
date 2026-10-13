# che-sync

Native offline-first synchronization for che-orm: current/shadow client tables,
durable delivery, server revisions and journal, and three-way field merging.

Shared `#[orm(sync)]` models are registered with `SchemaSet::client_model` or
`server_model`. Only clients create shadow tables. Use the configured
`SyncClient::database()` or `SyncServer::database()` handle to capture ordinary
ORM writes transactionally.

`pull()` downloads one incremental page, `flush()` delivers one durable operation,
and `full_load()` explicitly loads a snapshot. The compatibility `sync_once()`
combines retry, download and delivery. The immutable outbox stores patches and
revisions; one frozen shadow supplies their base. Incoming groups update shadow
before current, and reconciliation commits both together.

See [the walkthrough](../docs/sync.md) and run:

```bash
cargo run -p che-sync --example offline
```

The engine is a library with a transport trait. Both clients and servers support
SQLite or PostgreSQL, selecting exactly one backend per binary. The sibling
`che-rest` integration delivers client changes through ordinary REST writes.
Clients target one persistent server history; server replacement/history rewind
is outside the contract. Expired journal cursors require full loading while
pending requests retain their original IDs for retries.
