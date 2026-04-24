# Releasing elfshaker

Use this checklist when preparing a tagged release.

1. Update `Cargo.toml` to the release version and refresh `Cargo.lock` if
   needed.
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

5. Create and push an annotated tag, for example `v1.0.0`.
6. Create the GitHub release from the tag and use the matching section of
   `CHANGELOG.md` as the release notes.
7. Confirm release assets and `.sha256sum` files are attached for the supported
   Linux, Windows, and macOS targets.
