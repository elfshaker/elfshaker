# Installing elfshaker

## Using pre-built binaries
1. Grab the latest `elfshaker-Release-[arch].tar.gz` from [Releases](https://github.com/elfshaker/elfshaker/releases).

2. Extract to any location you would like and add the directory to your path:
```bash
mkdir -p ~/.local/opt && mkdir -p ~/.local/bin && tar -xf "elfshaker_v0.9.0_$(uname -m)-unknown-linux-musl.tar.gz" -C ~/.local/opt && ln -s ~/.local/opt/elfshaker/elfshaker ~/.local/bin/elfshaker
```
<details>
  <summary>Formatted version of the above one-liner</summary>

```bash
mkdir -p ~/.local/opt
mkdir -p ~/.local/bin
tar -xf "elfshaker_v0.9.0_$(uname -m)-unknown-linux-musl.tar.gz" -C ~/.local/opt
ln -s ~/.local/opt/elfshaker/elfshaker ~/.local/bin/elfshaker
```
</details>

**Please, make sure to add `~/.local/bin` to your PATH environment variable.**

*[How to add directory to PATH?](https://askubuntu.com/questions/60218/how-to-add-a-directory-to-the-path)*

## Building from source

elfshaker is written in Rust, so you will need to install the Rust build system.
- Install using rustup from https://www.rust-lang.org/tools/install
- Install a stable toolchain (1.67 known to work, YMMV for older versions):
  - ```bash
    rustup install stable
    ```
- Build elfshaker in Release mode
  - ```bash
    cargo +stable build --release --bin elfshaker
    ```
  - ```bash
    ./target/release/elfshaker --help
    ```
