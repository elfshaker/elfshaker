# Installing elfshaker

## Using pre-built binaries
1. Grab the latest release archive for your platform from [Releases](https://github.com/elfshaker/elfshaker/releases). Linux release archives include the release version and target triple, for example `elfshaker_v1.0.0_x86_64-unknown-linux-musl.tar.gz` or `elfshaker_v1.0.0_aarch64-unknown-linux-musl.tar.gz`.

2. Extract to any location you would like and add the directory to your path:
```bash
version=v1.0.0; archive="elfshaker_${version}_$(uname -m)-unknown-linux-musl.tar.gz"; mkdir -p ~/.local/opt ~/.local/bin && tar -xf "$archive" -C ~/.local/opt && ln -sf ~/.local/opt/elfshaker/elfshaker ~/.local/bin/elfshaker
```
<details>
  <summary>Formatted version of the above one-liner</summary>

```bash
version=v1.0.0
archive="elfshaker_${version}_$(uname -m)-unknown-linux-musl.tar.gz"
mkdir -p ~/.local/opt
mkdir -p ~/.local/bin
tar -xf "$archive" -C ~/.local/opt
ln -sf ~/.local/opt/elfshaker/elfshaker ~/.local/bin/elfshaker
```
</details>

**Please, make sure to add `~/.local/bin` to your PATH environment variable.**

*[How to add directory to PATH?](https://askubuntu.com/questions/60218/how-to-add-a-directory-to-the-path)*

## Building from source

elfshaker is written in Rust, so you will need to install the Rust build system.
1. Install using rustup from https://www.rust-lang.org/tools/install
2. Install a stable toolchain:
```bash
rustup install stable
```
3. Download the source code of elfshaker
```bash
git clone https://github.com/elfshaker/elfshaker.git
cd elfshaker
```
4. Build elfshaker in Release mode
```bash
cargo +stable build --release --bin elfshaker
```
5. You should be good to go! Run the freshly built executable
```bash
./target/release/elfshaker --help
```
