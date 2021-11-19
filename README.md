# `elfshaker`

## 400 GiB -> 100 MiB, with 1s access time†; [when applied to clang builds](https://github.com/elfshaker/manyclangs).

elfshaker is a low-footprint, high-performance version control system fine-tuned for binaries.

- elfshaker is a CLI tool written in the [Rust programming language](https://www.rust-lang.org/).

- It stores snapshots of directories into highly-compressed pack files and provides fast on-demand access to the stored files. It is particularly good for storing lots of similar files, for example object files from an incremental build.

- It allows few-second access to any commit of clang with the [manyclangs project](https://github.com/elfshaker/manyclangs). For example, this accelerates bisection of [LLVM](https://llvm.org/) by a factor of 60x! This is done by extracting builds of LLVM on-demand from locally stored elfshaker packs, each of which contains ~1,800 builds and is about 100 MiB in size, even though the full originals would take TiBs to store! Extracting a single builds takes 2-4s on modern hardware.

## †Applicability

Or, "how on earth do you get such a phenomenal result?".

It works particularly well for our [presented use case](https://github.com/elfshaker/manyclangs) because it has these properties:

* There are many files,
* Most of them don't change very often,
* When they do change, the deltas of the binaries are not huge.

We achieve this in manyclangs by compiling object code with the `-ffunction-sections` and `-fdata-sections` compiler flags. This has the effect that if you 'insert' a function into a translation unit, the insertion does not cause all of the addresses to change across the whole object file. If you looked at the binary delta on the executable from such a change, it will be large, because all of the absolute addresses after the insertion will change, and references to those addresses will change. These address changes are not handled well by compression algorithms, resulting in a poor compression ratio. The effect of this is large: if you compress many revisions of clang executables together, you will see a compression ratio of something like 20%. This is pretty good! But elfshaker achieves a ratio of something closer to 10,000x (or 0.01%, amortized across many builds).


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
