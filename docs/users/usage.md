# elfshaker — Usage guide

## Table of contents
- [Core concepts](#core-concepts)
- [Workflow](#workflow)
- [Create snapshot](#create-snapshot)
- [Append builds to a pack](#append-builds-to-a-pack)
- [Refresh remote packs](#refresh-remote-packs)
- [Extract snapshot](#extract-snapshot)
- [List packs, snapshots, files](#extract-snapshot)
- [Write files to stdout](#write-files-to-stdout)

**Important: Make sure you understand the following.**

- Remember there is **no warranty**, per the license on this software!
- Do your own validation before using it in any situation where **data loss** would be a problem.
- We recommend working in an **isolated directory** which does not contain important files (you may want to copy files in).
- elfshaker excels at storing lots of very **similar files**.

## Core concepts

**Important: Understanding of the following concepts is critical to working with the tool.**

- Snapshot
    - Stores a directory state (a set of files)
    - Created by `elfshaker store`
    - Can be packed into a pack file by `elfshaker pack` (otherwise called a loose snapshot)
- Repository
    - The parent directory of the directory `elfshaker_data`
    - The directory from which snapshots are created and into which they are extracted
- Packs
    - Store a set of snapshots
    - Represented by `[packname].pack` + `[packname].pack.idx`
    - Must reside under `./elfshaker_data/packs/`
    - Created by `elfshaker pack`
- elfshaker_data
    - Contains loose snapshots and pack files
    - Put the `.pack` + `.pack.idx` files under `./elfshaker_data/packs`
    - Created automatically by `elfshaker store` on its first run
    - Can be created manually (if you plan to use existing packs; for example from [manyclangs](https://github.com/elfshaker/manyclangs))

## Workflow

A basic workflow in elfshaker is to:
1) Add some files
2) Store them in a snapshot
3) Make some changes to the files
4) Store them again
5) Repeat 2) and 3) a number of times
6) Then, when you have lots of snapshots, they can be stored efficiently in pack, but will still be quickly accessible

## Create snapshot
```bash
elfshaker store <snapshot> [--files-from <file>] [--files0-from <file>]
```

### Example
```bash
elfshaker store my-snapshot
```

### Description
Creates the snapshot `my-snapshot` containing all files in the elfshaker repository.

*For full command usage, use the `--help` option.*
```bash
elfshaker store --help
```

### Implementation
1. Compute the checksums for all input file names.
2. Compare these checksums against the set of all loose objects (objects stored as part of a loose snapshot; not in a pack).
3. Store any new files as objects in the loose object store.
4. Create a loose pack index `<snapshot>.pack.idx` representing the snapshot in `./elfshaker_data/packs/loose`.
5. Sets the current snapshot reference `elfshaker_data/HEAD` to the string `loose/<snapshot>:<snapshot>`.

## Extract snapshot
```bash
elfshaker extract [<pack>:]<snapshot> [--reset] [--verify]
```

### Example
```bash
elfshaker extract my-pack:my-snapshot --verify
```

### Description
Extracts the snapshot `my-snapshot` from the pack `my-pack` (interpreted as `elfshaker_data/packs/my-pack.pack`) into the repository directory and verifies the files checksums during the extraction process.

*For full command usage, use the `--help` option.*
```bash
elfshaker extract --help
```

### Implementation
1. Look-up the set of packs in `elfshaker_data/packs` to see which packs contain the specified snapshot (skipped if specified as `my-pack:my-snapshot`).
2. Read the corresponding pack index and find the set of files in the snapshot.
3. Extract the files to the repository directory.
    a. If `--reset` is NOT specified, do perform an incremental update of the repository directory by comparing the set of files against the last extracted snapshot (`elfshaker_data/HEAD`).
    b. If `--reset` is specified, ignore `elfshaker_data/HEAD` and extract everything, overwriting if file names clash.

## Pack loose snapshots
```bash
elfshaker pack <pack> [--frames N]
```

The set of snapshots to be packed may be supplied with
--snapshots-from=<file> or --snapshots0-from=<file> for null delimited
lists. Otherwise all snapshots are packed.

### Example
```bash
elfshaker pack my-pack --frames 8
```

### Description
Creates the pack `my-pack` (file is `elfshaker_data/packs/my-pack.idx`) by packing all loose snapshots.

### Implementation
1. Enumerate all loose object files (belonging to loose snapshot) in `elfshaker_data/loose`.
2. Preprocess, sort and compress the objects, producing a .pack file with 8 frames.

    🛈 Our experiments indicate that 1 frame per 512 MiB is optimal for packing builds of LLVM and that is what omitting `--frames` does.

3. Combine all snapshots into a `<pack>.pack.idx`.

## Append builds to a pack

Create a pack, then append new builds as they become available:

```bash
elfshaker store build-0001
elfshaker pack current-month loose/build-0001

# After the next build finishes:
elfshaker store build-0002
elfshaker pack --append current-month loose/build-0002
elfshaker extract current-month:build-0002 --verify
```

Append accepts one or more loose snapshot indexes, or `--snapshots-from` /
`--snapshots0-from` lists of loose snapshots. Supply them in build order. Without
an explicit selection it tries all loose snapshots. Duplicate snapshot tags are
errors, even when their contents match. The target must already exist locally and
have a frame directory; headerless legacy packs must first be exploded and repacked.

Identical objects are reused by checksum across the entire pack. New versions of
a path are merged into the frame containing its last-seen version, including paths
that disappeared in an intervening snapshot. New paths use the frame with the
closest object-size range. Only affected frames are decompressed and recompressed;
unaffected compressed frames are copied unchanged. All historical objects and
snapshots remain accessible. Old loose objects are unnecessary for append; new
loose objects and rewritten old objects are verified before publication.

`--frame-size` sets the target decompressed frame size in MiB (default **512**).
`--frame-fill` sets the initial and post-split fill percentage (default **75**).
When old plus incoming objects exceed the target, the combined objects are sorted
by size and split into frames at the fill budget. A single object is never split,
so an oversized object occupies a frame by itself. Use the same settings when
creating the initial pack and appending:

```bash
elfshaker pack --frame-size 512 --frame-fill 75 current-month loose/build-0001
elfshaker pack --append --frame-size 512 --frame-fill 75 current-month loose/build-0002
```

The size and fill settings are supplied per command, not stored in the pack.
`--frames` remains available for an explicit frame count during initial packing;
it cannot be combined with append or explicit size/fill options. Compression uses
one encoder thread per frame, with `--threads` controlling parallel frame work.
Rewritten frames use the command's compression level; unaffected frames keep theirs.

Append needs temporary disk space for the replacement compressed pack and index.
It copies the compressed pack even when only a few frames change, and keeps the
compressed output plus active frames in memory. Lower `--threads` to reduce the
number of frames being decompressed and recompressed simultaneously.

The repository lock serializes readers and writers. A prepared pack/index pair is
published through a recovery journal: if interrupted, opening the repository with
an append-capable elfshaker finishes publication before reading either file. Keep
`elfshaker_data/.append-transaction` intact until recovery succeeds. Recovery needs
write access. After an interruption, check the target's snapshot list before
retrying; publication may already have completed. HEAD follows a packed loose
snapshot after normal success; if interrupted earlier, it can still point to the
retained loose snapshot. Append does not remove loose snapshots or objects.

The pack and index formats are unchanged, so completed packs remain readable by
older versions. Older versions do not recover unfinished append transactions;
finish recovery with the updated elfshaker before using them or publishing files.
Remote publication still requires a matching pack/index pair and updated manifest.

To compact into a new pack, make each unique object loose once and repack the index:

```bash
elfshaker explode current-month
elfshaker pack sealed-month current-month
```

There is no need to extract and store every historical snapshot individually.

## List packs, snapshots, files
```bash
(1) elfshaker list-packs
(2) elfshaker list <pack>...
(3) elfshaker list-files <snapshot>
```

### Description
(1) - Lists the names of all available packs AND loose snapshots (identified by prefix `loose/`).

(2) - Lists all snapshots available in `<pack>`.

(3) - Lists all files stored in `<snapshot>`.

## Write files to stdout
```bash
elfshaker show [<pack>:]<snapshot> <path>...
```

### Example
```bash
elfshaker show my-pack:my-snapshot my-file some-dir/my-file-2
```

### Description
Writes the contents of the files specified by the given paths in the snapshot to stdout.

## Cleanup loose snapshots and objects after creating a pack
```bash
(1) elfshaker gc -s
(2) elfshaker gc -o
(3) elfshaker gc -so
```

### Description
(1) - Cleanup redundant loose snapshots.

(2) - Cleanup unreferenced loose objects.

(3) - Cleanup snapshots and objects.

## Refresh remote packs

```sh
elfshaker clone https://example.org/index.esi builds
cd builds
elfshaker update
```

`update` refreshes the manifest and verifies index downloads against its SHA-1
hashes. It also hashes all cached remote payloads: unchanged packs stay cached,
while changed or corrupt payloads are discarded and downloaded on demand by
extraction. Reading all cached payloads can take time on large stores. Run
`update` once when upgrading an existing store from an older client.

Downloads are verified before installation. A publisher changing a pack/index
pair while it is being downloaded can cause a checksum error; rerun `update`
and then extraction after publication finishes. After an interrupted update,
a manifest/index mismatch is reported with instructions to run `update` again.
No pack or manifest format migration is needed. Remote publishers should retain
existing snapshot tags and their contents when appending monthly builds.
