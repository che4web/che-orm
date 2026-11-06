# Publication preparation: 2026-11-06

## Candidate scope

| Package | Version | Action |
| --- | --- | --- |
| che-orm-macros | 0.3.2 | Already published; retain version/tag |
| che-orm | 0.3.3 | Already published; core code unchanged since v0.3.3 |
| che-sync-protocol | 0.1.0 | Already published; retain version/tag |
| che-sync | 0.1.3 | New candidate; publish first |
| che-rest | 0.3.4 | New candidate; publish after sync indexing |

The crates.io API was checked on this date: all versions marked already
published above are present and not yanked; the latest published sync/REST
versions were 0.1.2/0.3.3. No package was published during preparation.

The candidates add batched protocol-2 journal reads, explicit per-model
positions, read-only server readers and opt-in REST transport. Default
per-model downloads, immutable write identity and pending payloads are retained.
No new migration or wire protocol version is introduced. Applications own the
aggregate HTTP endpoint and must authorize every participating ViewSet.

REST standalone typed actions add a required `ApiEndpoint::crud` field for
users constructing that struct directly. Set it to true for model CRUD and
false for action-only metadata. Generated admin and fullstack example build
dependencies now use patched Vite 6.4.5 and vue-tsc 3.3.14; example lock files
also refresh affected transitive dependencies.

## Completed verification

- ORM workspace: all-package formatting, all-target Clippy with warnings denied,
  workspace tests, API documentation, schema export and offline example.
- REST: all-feature tests and Clippy, API documentation, model/legacy sync
  examples, Rust 1.88 all-target/all-feature compilation, ignored generator
  smoke test, admin identity tests and fullstack HTTP smoke test.
- Rust 1.88: ORM SQLite and PostgreSQL all-target builds; extracted five-package
  archives checked for SQLite/HTTP and PostgreSQL.
- Live PostgreSQL: ORM runtime and sync runtime/model-role/model-log suites
  passed against a disposable local cluster, which was stopped afterwards.
- Template matrices: both ORM layouts for SQLite/PostgreSQL; both REST
  generators, layouts and authentication modes, including migrations and HTTP.
- Downstream: che-tauri sync tests, Todo workspace compilation and server/client
  tests without default features.
- RustSec audit: ORM, REST and fullstack example Cargo lock files passed.
- Web examples: client/admin builds passed; final npm audits report zero
  vulnerabilities. A fresh generated admin project also builds and audits clean.
- Archive file lists were reviewed for runtime databases, uploads, credentials
  and build outputs. No such files were found in the package lists.
- The ordinary che-sync 0.1.3 registry publish dry run passed.

Local verification logs and machine-readable results are retained at:

`/home/che/web/rust/tacit/target/crates-prep-20261106/`

The final sibling-archive report and SHA-256 hashes are in
`final-packages/result.json` there. Archives are under
`final-packages/source/target/package/`. This verifier packages a source snapshot
and does not publish or substitute registry availability. Repackaging from a
committed repository may change VCS metadata and archive hashes.

## Remaining publication sequence

The ordinary che-rest dry run was attempted and stopped at dependency
resolution: crates.io has no che-sync 0.1.3 yet. Joint packaging and extracted
archive builds passed locally; the registry gate must be checked again after
publishing sync. Do not lower the dependency requirement to bypass it.

From a reviewed, committed and clean ORM worktree:

```sh
cargo publish --dry-run -p che-sync --locked
cargo publish -p che-sync --locked
```

Wait until che-sync 0.1.3 is indexed. Then from the clean REST worktree:

```sh
cargo publish --dry-run --locked
cargo publish --locked
```

Verify crate pages and docs.rs builds after publication. Only then create new
sync `che-sync-v0.1.3` and REST `v0.3.4` tags; retain all published tags.
There is no new core ORM release to upload. This report records package/build
verification, not a manual GUI/device check or production database cutover.
