# `elfshaker`: 400 GiB -> 100 MiB, with 1s access time

elfshaker is a low-footprint, high-performance version control system fine-tuned for binaries.

- elfshaker is a CLI tool written in the [Rust programming language](https://www.rust-lang.org/).

- It stores snapshots of directories into highly-compressed pack files and provides fast on-demand access to the stored files. It is particularly good for storing lots of similar files, for example object files from an incremental build.

- It allows few-second access to any commit of clang with the [manyclangs project](https://github.com/elfshaker/manyclangs). For example, this accelerates bisection of [LLVM](https://llvm.org/) by a factor of 60x! This is done by extracting builds of LLVM on-demand from locally stored elfshaker packs, each of which contains ~1,800 builds and is about 100 MiB in size, even though the full originals would take TiBs to store! Extracting a single builds takes 2-4s on modern hardware.

## [Installation guide](docs/users/installing.md)

## [Usage guide](docs/users/usage.md)

## Quickstart

1. Consult our [installation](docs/users/installing.md) and [usage guide](docs/users/usage.md), make sure you know what you're doing.
2. `elfshaker store <snapshot>` -- capture the state of the current working directory into a named snapshot `<snapshot>`.
3. `elfshaker pack <pack name>` -- capture all 'loose' snapshots into a single pack file (this is what gets you the compression win).
4. `elfshaker extract <snapshot>` -- restore the state of a previous snapshot into the current working directory.

For more detail, take a look at our [workflow documentation](https://github.com/elfshaker/elfshaker/blob/main/docs/users/usage.md#workflow).

## System Compatibility

The following platforms are used for our CI tests:

- Ubuntu 20.04 LTS

But we aim to support all popular Linux platforms, macOS and Windows in production.

We officially support the following architectures:
- AArch64
- x86-64

## Current Status

The file format and directory structure is stable. We intend that pack files created with the current elfshaker version will remain compatible with future versions. Please kick the tyres and do your own validation, and file bugs if you find any. We have done a fair amount of validation for our use cases but there may be things we haven't needed yet, so please start a discussion and file issues/patches.


## Contributing

*Contributions are highly-appreciated.*
Refer to our [Contributing guide](docs/contributors/contributing.md).

<!-- TODO(veselink1): 🛈 View the [elfshaker API reference](https://elfshaker.github.io/docs). -->

## Contact

The best way to reach us to join the [elfshaker/community](https://gitter.im/elfshaker/community) on Gitter.
The original authors of elfshaker are Peter Waller ([@peterwaller-arm](https://github.com/peterwaller-arm)) \<peter.waller@arm.com\> and Veselin Karaganev ([@veselink1](https://github.com/veselink1)) \<veselin.karaganev@arm.com\> and you may also contact us via email.

## Security

Refer to our [Security policy](SECURITY.md).

## License

elfshaker is licensed under the [Apache License 2.0](LICENSE).
