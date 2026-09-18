{ pkgs, gnu, llvm, cmake, ninja, ccache }:
let
  inherit (pkgs) lib;
  clang = llvm.libclang;
  major = lib.versions.major clang.version;
  cmakeData = "share/cmake-${lib.versions.majorMinor cmake.version}";
  libc = gnu.glibc;
  gcc = gnu.gcc.cc;
  target = "aarch64-unknown-linux-gnu";
in pkgs.runCommand "toolchain-baseline-${clang.version}-gnu231" {
  nativeBuildInputs = [ pkgs.binutils pkgs.perl ];
  passthru = { inherit clang llvm cmake ninja ccache; };
} ''
  set -euo pipefail
  mkdir -p "$out/bin" "$out/lib/clang" "$out/share/cmake" \
    "$out/share/toolchain" \
    "$out/gnu/include" "$out/gnu/lib" "$out/gnu/gcc/include" \
    "$out/gnu/gcc/lib/gcc/${target}"
  install -m755 ${clang}/bin/clang "$out/bin/clang-${major}"
  for name in clang clang++ clang-cpp; do
    ln -s clang-${major} "$out/bin/$name"
  done
  for name in llvm-ar llvm-ranlib llvm-nm llvm-objcopy llvm-strip; do
    install -m755 ${llvm.llvm}/bin/$name "$out/bin/$name"
  done
  install -m755 ${llvm.lld}/bin/ld.lld "$out/bin/ld.lld"
  cp -aL ${clang.lib}/lib/clang/${major} "$out/lib/clang/"
  cp -aL ${libc.dev}/include/. "$out/gnu/include/"
  cp -aL ${libc}/lib/. "$out/gnu/lib/"
  chmod u+w "$out/gnu/lib"
  cp -aL ${gcc.lib}/lib/. "$out/gnu/lib/"
  cp -aL ${gcc}/include/c++ "$out/gnu/gcc/include/"
  cp -aL ${gcc}/lib/gcc/${target}/${gcc.version} "$out/gnu/gcc/lib/gcc/${target}/"
  chmod -R u+w "$out/gnu"
  for script in libc.so libm.so; do
    if head -c 100 "$out/gnu/lib/$script" | grep -q 'GNU ld script'; then
      sed -i 's@${libc}/lib/@/lib/@g' "$out/gnu/lib/$script"
      if grep -q /nix/store "$out/gnu/lib/$script"; then exit 1; fi
    fi
  done
  cat > "$out/bin/clang.cfg" <<'EOF'
--target=${target}
--sysroot=<CFGDIR>/../gnu
--gcc-toolchain=<CFGDIR>/../gnu/gcc
-resource-dir=<CFGDIR>/../lib/clang/${major}
--stdlib=libstdc++
--rtlib=libgcc
--unwindlib=libgcc
-shared-libgcc
-idirafter<CFGDIR>/../gnu/include
-B<CFGDIR>/../gnu/lib
-L<CFGDIR>/../gnu/lib
-fuse-ld=<CFGDIR>/ld.lld
-Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1
EOF
  cp "$out/bin/clang.cfg" "$out/bin/clang++.cfg"
  cp "$out/bin/clang.cfg" "$out/bin/clang-cpp.cfg"
  cp ${./verify-kit.py} "$out/share/toolchain/verify-kit.py"
  cp ${./test-kit.sh} "$out/share/toolchain/test-kit.sh"
  cat > "$out/share/cmake/gnu-toolchain.cmake" <<'EOF'
get_filename_component(KIT_ROOT "''${CMAKE_CURRENT_LIST_DIR}/../.." ABSOLUTE)
set(CMAKE_C_COMPILER "''${KIT_ROOT}/bin/clang")
set(CMAKE_CXX_COMPILER "''${KIT_ROOT}/bin/clang++")
set(CMAKE_AR "''${KIT_ROOT}/bin/llvm-ar")
set(CMAKE_RANLIB "''${KIT_ROOT}/bin/llvm-ranlib")
set(CMAKE_SYSROOT "''${KIT_ROOT}/gnu")
set(CMAKE_FIND_ROOT_PATH "''${KIT_ROOT}/gnu")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
EOF
  install -m755 ${cmake}/bin/cmake ${ninja}/bin/ninja ${ccache}/bin/ccache "$out/bin/"
  cp -aL ${cmake}/${cmakeData} "$out/share/"
  chmod -R u+w "$out/share"
  # CMake searches relative to its executable before its compiled-in fallback.
  # Equal-length replacement preserves ELF section offsets.
  export old_path="${cmake}/${cmakeData}" new_path="/${cmakeData}"
  perl -0pi -e 's/\Q$ENV{old_path}\E/$ENV{new_path}.("\0" x (length($ENV{old_path})-length($ENV{new_path})))/ge' "$out/bin/cmake"
  substituteInPlace "$out/${cmakeData}/Modules/Platform/UnixPaths.cmake" \
    --replace-fail '${cmake.stdenv.cc.libc.dev}' '\''${CMAKE_CURRENT_LIST_DIR}/../../../../gnu' \
    --replace-fail '${cmake.stdenv.cc.libc}' '\''${CMAKE_CURRENT_LIST_DIR}/../../../../gnu'
  export old_path="${ccache}/etc" new_path=/etc
  perl -0pi -e 's/\Q$ENV{old_path}\E/$ENV{new_path}.("\0" x (length($ENV{old_path})-length($ENV{new_path})))/ge' "$out/bin/ccache"
  for binary in "$out"/bin/*; do
    case "$binary" in *.cfg) continue;; esac
    readelf -lW "$binary" | awk '/INTERP/ { bad=1 } END { exit bad }'
    readelf -dW "$binary" | awk '/NEEDED/ { bad=1 } END { exit bad }'
    nm --defined-only "$binary" > symbols
    grep -Eq ' [Tt] mi_malloc$' symbols
    grep -Eq ' [Tt] malloc$' symbols
    test "$(awk '$3 == "malloc" {print $1}' symbols)" = \
         "$(awk '$3 == "mi_malloc" {print $1}' symbols)"
  done
  readelf -SW "$out/bin/clang-${major}" > sections
  grep -q '\.rela\.text' sections
  # Content identity follows the Manyclangs SHA256SUMS admission contract.
  cat > "$out/manifest.json" <<'EOF'
${builtins.toJSON {
  schema = 1;
  inherit target;
  host = "aarch64-unknown-linux-musl";
  isa = "armv8-a";
  optimization = lib.genAttrs [ "clang" "ld.lld" "llvm-ar" "llvm-ranlib" "llvm-nm" "llvm-objcopy" "llvm-strip" "cmake" "ninja" "ccache" ] (_: "release");
  lto = "off";
  glibc = libc.version;
  gcc = gcc.version;
  llvm = clang.version;
  cmake = cmake.version;
  ninja = ninja.version;
  ccache = ccache.version;
}}
EOF
  "$out/bin/clang" --print-targets > "$out/share/toolchain/backends.txt"
  cd "$out"
  LC_ALL=C find . \( -type f -o -type l \) ! -name SHA256SUMS -print0 | LC_ALL=C sort -z |
    xargs -0 sha256sum > SHA256SUMS
''
