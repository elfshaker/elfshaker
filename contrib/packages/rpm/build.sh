#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=contrib/packages/common.sh
source "$script_dir/../common.sh"

usage() {
    cat <<'EOF'
Build an RPM package for the current host.

Usage:
  contrib/packages/rpm/build.sh [options]

Options:
  --binary PATH      Package an existing elfshaker binary instead of building.
  --architecture A   Package architecture (default: current host).
  --preserve-binary  Do not strip the packaged executable.
  --output-dir DIR   Write the package to DIR (default: target/packages).
  --release VALUE    RPM packaging revision (default: 1).
  -h, --help         Show this help.

Environment:
  CARGO               Cargo executable to use.
  CARGO_TARGET_DIR    Cargo target directory.
  PACKAGE_RELEASE     RPM packaging revision.
  PACKAGE_VERSION     Override the version read from Cargo.toml.
EOF
}

sanitize_rpm_component() {
    printf '%s' "$1" | sed 's/[^A-Za-z0-9._+]/./g'
}

binary=
package_architecture=
preserve_binary=false
output_dir="$repo_root/target/packages"
package_release=${PACKAGE_RELEASE:-1}

while (($# > 0)); do
    case "$1" in
        --binary)
            (($# >= 2)) || die "--binary requires a path"
            binary=$2
            shift 2
            ;;
        --architecture)
            (($# >= 2)) || die "--architecture requires a value"
            package_architecture=$2
            shift 2
            ;;
        --preserve-binary)
            preserve_binary=true
            shift
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

[[ "$package_release" =~ ^[A-Za-z0-9._+]+$ ]] ||
    die "invalid RPM package release: $package_release"

require_command rpmbuild
require_command rpm
require_command tar

version=${PACKAGE_VERSION:-$(cargo_package_version)}
[[ -n "$version" ]] || die "could not read package version from Cargo.toml"

version_without_metadata=${version%%+*}
build_metadata=
if [[ "$version" == *+* ]]; then
    build_metadata=${version#*+}
fi

rpm_version=${version_without_metadata%%-*}
prerelease=
if [[ "$version_without_metadata" == *-* ]]; then
    prerelease=${version_without_metadata#*-}
fi

rpm_release=$package_release
if [[ -n "$prerelease" ]]; then
    rpm_release="0.$(sanitize_rpm_component "$prerelease").$package_release"
fi
if [[ -n "$build_metadata" ]]; then
    rpm_release="$rpm_release.$(sanitize_rpm_component "$build_metadata")"
fi

host_architecture=$(rpm --eval '%{_arch}')
[[ -n "$host_architecture" && "$host_architecture" != '%{_arch}' ]] ||
    die "could not determine RPM architecture"
architecture=${package_architecture:-"$host_architecture"}
output_dir=$(absolute_path "$output_dir")

if [[ "$architecture" != "$host_architecture" ]]; then
    die "RPM packages must be built under their target architecture"
fi

elfshaker_binary=$(build_elfshaker "$binary")
work_dir=$(make_work_dir)
trap 'remove_work_dir "$work_dir"' EXIT

package_root="$work_dir/root"
install_program_and_manpages "$elfshaker_binary" "$package_root"
if [[ "$preserve_binary" == false ]]; then
    require_command strip
    strip --strip-unneeded "$package_root/usr/bin/elfshaker"
fi
install -Dm0644 \
    "$repo_root/LICENSE" \
    "$package_root/usr/share/licenses/elfshaker/LICENSE"
install -Dm0644 \
    "$repo_root/README.md" \
    "$package_root/usr/share/doc/elfshaker/README.md"
install -Dm0644 \
    "$repo_root/CONTRIBUTORS" \
    "$package_root/usr/share/doc/elfshaker/CONTRIBUTORS"
install -Dm0644 \
    "$repo_root/CHANGELOG.md" \
    "$package_root/usr/share/doc/elfshaker/CHANGELOG.md"

top_dir="$work_dir/rpmbuild"
mkdir -p \
    "$top_dir/BUILD" \
    "$top_dir/BUILDROOT" \
    "$top_dir/RPMS" \
    "$top_dir/SOURCES" \
    "$top_dir/SPECS" \
    "$top_dir/SRPMS"

payload="$top_dir/SOURCES/elfshaker-payload.tar.gz"
tar -C "$package_root" -czf "$payload" .

spec="$top_dir/SPECS/elfshaker.spec"
cat >"$spec" <<EOF
%global debug_package %{nil}
%global _build_id_links none
%global source_date_epoch_from_changelog 0
%global __os_install_post %{nil}

Name:           elfshaker
Version:        $rpm_version
Release:        $rpm_release
Summary:        Low-footprint version control system for binaries
License:        Apache-2.0
URL:            https://github.com/elfshaker/elfshaker
Source0:        elfshaker-payload.tar.gz
BuildArch:      $architecture
AutoReqProv:    yes

%description
elfshaker stores and restores snapshots of working directories while
deduplicating similar binary files into compact pack files.

%prep

%build

%install
mkdir -p %{buildroot}
tar -xzf %{SOURCE0} -C %{buildroot}

%files
%license /usr/share/licenses/elfshaker/LICENSE
%doc /usr/share/doc/elfshaker/CHANGELOG.md
%doc /usr/share/doc/elfshaker/CONTRIBUTORS
%doc /usr/share/doc/elfshaker/README.md
/usr/bin/elfshaker
/usr/share/man/man1/*.1*
EOF

rpmbuild \
    --define "_topdir $top_dir" \
    --target "$architecture" \
    -bb "$spec"

package_path=$(
    find "$top_dir/RPMS" -type f -name 'elfshaker-*.rpm' -print -quit
)
[[ -n "$package_path" ]] || die "rpmbuild did not produce an RPM"

mkdir -p "$output_dir"
final_path="$output_dir/$(basename "$package_path")"
cp "$package_path" "$final_path"

[[ $(rpm -qp --queryformat '%{NAME}' "$final_path") == elfshaker ]] ||
    die "built package has an unexpected package name"
[[ $(rpm -qp --queryformat '%{VERSION}' "$final_path") == "$rpm_version" ]] ||
    die "built package has an unexpected version"

printf '%s\n' "$final_path"
