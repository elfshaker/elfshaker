# ESI test plan

This plan verifies the ESI editing command, versioned endpoint lookup, and
compatibility with an elfshaker binary that predates `meta v2` support.

The compatibility promise covers old clients using the unversioned remote
endpoint. It does not make a local repository containing a v2 ESI readable by
a v1-only binary. The suite includes that case as an expected failure so the
boundary remains visible.

## Test artifacts

The tests use:

- the elfshaker binary built from the change under test;
- an old binary built from commit
  `9c18314b72b0a64544b96932fa10d64f82798328`, the last main commit before v2
  ESI parsing was added;
- the existing verification pack and its `.pack.idx`;
- a small HTTP fixture server implemented with the Python standard library.

The server binds an ephemeral local port, serves configurable status codes and
files, and records request paths and headers. One server handles all ESI, pack,
and pack-index routes. Tests can add the v2 route between commands to exercise
an in-place upgrade.

The old binary is always the pinned revision. Using whichever released binary
happens to be installed would make the result depend on the test machine.

## Rust unit tests

Unit tests cover the document model without starting a process or server:

- Parse v1 and v2 entries, including relative and absolute pack URLs.
- Reject unknown meta versions, missing fields, invalid checksums, invalid
  sizes, and extra fields.
- Read and write an unchanged ESI byte-for-byte, including tabs, spaces, blank
  lines, LF or CRLF endings, and a missing final newline.
- Add a v1 entry with three fields and a v2 entry with pack and index sizes.
- Reject duplicate entry URLs; replace the entry in place when forced.
- Reject a missing removal target; ignore it when forced.
- Preserve all untouched lines and entry ordering after add, replace, or
  remove.
- Derive `index.v2.esi` only from an unversioned `.esi` URL. Leave explicitly
  versioned URLs unchanged.
- Stage every target before committing a multi-index edit. An injected failure
  during the second replacement restores the first target.

Checksum tests use small fixed files with known SHA-1 values. The production
checksum path reads through a bounded buffer rather than loading a pack into
memory.

## Current CLI integration tests

A single `test-scripts/check-esi.sh` script receives the current binary, the
verification pack, and optionally the old binary. Its current-client section
checks:

1. `esi create` emits the requested meta and public URL.
2. `esi add` computes the expected checksums and sizes.
3. Options work before the positional ESI and pack arguments.
4. Adding several packs preserves argument order.
5. A duplicate fails without changing the ESI or backup.
6. `--force` replaces the matching line and leaves unrelated bytes untouched.
7. `esi remove` and forced removal follow the documented missing-entry rules.
8. Each successful mutation replaces `<ESI_FILE>.bak` with the exact previous
   file; `--no-backup` leaves no backup.
9. One `esi add --also` invocation updates v1 and v2 indexes with the same
   checksums, omitting sizes only from v1.
10. Missing pack or `.pack.idx`, an invalid target, or a failed secondary
    target leaves every ESI unchanged.
11. `esi list` reports the stored URL, checksums, and v2 sizes.

The tests compare complete files where practical. They use focused diffs only
to verify that an add, replacement, or removal changes the intended line.

## Client compatibility matrix

The HTTP section exercises the following matrix:

| Client and URL | Server state | Expected result |
| --- | --- | --- |
| Old client, `index.esi` | v1 and v2 present | Clone, update, and remote pack extraction succeed using only v1. |
| Old client, `index.v2.esi` | v2 present | Command exits with a format error and does not panic. |
| New client, `index.esi` | v1 and v2 present | Client requests v2, does not request v1, and succeeds. |
| New client, `index.esi` | v2 returns 404 | Client requests v2, falls back to v1, and succeeds. |
| New client, `index.esi` | v2 returns 500 or malformed data | Command fails without requesting v1. |
| New client, `index.v2.esi` | v2 present | Client fetches the exact URL without sibling lookup. |

The old-client success case verifies the actual strategy: publishing
`index.v2.esi` does not alter what an old binary receives from `index.esi`.
The explicit-v2 failure is a negative control proving that the test binary
really lacks v2 support.

The new-client update test has two phases:

1. Clone while v2 returns 404, leaving the repository on the legacy endpoint.
2. Add the v2 route and run `elfshaker update`.

The second phase must request v2 without an `If-Modified-Since` header derived
from the legacy file, install the v2 index, and fetch its pack indexes. A later
update uses the v2 URL normally.

Finally, running the old binary against that migrated local repository must
fail with the known v2 format error. This is an expected boundary, not a
regression in remote compatibility.

## CI and acceptance

Normal CI runs:

```text
cargo test
test-scripts/check.sh <new-elfshaker> <verification-pack>
test-scripts/check-esi.sh <new-elfshaker> <verification-pack> <old-elfshaker>
```

CI checks out enough repository history to build the pinned old revision in a
separate worktree and target directory. It builds that binary once, then runs
the whole compatibility matrix against it.

The change is complete when every test above passes on Linux and the ordinary
Rust and shell suites remain green. Windows continues to run the unit tests;
the local HTTP compatibility script remains a Linux CI test because the
existing shell harness depends on Unix tools.

The suite does not attempt to simulate power loss between two file renames.
It verifies validation-before-write, per-file atomic replacement, and rollback
after ordinary write failures.
