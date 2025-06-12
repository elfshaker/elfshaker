#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.



[[ "$TRACE" ]] && set -x
set -euo pipefail
shopt -s extglob

if [[ $# < 2 ]]; then
  echo "Usage: ./check.sh path/to/elfshaker path/to/file.pack [tests]"
  exit 1
fi

timestamp() {
  date "+%Y%m%d%H%M%S" # Unix timestamp
}

# Use specified elfshaker binary
elfshaker=$(realpath "$1")
input=$(realpath "$2")
pack=$(basename -- "$input")
pack="${pack%.*}"

shift 2

temp_dir=$(realpath ${TMP-${TMPDIR-/tmp}}/"test-T$(timestamp)")
trap 'trap_exit' EXIT

cleanup() {
    rm -rf "$temp_dir"
}

trap_exit() {
  exit_code=$?
  if [[ $exit_code != 0 ]]; then
    echo "Script exited with error, tempfiles preserved in $temp_dir"
    read -n 1 -s -r -p "Press any key to continue with cleanup."
    echo
  fi
  cleanup
  exit $exit_code
}

all_pwd_sha1sums() {
  find . \( -name elfshaker_data -prune \) -o -type f -printf '%P\n' | \
    xargs sha1sum | \
    awk '{print $1" "$2}' | sort -k2,2
}

elfshaker_sha1sums() {
  # Note: Canonicalizes windows paths to unix paths for now.
  "$elfshaker" list-files "$@" 2> /dev/null | awk '{print $1" "$3}' | sort -k2,2 | sed 's|\\|/|g'
}

verify_snapshot() {
  head=$(cat ./elfshaker_data/HEAD)
  snapshot=$1

  if [[ "$head" != "$snapshot" ]]; then
    echo "Expected HEAD to be '$snapshot', but was '$head'";
    exit 1
  fi

  if ! DIFF=$(diff -u1 <(elfshaker_sha1sums "$snapshot") <(all_pwd_sha1sums))
  then
    echo "$DIFF"
    echo "Checksums differ!"
    exit 1
  fi
}

before_test() {
  cd "$temp_dir/elfshaker_data"
  rm -rf !("packs")
  rm -rf packs/loose
  cd "$temp_dir"
  rm -rf !("elfshaker_data")
}

# This monstrosity is required rather than just `if "$@"`` because running a
# function under an 'if' has the effect of suppressing set -e.
get_exit_code() {
  output_file=$1
  shift
  set +e
  (
    # Always trace in this shell, so that logs contain traces on failure.
    set -ex
    "$@"
  ) &> "$output_file"
  echo $?
  set -e
}

run_test() {
  echo -n "Running '$*'... "

  before_test

  output_file=$(mktemp)
  STATUS=$(get_exit_code "$output_file" "$@")
  if [[ "$STATUS" == 0 ]]; then
    echo "OK"
  else
    echo "FAIL"
    echo "----------------"
    echo -e "\033[0;31m"
    cat "$output_file"
    echo -e "\033[0m"
    echo -e "\n----------------"
    exit 1
  fi
  rm "$output_file"
}

serve_file_http() {
  port=$1
  file="$2"
  content_length="$(wc -c <"$file")"
  (printf "HTTP/1.1 200 OK\r\nContent-Length: $content_length\r\n\r\n"; cat "$file") | nc -l $port
}

# TESTS

test_list_works() {
  before_test
  "$elfshaker" list
}

test_extract_reset_on_empty_works() {
  "$elfshaker" list-files "$pack":"$snapshot_a"
  "$elfshaker" --verbose extract --reset --verify "$pack":"$snapshot_a"
  verify_snapshot "$pack":"$snapshot_a"
}

test_extract_again_works() {
  "$elfshaker" --verbose extract --reset --verify "$pack":"$snapshot_a"
  "$elfshaker" --verbose extract --verify "$pack":"$snapshot_a"
  verify_snapshot "$pack":"$snapshot_a"
}

test_extract_different_works() {
  "$elfshaker" --verbose extract --reset --verify "$pack":"$snapshot_a"
  "$elfshaker" --verbose extract --verify "$pack":"$snapshot_b"
  verify_snapshot "$pack":"$snapshot_b"
}


test_extract_multiple_file_removal_same_dir() {
  mkdir dir
  echo foo > dir/a
  echo bar > dir/b

  "$elfshaker" store snap1

  rm dir/a dir/b
  "$elfshaker" store snap2

  # This should remove both files and try to clean up 'dir' twice
  "$elfshaker" extract --force snap1
  "$elfshaker" extract --force snap2
  "$elfshaker" extract --force snap1
}

test_extract_filetype_change_works() {
  # Check that transitioning between a path being a directory and not a
  # directory works, when the files are empty.
  touch a
  "$elfshaker" --verbose store s0
  rm a
  mkdir a
  touch a/b
  "$elfshaker" --verbose store s1
  "$elfshaker" --verbose extract s0
  "$elfshaker" --verbose extract s1
  "$elfshaker" --verbose extract s0
}

test_extract_filetype_change_works_with_contents() {
  # Check that transitioning between a path being a directory and not a
  # directory works; same as test_extract_filetype_change_works, but
  # with non-empty files.
  echo x > a
  "$elfshaker" --verbose store s0
  rm a
  mkdir a
  echo y >> a/b
  "$elfshaker" --verbose store s1
  "$elfshaker" --verbose extract s0
  "$elfshaker" --verbose extract s1
  "$elfshaker" --verbose extract s0
}

test_extract_file_modes_preserved() {
  umask 0002
  touch foobar
  [[ "$(stat -c %A foobar)" == "-rw-rw-r--" ]] || {
    echo "Failed."
    exit 1
  }

  # Pack an empty file called foobar, change the mode.
  "$elfshaker" store s0
  chmod +x foobar
  "$elfshaker" store s1
  chmod -x foobar
  "$elfshaker" store s2

  # Verify that extract from loose snapshot sets the expected mode.
  "$elfshaker" extract s0
  [[ "$(stat -c %A foobar)" == "-rw-rw-r--" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract s1
  [[ "$(stat -c %A foobar)" == "-rwxrwxr-x" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract s2
  [[ "$(stat -c %A foobar)" == "-rw-rw-r--" ]] || {
    echo "Failed."
    exit 1
  }

  # Verify that extracting from a pack  sets the mode.
  "$elfshaker" pack p0
  "$elfshaker" gc --loose-snapshots

  "$elfshaker" extract p0:s0
  [[ "$(stat -c %A foobar)" == "-rw-rw-r--" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract p0:s1
  [[ "$(stat -c %A foobar)" == "-rwxrwxr-x" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract p0:s2
  [[ "$(stat -c %A foobar)" == "-rw-rw-r--" ]] || {
    echo "Failed."
    exit 1
  }

  rm elfshaker_data/packs/p0.pack{,.idx}
}

test_extract_zero_length_noreadperm_works() {
  umask 0022

  touch foobar
  "$elfshaker" store s0
  chmod -rw foobar
  "$elfshaker" store s1

  "$elfshaker" extract s0
  [[ "$(stat -c %A foobar)" == "-rw-r--r--" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract s1
  [[ "$(stat -c %A foobar)" == "----------" ]] || {
    echo "Failed."
    exit 1
  }

  "$elfshaker" pack p0
  "$elfshaker" gc --loose-snapshots

  "$elfshaker" extract s0
  [[ "$(stat -c %A foobar)" == "-rw-r--r--" ]] || {
    echo "Failed."
    exit 1
  }
  "$elfshaker" extract s1
  [[ "$(stat -c %A foobar)" == "----------" ]] || {
    echo "Failed."
    exit 1
  }

  rm elfshaker_data/packs/p0.pack{,.idx}
}

test_store_works() {
  "$elfshaker" --verbose extract --verify --reset "$pack":"$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  verify_snapshot loose/"$snapshot_b":"$snapshot_b"
}

test_store_and_extract_different_works() {
  "$elfshaker" --verbose extract --verify --reset "$pack":"$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  "$elfshaker" --verbose extract --verify "$pack":"$snapshot_a"
  verify_snapshot "$pack":"$snapshot_a"
}

test_store_twice_works() {
  "$elfshaker" --verbose extract --verify --reset "$pack":"$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b-again"
  diff_output=$(diff <("$elfshaker" list-files loose/"$snapshot_b":"$snapshot_b" | sort) <("$elfshaker" list-files loose/"$snapshot_b-again":"$snapshot_b-again" | sort))
  if [[ -n "${diff_output// }" ]]; then
    echo "'$diff_output'"
    exit 1
  fi
}

test_store_finds_new_files() {
  "$elfshaker" --verbose extract --verify --reset "$pack":"$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  test_file=$(mktemp -p .)
  "$elfshaker" --verbose store "$snapshot_b-mod"
  "$elfshaker" list-files loose/"$snapshot_b-mod":"$snapshot_b-mod" | grep $(basename "$test_file") || {
    echo 'Failed to store newly created file!'
    exit 1
  }
}

test_store_empty_directory_works() {
  mkdir -p emptydir
  cd emptydir
  # Store in an empty directory should create a repository. Additionally, it's a
  # store of an empty pack. Make sure that is handled without crashing.
  "$elfshaker" --verbose store test
  [ "$("$elfshaker" list | wc -l)" -eq 1 ]
}

test_store_data_dir_works() {
  mkdir -p testdir
  cd testdir

  "$elfshaker" --verbose --data-dir=../data_dir store test

  if [[ -d "elfshaker_data" ]]; then
    echo "elfshaker_data should not have been created!";
    exit 1
  fi

  [ "$("$elfshaker" --verbose --data-dir=../data_dir list | wc -l)" -eq 1 ]
}

rand_megs() {
  dd if=/dev/urandom bs="$1M" count=1 iflag=fullblock
}

test_pack_simple_works() {
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_before=$(cat ./foo ./bar | sha1sum)
  # Pack the two files
  "$elfshaker" --verbose store SS-1
  "$elfshaker" --verbose pack --compression-level 1 P-1
  # Delete the files
  rm ./foo ./bar
  # Then extract them from the pack and verify the checksums
  "$elfshaker" --verbose extract --reset P-1:SS-1
  sha1_after=$(cat ./foo ./bar | sha1sum)
  if [[ "$sha1_before" != "$sha1_after" ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
}

test_pack_two_snapshots_works() {
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss1=$(cat ./foo ./bar | sha1sum)
  # Store the two files in SS-1
  "$elfshaker" --verbose store SS-1
  # Modify the files
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss2=$(cat ./foo ./bar | sha1sum)
  if [[ "$sha1_ss1" == "$sha1_ss2" ]]; then
    echo 'I/O error!'
    exit 1
  fi
  # Store the modified files in SS-2 and pack all into P-1
  "$elfshaker" --verbose store SS-2
  "$elfshaker" --verbose pack --compression-level 1 P-1
  # Then extract from the pack and verify the checksums
  "$elfshaker" --verbose extract --reset P-1:SS-1
  if [[ "$sha1_ss1" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
  "$elfshaker" --verbose extract --reset P-1:SS-2
  if [[ "$sha1_ss2" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
}

test_pack_snapshots_from_list_works() {
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss1=$(cat ./foo | sha1sum)
  sha1_ss2=$(cat ./bar | sha1sum)

  # Store the first file in SL-1 and second in SL-2
  printf "foo\n" | "$elfshaker" --verbose store --files-from - SL-1
  printf "bar\n" | "$elfshaker" --verbose store --files-from - SL-2

  printf "SL-1\n" | "$elfshaker" --verbose pack --compression-level 1 --snapshots-from - PL-1
  printf "SL-2\n" | "$elfshaker" --verbose pack --compression-level 1 --snapshots-from - PL-2

  # Modify the files
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar

  # Then extract from the pack and verify the checksums
  "$elfshaker" --verbose extract --reset PL-1:SL-1
  if [[ "$sha1_ss1" != $(cat ./foo | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi

  "$elfshaker" --verbose extract --reset PL-2:SL-2
  if [[ "$sha1_ss2" != $(cat ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi

  # Check that the pack contains only one snapshot
  SNAPSHOTS=$("$elfshaker" list PL-1 --format "%t" 2> /dev/null | awk '{print $1}' | xargs)
  # Expect the output to be a single snapshot
  [[ "${SNAPSHOTS}" == "SL-1" ]]
}

test_pack_multi_snapshots_from_list_works() {
  rand_megs 1 > ./foo1
  rand_megs 1 > ./foo2
  rand_megs 1 > ./bar
  sha1_ss1a=$(cat ./foo1 | sha1sum)
  sha1_ss1b=$(cat ./foo2 | sha1sum)
  sha1_ss2=$(cat ./bar | sha1sum)

  # Store the first two files in SL2-1 and second in SL2-2
  printf "foo1\n" | "$elfshaker" --verbose store --files-from - SL2-1
  printf "foo2\n" | "$elfshaker" --verbose store --files-from - SL2-2
  printf "bar\n" | "$elfshaker" --verbose store --files-from - SL2-3

  printf "SL2-1\nSL2-2\n" | "$elfshaker" --verbose pack --compression-level 1 --snapshots-from - PL2-1
  printf "SL2-3\n" | "$elfshaker" --verbose pack --compression-level 1 --snapshots-from - PL2-2

  # Modify the files
  rand_megs 1 > ./foo1
  rand_megs 1 > ./foo2
  rand_megs 1 > ./bar

  # Then extract from the pack and verify the checksums
  "$elfshaker" --verbose extract --reset PL2-1:SL2-1
  if [[ "$sha1_ss1a" != $(cat ./foo1 | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi

  "$elfshaker" --verbose extract --reset PL2-1:SL2-2
  if [[ "$sha1_ss1b" != $(cat ./foo2 | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi

  "$elfshaker" --verbose extract --reset PL2-2:SL2-3
  if [[ "$sha1_ss2" != $(cat ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi

  # Check that the pack contains only one snapshot
  SNAPSHOTS=$("$elfshaker" list PL2-1 --format "%t" 2> /dev/null | awk '{print $1}' | xargs)
  # Expect the output to be two snapshots
  [[ "${SNAPSHOTS}" == "SL2-1 SL2-2" ]]
}

# Same as above, but objects have different sizes, which will cause
# them to be reordered when emitting the compressed pack.
test_pack_two_snapshots_object_sort_works() {
  rand_megs 1 > ./foo
  rand_megs 2 > ./bar
  sha1_ss1=$(cat ./foo ./bar | sha1sum)
  # Store the two files in SS-1
  "$elfshaker" --verbose store SS-1
  # Modify the files
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss2=$(cat ./foo ./bar | sha1sum)
  if [[ "$sha1_ss1" == "$sha1_ss2" ]]; then
    echo 'I/O error!'
    exit 1
  fi
  # Store the modified files in SS-2 and pack all into P-1
  "$elfshaker" --verbose store SS-2
  "$elfshaker" --verbose pack --compression-level 1 P-1
  # Then extract from the pack and verify the checksums
  "$elfshaker" --verbose extract --reset P-1:SS-1
  if [[ "$sha1_ss1" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
  "$elfshaker" --verbose extract --reset P-1:SS-2
  if [[ "$sha1_ss2" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
}

test_pack_two_snapshots_multiframe_works() {
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss1=$(cat ./foo ./bar | sha1sum)
  # Store the two files in SS-1
  "$elfshaker" store SS-1
  # Modify the files
  rand_megs 1 > ./foo
  rand_megs 1 > ./bar
  sha1_ss2=$(cat ./foo ./bar | sha1sum)
  # Store the modified files in SS-2 and pack all into P-1
  "$elfshaker" store SS-2
  # When the number of frames is too large (> #objects), elfshaker should
  # silently emit less frames and not crash
  "$elfshaker" pack --compression-level 1 --frames 999 P-1
  # Then extract from the pack and verify the checksums
  "$elfshaker" extract --reset --verify P-1:SS-1
  if [[ "$sha1_ss1" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
  "$elfshaker" extract --reset --verify P-1:SS-2
  if [[ "$sha1_ss2" != $(cat ./foo ./bar | sha1sum) ]]; then
    echo "Checksums do not match!";
    exit 1
  fi
}

test_pack_order_by_mtime() {
  "$elfshaker" store test1
  "$elfshaker" store test2
  "$elfshaker" store test4 # 4
  "$elfshaker" store test3 # 3. Lexical order != store order

  # Ensure that the timestamps match the store order.
  EPOCH="$(date)"
  touch -d "$(date --date="$EPOCH"' +0 min')" elfshaker_data/packs/loose/test2.pack.idx
  touch -d "$(date --date="$EPOCH"' +1 min')" elfshaker_data/packs/loose/test2.pack.idx
  touch -d "$(date --date="$EPOCH"' +2 min')" elfshaker_data/packs/loose/test4.pack.idx # 4
  touch -d "$(date --date="$EPOCH"' +3 min')" elfshaker_data/packs/loose/test3.pack.idx # 3. (Lex != store)

  "$elfshaker" pack testpack
  SNAPSHOTS=$("$elfshaker" list testpack --format "%t" 2> /dev/null | awk '{print $1}' | xargs)
  # Expect the output to be sorted lexicographically
  [[ "${SNAPSHOTS}" == "test1 test2 test3 test4" ]]
}

test_show_from_pack_works() {
  "$elfshaker" extract --reset --verify "$pack":"$snapshot_a"
  file_path="$({ "$elfshaker" list-files "$pack":"$snapshot_a" || true; } | head -n1 | awk '{print $NF}')"
  sha1_extract="$(sha1sum < "$file_path")"
  sha1_show="$("$elfshaker" --verbose show "$pack":"$snapshot_a" "$file_path" | sha1sum)"
  if [[ "$sha1_extract" != "$sha1_show" ]]; then
    echo 'Outputs of extract and show do not match!'
    exit 1
  fi
}

test_show_from_loose_works() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  "$elfshaker" store "$snapshot_a"
  file_path="$({ "$elfshaker" list-files "$pack":"$snapshot_a" || true; } | head -n1 | awk '{print $NF}')"
  sha1_extract="$(sha1sum < "$file_path")"
  sha1_show="$("$elfshaker" --verbose show loose/"$snapshot_a":"$snapshot_a" "$file_path" | sha1sum)"
  if [[ "$sha1_extract" != "$sha1_show" ]]; then
    echo 'Outputs of extract and show do not match!'
    exit 1
  fi
}

test_head_updated_after_packing() {
  rand_megs 1 > ./foo
  "$elfshaker" store test-snapshot
  "$elfshaker" pack --compression-level 1 test-pack
  if [[ "$(cat elfshaker_data/HEAD)" != "test-pack:test-snapshot" ]]; then
    echo 'HEAD was expected to point to the newly-created pack!'
    exit 1
  fi
}

test_touched_file_dirties_repo() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  find . -not -path "./elfshaker_data/*" -exec touch -d "$(date -R --date='now +10 sec')" {} +
  if "$elfshaker" extract --verbose --verify "$pack":"$snapshot_b"; then
    echo 'Failed to detect files changes!'
    exit 1
  fi
}

test_dirty_repo_can_be_forced() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  find . -not -path "./elfshaker_data/*" -exec touch -d "$(date -R --date='now +10 sec')" {} +
  if ! "$elfshaker" extract --verbose --force --verify "$pack":"$snapshot_b"; then
    echo 'Could not use --force to skip dirty repository checks!'
    exit 1
  fi
}

# Test if elfshaker update fetches pack indexes successfully
test_update_fetches_indexes() {
  pack_sha="$(sha1sum "$input" | awk '{print $1}')"
  pack_idx_sha="$(sha1sum "$input.idx" | awk '{print $1}')"

  index="./elfshaker_data/remotes/origin.esi"
  mkdir ./elfshaker_data/remotes
  {
    printf 'meta\tv1\n'
    printf 'url\thttp://127.0.0.1:43102/index.esi\n'
    printf "$pack_idx_sha\t$pack_sha\thttp://127.0.0.1:43103/test.pack\n"
  } > "$index"

  # First request returns the .esi
  serve_file_http 43102 "$index" &
  server1_pid=$!
  # Second request returns the .pack.idx
  serve_file_http 43103 "$input.idx" &
  server2_pid=$!

  # Give the servers time to open the sockets and start listening
  sleep 1

  if ! "$elfshaker" update; then
    echo 'elfshaker update failed!'
    kill $server1_pid $server2_pid || true
    exit 1
  fi

  if [ ! -f "./elfshaker_data/packs/origin/test.pack.idx" ]; then
    echo 'elfshaker update did not fetch the test pack index!'
    kill $server1_pid $server2_pid || true
    exit 1
  fi

  kill $server1_pid $server2_pid || true
}

# Test if elfshaker clone fetches pack indexes successfully
test_clone_fetches_indexes() {
  pack_sha="$(sha1sum "$input" | awk '{print $1}')"
  pack_idx_sha="$(sha1sum "$input.idx" | awk '{print $1}')"

  index="$(mktemp)"
  {
    printf 'meta\tv1\n'
    printf 'url\thttp://127.0.0.1:43103/index.esi\n'
    printf "$pack_idx_sha\t$pack_sha\thttp://127.0.0.1:43104/test.pack\n"
    echo
  } > "$index"

  # First request returns the .esi
  serve_file_http 43102 "$index" &
  server1_pid=$!
  # Second request returns the .esi for the update step
  serve_file_http 43103 "$index" &
  server2_pid=$!
  # Second request returns the .pack.idx
  serve_file_http 43104 "$input.idx" &
  server3_pid=$!

  # Give the servers time to open the sockets and start listening
  sleep 1

  if ! "$elfshaker" clone http://127.0.0.1:43102/index.esi clone_fetches_indexes; then
    echo 'elfshaker clone failed!'
    kill $server1_pid $server2_pid $server3_pid || true
    exit 1
  fi

  if [ ! -f "./clone_fetches_indexes/elfshaker_data/packs/origin/test.pack.idx" ]; then
    echo 'elfshaker clone did not fetch the test pack index!'
    kill $server1_pid $server2_pid $server3_pid || true
    exit 1
  fi

  kill $server1_pid $server2_pid $server3_pid || true
}

test_gc_snapshots() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  # Create a duplicate loose snapshot
  "$elfshaker" store "$snapshot_a"
  duplicate_snapshot_path="./elfshaker_data/packs/loose/$snapshot_a.pack.idx"

  if [ ! -f "$duplicate_snapshot_path" ]; then
    echo 'Expected to find the packed snapshot file'
    exit 1
  fi

  # Create another loose snapshot that is not duplicated
  touch foo bar
  "$elfshaker" store non_duplicate
  non_duplicate_snapshot_path="./elfshaker_data/packs/loose/non_duplicate.pack.idx"

  if [ ! -f "$non_duplicate_snapshot_path" ]; then
    echo 'Expected to find the loose snapshot file'
    exit 1
  fi

  "$elfshaker" gc -s --verbose

  if [ -f "$duplicate_snapshot_path" ]; then
    echo 'elfshaker gc did not clean up the snapshot'
    exit 1
  fi

  if [ ! -f "$non_duplicate_snapshot_path" ]; then
    echo 'elfshaker gc cleaned up a snapshot that was not redundant'
    exit 1
  fi
}

test_gc_snapshots_dry_run() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  # Create a duplicate loose snapshot
  "$elfshaker" store "$snapshot_a"
  duplicate_snapshot_path="./elfshaker_data/packs/loose/$snapshot_a.pack.idx"

  if [ ! -f "$duplicate_snapshot_path" ]; then
    echo 'Expected to find the packed snapshot file'
    exit 1
  fi

  "$elfshaker" gc -s --dry-run --verbose

  if [ ! -f "$duplicate_snapshot_path" ]; then
    echo 'elfshaker gc dry run deleted the snapshot'
    exit 1
  fi
}

test_gc_objects() {
  # Create loose snapshot with two objects
  touch foo bar
  "$elfshaker" store two_objects
  two_objects_pack_path="./elfshaker_data/packs/loose/two_objects.pack.idx"

    # Create loose snapshot with two *more* objects
  touch baz blar
  "$elfshaker" store four_objects
  four_objects_pack_path="./elfshaker_data/packs/loose/four_objects.pack.idx"

  # Delete the second loose snapshot
  rm $four_objects_pack_path

  objects_before_gc=$(find elfshaker_data/loose -type f | wc -l)
  if [ objects_before_gc -ne 4 ]; then
    echo 'Expected 4 loose objects'
    exit 1
  fi

  "$elfshaker" gc -o --verbose

  objects_after_gc=$(find elfshaker_data/loose -type f | wc -l)
  if [ objects_after_gc -ne 2 ]; then
    echo 'Expected 2 loose objects after GC'
    exit 1
  fi
}

# Regression test for issue with `create_empty` when exploding a pack that
# contains an empty object. After removing the loose object store, exploding
# the pack should recreate the necessary directory hierarchy for zero-length
# objects.
test_explode_zero_length_creates_parents() {
  umask 0022

  # Create an empty file and store it in a snapshot
  touch emptyfile
  "$elfshaker" store s0

  # Pack the snapshot
  "$elfshaker" pack p0

  # Remove the entire loose object store to force explode to recreate it
  rm -rf ./elfshaker_data/loose

  # Explode the pack back into loose objects; should succeed
  if ! "$elfshaker" explode p0; then
    echo 'elfshaker explode failed!' ;
    exit 1
  fi

  # Verify that the empty object now exists in the loose store
  empty_sha=$(sha1sum emptyfile | awk '{print $1}')
  obj_path="./elfshaker_data/loose/${empty_sha:0:2}/${empty_sha:2:2}/${empty_sha:4}"
  if [ ! -f "$obj_path" ]; then
    echo 'Explode did not recreate empty object path!'
    exit 1
  fi
}

test_gc_objects_dry_run() {
  # Create loose snapshot with two objects
  touch foo bar
  "$elfshaker" store two_objects
  two_objects_pack_path="./elfshaker_data/packs/loose/two_objects.pack.idx"

  # Delete the loose snapshot
  rm $two_objects_pack_path

  objects_before_gc=$(find elfshaker_data/loose -type f | wc -l)
  if [ objects_before_gc -ne 2 ]; then
    echo 'Expected 2 loose objects'
    exit 1
  fi

  "$elfshaker" gc -o --dry-run --verbose

  objects_after_gc=$(find elfshaker_data/loose -type f | wc -l)
  if [ objects_after_gc -ne 2 ]; then
    echo 'Expected 2 loose objects after GC'
    exit 1
  fi
}

test_read_only() {
  chmod u-w -R $temp_dir/elfshaker_data

  "$elfshaker" list
  "$elfshaker" --verbose extract --reset --verify "$pack":"$snapshot_a"

  chmod u+w -R $temp_dir/elfshaker_data
}

main() {
  mkdir "$temp_dir"
  cd "$temp_dir"

  # Setup repository
  mkdir ./elfshaker_data/
  mkdir ./elfshaker_data/packs/
  cp "$input" ./elfshaker_data/packs/
  cp "$input.idx" ./elfshaker_data/packs/

  list_output=$(mktemp)
  # Grab 2 snapshots from the pack
  "$elfshaker" list "$pack" --format "%t" 2>/dev/null | sed '1d' > "$list_output"
  snapshot_a=$(head -n 1 "$list_output" | awk '{print $1}')
  snapshot_b=$(tail -n 1 "$list_output" | awk '{print $1}')
  rm "$list_output"

  if [[ "$snapshot_a" == "$snapshot_b" ]]; then
    echo "Testing failed because the specified pack file contains only 1 snapshot. At least 2 snapshots are needed for a successful test!";
    exit 1
  fi

  if [[ $# != 0 ]];
  then
    for test in "$@"; do
      run_test "$test"
    done
    return
  fi

  SKIP_BAD_WINDOWS_TESTS="${SKIP_BAD_WINDOWS_TESTS-}" # default empty; set to non-empty to skip failing tests on window.

  run_test test_list_works
  run_test test_extract_reset_on_empty_works
  run_test test_extract_again_works
  run_test test_extract_different_works
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_extract_filetype_change_works
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_extract_filetype_change_works_with_contents
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_extract_file_modes_preserved
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_extract_zero_length_noreadperm_works
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && test_extract_multiple_file_removal_same_dir
  run_test test_store_works
  run_test test_store_and_extract_different_works
  run_test test_store_twice_works
  run_test test_store_finds_new_files
  run_test test_store_empty_directory_works
  run_test test_store_data_dir_works
  run_test test_pack_simple_works
  run_test test_pack_two_snapshots_works
  run_test test_pack_snapshots_from_list_works
  run_test test_pack_multi_snapshots_from_list_works
  run_test test_pack_two_snapshots_object_sort_works
  run_test test_pack_two_snapshots_multiframe_works
  run_test test_pack_order_by_mtime
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_show_from_pack_works
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_show_from_loose_works
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_head_updated_after_packing
  run_test test_touched_file_dirties_repo
  run_test test_dirty_repo_can_be_forced
  run_test test_update_fetches_indexes
  run_test test_clone_fetches_indexes
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_gc_snapshots
  [ -z "$SKIP_BAD_WINDOWS_TESTS" ] && run_test test_gc_snapshots_dry_run
  run_test test_gc_objects
  run_test test_gc_objects_dry_run
  run_test test_read_only
  run_test test_explode_zero_length_creates_parents
}

main "$@"
