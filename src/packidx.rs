//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use crate::pathidx::PathTree;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::iter::FromIterator;

/// Error type used in the packidx module.
#[derive(Debug, Clone)]
pub enum PackError {
    CompleteListNeeded,
    PathNotFound,
    ObjectNotFound,
    ChecksumMismatch,
}

impl std::error::Error for PackError {}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            PackError::CompleteListNeeded => write!(
                f,
                "Expected a complete file list, but got the delta format instead!"
            ),
            PackError::ObjectNotFound => write!(f, "The object was not found!"),
            PackError::PathNotFound => write!(f, "The path was not found!"),
            PackError::ChecksumMismatch => write!(f, "The object checksum did not match!"),
        }
    }
}

pub type ObjectChecksum = [u8; 20];

/// Metadata for a object stored in a pack.
///
/// Contains a size and an offset (relative to the decompressed stream).
///
/// The checksum can be used to validate the contents of the object.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ObjectIndex {
    pub checksum: ObjectChecksum,
    pub offset: u64,
    pub size: u64,
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
        PackedFile {
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
    pub tag: String,
    pub list: PackedFileList,
}

impl Snapshot {
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
                    let set = HashSet::from_iter(c.into_iter().cloned());
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

/**
 * Represents the pack index in full.
 */
#[derive(Serialize, Deserialize, Debug)]
pub struct PackIndex {
    tree: PathTree,
    objects: Vec<ObjectIndex>,
    snapshots: Vec<Snapshot>,
}

impl PackIndex {
    pub fn new(tree: PathTree, objects: Vec<ObjectIndex>, snapshots: Vec<Snapshot>) -> Self {
        PackIndex {
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
        let mut set = Snapshot::create_set(self.snapshots.first()?).unwrap();
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
}
