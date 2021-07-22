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
  date "+%s" # Unix timestamp
}

# Use specified elfshaker binary
elfshaker=$(realpath "$1")
input=$(realpath "$2")
pack=$(basename -- "$input")
pack="${pack%.*}"

temp_dir=$(realpath ./"test-T$(timestamp)")
trap 'trap_exit' EXIT

cleanup() {
    rm -rf "$temp_dir"
}

trap_exit() {
  exit_code=$?
  if [[ $exit_code != 0 ]]; then
    read -n 1 -s -r -p "Press any key to continue with cleanup";
  fi
  cleanup
  exit $exit_code
}

verify_snapshot() {
  head=$(cat ./elfshaker_data/HEAD)
  pack=$(dirname "$head")
  tag=$(basename "$head")

  if [[ "$head" != "$1/$2" ]]; then
    echo "Expected HEAD to be '$1/$2', but was '$head'";
    exit 1
  fi

  "$elfshaker" list -P "$pack" "$tag" | sed '1d' | sort > LIST_OUTPUT
  while read -r line; do
    path=$(echo "$line" | awk '{print $1;}')
    checksum=$(echo "$line" | awk '{print $3;}')
    actual_checksum=$(sha1sum "$path" | awk '{print $1;}')
    if [ "${checksum,,}" != "${actual_checksum,,}" ]; then
        echo "Checksums for $path do NOT match ($actual_checksum, expected: $checksum)!"
    fi
  done < LIST_OUTPUT
  rm LIST_OUTPUT
}

before_test() {
  cd "$temp_dir/elfshaker_data"
  rm -rf !("packs")
  cd "$temp_dir"
  rm -rf !("elfshaker_data")
}

run_test() {
  echo -n "Running '$*'... "
  output_file=$(mktemp)
  before_test
  if ("$@" > "$output_file" 2>&1); then
    echo "OK"
  else
    echo "FAIL"
    echo "----------------"
    echo -e "\033[0;31m"
    tail -n 25 "$output_file"
    echo -e "\033[0m"
    echo -e "\n----------------"
  fi
  rm "$output_file"
}

# TESTS

test_list_works() {
  before_test
  "$elfshaker" update-index
  "$elfshaker" list
}

test_extract_reset_on_empty_works() {
  "$elfshaker" update-index
  "$elfshaker" list -P "$pack" "$snapshot_a"
  "$elfshaker" --verbose extract --reset --verify -P "$pack" "$snapshot_a"
  verify_snapshot "$pack" "$snapshot_a"
}

test_extract_again_works() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --reset --verify -P "$pack" "$snapshot_a"
  "$elfshaker" --verbose extract --verify -P "$pack" "$snapshot_a"
  verify_snapshot "$pack" "$snapshot_a"
}

test_extract_different_works() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --reset --verify -P "$pack" "$snapshot_a"
  "$elfshaker" --verbose extract --verify -P "$pack" "$snapshot_b"
  verify_snapshot "$pack" "$snapshot_b"
}

test_store_works() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --verify --reset -P "$pack" "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  "$elfshaker" --verbose update-index
  verify_snapshot unpacked "$snapshot_b"
}

test_store_and_extract_different_works() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --verify --reset -P "$pack" "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  "$elfshaker" --verbose update-index
  "$elfshaker" --verbose extract --verify -P "$pack" "$snapshot_a"
  verify_snapshot "$pack" "$snapshot_a"
}

test_store_twice_works() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --verify --reset -P "$pack" "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b-again"
  "$elfshaker" --verbose update-index
  diff_output=$(diff <("$elfshaker" list -P unpacked "$snapshot_b" | sort) <("$elfshaker" list -P unpacked "$snapshot_b-again" | sort))
  if [[ -n "${diff_output// }" ]]; then
    echo "'$diff_output'"
    exit 1
  fi
}

test_store_finds_new_files() {
  "$elfshaker" update-index
  "$elfshaker" --verbose extract --verify --reset -P "$pack" "$snapshot_b"
  "$elfshaker" --verbose store "$snapshot_b"
  test_file=$(mktemp -p .)
  "$elfshaker" --verbose store "$snapshot_b-mod"
  "$elfshaker" --verbose update-index
  "$elfshaker" list -P unpacked "$snapshot_b-mod" | grep $(basename "$test_file") || {
    echo 'Failed to store newly created file!'
    exit 1
  }
}

main() {
  mkdir "$temp_dir"
  cd "$temp_dir"

  # Setup repository
  mkdir ./elfshaker_data/
  mkdir ./elfshaker_data/packs/
  cp "$input" ./elfshaker_data/packs/
  cp "$input.idx" ./elfshaker_data/packs/

  "$elfshaker" update-index

  list_output=$(mktemp)
  # Grab 2 snapshots from the pack
  "$elfshaker" list -P "$pack" | sed '1d' > "$list_output"
  snapshot_a=$(head -n 1 "$list_output" | awk '{print $1;}')
  snapshot_b=$(tail -n 1 "$list_output" | awk '{print $1;}')
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
}

main "$@"
