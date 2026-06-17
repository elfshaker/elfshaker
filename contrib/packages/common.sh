#!/usr/bin/env bash

# Shared helpers for the DEB and RPM package builders.

set -euo pipefail

packages_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH='' cd -- "$packages_dir/../.." && pwd)

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '%s\n' "$*" >&2
}

require_command() {
    command -v "$1" >/dev/null 2>&1 ||
        die "required command not found: $1"
}

absolute_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s/%s\n' "$PWD" "$1" ;;
    esac
}

cargo_package_version() {
    awk '
        /^\[package\]$/ {
            in_package = 1
            next
        }
        /^\[/ {
            if (in_package) {
                exit
            }
        }
        in_package && /^version[[:space:]]*=/ {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }
    ' "$repo_root/Cargo.toml"
}

build_elfshaker() {
    local supplied_binary=$1
    local cargo_command=${CARGO:-cargo}
    local target_dir
    local binary

    if [[ -n "$supplied_binary" ]]; then
        binary=$(absolute_path "$supplied_binary")
    else
        require_command "$cargo_command"
        note "Building elfshaker with Cargo"
        (
            cd "$repo_root"
            "$cargo_command" build --locked --release --bin elfshaker
        )

        target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
        if [[ "$target_dir" != /* ]]; then
            target_dir="$repo_root/$target_dir"
        fi

        binary="$target_dir/release/elfshaker"
    fi

    [[ -f "$binary" ]] || die "elfshaker binary not found: $binary"
    [[ -x "$binary" ]] || die "elfshaker binary is not executable: $binary"
    "$binary" --version >/dev/null
    printf '%s\n' "$binary"
}

install_program_and_manpages() {
    local binary=$1
    local package_root=$2
    local manpage
    local installed_manpages=0

    require_command install
    require_command gzip

    install -Dm0755 "$binary" "$package_root/usr/bin/elfshaker"

    for manpage in "$repo_root"/man/*.1; do
        [[ -f "$manpage" ]] || continue
        install -Dm0644 \
            "$manpage" \
            "$package_root/usr/share/man/man1/$(basename "$manpage")"
        gzip -9n "$package_root/usr/share/man/man1/$(basename "$manpage")"
        installed_manpages=$((installed_manpages + 1))
    done

    ((installed_manpages > 0)) ||
        die "no man pages found under $repo_root/man"
}

make_work_dir() {
    local work_dir

    work_dir=$(mktemp -d)
    printf '%s\n' "$work_dir"
}

remove_work_dir() {
    local work_dir=$1

    [[ -n "$work_dir" && -d "$work_dir" ]] || return 0
    chmod -R u+w "$work_dir"
    rm -r -- "$work_dir"
}
