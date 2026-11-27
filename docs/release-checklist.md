# crates.io Release Checklist

Use this checklist for every release. [The release procedure](releasing.md)
records the planned versions, full verification commands and publication order
for all four public crates. Crates in `example/` are not published.

Prepare che-orm-macros 0.4.0, che-orm 0.5.0, che-sync 0.3.0 and che-rest
0.5.0. Use the published che-sync-protocol 0.1.0. Earlier verification reports
cover historical source snapshots.

## Prepare

- [ ] Decide versions for `che-orm`, `che-orm-macros`, `che-sync` and `che-sync-protocol`.
- [ ] Update versions and internal dependency requirements in their manifests.
- [ ] Update `CHANGELOG.md` with user-visible changes and migration notes.
- [ ] Review `README.md`, `README.en.md`, and `docs/en/` for API accuracy.
- [ ] Confirm migration files in `example/orm-api/src/migrations/` and the two template examples are intentional.
- [ ] Ensure the worktree contains no local database files or `target/` changes.
- [ ] Review the sync history contract and rehearse application-owned upgrades
      when deploying to existing databases.

## Verify

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
cargo test -p che-sync --no-default-features --features postgres
cargo test -p che-sync --no-default-features --features postgres --test runtime --test model_roles --test model_log_postgres -- --ignored
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-sync --example offline
cargo +1.89.0 check --workspace --all-targets --locked
cargo +1.89.0 check -p che-orm -p che-sync --all-targets --locked --no-default-features --features postgres
```

The ignored PostgreSQL tests require `TEST_POSTGRES_URL` and a live database
with permission to create isolated test schemas. Run the matching downstream
`che-rest` sync tests as documented in [releasing.md](releasing.md).

Before publishing, package all public crates together so Cargo verifies them
against the newly packaged dependencies rather than older registry versions:

```bash
cargo package -p che-orm-macros -p che-orm -p che-sync-protocol -p che-sync --locked
```

Run downstream derive and UUID regression tests from the packaged core. Until
the new macros are published, patch only that dependency to the unpacked macro
archive (use the release's macro and core versions in these paths):

```bash
cargo test --manifest-path target/package/che-orm-0.5.0/Cargo.toml \
  --config 'patch.crates-io.che-orm-macros.path="target/package/che-orm-macros-0.4.0"' \
  --test public_api_examples --test uuid --test runtime --test sqlite_datetime --test app_table
```

Inspect the macro archive with:

```bash
cargo package --manifest-path che-orm-macros/Cargo.toml --list
```

## Publish this candidate

1. Review the verification results and commit the prepared sources.
2. Dry-run and publish che-orm-macros 0.4.0, then che-orm 0.5.0, then
   che-sync 0.3.0, and finally che-rest 0.5.0. Wait for crates.io indexing
   between releases. Each publication requires separate explicit authorization.
3. Verify crate pages and docs.rs builds before creating the matching tags.
