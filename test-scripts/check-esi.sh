#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "Usage: $0 path/to/new-elfshaker path/to/verification.pack [path/to/old-elfshaker]"
  exit 1
fi

script_dir=$(cd "$(dirname "$0")" && pwd)
elfshaker=$(realpath "$1")
input=$(realpath "$2")
old_elfshaker=
if [[ $# == 3 ]]; then
  old_elfshaker=$(realpath "$3")
fi

temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/elfshaker-esi.XXXXXX")
server_pid=

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -r "$temp_dir"
}
trap cleanup EXIT

fail() {
  echo "check-esi.sh: $*" >&2
  exit 1
}

expect_fail() {
  if "$@"; then
    fail "command unexpectedly succeeded: $*"
  fi
}

expect_old_format_error() {
  local stderr_file
  stderr_file=$(mktemp "$temp_dir/old-format-error.XXXXXX")
  if "$@" >/dev/null 2>"$stderr_file"; then
    fail "old client unexpectedly accepted v2 ESI: $*"
  fi
  assert_contains "$stderr_file" "Too many values on line 4"
  if grep -F -- "panicked" "$stderr_file" >/dev/null; then
    cat "$stderr_file" >&2
    fail "old client panicked while rejecting v2 ESI"
  fi
}

assert_no_file() {
  [[ ! -e "$1" ]] || fail "unexpected file: $1"
}

assert_contains() {
  grep -F -- "$2" "$1" >/dev/null || fail "'$1' does not contain '$2'"
}

assert_not_contains() {
  if grep -F -- "$2" "$1" >/dev/null; then
    fail "'$1' unexpectedly contains '$2'"
  fi
}

pack_name=$(basename "$input")
work="$temp_dir/editor"
mkdir "$work"
cp "$input" "$work/$pack_name"
cp "$input.idx" "$work/$pack_name.idx"
pack="$work/$pack_name"
pack_sha=$(sha1sum "$pack" | awk '{print $1}')
index_sha=$(sha1sum "$pack.idx" | awk '{print $1}')
pack_size=$(wc -c < "$pack")
index_size=$(wc -c < "$pack.idx")

v1="$work/index.esi"
v2="$work/index.v2.esi"
v1_second="$work/index-second.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/index.esi "$v1"
"$elfshaker" esi create --meta v2 --url https://example.test/index.v2.esi "$v2"
"$elfshaker" esi create --meta v1 --url https://example.test/index-second.esi "$v1_second"
printf 'meta\tv1\nurl\thttps://example.test/index.esi\n' > "$work/expected-v1-empty"
printf 'meta\tv2\nurl\thttps://example.test/index.v2.esi\n' > "$work/expected-v2-empty"
printf 'meta\tv1\nurl\thttps://example.test/index-second.esi\n' > "$work/expected-v1-second-empty"
cmp "$work/expected-v1-empty" "$v1"
cmp "$work/expected-v2-empty" "$v2"
expect_fail "$elfshaker" esi create --meta v2 \
  --url https://example.test/replaced.v2.esi "$v1"
cp "$v1" "$work/v1-before-create-force"
"$elfshaker" esi create --force --meta v2 \
  --url https://example.test/replaced.v2.esi "$v1"
cmp "$work/v1-before-create-force" "$v1.bak"
"$elfshaker" esi create --force --meta v1 \
  --url https://example.test/index.esi "$v1"

"$elfshaker" esi add --also "$v1" --also "$v1_second" "$v2" "$pack"
printf '%s\t%s\t%s\n' "$index_sha" "$pack_sha" "$pack_name" > "$work/v1-entry"
printf '%s\t%s\t%s\t%s\t%s\n' \
  "$index_sha" "$pack_sha" "$pack_name" "$pack_size" "$index_size" > "$work/v2-entry"
tail -n 1 "$v1" | cmp "$work/v1-entry" -
tail -n 1 "$v2" | cmp "$work/v2-entry" -
tail -n 1 "$v1_second" | cmp "$work/v1-entry" -
cmp "$work/expected-v1-empty" "$v1.bak"
cmp "$work/expected-v2-empty" "$v2.bak"
cmp "$work/expected-v1-second-empty" "$v1_second.bak"

"$elfshaker" esi list "$v2" > "$work/list"
cmp "$work/v2-entry" "$work/list"

cp "$v1" "$work/v1-before-duplicate"
cp "$v1.bak" "$work/v1-backup-before-duplicate"
expect_fail "$elfshaker" esi add "$v1" "$pack"
cmp "$work/v1-before-duplicate" "$v1"
cmp "$work/v1-backup-before-duplicate" "$v1.bak"

printf x >> "$pack"
new_pack_sha=$(sha1sum "$pack" | awk '{print $1}')
"$elfshaker" esi add --force "$v1" "$pack"
cmp "$work/v1-before-duplicate" "$v1.bak"
assert_contains "$v1" "$new_pack_sha"
assert_not_contains "$v1" "$pack_sha"

cp "$v1" "$work/v1-before-remove"
"$elfshaker" esi remove "$v1" "$pack_name"
cmp "$work/v1-before-remove" "$v1.bak"
cmp "$work/expected-v1-empty" "$v1"
expect_fail "$elfshaker" esi remove "$v1" "$pack_name"
"$elfshaker" esi remove --force "$v1" "$pack_name"

no_backup="$work/no-backup.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/no-backup.esi "$no_backup"
"$elfshaker" esi add --no-backup "$no_backup" "$pack"
assert_no_file "$no_backup.bak"

for name in first.pack second.pack; do
  cp "$input" "$work/$name"
  cp "$input.idx" "$work/$name.idx"
done
ordered="$work/ordered.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/ordered.esi "$ordered"
"$elfshaker" esi add "$ordered" "$work/first.pack" "$work/second.pack"
[[ $(sed -n '3p' "$ordered" | cut -f3) == first.pack ]] || fail "first pack order changed"
[[ $(sed -n '4p' "$ordered" | cut -f3) == second.pack ]] || fail "second pack order changed"

custom="$work/custom.esi"
"$elfshaker" esi create --meta v2 --url https://example.test/custom.v2.esi "$custom"
"$elfshaker" esi add --pack-url ../packs/custom.pack "$custom" "$work/first.pack"
assert_contains "$custom" $'\t../packs/custom.pack\t'
expect_fail "$elfshaker" esi add --pack-url custom.pack "$custom" \
  "$work/first.pack" "$work/second.pack"

unrepresentable="$work/unrepresentable.esi"
expect_fail "$elfshaker" esi create --meta v1 \
  --url "https://example.test/index with-space.esi" "$unrepresentable"
assert_no_file "$unrepresentable"
"$elfshaker" esi create --meta v1 \
  --url https://example.test/unrepresentable.esi "$unrepresentable"
cp "$input" "$work/pack with-space.pack"
cp "$input.idx" "$work/pack with-space.pack.idx"
"$elfshaker" esi add --no-backup "$unrepresentable" "$work/pack with-space.pack"
assert_contains "$unrepresentable" $'\tpack%20with-space.pack'
cp "$unrepresentable" "$work/unrepresentable-before-explicit-url"
expect_fail "$elfshaker" esi add --pack-url "pack with-space.pack" \
  "$unrepresentable" "$work/first.pack"
cmp "$work/unrepresentable-before-explicit-url" "$unrepresentable"
assert_no_file "$unrepresentable.bak"

url_syntax="$work/url-syntax.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/url-syntax.esi "$url_syntax"
for name in 'hash#name.pack' 'release:debug.pack'; do
  cp "$input" "$work/$name"
  cp "$input.idx" "$work/$name.idx"
done
"$elfshaker" esi add --no-backup "$url_syntax" \
  "$work/hash#name.pack" "$work/release:debug.pack"
assert_contains "$url_syntax" $'\thash%23name.pack'
assert_contains "$url_syntax" $'\trelease%3Adebug.pack'
"$elfshaker" esi list "$url_syntax" > "$work/url-syntax-list"
assert_contains "$work/url-syntax-list" $'\thash%23name.pack'
assert_contains "$work/url-syntax-list" $'\trelease%3Adebug.pack'

alias_target="$work/alias.esi"
alias_backup="$alias_target.bak"
"$elfshaker" esi create --no-backup --meta v1 \
  --url https://example.test/alias.esi "$alias_target"
"$elfshaker" esi create --no-backup --meta v1 \
  --url https://example.test/alias-backup.esi "$alias_backup"
cp "$alias_target" "$work/alias-before"
cp "$alias_backup" "$work/alias-backup-before"
expect_fail "$elfshaker" esi add --also "$alias_backup" "$alias_target" "$work/first.pack"
cmp "$work/alias-before" "$alias_target"
cmp "$work/alias-backup-before" "$alias_backup"
assert_no_file "$alias_backup.bak"

filtered_alias_target="$work/filtered-alias.esi"
filtered_alias_backup="$filtered_alias_target.bak"
"$elfshaker" esi create --no-backup --meta v1 \
  --url https://example.test/filtered-alias.esi "$filtered_alias_target"
"$elfshaker" esi add --no-backup "$filtered_alias_target" "$work/first.pack"
"$elfshaker" esi create --no-backup --meta v1 \
  --url https://example.test/filtered-alias-backup.esi "$filtered_alias_backup"
cp "$filtered_alias_target" "$work/filtered-alias-before"
cp "$filtered_alias_backup" "$work/filtered-alias-backup-before"
expect_fail "$elfshaker" esi remove --force --also "$filtered_alias_backup" \
  "$filtered_alias_target" first.pack
cmp "$work/filtered-alias-before" "$filtered_alias_target"
cmp "$work/filtered-alias-backup-before" "$filtered_alias_backup"
assert_no_file "$filtered_alias_backup.bak"

invalid="$work/invalid.esi"
printf 'not an esi\n' > "$invalid"
cp "$invalid" "$work/invalid-before"
cp "$v2" "$work/v2-before-invalid"
expect_fail "$elfshaker" esi add --also "$invalid" "$v2" "$work/first.pack"
cmp "$work/v2-before-invalid" "$v2"
cmp "$work/invalid-before" "$invalid"

failed_dir="$work/failed-secondary"
mkdir "$failed_dir"
failed_secondary="$failed_dir/secondary.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/secondary.esi "$failed_secondary"
cp "$v2" "$work/v2-before-secondary-failure"
cp "$failed_secondary" "$work/secondary-before-failure"
chmod 0555 "$failed_dir"
expect_fail "$elfshaker" esi add --also "$failed_secondary" "$v2" "$work/second.pack"
chmod 0755 "$failed_dir"
cmp "$work/v2-before-secondary-failure" "$v2"
cmp "$work/secondary-before-failure" "$failed_secondary"

missing="$work/missing.esi"
"$elfshaker" esi create --meta v1 --url https://example.test/missing.esi "$missing"
cp "$missing" "$work/missing-before"
expect_fail "$elfshaker" esi add "$missing" "$work/does-not-exist.pack"
cmp "$work/missing-before" "$missing"
cp "$input" "$work/no-index.pack"
expect_fail "$elfshaker" esi add "$missing" "$work/no-index.pack"
cmp "$work/missing-before" "$missing"

http_root="$temp_dir/http"
clients="$temp_dir/clients"
mkdir "$http_root" "$clients"
cp "$input" "$http_root/verification.pack"
cp "$input.idx" "$http_root/verification.pack.idx"
cp "$input" "$http_root/v2-verification.pack"
cp "$input.idx" "$http_root/v2-verification.pack.idx"
request_log="$temp_dir/requests.jsonl"
port_file="$temp_dir/port"
: > "$request_log"
python3 "$script_dir/esi-http-server.py" \
  --root "$http_root" --log "$request_log" --port-file "$port_file" &
server_pid=$!
for _ in $(seq 1 100); do
  [[ -s "$port_file" ]] && break
  sleep 0.05
done
[[ -s "$port_file" ]] || fail "HTTP fixture did not start"
port=$(cat "$port_file")
base="http://127.0.0.1:$port"

"$elfshaker" esi create --meta v1 --url "$base/index.esi" "$http_root/index.esi"
"$elfshaker" esi add --pack-url "$base/verification.pack" \
  "$http_root/index.esi" "$input"
"$elfshaker" esi create --meta v2 --url "$base/index.v2.esi" "$http_root/index.v2.esi"
"$elfshaker" esi add --pack-url "$base/v2-verification.pack" \
  "$http_root/index.v2.esi" "$input"
cp "$http_root/index.v2.esi" "$temp_dir/valid-index.v2.esi"

clear_log() {
  : > "$request_log"
}

extract_first_snapshot() {
  local binary=$1
  local repository=$2
  local snapshot
  snapshot=$(cd "$repository" && "$binary" list | head -n 1)
  [[ -n "$snapshot" ]] || fail "no snapshots listed in $repository"
  (cd "$repository" && "$binary" extract --reset --verify "$snapshot")
}

clear_log
(cd "$clients" && "$elfshaker" clone "$base/index.esi" new-v2)
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.esi"'
assert_contains "$clients/new-v2/elfshaker_data/remotes/origin.esi" $'meta\tv2'
extract_first_snapshot "$elfshaker" "$clients/new-v2"
assert_contains "$request_log" '"path": "/v2-verification.pack"'

printf '404\n' > "$http_root/index.v2.esi.status"
clear_log
(cd "$clients" && "$elfshaker" clone "$base/index.esi" new-fallback)
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_contains "$request_log" '"path": "/index.esi"'
assert_contains "$clients/new-fallback/elfshaker_data/remotes/origin.esi" $'meta\tv1'

printf '500\n' > "$http_root/index.v2.esi.status"
clear_log
expect_fail bash -c "cd '$clients' && '$elfshaker' clone '$base/index.esi' new-500"
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.esi"'

rm "$http_root/index.v2.esi.status"
printf 'meta\tv2\nurl\t%s/index.v2.esi\nbroken\n' "$base" > "$http_root/index.v2.esi"
clear_log
expect_fail bash -c "cd '$clients' && '$elfshaker' clone '$base/index.esi' new-malformed"
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.esi"'
cp "$temp_dir/valid-index.v2.esi" "$http_root/index.v2.esi"

clear_log
(cd "$clients" && "$elfshaker" clone "$base/index.v2.esi" new-explicit-v2)
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.v2.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.esi"'

printf '404\n' > "$http_root/index.v2.esi.status"
clear_log
(cd "$clients" && "$elfshaker" clone "$base/index.esi" new-migrate)
rm "$http_root/index.v2.esi.status"
clear_log
(cd "$clients/new-migrate" && "$elfshaker" update)
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_contains "$request_log" '"path": "/v2-verification.pack.idx"'
assert_contains "$clients/new-migrate/elfshaker_data/remotes/origin.esi" $'meta\tv2'
python3 - "$request_log" <<'PY'
import json
import sys

records = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
probe = next(record for record in records if record["path"] == "/index.v2.esi")
if "if-modified-since" in probe["headers"]:
    raise SystemExit("v2 upgrade probe unexpectedly sent If-Modified-Since")
PY
clear_log
(cd "$clients/new-migrate" && "$elfshaker" update)
assert_contains "$request_log" '"path": "/index.v2.esi"'
assert_not_contains "$request_log" '"path": "/index.esi"'
python3 - "$request_log" <<'PY'
import json
import sys

records = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
probe = next(record for record in records if record["path"] == "/index.v2.esi")
if "if-modified-since" not in probe["headers"]:
    raise SystemExit("normal v2 update did not send If-Modified-Since")
PY

if [[ -n "$old_elfshaker" ]]; then
  clear_log
  (cd "$clients" && "$old_elfshaker" clone "$base/index.esi" old-v1)
  assert_contains "$request_log" '"path": "/index.esi"'
  assert_not_contains "$request_log" '"path": "/index.v2.esi"'
  (cd "$clients/old-v1" && "$old_elfshaker" update)
  extract_first_snapshot "$old_elfshaker" "$clients/old-v1"
  assert_contains "$request_log" '"path": "/verification.pack"'

  clear_log
  expect_old_format_error bash -c \
    "cd '$clients' && '$old_elfshaker' clone '$base/index.v2.esi' old-v2"
  assert_contains "$request_log" '"path": "/index.v2.esi"'

  expect_old_format_error bash -c \
    "cd '$clients/new-migrate' && '$old_elfshaker' update"
fi

echo "ESI checks passed"
