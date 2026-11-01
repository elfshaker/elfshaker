# Contributor guidance

## Local validation

Before marking a change ready for review or pushing it, run these checks
on the exact revision being submitted:

```sh
cargo fmt --all -- --check
cargo clippy --all-features -- -D warnings
cargo test --locked
```

Use a current stable Rust toolchain with Clippy and rustfmt installed,
as CI does. Record the toolchain version with validation results. A pass
with an older toolchain does not establish that current CI passes.

Consult the CI workflows for additional checks affected by the change,
including integration checks and generated output. Resolve failures or
report the blocked check explicitly before calling the change ready.

## Commits

Keep each commit understandable on its own. State the outcome in its
title and explain intent and material validation in its body. Separate
the title and body with a blank line; wrap both at 72 characters.
