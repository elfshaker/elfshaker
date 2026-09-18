#!/usr/bin/env bash
# Run on a host without /nix/store, with readelf, nm, Python, Bash and GNU coreutils.
set -euo pipefail
if test "${KIT_TEST_CLEAN:-}" != 1; then
  exec env -i KIT_TEST_CLEAN=1 PATH=/usr/bin:/bin /bin/bash "$0" "$@"
fi
kit=$(realpath "${1:?usage: test-kit.sh KIT FRESH-WORK-DIR}")
work=${2:?fresh work directory required}
test ! -e "$work"
mkdir -p "$work"
work=$(realpath "$work")
test ! -e /nix/store || { echo 'Run relocation acceptance on a host without /nix/store' >&2; exit 1; }
export PATH="$kit/bin:/usr/bin:/bin" HOME="$work/home"
unset CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH LIBRARY_PATH LD_LIBRARY_PATH
unset CMAKE_PREFIX_PATH CMAKE_TOOLCHAIN_FILE CCACHE_REMOTE_STORAGE AWS_CA_BUNDLE
mkdir "$HOME"
(cd "$kit" && sha256sum -c SHA256SUMS)
script_dir=$(dirname "$(realpath "$0")")
python3 -B "$script_dir/verify-kit.py" "$kit"
for name in clang clang++ clang-cpp ld.lld llvm-ar llvm-ranlib llvm-nm llvm-objcopy llvm-strip cmake ninja ccache; do
  "$kit/bin/$name" --version
  readelf -lW "$kit/bin/$name" | awk '/INTERP/ { bad=1 } END { exit bad }'
  readelf -dW "$kit/bin/$name" | awk '/NEEDED/ { bad=1 } END { exit bad }'
done
while IFS= read -r -d '' link; do
  case "$(realpath "$link")" in "$kit"/*) ;; *) echo "Escaping symlink: $link" >&2; exit 1;; esac
done < <(find "$kit" -type l -print0)
mkdir "$work/src"
cat > "$work/src/CMakeLists.txt" <<'EOF'
cmake_minimum_required(VERSION 3.16)
project(kit_smoke LANGUAGES C CXX)
add_library(plugin SHARED plugin.cpp)
add_executable(smoke main.cpp)
target_link_libraries(smoke PRIVATE plugin dl)
enable_testing()
add_test(NAME smoke COMMAND smoke $<TARGET_FILE:plugin>)
EOF
cat > "$work/src/plugin.cpp" <<'EOF'
#include <stdexcept>
extern "C" void throwing_function() { throw std::runtime_error("kit"); }
extern "C" int answer() { return 42; }
EOF
cat > "$work/src/main.cpp" <<'EOF'
#include <dlfcn.h>
#include <stdexcept>
extern "C" void throwing_function();
int main(int argc, char **argv) {
  if (argc != 2) return 1;
  void *p = dlopen(argv[1], RTLD_NOW);
  if (!p) return 2;
  auto f = reinterpret_cast<int (*)()>(dlsym(p, "answer"));
  if (!f || f() != 42) return 3;
  dlclose(p);
  try { throwing_function(); } catch (const std::runtime_error&) { return 0; }
  return 4;
}
EOF
export CCACHE_DIR="$work/cache" CCACHE_CONFIGPATH="$work/ccache.conf"
printf 'max_size = 64M\n' > "$CCACHE_CONFIGPATH"
cmake -S "$work/src" -B "$work/build" -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE="$kit/share/cmake/gnu-toolchain.cmake" \
  -DCMAKE_CXX_COMPILER_LAUNCHER="$kit/bin/ccache"
cmake --build "$work/build" --parallel 2
"$work/build/smoke" "$work/build/libplugin.so"
readelf -lW "$work/build/smoke" | grep -F '/lib/ld-linux-aarch64.so.1'
readelf --version-info "$work/build/smoke" > "$work/abi.txt"
readelf --version-info "$work/build/libplugin.so" >> "$work/abi.txt"
python3 - "$work/abi.txt" <<'EOF'
import re
import sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
for family, ceiling in [('GLIBC', (2, 31)), ('GLIBCXX', (3, 4, 28)), ('CXXABI', (1, 3, 12))]:
    versions = re.findall(r'\b' + family + r'_([0-9.]+)', text)
    if any(tuple(map(int, v.split('.'))) > ceiling for v in versions):
        raise SystemExit(f'{family} requirements exceed GNU231: {versions}')
EOF
if readelf -dW "$work/build/smoke" | grep -F /nix/store; then exit 1; fi
ccache --zero-stats
ninja -C "$work/build" -t clean
cmake --build "$work/build" --parallel 2
ccache --print-stats > "$work/cache-stats.txt"
awk '$1 ~ /^cache_hit/ { hits += $2 } END { exit !(hits > 0) }' "$work/cache-stats.txt"
