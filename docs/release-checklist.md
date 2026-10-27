# crates.io Release Checklist

Use this checklist for every release. [The release procedure](releasing.md)
records the planned versions, full verification commands and publication order
for all four public crates. `che-orm-examples` is not published.

## Prepare

- [ ] Decide versions for `che-orm`, `che-orm-macros`, `che-sync` and `che-sync-protocol`.
- [ ] Update versions and internal dependency requirements in their manifests.
- [ ] Update `CHANGELOG.md` with user-visible changes and migration notes.
- [ ] Review `README.md`, `README.en.md`, and `docs/en/` for API accuracy.
- [ ] Confirm example migration files in `che-orm-examples/migrations/` are intentional.
- [ ] Ensure the worktree contains no local database files or `target/` changes.
- [ ] Review the sync history contract and rehearse application-owned upgrades
      when deploying to existing databases.

## Verify

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo test -p che-orm --no-default-features --features postgres --test runtime -- --ignored
cargo test -p che-sync --no-default-features --features postgres
cargo test -p che-sync --no-default-features --features postgres --test runtime --test model_roles --test model_log_postgres -- --ignored
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
cargo run -p che-sync --example offline
cargo +1.88.0 check --workspace --all-targets --locked
cargo +1.88.0 check -p che-orm -p che-sync --all-targets --locked --no-default-features --features postgres
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
cargo test --manifest-path target/package/che-orm-0.3.2/Cargo.toml \
  --config 'patch.crates-io.che-orm-macros.path="target/package/che-orm-macros-0.3.2"' \
  --test public_api_examples --test uuid --test runtime --test sqlite_datetime --test app_table
```

Inspect the macro archive with:

```bash
cargo package --manifest-path che-orm-macros/Cargo.toml --list
```

## Publish

1. Publish `che-orm-macros`:

   ```bash
   cargo publish --manifest-path che-orm-macros/Cargo.toml
   ```

2. Wait until the new macro version is available in the crates.io index.
3. Package and publish the core crate:

   ```bash
   cargo package
    cargo test --manifest-path target/package/che-orm-0.3.2/Cargo.toml --test public_api_examples --test uuid --test runtime --test sqlite_datetime --test app_table
   cargo publish
   ```

4. For a versioned sync release, publish `che-sync-protocol`, wait for its registry availability, then publish
   `che-sync` (which also requires the new core version):

   ```bash
   cargo publish -p che-sync-protocol
   cargo publish -p che-sync
   ```

5. Verify the published documentation and create the matching Git tag.
   For 0.3.2, sync versions remain unchanged; publish only macros and core, run
   `python3 scripts/check-template.py --published`, and tag `v0.3.2`.

Do not publish if the package contents, generated SQL, or migration changes are
not understood.
