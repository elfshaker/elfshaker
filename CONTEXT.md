# elfshaker

elfshaker stores and restores snapshots of a working directory while keeping
its storage state in a separate data directory.

## Language

**Working directory**:
The directory whose files elfshaker stores in and restores from snapshots.
_Avoid_: Repository directory, repository

**Data directory**:
The directory containing elfshaker's packs, indexes, loose objects, remotes,
and current snapshot reference.
_Avoid_: Repository, working directory

**Repository**:
A working directory and its associated data directory.

**Remote index (.esi)**:
A small published catalog of packs. It records each pack's URL and checksums
and, in v2, its byte sizes. The `esi` commands edit local remote-index files;
`clone` and `update` consume their published form.

## Relationships

- A **Repository** has exactly one **Working directory**
- A **Repository** has exactly one **Data directory**
- A **Data directory** may be located outside its **Working directory**
- A **Remote index** describes packs available from a remote source
- Publisher-facing `esi` commands edit explicitly named **Remote index** files
  independently of a **Data directory**

## Example dialogue

> **Dev:** "Does `--data-dir` change where `extract` writes files?"
> **Domain expert:** "No. It selects the **Data directory**; `extract` writes
> into the current **Working directory**."

## Flagged ambiguities

- "repository" previously referred to both the **Working directory** and the
  **Data directory**; it now means the pair.
