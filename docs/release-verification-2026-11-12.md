# Coordinated candidate preparation: 2026-11-12

Candidates: che-orm 0.4.0, che-sync 0.2.0 and downstream che-rest 0.4.0.
The [registry audit](registry-audit-2026-11-12.json) records published versions,
archive checksums and runtime-source differences at the time of inspection.
Macros 0.3.2 and protocol 0.1.0 retain their published versions: runtime sources
and original manifests are unchanged. che-tauri is not registered on crates.io;
its unused 0.1.1 candidate now requires the new ORM/sync versions.

## Completed preparation checks

- ORM workspace formatting, tests and documentation; sample schema export and
  the offline sync example.
- ORM generator checks for both SQLite/PostgreSQL and single/modular layouts
  against the local candidate (`scripts/check-template.py`).
- REST formatting, tests and documentation with `sync-http,push` enabled.
- Rust 1.88 workspace/all-target compilation for ORM and SQLite REST with
  `sync-http,push`.
- Rust 1.88 PostgreSQL all-target REST compilation with defaults disabled and
  `postgres,sync-http,push` enabled.
- All five public crates packaged together using
  `../che-rest/scripts/verify_sync_packages.py`; extracted archives passed Rust
  1.88 SQLite/HTTP and PostgreSQL all-target checks. Verification artifacts are
  local under `/tmp/opencode/che-release-040-package-check/`.
- che-tauri tests with `sync`, and independent Todo server/client tests with
  default features disabled.
- Independent REST fullstack example all-target compilation.
- Tacit 0.1.14 server tests and native mobile all-target compilation on Rust
  1.99.1. The preceding application upgrade also passed mobile tests and all
  three frontend builds; see Tacit's `docs/framework-upgrade.md`.

The archive verifier ran before final documentation edits. Its hashes identify
those local verification artifacts, not final release archives. Repackage the
reviewed commits before publication.

## Gates and limitations

No package was published, no tag moved, no commit pushed and no application
database migrated or deployed. Ordinary registry dry runs for dependent crates
must wait for core 0.4.0 and sync 0.2.0 publication and indexing. Publication
requires separate explicit authorization, in core -> sync -> REST order.

Existing SQLite installations require reviewed application-owned normalization
migrations before startup. Tacit adds server `tacit:0003_canonical_datetimes`
and client `mobile:0008_canonical_datetimes`, preserving frozen earlier history
and opaque pending payloads. Real database-copy/APK rehearsal and a lost-response
retry spanning the client migration remain deployment checks. Existing unrelated
Tacit strict-Clippy warnings are not fixed by this preparation.
