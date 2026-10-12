# che-sync

Native offline-first synchronization for che-orm: current/shadow client tables,
durable delivery, server revisions and journal, and three-way field merging.

Synchronization is push-first. The immutable outbox stores patches and revisions;
one frozen shadow supplies their base. Incoming groups update shadow before
current, and reconciliation commits both together.

See [the walkthrough](../docs/sync.md) and run:

```bash
cargo run -p che-sync --example offline
```

The initial engine is a library with a transport trait. Clients use SQLite;
separately built servers support SQLite or PostgreSQL.
