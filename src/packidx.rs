//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use crate::pathidx::{PathError, PathIndex, PathTree};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;

/// Error type used in the packidx module.
#[derive(Debug, Clone)]
pub enum PackError {
    PathError(PathError),
    CompleteListNeeded,
    PathNotFound,
    ObjectNotFound,
    SnapshotNotFound,
    /// A snapshot with that tag is already present in the pack
    SnapshotAlreadyExists,
    ChecksumMismatch,
}

impl std::error::Error for PackError {}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            PackError::PathError(e) => e.fmt(f),
            PackError::CompleteListNeeded => write!(
                f,
                "Expected a complete file list, but got the delta format instead!"
            ),
            PackError::ObjectNotFound => write!(f, "The object was not found!"),
            PackError::PathNotFound => write!(f, "The path was not found!"),
            PackError::SnapshotNotFound => write!(f, "The snapshot was not found!"),
            PackError::SnapshotAlreadyExists => write!(
                f,
                "A snapshot with this tag is already present in the pack!"
            ),
            PackError::ChecksumMismatch => write!(f, "The object checksum did not match!"),
        }
    }
}

pub type ObjectChecksum = [u8; 20];
pub const LOOSE_OBJECT_OFFSET: u64 = std::u64::MAX;

/// Metadata for an object stored in a pack.
///
/// Contains a size and an offset (relative to the decompressed stream).
///
/// The checksum can be used to validate the contents of the object.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ObjectIndex {
    checksum: ObjectChecksum,
    offset: u64,
    size: u64,
}

impl ObjectIndex {
    pub fn new(checksum: ObjectChecksum, offset: u64, size: u64) -> Self {
        Self {
            checksum,
            offset,
            size,
        }
    }
    pub fn loose(checksum: ObjectChecksum, size: u64) -> Self {
        Self {
            checksum,
            size,
            offset: LOOSE_OBJECT_OFFSET,
        }
    }

    pub fn checksum(&self) -> &ObjectChecksum {
        &self.checksum
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Identifies a file reference in a pack file.
///
/// The object_index can be used to get an ObjectIndex instance from the list
/// of ObjectIndexes in the pack index.AsRef
///
/// The tree_index can be used to get the file path from the PathTree.
#[derive(Hash, PartialEq, Clone, Serialize, Deserialize, Debug)]
pub struct PackedFile {
    tree_index: u32,
    object_index: u32,
}

impl Eq for PackedFile {}

impl PackedFile {
    pub fn new(tree_index: u32, object_index: u32) -> Self {
        Self {
            tree_index,
            object_index,
        }
    }
    pub fn tree_index(&self) -> u32 {
        self.tree_index
    }
    pub fn object_index(&self) -> u32 {
        self.object_index
    }
}

/// A set of changes that can be applied to a set of items
/// to get another set.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ChangeSet<T> {
    added: Vec<T>,
    removed: Vec<T>,
}

impl<T> ChangeSet<T> {
    pub fn new(added: Vec<T>, removed: Vec<T>) -> Self {
        Self { added, removed }
    }

    pub fn added(&self) -> &[T] {
        &self.added
    }
    pub fn removed(&self) -> &[T] {
        &self.removed
    }
}

/// A list of packed files can be stored either as a complete list of file
/// references or as a delta to be applied to the previous list.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum PackedFileList {
    Complete(Vec<PackedFile>),
    Delta(ChangeSet<PackedFile>),
}

/// A snapshot is identified by a string tag and specifies a list of files.
///
/// The list of files can be a complete list or a list diff.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Snapshot {
    tag: String,
    list: PackedFileList,
}

impl Snapshot {
    pub fn new(tag: &str, list: PackedFileList) -> Self {
        Self {
            tag: tag.to_owned(),
            list,
        }
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn list(&self) -> &PackedFileList {
        &self.list
    }

    pub fn compute_deltas<'a, I>(mut snapshots: I) -> Result<Vec<Snapshot>, PackError>
    where
        I: Iterator<Item = &'a Snapshot>,
    {
        let mut results: Vec<Snapshot> = vec![];
        match snapshots.next() {
            Some(first) => results.push(first.clone()),
            None => return Ok(vec![]),
        };
        // We keep track of the complete set of files at the current snapshot.
        let mut complete: HashSet<PackedFile> = results.last().unwrap().create_set()?;
        for snapshot in snapshots {
            match &snapshot.list {
                PackedFileList::Complete(c) => {
                    let set: HashSet<_, _> = c.iter().cloned().collect();
                    let changes = Self::get_changes(&mut complete, &set);
                    results.push(Snapshot {
                        tag: snapshot.tag.clone(),
                        list: changes,
                    });
                }
                PackedFileList::Delta(d) => {
                    // Snapshot is already in delta format.
                    Self::apply_changes(&mut complete, d);
                    results.push(snapshot.clone());
                }
            }
        }

        Ok(results)
    }

    fn apply_changes(set: &mut HashSet<PackedFile>, changes: &ChangeSet<PackedFile>) {
        for removed in &changes.removed {
            assert!(set.remove(removed));
        }
        for added in &changes.added {
            assert!(set.insert(added.clone()));
        }
    }
    fn get_changes(set: &mut HashSet<PackedFile>, next: &HashSet<PackedFile>) -> PackedFileList {
        let mut added = vec![];
        let mut removed = vec![];

        // This is a complete list. We need to diff it with the previous snapshot.
        for file in set.iter() {
            // Look for things that are in the previous snapshot `set`,
            // but not in the snapshot after that `next`.
            if !next.contains(file) {
                removed.push(file.clone());
            }
        }

        for file in next {
            // Look for things that are in the new snapshot,
            // but not in the one preceeding it.
            if !set.contains(file) {
                added.push(file.clone());
            }
        }

        for file in &added {
            // File was added in the new snapshot
            assert!(set.insert(file.clone()));
        }
        for file in &removed {
            // File was removed in the new snapshot
            assert!(set.remove(file));
        }

        PackedFileList::Delta(ChangeSet { added, removed })
    }

    fn create_set(&self) -> Result<HashSet<PackedFile>, PackError> {
        let mut set = HashSet::new();
        match &self.list {
            PackedFileList::Complete(c) => {
                for file in c {
                    assert!(set.insert(file.clone()));
                }
            }
            PackedFileList::Delta(_) => return Err(PackError::CompleteListNeeded),
        }
        Ok(set)
    }
}

/// Holds a path and object index, enough information to
/// extract any packed file. This is a useful representation to have at runtime.
#[derive(Clone, Debug)]
pub struct PackEntry {
    path: OsString,
    object_index: ObjectIndex,
}

impl PackEntry {
    pub fn new(path: &OsStr, object_index: ObjectIndex) -> Self {
        Self {
            path: path.to_os_string(),
            object_index,
        }
    }

    pub fn path(&self) -> &OsStr {
        &self.path
    }

    pub fn object_index(&self) -> &ObjectIndex {
        &self.object_index
    }
}

/**
 * Represents the pack index in full.
 */
#[derive(Default, Serialize, Deserialize)]
pub struct PackIndex {
    tree: PathTree,
    objects: Vec<ObjectIndex>,
    snapshots: Vec<Snapshot>,
}

impl PackIndex {
    pub fn new(tree: PathTree, objects: Vec<ObjectIndex>, snapshots: Vec<Snapshot>) -> Self {
        Self {
            tree,
            objects,
            snapshots,
        }
    }

    pub fn tree(&self) -> &PathTree {
        &self.tree
    }
    pub fn objects(&self) -> &[ObjectIndex] {
        &self.objects
    }
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    pub fn find_snapshot(&self, tag: &str) -> Option<Snapshot> {
        let first_snapshot = self.snapshots.first()?;
        if first_snapshot.tag == tag {
            if matches!(first_snapshot.list, PackedFileList::Delta(_)) {
                panic!("{:?}", PackError::CompleteListNeeded);
            }
            return Some(first_snapshot.clone());
        }
        let mut set = Snapshot::create_set(first_snapshot).unwrap();
        for snapshot in self.snapshots.iter().skip(1) {
            match snapshot.list {
                PackedFileList::Complete(_) => {
                    set = snapshot.create_set().unwrap();
                }
                PackedFileList::Delta(ref d) => {
                    Snapshot::apply_changes(&mut set, d);
                }
            }
            if snapshot.tag == tag {
                return Some(Snapshot {
                    tag: tag.into(),
                    list: PackedFileList::Complete(set.into_iter().collect()),
                });
            }
        }

        None
    }

    /// Creates a list of PackEntry for the given files.
    pub fn entries<I>(&self, files: I) -> Result<Vec<PackEntry>, PackError>
    where
        I: Iterator<Item = PackedFile>,
    {
        let path_lookup = self.tree.create_lookup();
        // Map the indexes to the correspondng path and object info.
        files
            .map(|x| -> Result<_, PackError> {
                let path = path_lookup
                    .get(&x.tree_index())
                    .ok_or(PackError::PathNotFound)?;
                let object = self
                    .objects
                    .get(x.object_index() as usize)
                    .ok_or(PackError::ObjectNotFound)?;
                Ok(PackEntry::new(path, object.clone()))
            })
            .collect::<Result<Vec<_>, PackError>>()
    }
    /// Returns the full list of [`PackEntry`].
    pub fn entries_from_snapshot(&self, tag: &str) -> Result<Vec<PackEntry>, PackError> {
        let snapshot = self.find_snapshot(tag).ok_or(PackError::SnapshotNotFound)?;

        // Convert the PackedFileList into a list of entries to write to disk.
        let entries = match snapshot.list {
            PackedFileList::Complete(ref list) => self.entries(list.iter().cloned())?,
            _ => unreachable!(),
        };
        Ok(entries)
    }

    /// Create and add a new snapshot compatible with the unpacked
    /// index format using the list of tuples of (filepath, checksum, filesize).
    pub fn push_snapshot(&mut self, tag: &str, input: &[PackEntry]) -> Result<(), PackError> {
        if self.snapshots().iter().any(|s| s.tag == tag) {
            return Err(PackError::SnapshotAlreadyExists);
        }

        // Expand all snapshots into String -> [PackEntry]
        let mut snapshots: Vec<(String, Vec<PackEntry>)> = self
            .snapshots
            .iter()
            .map(|s| Ok((s.tag.clone(), self.entries_from_snapshot(&s.tag)?)))
            .collect::<Result<_, PackError>>()?;

        let existing_objects: HashSet<&_> = self.objects().iter().map(|o| &o.checksum).collect();

        // Compute and extend with the set of objects which are not available in the unpacked storage
        let new_objects: Vec<_> = input
            .iter()
            .filter(|x| !existing_objects.contains(&x.object_index().checksum))
            .map(|x| x.object_index().clone())
            .collect();

        // Operating on a clone is safer, since the below operation might fail half-way.
        // If we were extending self.tree directly, a failure would corrupt the tree.
        let mut new_tree = self.tree.clone();
        for path in input.iter().map(|e| e.path()) {
            new_tree.create_file(path).map_err(PackError::PathError)?;
        }
        snapshots.push((tag.to_owned(), input.into()));
        let path_lookup = new_tree
            .create_lookup()
            .into_iter()
            .map(|(i, p)| (p, i))
            .collect();
        let object_lookup: HashMap<ObjectChecksum, u32> = self
            .objects
            .iter()
            .chain(new_objects.iter())
            .enumerate()
            .map(|(i, o)| (*o.checksum(), i as u32))
            .collect();
        let new_snapshots = self.to_snapshots(&path_lookup, &object_lookup, &snapshots)?;

        self.objects.extend_from_slice(&new_objects);
        self.snapshots = new_snapshots;
        self.tree = new_tree;

        Ok(())
    }

    fn to_snapshots(
        &self,
        path_lookup: &HashMap<OsString, u32>,
        object_lookup: &HashMap<ObjectChecksum, u32>,
        snapshots: &[(String, Vec<PackEntry>)],
    ) -> Result<Vec<Snapshot>, PackError> {
        snapshots
            .iter()
            .map(|s| {
                let list =
                    s.1.iter()
                        .map(|e| self.entry_to_packed_file(e, path_lookup, object_lookup))
                        .collect::<Result<Vec<_>, PackError>>()?;
                Ok(Snapshot::new(&s.0, PackedFileList::Complete(list)))
            })
            .collect::<Result<Vec<Snapshot>, PackError>>()
    }

    /// Converts between the [`PackEntry`] (runtime) and [`PackedFile`] (on-disk) representation.
    fn entry_to_packed_file(
        &self,
        entry: &PackEntry,
        path_lookup: &HashMap<OsString, u32>,
        object_lookup: &HashMap<ObjectChecksum, u32>,
    ) -> Result<PackedFile, PackError> {
        let tree_index = path_lookup
            .get(entry.path())
            .ok_or(PackError::PathNotFound)?;
        let object_index = object_lookup
            .get(entry.object_index().checksum())
            .ok_or(PackError::ObjectNotFound)?;
        Ok(PackedFile::new(*tree_index, *object_index))
    }
}

/// Custom debug representation
impl fmt::Debug for PackIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[derive(Debug)]
        struct DebugPackIndex {
            objects: usize,
            snapshots: usize,
            paths: usize,
        }

        let index = DebugPackIndex {
            objects: self.objects().len(),
            snapshots: self.snapshots().len(),
            paths: self.tree().file_count(),
        };
        fmt::Debug::fmt(&index, f)
    }
}
