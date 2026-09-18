# Static GNU compiler kit

The AArch64 Linux kit provides Clang, LLD, LLVM binary tools, CMake, Ninja
and ccache as static host executables. Generated programs use the bundled
GNU target headers and libraries: glibc 2.31 and GCC 9.3 libstdc++. This
keeps the build tools independent of host distribution libraries while
retaining a defined GNU ABI for generated programs.

Build the pinned baseline on AArch64 Linux:

```sh
nix build .#toolchain-baseline
```

The output is a relocatable directory. Copy its complete contents to the
consumer machine, then configure a project with its CMake toolchain file:

```sh
cmake -S . -B build -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE="$KIT/share/cmake/gnu-toolchain.cmake"
```

For relocation acceptance, copy the kit to an AArch64 Linux host without
`/nix/store`, with Bash, Python, GNU coreutils, readelf and nm available:

```sh
bash "$KIT/share/toolchain/test-kit.sh" "$KIT" "$PWD/kit-acceptance"
```

The work directory must be new. Acceptance verifies the complete checksum
manifest, static host tools, contained symlinks, C/C++ shared-library and
exception support, generated-program ABI requirements and ccache reuse.

The build-free `toolchainEval.aarch64-linux` flake output reports pinned
inputs, tool versions and derivations. The `toolchain-eval` flake check
validates the static compiler and GNU runtime configuration during evaluation.
