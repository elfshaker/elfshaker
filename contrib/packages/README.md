# Native Linux packages

The package builders create host-native `elfshaker` packages:

- `deb/build.sh` creates a DEB for Debian and Ubuntu.
- `rpm/build.sh` creates an RPM for Fedora, Red Hat Enterprise Linux, Rocky
  Linux, AlmaLinux, and compatible distributions.

Each builder compiles the locked Cargo dependency set, installs the executable
and man pages, processes the executable using the native package toolchain, and
derives runtime library dependencies from the packaged ELF binary.

## Build a DEB

Install a current stable Rust toolchain and the Debian package tools. On Debian
and Ubuntu, the native prerequisites are:

```bash
sudo apt-get install build-essential dpkg-dev gzip
```

Then run:

```bash
./contrib/packages/deb/build.sh
sudo apt-get install ./target/packages/elfshaker_*.deb
```

## Build an RPM

Install a current stable Rust toolchain and the RPM package tools. On Fedora,
RHEL, and compatible distributions, the native prerequisites are:

```bash
sudo dnf install gcc rpm-build gzip
```

Then run:

```bash
./contrib/packages/rpm/build.sh
sudo dnf install ./target/packages/elfshaker-*.rpm
```

## Package an existing binary

Both builders accept `--binary PATH`. This skips the Cargo build and packages
the supplied executable:

```bash
./contrib/packages/deb/build.sh --binary ./elfshaker
./contrib/packages/rpm/build.sh --binary ./elfshaker
```

Native dynamically linked binaries inherit the compatibility baseline of the
system where they are built. Build on each target distribution release, or on
the oldest supported release in a compatible family. A static Linux release
binary can instead be supplied with `--binary` when one artifact must run
across distribution families. The builders package the current host
architecture and do not cross-compile.

Use `--output-dir`, `--release`, and the environment variables listed by each
script's `--help` output to integrate the builders into release automation.
