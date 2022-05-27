#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

# Install everything needed to build elfshaker packs for clang.

# Design considerations:
#
# * Idempotency. Fast when in the good state. A few minutes to run (LLVM clone slow though).
# * Assume we can put a bit of junk in $PWD, (and probably assume it's $HOME, sorry.)
# * Stuff gets installed into $ELFSHAKER_BIN_DIR which defaults to $HOME/.local/bin which assumed to be in the path.
# * We build ccache from source, since it has various fixes in it.
# * elfshaker is installed from binary by default (set ELFSHAKER_FROM_SOURCE for source build).

set -euo pipefail
[ "${MANYCLANGS_TRACE-}" ] && set -x

onexit() {
    STATUS=$?
    if [ $STATUS -eq 0 ];
    then
        return
    fi
    echo
    echo
    echo $0 had non-zero exit. Set MANYCLANGS_TRACE=1 to see trace.
    exit $STATUS
}

trap onexit EXIT

# Set to nonempty to build elfshaker, otherwise install binary.
ELFSHAKER_FROM_SOURCE=${ELFSHAKER_FROM_SOURCE-}
ELFSHAKER_BINARY_VERSION=${ELFSHAKER_BINARY_VERSION-v0.9.0}
MANYCLANGS_ARCH=${MANYCLANGS_ARCH-$(uname -m)}
ELFSHAKER_ARCH=${ELFSHAKER_ARCH-$MANYCLANGS_ARCH}

# If modifying this, beware of the contrib scripts.
MANYCLANGS_CLANG_VERSION=${MANYCLANGS_CLANG_VERSION-12}

# Installation path of elfshaker. Installed if not present.
ELFSHAKER_BIN=${ELFSHAKER_BIN-$HOME/.local/bin/elfshaker}
ELFSHAKER_BIN_DIR=$(dirname "${ELFSHAKER_BIN}")
mkdir -p "${ELFSHAKER_BIN_DIR}"

export PATH="${ELFSHAKER_BIN_DIR}:$PATH"

install_system_dependencies() {
    if { 
        command -v c++ &&
        command -v cc &&
        command -v clang-${MANYCLANGS_CLANG_VERSION} &&
        command -v cmake &&
        command -v git &&
        command -v go &&
        command -v jq &&
        command -v lld-${MANYCLANGS_CLANG_VERSION} &&
        command -v make &&
        command -v ninja &&
        command -v pv &&
        command -v python3 &&
        :
    } > /dev/null
    then
        return
    fi
    PKGS=(
        clang-${MANYCLANGS_CLANG_VERSION}
        cmake
        g++
        gcc
        git
        golang-go
        jq
        lld-${MANYCLANGS_CLANG_VERSION}
        make
        ninja-build
        pv
        python3
    )
    sudo apt update
    sudo apt install -y "${PKGS[@]}"
}

fetch_elfshaker_source() {
    if [ ! -d elfshaker ]
    then
        git clone https://github.com/elfshaker/elfshaker
    fi
}

install_elfshaker_build_dependencies() {
    if command -v elfshaker > /dev/null
    then
        return
    fi

    if ! command -v cargo &>/dev/null
    then
        echo "Rust unavailable, installing"
        echo
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -- -y
    fi

    if ! command -v cc &> /dev/null
    then
        sudo apt install -y gcc
    fi
}

build_elfshaker() {
    (cd elfshaker && cargo build --release && cp target/release/elfshaker "${ELFSHAKER_BIN}")
}

install_elfshaker() {
    if [ "$ELFSHAKER_FROM_SOURCE" ]
    then
        install_elfshaker_build_dependencies
        build_elfshaker
    else
        ELFSHAKER_TGZ=elfshaker_${ELFSHAKER_BINARY_VERSION}_${ELFSHAKER_ARCH}-unknown-linux-musl.tar.gz
        wget --continue \
            https://github.com/elfshaker/elfshaker/releases/download/${ELFSHAKER_BINARY_VERSION}/${ELFSHAKER_TGZ} \
            https://github.com/elfshaker/elfshaker/releases/download/${ELFSHAKER_BINARY_VERSION}/${ELFSHAKER_TGZ}.sha256sum
        sha256sum --check ${ELFSHAKER_TGZ}.sha256sum
        
        tar --extract --directory $HOME/.local/bin --file ${ELFSHAKER_TGZ} --strip-components=1 elfshaker/elfshaker
    fi
}

fetch_llvm() {
    if [ ! -d llvm-project ]
    then
        git clone --no-checkout https://github.com/llvm/llvm-project llvm-project
    fi
    (cd llvm-project && git fetch origin)
}

install_jobserver() {
    if ! command jobserver &> /dev/null
    then
        if [ ! -d jobclient ]
        then
            git clone https://github.com/olsner/jobclient
        fi
        (cd jobclient && make && cp jobserver "${ELFSHAKER_BIN_DIR}"/jobserver)
    fi
}

install_ninja_jobclient() {
    if ! command -v ninja-jobclient &> /dev/null
    then
        if [ ! -d ninja-jobclient ]
        then
            git clone --depth 1 https://github.com/stefanb2/ninja ninja-jobclient
            NINJA_JOBCLIENT_COMMIT=${NINJA_JOBCLIENT_COMMIT-2b9c81c0ec1226d8795e7725529f13be41eaa385}
            (cd ninja-jobclient && git fetch origin ${NINJA_JOBCLIENT_COMMIT} && git checkout -B ninja-jobclient FETCH_HEAD) || {
                echo "See https://github.com/ninja-build/ninja/pull/1140"
                echo "Commit ${NINJA_JOBCLIENT_COMMIT} failed to checkout."
                echo "It's possible it's gone, or something else is wrong."
                echo "Look for a branch which includes jobclient functionality"
                exit 1
            }
        fi
        (cd ninja-jobclient && python3 ./configure.py && ninja && cp ninja ${ELFSHAKER_BIN_DIR}/ninja-jobclient)
    fi
}

install_ccache_from_source() {
    if ! command -v ccache &> /dev/null
    then
        if [ ! -d ccache ]
        then
            git clone --depth 1 --single-branch --branch v4.6.1 https://github.com/ccache/ccache
        fi
        (cd ccache &&
         cmake -GNinja -Bbld -DCMAKE_BUILD_TYPE=Release -DZSTD_FROM_INTERNET=ON -DREDIS_STORAGE_BACKEND=OFF &&
         ninja -C bld &&
         cp bld/ccache "${ELFSHAKER_BIN_DIR}"/ccache)
    fi
}

install_compdb2line() {
    if ! command -v compdb2line
    then
        (cd elfshaker/contrib/compdb2line && GOBIN=${ELFSHAKER_BIN_DIR} go install -v)
    fi
}

main() {
    install_system_dependencies

    fetch_elfshaker_source # Needed for manyclangs build scripts.
    fetch_llvm

    install_elfshaker
    install_jobserver
    install_ninja_jobclient
    install_ccache_from_source
    install_compdb2line

    echo "Dependencies installed."
    echo
    echo "Now run, e.g:"
    echo 'sudo mkdir /build && sudo mount -t tmpfs -o size=100G none /build && sudo chown $(id -u):$(id -g) /build && cd /build && time PATH=$HOME/elfshaker/contrib:'"$ELFSHAKER_BIN_DIR"':$PATH GIT_DIR=${HOME}/llvm-project/.git $HOME/elfshaker/contrib/manyclangs-build-month '$(date "+%Y %m")
}

main