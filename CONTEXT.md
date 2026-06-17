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

## Relationships

- A **Repository** has exactly one **Working directory**
- A **Repository** has exactly one **Data directory**
- A **Data directory** may be located outside its **Working directory**

## Example dialogue

> **Dev:** "Does `--data-dir` change where `extract` writes files?"
> **Domain expert:** "No. It selects the **Data directory**; `extract` writes
> into the current **Working directory**."

## Flagged ambiguities

- "repository" previously referred to both the **Working directory** and the
  **Data directory**; it now means the pair.
