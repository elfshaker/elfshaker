#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=contrib/packages/common.sh
source "$script_dir/../common.sh"

usage() {
    cat <<'EOF'
Build a Debian package for the current host.

Usage:
  contrib/packages/deb/build.sh [options]

Options:
  --binary PATH      Package an existing elfshaker binary instead of building.
  --output-dir DIR   Write the package to DIR (default: target/packages).
  --release VALUE    Debian packaging revision (default: 1).
  -h, --help         Show this help.

Environment:
  CARGO               Cargo executable to use.
  CARGO_TARGET_DIR    Cargo target directory.
  PACKAGE_MAINTAINER  Debian Maintainer field.
  PACKAGE_RELEASE     Debian packaging revision.
  PACKAGE_VERSION     Override the version read from Cargo.toml.
EOF
}

binary=
output_dir="$repo_root/target/packages"
package_release=${PACKAGE_RELEASE:-1}

while (($# > 0)); do
    case "$1" in
        --binary)
            (($# >= 2)) || die "--binary requires a path"
            binary=$2
            shift 2
            ;;
        --output-dir)
            (($# >= 2)) || die "--output-dir requires a directory"
            output_dir=$2
            shift 2
            ;;
        --release)
            (($# >= 2)) || die "--release requires a value"
            package_release=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[[ "$package_release" =~ ^[A-Za-z0-9.+~]+$ ]] ||
    die "invalid Debian package release: $package_release"

require_command dpkg
require_command dpkg-deb
require_command dpkg-shlibdeps
require_command strip

version=${PACKAGE_VERSION:-$(cargo_package_version)}
[[ -n "$version" ]] || die "could not read package version from Cargo.toml"
debian_version=${version/-/\~}-$package_release
architecture=$(dpkg --print-architecture)
maintainer=${PACKAGE_MAINTAINER:-"Peter Waller <peter.waller@arm.com>"}
output_dir=$(absolute_path "$output_dir")

elfshaker_binary=$(build_elfshaker "$binary")
work_dir=$(make_work_dir)
trap 'remove_work_dir "$work_dir"' EXIT

package_root="$work_dir/root"
mkdir -p "$package_root/DEBIAN"
install_program_and_manpages "$elfshaker_binary" "$package_root"
strip --strip-unneeded "$package_root/usr/bin/elfshaker"

install -Dm0644 \
    "$repo_root/LICENSE" \
    "$package_root/usr/share/doc/elfshaker/copyright"
install -Dm0644 \
    "$repo_root/README.md" \
    "$package_root/usr/share/doc/elfshaker/README.md"
install -Dm0644 \
    "$repo_root/CONTRIBUTORS" \
    "$package_root/usr/share/doc/elfshaker/CONTRIBUTORS"
install -Dm0644 \
    "$repo_root/CHANGELOG.md" \
    "$package_root/usr/share/doc/elfshaker/changelog"
gzip -9n "$package_root/usr/share/doc/elfshaker/changelog"

mkdir -p "$work_dir/debian"
cat >"$work_dir/debian/control" <<EOF
Source: elfshaker
Section: utils
Priority: optional
Maintainer: $maintainer
Standards-Version: 4.6.2
Homepage: https://github.com/elfshaker/elfshaker

Package: elfshaker
Architecture: any
Description: low-footprint version control system for binaries
 elfshaker stores and restores snapshots of working directories while
 deduplicating similar binary files into compact pack files.
EOF

dependency_output=$(
    cd "$work_dir"
    dpkg-shlibdeps -O "$package_root/usr/bin/elfshaker"
)
dependencies=$(
    printf '%s\n' "$dependency_output" |
        sed -n 's/^shlibs:Depends=//p'
)

installed_size=$(du -sk "$package_root/usr" | awk '{print $1}')
{
    printf 'Package: elfshaker\n'
    printf 'Version: %s\n' "$debian_version"
    printf 'Section: utils\n'
    printf 'Priority: optional\n'
    printf 'Architecture: %s\n' "$architecture"
    printf 'Maintainer: %s\n' "$maintainer"
    printf 'Installed-Size: %s\n' "$installed_size"
    printf 'Homepage: https://github.com/elfshaker/elfshaker\n'
    if [[ -n "$dependencies" ]]; then
        printf 'Depends: %s\n' "$dependencies"
    fi
    cat <<'EOF'
Description: low-footprint version control system for binaries
 elfshaker stores and restores snapshots of working directories while
 deduplicating similar binary files into compact pack files.
EOF
} >"$package_root/DEBIAN/control"

mkdir -p "$output_dir"
package_path="$output_dir/elfshaker_${debian_version}_${architecture}.deb"
dpkg-deb --root-owner-group --build "$package_root" "$package_path"

[[ $(dpkg-deb --field "$package_path" Package) == elfshaker ]] ||
    die "built package has an unexpected package name"
[[ $(dpkg-deb --field "$package_path" Version) == "$debian_version" ]] ||
    die "built package has an unexpected version"

printf '%s\n' "$package_path"
