# Changelog

## 0.1.0

Initial publication.

- Transport-independent scoped synchronization messages and typed tagged values.
- Protocol-2 per-model journal operations, field diffs, model states and immutable
  `ModelWrite` mutations/results.
- Explicit protocol-3 addressed object delivery and access/revocation messages.
- Serde encodings preserve integer precision and distinguish absent values,
  JSON null and SQL NULL where required by the protocol.
- Rust 1.88 minimum supported version; no ORM, HTTP or Tauri dependency.
