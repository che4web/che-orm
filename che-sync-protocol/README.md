# che-sync-protocol

Transport- and database-independent synchronization messages for the `che-orm`
ecosystem. Requires Rust 1.88 or later. Licensed under MIT.

```toml
[dependencies]
che-sync-protocol = "0.1.0"
```

## Protocols

- The crate root exposes the scoped compatibility protocol (version 1), tagged
  values, records, operations, cursors and mutation results.
- `model_log` exposes protocol-2 per-model journals, field diffs, model states,
  transport-neutral `ModelWrite` mutations and their results.
- `object_log` exposes explicit protocol-3 addressed delivery, current access,
  revocation and object synchronization messages.

The Cargo package version is independent of these wire protocol versions.
Selecting a newer API must not implicitly switch an existing client database or
discard pending operations. Submitted identities and adapter payloads must be
preserved for retries.

Messages use Serde. Integer revisions, cursors and tagged integer values use
lossless decimal-string encodings where specified by the DTO; nullable JSON
fields distinguish absent values, JSON null and SQL NULL. Use the supplied types
and serialization rather than replacing them with ad hoc JSON encodings.

This crate contains no database engine, HTTP client, REST routes, authentication
policy or Tauri integration. Use [che-sync](https://crates.io/crates/che-sync)
for the native durable engine and
[che-rest](https://github.com/che4web/che-rest) for HTTP delivery.

## Documentation

- [Rust API](https://docs.rs/che-sync-protocol)
- [Per-model synchronization](https://github.com/che4web/che-orm/blob/main/docs/model-sync.md)
- [Scoped compatibility protocol](https://github.com/che4web/che-orm/blob/main/docs/sync.md)
- [Object synchronization](https://github.com/che4web/che-rest/blob/main/docs/object-sync.md)
- [Changelog](CHANGELOG.md)
