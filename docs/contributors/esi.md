# ESI index design

An ESI file describes the packs published by a remote elfshaker repository.
The `meta` field versions the ESI wire format. Elfshaker release versions and
ESI format versions are independent.

The [ESI test plan](esi-test-plan.md) defines the required unit, integration,
and old-client compatibility coverage.

## Versioned endpoints

The unversioned `index.esi` endpoint remains the compatibility endpoint for
clients that only understand `meta v1`. A versioned endpoint contains the
matching wire format:

| Endpoint | Wire format |
| --- | --- |
| `index.esi` | `meta v1` |
| `index.v1.esi` | `meta v1` |
| `index.v2.esi` | `meta v2` |

The invariant is that `.vN.esi` contains `meta vN`. The unversioned endpoint is
a permanent legacy alias rather than an alias for the newest format. This lets
publishers add formats without changing the resource requested by old clients.

When cloning from an unversioned ESI URL, a newer client first requests the
highest format it supports. A client that supports v2 therefore tries the
`.v2.esi` sibling before the supplied URL. It falls back to the unversioned URL
only when the versioned resource returns HTTP 404. Authentication errors,
transport errors, server errors, and malformed versioned indexes remain
visible instead of being hidden by a fallback.

Updates keep the endpoint recorded in the local index. An existing v1 remote
continues to use v1 when a publisher adds v2, so the repository remains usable
with older clients. A remote created from a v2 endpoint continues to use v2.
Updates send the modification time of that same local resource for conditional
requests. Explicitly versioned URLs are fetched as written.

## Editing command

The `elfshaker esi` command owns local ESI creation and entry maintenance:

```text
elfshaker esi create [OPTIONS] <ESI_FILE>
elfshaker esi list <ESI_FILE>
elfshaker esi add [OPTIONS] <ESI_FILE> <PACK>...
elfshaker esi remove [OPTIONS] <ESI_FILE> <PACK_URL>...
```

Options precede the positional arguments in help and examples. In particular,
the variadic `<PACK>...` argument remains last so shell globs and generated
argument lists are unambiguous.

`esi create` requires the wire-format version and the public URL recorded in
the index. It refuses to replace an existing file unless `--force` is present.
`esi add` reads each pack and its adjacent `.pack.idx`, computes
their SHA-1 checksums with streaming I/O, and records their sizes when writing
v2. The entry URL defaults to the pack filename encoded as one relative URL
path segment. `--pack-url` supplies a different stored URL when adding exactly
one pack.

Index and pack URL fields are non-empty and contain no literal whitespace,
because ESI records are whitespace-delimited. Publishers percent-encode any
whitespace required by the URL.

Adding an existing pack URL is an error. `--force` replaces that entry in its
current position. Removing a missing URL is likewise an error unless
`--force` is present.

A repeated `--also <ESI_FILE>` option applies one add or remove operation to
several indexes. Pack files are hashed once, then each target writes the fields
required by its own `meta` version. This keeps `index.esi` and `index.v2.esi`
in step without reading large pack files twice.

Every changed existing index is copied byte-for-byte to `<ESI_FILE>.bak`
before atomic replacement. The backup is replaced on the next successful
change. `--no-backup` disables this behaviour. Validation and hashing finish
before any target changes, and a failed multi-index operation does not leave
only part of the target set updated.

An operation rejects target sets where a target or derived backup path aliases
another target or backup path.

Unchanged headers, blank lines, entry order, URL spelling, line endings, and
entry lines remain byte-for-byte intact. New or replaced entries use
tab-separated canonical formatting. Backups therefore produce focused diffs.

`esi list` prints entries in their stored order using that canonical field
order: index checksum, pack checksum, pack URL, and, for v2, pack size and pack
index size.

## Publishing workflow

The user-facing [pack publishing guide](../users/publishing.md) owns the
operational procedure. The design requires publishers to keep the unversioned
v1 compatibility index and the v2 sibling in step. The `--also` option applies
one add or remove operation to both local files before the surrounding release
workflow publishes them.
