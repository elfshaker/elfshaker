# `elfshaker`
elfshaker is a low-footprint, high-performance version control system fine-tuned for binaries.

- elfshaker is a CLI tool written in the [Rust programming language](https://www.rust-lang.org/).

- It stores snapshots of directories into highly-compressed pack files and provides fast on-demand access to the stored files. It is particularly good for storing lots of similar files.

- It is also the storage system used by the [manyclangs project](https://github.com/elfshaker/manyclangs), a parent project which accelerates bisection of [LLVM](https://llvm.org/) by a factor of 60x!
This is done by extracting builds of LLVM on-demand from locally stored elfshaker packs, each of which contains ~1,800 builds and is about 100 MiB in size, even though the full originals would take TiBs to store! Extracting a single builds takes 2-4s on modern hardware.

## Getting started

*See our [Installation guide](docs/users/installing.md) for instructions.*

## System Compatibility

The following platforms are used for our CI tests:

- Ubuntu 20.04 LTS

But we aim to support all popular Linux platforms, macOS and Windows in production.

We officially support the following architectures:
- AArch64 (release tag is `aarch64`)
- x86-64 (release tag is `x86_64`)

## Current Status

elfshaker is production ready! The file format and directory structure is stable.
Packs files created with the current elfshaker version will remain compatible with future versions.

## Documentation

*See our [Usage guide](docs/users/usage.md) for instructions.*

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
