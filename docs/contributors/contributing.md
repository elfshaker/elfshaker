# Contributors' guide

Contributions are welcome and appreciated. Any work you submit is considered to be under the Apache 2.0 software license (see [LICENSE](../../LICENSE)), unless otherwise stated.

Please make sure to go through the following steps before you open a pull request that edits the source code.

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
./test-scripts/check.sh ./target/release/elfshaker $(contrib/create-test-pack ./target/release/elfshaker 5 64)
```

**Some tests may fail to run if you are running on a non-standard Linux environment (like if you don't have `/dev/shm`).**

3. Run clippy to ensure your code is formatted appropriately.
```bash
cargo clippy
```

## After opening a pull request
Go to the `Checks` tab on your new pull request and make sure all tests are passing and there are no warnings.
