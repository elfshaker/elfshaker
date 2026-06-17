# Releasing elfshaker

Use this checklist when preparing a tagged release.

1. Update `Cargo.toml` to the release version and refresh `Cargo.lock` if
   needed. Regenerate the manual pages after changing the version or CLI
   definitions, and include the updated pages in the release commit:

```bash
cargo run --example generate-manpages -- man
cargo test --locked generated_manpages_are_current
```

   The freshness test compares every committed page with generated output,
   including the embedded package version. It also runs under `cargo test`
   in CI.
2. Update `CHANGELOG.md`, replacing `Unreleased` with the release date.
3. Run the local release checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-features -- -D warnings
cargo test
cargo build --release --bin elfshaker
./test-scripts/check.sh ./target/release/elfshaker ./test-scripts/artifacts/verification.pack
pack=$(contrib/create-test-pack ./target/release/elfshaker 5 64)
./test-scripts/check.sh ./target/release/elfshaker "$pack"
```

4. Build release artifacts with Nix on the supported release platforms:

```bash
nix build --log-lines 1000 .#release
```

5. Run a non-publishing workflow test from **Actions → Release → Run workflow**.
   The manual trigger appears after this workflow definition is present on the
   default branch. Select the release branch or tag and leave the scope as
   `linux` to build the Linux archives, DEBs, and RPMs. Download the
   `release-ubuntu-22.04` workflow artifact and test the packages. Select `all`
   when the macOS artifacts also need validation. Manual runs retain workflow
   artifacts for seven days and never upload to a GitHub Release.
6. Create and push an annotated tag, for example `v1.0.0`.
7. Create the GitHub release from the tag and use the matching section of
   `CHANGELOG.md` as the release notes.
8. Confirm release assets and `.sha256sum` files are attached for the supported
   Linux, Windows, macOS, DEB, and RPM targets.
