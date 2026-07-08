# elfshaker — Usage guide

This guide covers the basic local snapshot workflow. Use `man elfshaker` or
`elfshaker --help` for the complete command reference.

## Before you start

- Work in an isolated directory that contains no irreplaceable files.
- Validate the workflow before relying on it where data loss would matter.
- Expect the best compression when snapshots contain many similar files.

## Core concepts

- The **working directory** contains the files that elfshaker stores and
  restores.
- The **data directory** contains packs, indexes, loose objects, remotes, and
  `HEAD`. It defaults to `./elfshaker_data` and can be selected with
  `--data-dir` or `ELFSHAKER_DATA`.
- A **repository** is a working directory and its associated data directory.
- A **snapshot** records a set of files from the working directory.
- A **loose snapshot** has not yet been combined into a compressed pack.
- A **pack** stores one or more snapshots as `<name>.pack` with an accompanying
  `<name>.pack.idx` index.

## Store snapshots

Start in the working directory:

```bash
elfshaker store build-001
```

Make changes, then store another snapshot:

```bash
elfshaker store build-002
```

Without a file-list option, `store` recursively selects files from the working
directory. To select explicit paths, pass a newline-delimited file:

```bash
elfshaker store build-003 --files-from paths.txt
```

Use `--files0-from` for a NUL-delimited list. Pass `-` as the filename to read
the list from standard input.

Stored paths are relative to the working directory. Elfshaker preserves Unix
filename bytes and stores Windows names as UTF-8. It rejects absolute paths,
paths that escape through `..`, and duplicate paths. Symbolic links are stored
as links without reading the files or directories they name. Directory links
are not traversed.

Snapshot tags consist of one or more `/`-separated components. Dots, visible
Unicode characters, punctuation, and internal ASCII spaces are accepted.
Components cannot be empty, `.` or `..`, begin or end with a space, or end with
a period. Quote tags containing spaces or shell punctuation.

Loose snapshot index paths mirror their tags beneath
`elfshaker_data/packs/loose`. Filesystem case handling, Unicode normalization,
and path-length limits therefore apply. A loose data directory that relies on
case or normalization distinctions may not copy safely to another filesystem.
Pack snapshots before sharing them between systems.

Pack indexes written by this version use the portable version 3 path format.
They can be listed and inspected on Linux, macOS, and Windows. Version 1 and 2
indexes from Linux or Windows remain readable.

## Pack loose snapshots

Combine all loose snapshots into a compressed pack:

```bash
elfshaker pack builds
```

This creates `elfshaker_data/packs/builds.pack` and
`elfshaker_data/packs/builds.pack.idx`. The loose source data remains until it
is removed with `gc`.

To pack selected snapshots, provide their names in a file:

```bash
elfshaker pack selected-builds --snapshots-from snapshots.txt
```

## List stored data

List known packs:

```bash
elfshaker list-packs
```

List snapshots from all packs, or from selected packs:

```bash
elfshaker list
elfshaker list builds another-pack
```

List files in a snapshot:

```bash
elfshaker list-files builds:build-002
```

Ordinary UTF-8 names print unchanged. `%XX` escapes represent control bytes,
literal percent signs, and filename bytes that are not valid UTF-8.
`list-files --format "%k %p %f"` prints each entry's kind, permissions, and
path. The kind is `file` or `symlink`; links have `-` in the permissions field.

## Extract a snapshot

Restore a snapshot into the working directory:

```bash
elfshaker extract builds:build-002 --verify
```

A plain snapshot tag also works when elfshaker can resolve it unambiguously:

```bash
elfshaker extract build-002
```

Extraction normally transitions incrementally from `HEAD`. It may delete files
that belonged to the previous snapshot but not the new one. Untracked files are
left alone.

Use `--reset` to ignore `HEAD` and write every file in the selected snapshot.
It does not remove unrelated paths. Use `--force` only when you intend to
bypass checks for unexpected working-directory modifications.

Before changing snapshot entries, elfshaker checks whether the destination can
represent the complete path set. Extraction fails without partial snapshot
changes for reserved names, case or Unicode aliases, file/directory prefix
conflicts, and symbolic-link ancestors. A pack may therefore be portable for
listing while containing a name that one destination cannot extract.

Relative Unicode link targets use a portable representation and extract on
every supported host with native symlink support. Absolute, non-Unicode, and
platform-specific targets retain their native encoding. Unix-native targets
extract only on Unix, and Windows-native targets only on Windows. Windows
packs record whether a link names a file or directory so dangling links can be
restored. Extraction fails before changing snapshot entries when the target
encoding or native symlink capability is unavailable; it never replaces a
link with a regular file.

## Write snapshot files to standard output

Write one file:

```bash
elfshaker show builds:build-002 bin/compiler > compiler
```

When multiple paths are supplied, `show` concatenates their contents without
separators. For a symbolic link, it writes the stored target rather than the
contents reached through the link.

## Remove redundant loose data

Inspect what garbage collection would remove:

```bash
elfshaker gc --dry-run --loose-snapshots --loose-objects
```

After checking the output, remove redundant loose snapshot indexes and
unreferenced loose objects:

```bash
elfshaker gc --loose-snapshots --loose-objects
```

Packed data is not removed.

## Publish packs

To make packs available through `elfshaker clone` and `elfshaker update`,
follow the [pack publishing guide](publishing.md). It covers remote indexes,
v1 compatibility, v2 size metadata, safe upload ordering, and validation from
a clean directory.
