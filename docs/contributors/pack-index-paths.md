# Portable paths in pack indexes

Pack indexes store repository-relative paths as `PackPath` values. A
`PackPath` owns canonical bytes and has no implicit host filesystem semantics.
Native paths appear only while elfshaker captures or materializes an entry.

## Logical path rules

`/` separates components. A path is non-empty and relative, with no empty,
`.` or `..` component and no NUL byte. Components retain their original case,
Unicode spelling, and bytes. A backslash in a Unix filename remains part of
the component.

Unix capture preserves component bytes. Windows capture requires each
component to be valid Unicode and stores its UTF-8 form. Listing escapes a
literal percent sign, control bytes, and invalid UTF-8 bytes as `%XX`; ordinary
UTF-8 names remain unchanged.

Store normalizes input paths lexically. It rejects absolute paths, parent
escapes, and duplicate logical paths. Symbolic links are entries in their own
right, and directory links are not traversed. The repository data directory is
excluded on a component boundary.

## Legacy indexes

Versions 1 and 2 encode paths with Serde's native `OsString` variants. The
reader accepts both variants on every supported host:

- Unix values become logical path bytes. `/` is the separator.
- Windows UTF-16 values are decoded strictly. `/` and `\` are separators.

An invalid UTF-16 value is an unsupported encoding. A decoded value that
breaks the logical path rules is an invalid logical path. Snapshot-tag-only
reads stop before the path pool, so they continue to work when an entry path
cannot be decoded.

Legacy entries become regular-file entries in memory. Reading an old index
does not rewrite it or require repository migration.

## Version 3

New indexes use version 3. The MessagePack envelope contains required feature
bits, snapshot tags and deltas, a binary path pool, the object pool, object
metadata, and optional extension records. Regular entries carry an explicit
metadata tag and mode.

Symlink entries carry a target encoding and target-kind hint. Their object
contains the link target. Portable relative UTF-8 targets work across hosts;
Unix-byte and Windows-UTF-16LE targets require the matching platform family.

Unknown required features stop a full read before entry decoding. Unknown
optional extensions are skipped. Older readers reject version 3 from the
header. Elfshaker writes version 3 only.

The `.pack` object stream is unchanged. Existing pack data can be paired with
an index converted from version 1 or 2 without recompressing its objects.

## Destination checks

Listing and transport preserve logical names even when the current host could
not create them. Extraction checks the complete selected path set before it
removes or writes an entry.

Every host rejects duplicate paths, file/directory prefix conflicts, names it
cannot convert, and existing symbolic-link ancestors. Windows also rejects
reserved device names, forbidden characters, trailing dots or spaces, and
case aliases. macOS uses a conservative case-insensitive, decomposed-Unicode
collision check. Unix compares exact component bytes.

Elfshaker reports these failures instead of renaming paths. Ordinary I/O can
still fail after preflight; hostile concurrent changes to the working tree are
outside this check.

## Snapshot identity

Snapshot identity hashes a domain marker and a path-sorted entry stream. Each
regular entry contributes its length-delimited logical path, entry kind,
object checksum, and mode. Equivalent legacy Unix and Windows paths therefore
have the same identity. Path spelling, contents, kind, and mode remain
significant.
