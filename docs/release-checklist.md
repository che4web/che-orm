# crates.io Release Checklist

Use this checklist for every release. [The release procedure](releasing.md)
records the planned versions, full verification commands and publication order
for all four public crates. `che-orm-examples` is not published.

The current preparation targets **che-sync 0.1.3**, followed by downstream
**che-rest 0.3.4**. Core **0.3.3**, macros **0.3.2**, protocol **0.1.0** and
sync **0.1.2** are already published (registry checked 2026-11-06). Core source
is unchanged since its published tag. Keep those versions and tags intact.

See [the current verification report](release-verification-2026-11-06.md) for
completed checks, artifacts and the remaining registry publication gate.

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
cargo test --manifest-path target/package/che-orm-0.3.3/Cargo.toml \
  --config 'patch.crates-io.che-orm-macros.path="target/package/che-orm-macros-0.3.2"' \
  --test public_api_examples --test uuid --test runtime --test sqlite_datetime --test app_table
```

Inspect the macro archive with:

```bash
cargo package --manifest-path che-orm-macros/Cargo.toml --list
```

## Publish this candidate

1. Review the verification report linked in [releasing.md](releasing.md), commit
   the prepared sources, and ensure the worktree is clean.
2. From the ORM workspace:

   ```bash
   cargo publish --dry-run -p che-sync --locked
   cargo publish -p che-sync --locked
   ```

3. Wait for che-sync 0.1.3 to appear in the registry index. Then from che-rest:

   ```bash
   cargo publish --dry-run --locked
   cargo publish --locked
   ```

4. Verify crate pages and docs.rs builds before creating new tags
   `che-sync-v0.1.3` and REST `v0.3.4`. Do not move old tags.

Preparing archives or running a dry run does not publish a crate. Before sync
0.1.3 is indexed, the ordinary REST dry run cannot resolve that dependency;
use the sibling archive verifier for preparation. Repeat the registry dry run
after sync publication. Do not republish the unchanged core/macros/protocol.
