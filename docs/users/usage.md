# elfshaker — Usage guide

## Table of contents
- [Core concepts](#core-concepts)
- [Workflow](#workflow)
- [Create snapshot](#create-snapshot)
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
