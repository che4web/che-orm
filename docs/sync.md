# Offline-first synchronization

`che-sync` is the native Rust synchronization engine for `che-orm`.
`che-sync-protocol` contains backend-independent, versioned messages. The client
and server both support SQLite and PostgreSQL with the same synchronization
algorithm and API. Select one backend per binary at build time.

The client stores two representations of each synchronized model:

- The ordinary application table is the visible, editable current state.
- `_sync_server_<table>` stores the last confirmed server state (the shadow).

The server stores authoritative application rows, revisions, tombstones, a
commit-ordered change journal, and durable operation receipts. It does not need
per-client shadow tables.

## Register a model

```rust
use che_orm::{Model, Uuid};
use che_sync::{Field, Registry, SyncModel};

#[derive(Debug, Model)]
#[orm(table = "notes")]
struct Note {
    #[orm(primary_key)]
    id: Uuid,
    title: String,
    body: String,
    #[orm(default = "0")]
    revision: i64,
    #[orm(auto_now)]
    updated_at: che_orm::time::OffsetDateTime,
    deleted_at: Option<che_orm::time::OffsetDateTime>,
    #[orm(default = "''")]
    sync_scope: String,
}

impl SyncModel for Note {
    const SYNC_NAME: &'static str = "notes.note";
    const SYNC_VERSION: u32 = 1;

    fn sync_fields() -> Vec<Field> {
        vec![
            Field::server_owned("id", "id"),
            Field::writable("title", "title"),
            Field::writable("body", "body"),
        ]
    }
}

let registry = Registry::new().register::<Note>()?;
# Ok::<(), che_orm::OrmError>(())
```

Map every business column exactly once. The service columns `revision` (i64),
`updated_at` (OffsetDateTime), `deleted_at` (Option<OffsetDateTime>) and
`sync_scope` (String) must exist in the actual Rust model; registration validates
their types and adds server-owned mappings automatically. Override the
`SyncModel::*_COLUMN` constants to use different SQL names. The first argument is a stable wire
name, the second is a physical SQL column. Rust or `db_column` renames do not
require changing the wire name. Keys, managed timestamps and image storage keys
are server-owned. The Model derive exposes image columns through
`Model::write_protected_columns()`, and registration rejects writable mappings
for those columns. Manual Model implementations must provide this metadata.

Synchronized models and foreign keys use UUIDs. Create an offline key with
`Uuid::now_v7()`. Register FK parents before children; cyclic dependencies are
rejected. All ORM scalar types are represented by tagged protocol values,
including nullable fields, UUIDs, timestamps and binary data. Non-finite real
values are rejected. Enum choices remain validated by the model schema.

## Migrations

Create the application's model tables with its normal compiled migrations.
Append `registry.migration_operations(SchemaRole::Client)` for a client database,
or `SchemaRole::Server` for a server database, to an initial compiled migration.
Apply that migration before opening the engine. Existing example migration
baselines are independent and remain frozen.

The registry returns owned, backend-neutral `MigrationOperation::CreateModel`
operations. Sync table identities belong to the `che_sync` application; shadow
identities use stable wire model names, so a physical table rename retains the
shadow identity. Include these tables in the planner's desired ProjectState:

```rust,ignore
let mut desired = ProjectState::from_app_schemas(app_registry.app_schemas())?;
registry.extend_migration_state(&mut desired, SchemaRole::Client)?;
```

Freeze historical operations instead of recalculating old migrations from
current models. Follow-up migrations must update current tables and shadows,
and explicitly transform persisted pending requests/conflicts when wire data
changes. `SYNC_VERSION` is checked on incoming messages; incompatible messages
are never applied silently. This version does not automatically translate old
wire representations.

Shadows omit business unique constraints, FK actions, automatic timestamp
defaults and checks. Business fields are nullable to allow tombstones without
payloads. `_sync_epoch` identifies the server generation for each shadow row.
The server retains business data on logical deletion; querysets should filter
`DELETED_AT.is_null()`. The client's current table is an active-row projection;
confirmed tombstones live in the shadow. Use partial unique indexes with
`WHERE deleted_at IS NULL` when deleted server rows should release a business key.

### Upgrading the previous metadata layout

Do not recalculate an applied initial migration. Add a compiled follow-up
migration after stopping synchronization and settling all pending operations:

1. Add the four service fields to application models and their current/shadow
   tables; make shadow business columns nullable and add nullable `_sync_epoch`.
2. Backfill server `revision`, `sync_scope` and `deleted_at` from `_sync_entities`.
   Restore physically deleted rows from application history or the last live
   journal record before marking them deleted. Non-nullable business values
   cannot be fabricated for a tombstone. Preserve UUIDs and existing revisions.
3. Backfill shadow revisions/deletion from `_sync_client_entities`, including
   tombstone-only shadow rows. Backfill `_sync_epoch` from persisted feed cursors.
   Use existing managed timestamps or an explicit migration timestamp for old
   rows; timestamps are descriptive, not synchronization cursors.
4. Create `_sync_effects` on the server. Preserve `_sync_scopes`, journals,
   receipts, cursors, dirty rows and conflicts. Review active-only unique indexes.
5. Check row counts, scope ownership and FK relationships, then drop the two
   legacy entity metadata tables in the same reviewed migration.

These are application-owned data migrations: automatic schema planning cannot
recover missing business records or infer ownership. Freeze their operations and
checksums alongside previous history before deploying the new engine.

## Offline writes

```rust,ignore
let client = SyncClient::open(database, registry, "workspace-42").await?;
let note = Note {
    id: Uuid::now_v7(),
    title: "Offline title".into(),
    body: "Offline body".into(),
    revision: 0,
    updated_at: che_orm::time::OffsetDateTime::UNIX_EPOCH,
    deleted_at: None,
    sync_scope: String::new(),
};
client.save(&note).await?;
let visible = client.database().get::<Note>(note.id).await?;
client.delete::<Note>(note.id).await?;
```

`save` writes a complete materialized model. For an existing row it preserves
server-owned fields from the current row. For a new row these fields can have
provisional values for local display; outgoing requests only contain writable
fields. The server supplies canonical managed timestamps/defaults. Trusted
`SyncServer::save` can set server-owned values such as validated image keys.
File transfer itself belongs to a separate media transport.

`SyncClient::transaction` groups related local writes with dirty tracking:

```rust,ignore
client.transaction(move |tx| Box::pin(async move {
    tx.save(&project).await?;
    tx.save(&task).await?;
    Ok(())
})).await?;
```

Callbacks own captured data (`Send + 'static`). Use `SyncTransaction::get` for
reads within a transaction. Local CASCADE and SET NULL actions are expanded
explicitly and tracked. Restricted deletes fail without partial changes.

An explicit local deletion has a durable intent marker in `_sync_meta`, written
in the same transaction as current and dirty tracking. Physical absence alone
does not indicate deletion: an incoming row may be absent because its INSERT
was deferred. Deleting such a known row cancels its automatic INSERT retry and
queues deletion against the current shadow revision. Confirmed absence, a new
local save, or cancellation of an unsubmitted create consumes the marker.

All writes to synchronized tables go through the sync facade, on both client
and server. Raw SQL and ordinary ORM writes do not capture synchronization
events. Other, unsynchronized tables may use the ordinary ORM normally. Do not
add triggers that mutate synchronized rows outside this capture path.

One SyncClient owns one database and one scope. Clone that client to share its
write and synchronization locks. Concurrent independently opened clients or
processes against the same client database are outside this ownership contract.
An application can reopen the database after shutting down the prior owner.

## Server and transport

```rust,ignore
let server = SyncServer::open(
    database,
    registry,
    trusted_scope,
    authenticated_principal,
    policy,
).await?;
```

An application implements `AccessPolicy` to authorize the entire scope and
validate prospective writes. The server also validates implicit cascade
effects. Scope ownership is recorded on entities; cross-scope UUID references
and mutations are rejected. The principal comes from trusted transport context,
not the request body. Receipts are scoped to principal, replica and operation.

In this version a scope is a uniformly readable synchronization partition.
Per-record read filtering and subscription eviction events are not implemented.
Scope authorization is checked on every push, pull and snapshot.

Implement `SyncTransport` for an authenticated HTTP or IPC adapter. It provides
boxed async `push`, `pull`, and `snapshot` methods returning `OrmError` on
transport failures. `SyncServer` implements it directly for in-process use and
tests. No HTTP framework is selected by the engine.

The sibling `che-rest` crate provides authenticated ViewSet routes through
`SyncViewSet` and `SyncEndpoints`. Its extension also routes ordinary ViewSet
CRUD through the sync journal. HTTP synchronization writes use ordinary
POST/PATCH/DELETE with serializer JSON, conditional revisions and idempotency
headers; there is no separate HTTP push route. Sync-mode responses contain the
canonical record/tombstone envelope. `RestSyncRequest` maps wire patches to
serializer field names. REST transports advertise `max_mutations() == 1`;
native transports can still batch operations. See `che-rest/docs/sync.md` and run
`cargo run --example todo_sync` in that repository for a two-device HTTP demo.
Trusted serializer writes can use `SyncServer::write(ValidatedWrite<M>)`;
updates must target a single UUID primary key.

Build a SQLite client with defaults. Build a PostgreSQL server with:

```toml
che-sync = { path = "../che-sync", default-features = false, features = ["postgres"] }
```

`che-orm` selects exactly one backend per build. SQLite and PostgreSQL runtimes
therefore live in separate binaries and communicate using the common protocol.

## Synchronization cycle

```rust,ignore
let engine = SyncEngine::new(client.clone(), transport);
let report = engine.sync_once().await?;
```

The same offline example runs on either backend:

```bash
cargo run -p che-sync --example offline
CHE_SYNC_DATABASE_URL=postgres://user:password@localhost/database cargo run -p che-sync --no-default-features --features postgres --example offline
```

SQLite uses three in-memory databases. PostgreSQL uses three disposable schemas
in the supplied database (and removes them after a successful run). Both execute
the same client writes, pull/merge/push cycle and two-device field merging.

For independent model synchronization, bind the transport to its model and
create a model engine:

```rust,ignore
let transport = server.for_model::<Note>()?; // Or its che-rest ViewSet endpoint.
let engine = SyncEngine::for_model::<Note>(client.clone(), transport)?;
let report = engine.sync_once().await?;
```

Each model engine has its own cursor and immutable outgoing operation. Use one
SyncClient with a registry containing the related models, and clone it for the
engines. A conflict in another model does not stop this model's writes. New local
FK parents are automatically synchronized in separate parent steps, recursively. An
existing parent's independent edits remain in its own queue. Parent deletions
include required dirty child moves/deletions so a cascade cannot erase a move.
An unresolved conflict in a required dependency blocks that dependent group.

Model snapshots and pull pages include the selected model and its FK parent
models, so remote dependencies are loaded automatically. Model-bound push
contains only its selected model. The full registry
is retained for cascade capture. Cursors include the selected `model`, and a
cursor from another model is rejected. The server keeps one commit-ordered scope
journal; model pages project its groups and may contain empty groups to preserve
sequence continuity. A selected parent's deletion also carries the connected
group's cascade and SET NULL effects. Snapshots containing parent tombstones
include the related component's current rows, so reset can restore those effects
even after journal retention. Unconnected models remain outside the feed.

Client cursors use separate metadata keys; outbox payloads record their owning
model without duplicating base or local records. A lost parent reply is retried
with its original operation ID before sending a dependent child. Unrelated model
operations can remain queued concurrently. Pending operations freeze overlapping
entities and their FK dependencies until their outcome is known;
`report.blocked` indicates this condition or a required conflict. Cycles are
serialized per model and retries per operation ID. Network requests do not hold
a database-wide cycle lock; unrelated models can synchronize concurrently.
The caller controls transport timeouts. Finish a historical whole-scope pending operation
through its original transport before switching that database to model engines.

JSON encodes tagged Integer values, entity revisions, base revisions and journal
sequences as decimal strings, preserving the entire i64 range in JavaScript.
Decoders also accept historical integer JSON tokens, so existing local journals
and outboxes remain readable. Generated TypeScript uses string for these values;
use BigInt when arithmetic is needed and convert back to a decimal string before
JSON serialization. Regenerate clients after upgrading.

A cycle retries an immutable operation with unknown outcome first. It then
pulls and merges a page (or performs a full load), and prepares a fresh patch
against the updated shadow before pushing. Pages are drained before fresh writes.
Call it again while `report.has_more` is true, or schedule it on connectivity
changes. A failed push leaves the outbox intact and prevents pull from replacing
its shadow base. Network requests never hold a local database transaction. The
caller controls timeouts, cancellation and retry backoff.

Within a model feed, currently dirty rows of one model are coalesced into an
atomic outgoing group. FK creates and child moves use separate model operations,
ordered before dependent writes. Local cross-model transactions are atomic only
locally; their delivery is sequential. Server cascade effects remain atomic.
Multi-model transports override `push_model` to route each step to its own endpoint.
Initial loading and history recovery use the same `full_load` implementation;
HTTP transports can request `pull` with a null cursor to receive `Full(Snapshot)`.
The legacy `SyncEngine::new` whole-scope mode coalesces all dirty models and is
blocked by any conflict. Neither mode has a per-transaction scheduler or maximum
group byte size.

The outbox saves only the immutable request: operation ID, base revisions and
patches (or deletions). It does not store server-base or local-state snapshots.
The sole shadow stays frozen until the request has a definite result. On an
acknowledgement the client reconstructs the sent writable state in memory from
that shadow and the saved patch, then preserves edits made since sending it.
Server-owned values always follow the canonical response. Retrying the same
operation returns its saved server result; reusing an ID with different contents
fails. Historical outbox payloads containing redundant `bases` and `sent` fields
can still be read; those fields are ignored during recovery.

For each response or incoming group the client reads the old shadow and local
rows, computes reconciliation in memory, writes the new shadow first, then
updates the visible rows and synchronization metadata in the same transaction.
No persistent third copy of server state is needed for ordinary merging. An
unexpected failure rolls shadow, current, outbox changes and cursor back together.

The server journals data, revisions and receipts atomically. A locked scope
counter provides commit-ordered sequence numbers, including for concurrent
PostgreSQL writers. A pull page contains whole atomic groups and the client
reconciles them in journal order, rather than flattening their entities into a
single final-state batch. Applying the page's shadows, visible rows, conflicts
and cursor is atomic.

Within a group, statements blocked by FK or UNIQUE dependencies are postponed
and retried after other statements succeed. Each attempt has its own savepoint,
including on PostgreSQL where a constraint error otherwise aborts the transaction.
This supports delete/create replacements that reuse a unique value and moving
children before deleting their old parent. If no constraint-safe statement order
exists, the group is rejected or retained as a conflict; the executor does not
invent temporary field values for cyclic UNIQUE swaps.

Initial loading uses one consistent snapshot with its cursor. The initial
snapshot is currently materialized as a whole; large-data snapshot sessions and
pagination can be added to the transport later. Independent live-table pages
must not be used as a replacement. A journal gap requests a fresh snapshot while
retaining local edits. Snapshots retain known tombstones; omission of a known
entity fails instead of silently discarding local state.

## Conflicts

Three-way field merging compares the previous shadow, current local row and
new remote row. Independent writable-field changes merge automatically.
Different changes to the same field produce a durable conflict while keeping
the local field visible. Server-owned fields follow canonical remote values.
Delete/update and conflicting creations produce entity-level conflicts.

```rust,ignore
let conflicts = client.conflicts().await?;
client.resolve("notes.note", id, Resolution::Local).await?;
// Or Resolution::Remote, or Resolution::Record(Some(complete_record)).
```

Resolution is relative to the latest saved shadow. New operations receive new
IDs and revisions. A deleted server UUID cannot be resurrected; copy desired
data into a new entity instead. Server rejections also persist as conflicts so
the application can fix data or choose the remote state.

Constraint failures or cascading changes that would destroy a locally merged
row roll back the entire visible incoming group. Dependency checks include
never-sent local children, not just entities listed in the server response.
Their data is preserved in current rows and conflicts, without inventing a
server revision or shadow tombstone for them. A conflict resolution also cannot
silently cascade through such children.

Remote shadows and conflicts are still recorded atomically with the cursor.
An unchanged row blocked by a constraint has `Conflict::retryable = true`; the
client retries reconciliation after receiving a page, even an empty page, so
releasing a dependency can apply it automatically. A deferred accepted write
uses its reconstructed sent state as `Conflict::base`, with the original
canonical response in `Conflict::acknowledgement`. This context exists only in
an actual constraint conflict, not in the outbox or another shadow table. Retry
overlays edits made since sending and honors server normalization. Newer server
revisions are merged against the original acknowledgement, so a real concurrent
edit still becomes a field conflict.

Competing field edits, server rejections and protected local children require
explicit resolution. Dependency protection augments existing conflicts with
fresh local/remote information; it preserves their original base, field markers
and rejection reason. Resolve related conflicts in FK order (for deleted objects,
children before parents). Conflict records include local data, remote state and
a reason. Newly added acknowledgement context is optional when reading older
persisted conflict payloads.

Journal entries, receipts and tombstones are retained indefinitely by this
initial implementation. History pruning requires preserving tombstones and
receipt retry guarantees; automatic retention/garbage collection is not provided.

## Run the example and checks

```bash
cargo run -p che-sync --example offline
cargo test -p che-sync
cargo test -p che-sync --no-default-features --features postgres
cargo test -p che-sync --no-default-features --features postgres --test postgres -- --ignored
```

The explicit PostgreSQL runtime test requires `TEST_POSTGRES_URL` and a live
server. It tests concurrent commit ordering, receipt replay, managed values,
atomic constraint rejection and snapshot consistency in an isolated schema.
