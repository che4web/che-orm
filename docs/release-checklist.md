# crates.io Release Checklist

Use this checklist for every release. `che-orm-macros` must be published before
the core crate because `che-orm` depends on its released version.

## Prepare

- [ ] Decide the version for `che-orm` and `che-orm-macros`.
- [ ] Update both versions and the dependency requirement in `Cargo.toml`.
- [ ] Update `CHANGELOG.md` with user-visible changes and migration notes.
- [ ] Review `README.md`, `README.en.md`, and `docs/en/` for API accuracy.
- [ ] Confirm example migration files in `che-orm-examples/migrations/` are intentional.
- [ ] Ensure the worktree contains no local database files or `target/` changes.

## Verify

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p che-orm --no-default-features --features postgres
cargo doc --workspace --no-deps
cargo run -p che-orm-examples --bin manage -- schema
cargo package --manifest-path che-orm-macros/Cargo.toml
```

Before publishing, package both crates together so Cargo verifies the core
against the newly packaged macros rather than an older registry version:

```bash
cargo package -p che-orm-macros -p che-orm
```

Run downstream derive and UUID regression tests from the packaged core. Until
the new macros are published, patch only that dependency to the unpacked macro
archive (use the release's macro and core versions in these paths):

```bash
cargo test --manifest-path target/package/che-orm-0.3.0/Cargo.toml \
  --config 'patch.crates-io.che-orm-macros.path="target/package/che-orm-macros-0.3.0"' \
  --test public_api_examples --test uuid --test runtime
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
   cargo test --manifest-path target/package/che-orm-0.3.0/Cargo.toml --test public_api_examples --test uuid --test runtime
   cargo publish
   ```

4. Verify the published documentation and create the matching Git tag.

Do not publish if the package contents, generated SQL, or migration changes are
not understood.
