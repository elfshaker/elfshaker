#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.



[[ "$TRACE" ]] && set -x
set -euo pipefail
shopt -s extglob

if [[ $# != 2 ]]; then
  echo "Usage: ./check.sh path/to/elfshaker path/to/file.pack"
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

temp_dir=$(realpath /dev/shm/"test-T$(timestamp)")
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
  "$elfshaker" list-files "$@" 2> /dev/null | awk '{print $1" "$3}' | sort -k2,2
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
  (printf "HTTP/1.1 200 OK\r\nContent-Length: $content_length\r\n\r\n"; cat "$file") | nc -l -p $port
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
  find . -not -path "./elfshaker_data/*" -exec touch -d "$(date --date='now +10 sec')" {} +
  if "$elfshaker" extract --verbose --verify "$pack":"$snapshot_b"; then
    echo 'Failed to detect files changes!'
    exit 1
  fi
}

test_dirty_repo_can_be_forced() {
  "$elfshaker" extract --verify --reset "$pack":"$snapshot_a"
  find . -not -path "./elfshaker_data/*" -exec touch -d "$(date --date='now +10 sec')" {} +
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

  run_test test_list_works
  run_test test_extract_reset_on_empty_works
  run_test test_extract_again_works
  run_test test_extract_different_works
  run_test test_store_works
  run_test test_store_and_extract_different_works
  run_test test_store_twice_works
  run_test test_store_finds_new_files
  run_test test_store_empty_directory_works
  run_test test_store_data_dir_works
  run_test test_pack_simple_works
  run_test test_pack_two_snapshots_works
  run_test test_pack_two_snapshots_object_sort_works
  run_test test_pack_two_snapshots_multiframe_works
  run_test test_pack_order_by_mtime
  run_test test_show_from_pack_works
  run_test test_show_from_loose_works
  run_test test_head_updated_after_packing
  run_test test_touched_file_dirties_repo
  run_test test_dirty_repo_can_be_forced
  run_test test_update_fetches_indexes
  run_test test_clone_fetches_indexes
}

main "$@"
