# Contributors' guide

Contributions are welcome and appreciated. Any work you submit is considered to be under the Apache 2.0 software license (see [LICENSE](../../LICENSE)), unless otherwise stated.

Please make sure to go through the following steps before you open a pull request that edits the source code.

Release preparation is covered in the [releasing guide](releasing.md).

## Building

*See how to build from source in our [Installation guide](../users/installing.md#building-from-source).*

## License header
Please ensure that any new source files contain the following header:
```
//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.
```

## Before opening a pull request

1. Make sure all unit tests are passing.
```bash
cargo test
```

2. Run the system tests twice: once with the verification pack and once with a fresh pack.
```bash
./test-scripts/check.sh ./target/release/elfshaker ./test-scripts/artifacts/verification.pack
pack=$(contrib/create-test-pack ./target/release/elfshaker 5 64)
./test-scripts/check.sh ./target/release/elfshaker "$pack"
```

**Some tests may fail to run if you are running on a non-standard Linux environment (like if you don't have `/dev/shm`).**

3. Run clippy to ensure your code is formatted appropriately.
```bash
cargo clippy
```

## After opening a pull request
Go to the `Checks` tab on your new pull request and make sure all tests are passing and there are no warnings.

## CI during PR iteration

Rust build, unit and integration tests, formatting, and Clippy run before
Nix builds start. Nix builds run when both Rust jobs pass.

To skip Nix builds while iterating on a PR, add this trailer to the end of
the latest PR head commit message, separated from the body by a blank line:

```text
CI-Skip-Nix: true
```

The trailer applies to that head commit. A new head commit without the
trailer restores Nix builds. Rust tests, formatting, and Clippy run with
either setting. Pushes to `main` and `test-ci` run Nix after the Rust jobs
pass regardless of the trailer. Release builds also run independently of
the trailer.
