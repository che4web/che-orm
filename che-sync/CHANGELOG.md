# Changelog

## 0.2.0 (unreleased)

- **Breaking:** require che-orm 0.4.0 and its bound-query Database/Transaction
  types. Adapt internal row reads without changing sync protocol versions.
- Existing SQLite installations require explicit application-owned datetime
  normalization migrations, including current/shadow/journal columns. Preserve
  opaque submitted requests, receipts, cursors and pending work.

## 0.1.3

- Read multiple independent model journals in one consistent snapshot with explicit
  cursors, bounded per-model quotas and history reset reporting.
- Open read-only journal readers without initializing service rows.
- Add optional batched transport reads with registry-order application and
  preservation of pending work on history reset, invalid or lost responses.
- Keep per-model transports as the default and submitted write payloads unchanged.

## 0.1.2

- Support self-referencing UUID models when registering model journals and
  preserve parent references across backfill and deletion.
- Expose initial-pull and pending-entity inspection for native consumers.
- Restrict the SQLite-only external foreign-key fixture to the SQLite feature,
  keeping PostgreSQL all-target and extracted-package checks buildable.

## 0.1.1

- Capture ordinary integer-parent CASCADE and SET NULL actions in model journals.
- Require che-orm 0.3.3 for the configured referential-action runtime.

## 0.1.0

Initial publication. Requires che-orm 0.3.2, che-sync-protocol 0.1.0 and Rust 1.88.

- Durable client/server synchronization with SQLite (default) or PostgreSQL.
- Configured ORM write capture, role-specific schemas, compiled migration
  operations, journals, revisions, tombstones and transactional receipts.
- Protocol-2 per-model journals with independent bounded pull/flush cycles,
  coalesced unsent edits and immutable submitted transport payloads.
- Three-way field merging, persisted conflicts and explicit resolution.
- Explicit protocol-3 object delivery, transactional visibility reconciliation,
  revocation-aware local reads and explicit review of regranted drafts.
- Retain the scoped SyncClient/SyncEngine compatibility API and explicit full-load
  recovery rules without implicitly migrating client databases or pending work.
- Generic transport traits keep HTTP, REST authorization and Tauri adapters in
  downstream crates.
