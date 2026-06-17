# Snapshot tags

Snapshot tags are logical names stored as UTF-8 strings in pack indexes. A
loose snapshot also uses its tag to derive an index path. Pack files are the
portable form for moving snapshots between systems.

## Syntax

A tag contains one or more `/`-separated components.

Each component:

- is non-empty and is not `.` or `..`;
- contains Unicode letters, marks, numbers, punctuation, or symbols;
- may contain ASCII spaces except at the beginning or end;
- may contain periods except as its final character;
- does not contain `:`, `\`, `<`, `>`, `"`, `|`, `?`, or `*`;
- does not contain Unicode control, format, private-use, unassigned, separator,
  or surrogate categories;
- does not use a Windows device name with a dotted suffix, such as `CON.txt`
  or `LPT1.log`, compared case-insensitively before its first period.

Existing ASCII names such as `CON`, `PRN`, `AUX`, `NUL`, `COM1` through `COM9`
and `LPT1` through `LPT9` remain valid. Filesystems that reserve these names
cannot store them as loose indexes; pack those snapshots before transfer.

A non-final component cannot end in `.pack.idx`, compared
ASCII-case-insensitively. Without this rule, the index for tag `a` and the
directory needed by tag `a.pack.idx/b` require the same path.

Examples:

- Valid: `v1.2.3`, `release 1.2`, `llvm/18.1.2`, `.candidate`,
  `résumé/β.2`.
- Invalid: `.`, `..`, `a/../b`, `a//b`, `a:b`, `a\b`, `a.`,
  `foo.pack.idx/bar`, or a tag containing a tab or newline.

Tags are case-sensitive. Elfshaker preserves their exact Unicode spelling and
does not normalize them.

## Loose index paths

The loose index for tag `release/v1.2` is:

```text
elfshaker_data/packs/loose/release/v1.2.pack.idx
```

Path construction appends `.pack.idx` literally. Filesystem extension
replacement is not used because it would turn `v1.2` into `v1.pack.idx`.
Existing tags containing only ASCII letters, digits, `-`, and `_` keep their
established paths.

The tag inside a loose index is authoritative. Repository discovery rebuilds
the logical loose pack ID from that stored tag rather than trusting the
filesystem spelling. Before replacing an existing loose index, elfshaker reads
its tag and requires an exact match. A case-folding or normalization collision
returns an error and leaves the existing index intact.

Logical loose pack IDs use the reserved `loose/<tag>` namespace. Regular pack
creation does not accept names in this namespace.

Pack identifiers are validated again when converted into filesystem paths.
This protects repository operations from absolute paths and traversal even
when library callers construct `PackId::Pack` directly.

Deleting a nested loose index removes empty tag directories up to, but not
including, `packs/loose`.

## Portability

Raw loose paths inherit filesystem behavior. Two tags that differ only by case
or Unicode normalization may coexist on one filesystem and collide on another.
Component-length and total-path limits also vary. Leading-period components
produce hidden paths on Unix.

These limits apply to loose storage, not tags inside a pack index. Pack
snapshots before copying them between systems.

## Test obligations

Changes to this contract cover:

- accepted and rejected Unicode categories and component forms;
- Windows device names and reserved characters;
- exact canonical-ID and `HEAD` round trips;
- literal suffix handling for dotted tags;
- collision detection without overwrite;
- store, list, extract, pack, and garbage-collection behavior;
- cleanup of empty hierarchy directories;
- validation of unchecked `PackId` values at storage boundaries.
