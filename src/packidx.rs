//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::entrypool::{EntryPool, Handle};
use crate::repo::partition_by_u64;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::iter::FromIterator;

/// Error type used in the packidx module.
#[derive(Debug, Clone)]
pub enum PackError {
    CompleteListNeeded,
    PathNotFound(Handle),
    ObjectNotFound,
    SnapshotNotFound(String),
    /// A snapshot with that tag is already present in the pack
    SnapshotAlreadyExists(String, String),
    ChecksumMismatch(ObjectChecksum, ObjectChecksum),
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
            PackError::PathNotFound(p) => write!(f, "Corrupt pack, PathHandle {:?} not found", p),
            PackError::SnapshotNotFound(s) => write!(f, "The snapshot '{}' was not found!", s),
            PackError::SnapshotAlreadyExists(p, s) => write!(
                f,
                "A snapshot with the tag '{}' is already present in the pack '{}'!",
                s, p,
            ),
            PackError::ChecksumMismatch(exp, got) => write!(
                f,
                "The object checksum did not match! exp {} got {}",
                hex::encode(exp),
                hex::encode(got)
            ),
        }
    }
}

/// The content checksum of an object.
pub type ObjectChecksum = [u8; 20];
/// The offset used for [`ObjectEntry::offset`], when the object is loose (not
/// in a pack file).
pub const LOOSE_OBJECT_OFFSET: u64 = std::u64::MAX;

/// A [`FileHandle`] identifies a file stored in a pack. It contains two
/// handles: a path, which can be used to get the path of the file
/// from the index path_pool, and an object, which can be used to get
/// the corresponding [`ObjectChecksum`] and [`ObjectMetadata`].
///
/// [`FileEntry`] and [`FileHandle`] can both be used to find the path and
/// object of a file, but [`FileHandle`] has an additional level of indirection,
/// because it stores handles, not the actual values themselves.
///
/// [`FileHandle`] is the representation that gets written to disk.
#[derive(Hash, PartialEq, Clone, Copy, Serialize, Deserialize, Debug)]
pub struct FileHandle {
    pub path: Handle, // offset into path_pool
    pub object: Handle, // ofset into object_pool
}

impl Eq for FileHandle {}

impl FileHandle {
    pub fn new(path: Handle, object: Handle) -> Self {
        Self { path, object }
    }
}

/// A set of changes that can be applied to a set of items
/// to get another set.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ChangeSet<T> {
    added: Vec<T>,
    removed: Vec<T>,
}

impl FromIterator<FileHandle> for ChangeSet<FileHandle> {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = FileHandle>,
    {
        Self {
            added: iter.into_iter().collect(),
            removed: Vec::new(),
        }
    }
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

/// A snapshot is identified by a string tag and specifies a list of files.
///
/// The list of files can be a complete list or a list diff.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Snapshot {
    tag: String,
    list: ChangeSet<FileHandle>,
}

impl Snapshot {
    pub fn new(tag: &str, list: ChangeSet<FileHandle>) -> Self {
        Self {
            tag: tag.to_owned(),
            list,
        }
    }

    pub fn n_added(&self) -> usize {
        self.list.added.len()
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    fn apply_changes(set: &mut HashSet<FileHandle>, changes: &ChangeSet<FileHandle>) {
        for removed in &changes.removed {
            assert!(set.remove(removed));
        }
        for added in &changes.added {
            assert!(set.insert(*added));
        }
    }

    pub fn get_changes(
        set: &mut HashSet<FileHandle>,
        next: &HashSet<FileHandle>,
    ) -> ChangeSet<FileHandle> {
        let mut added = vec![];
        let mut removed = vec![];

        // This is a complete list. We need to diff it with the previous snapshot.
        for file in set.iter() {
            // Look for things that are in the previous snapshot `set`,
            // but not in the snapshot after that `next`.
            if !next.contains(file) {
                removed.push(*file);
            }
        }

        for file in next {
            // Look for things that are in the new snapshot,
            // but not in the one preceding it.
            if !set.contains(file) {
                added.push(*file);
            }
        }

        for file in &added {
            // File was added in the new snapshot
            assert!(set.insert(*file));
        }
        for file in &removed {
            // File was removed in the new snapshot
            assert!(set.remove(file));
        }

        ChangeSet { added, removed }
    }
}

/// A [`FileEntry`] can be used to identify a specific file in a pack.
///
/// This is a practical representation to have at runtime, but has a higher
/// memory cost, compared to [`FileHandle`], contains handles to the path and to
/// the object metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: OsString,
    pub checksum: ObjectChecksum,
    pub metadata: ObjectMetadata,
}

impl FileEntry {
    pub fn new(path: OsString, checksum: ObjectChecksum, metadata: ObjectMetadata) -> Self {
        Self {
            path,
            checksum,
            metadata,
        }
    }
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ObjectMetadata {
    pub offset: u64,
    pub size: u64,
}

/// Contains the metadata needed to extract files from a pack file.
#[derive(Serialize, Deserialize)]
pub struct PackIndex {
    path_pool: EntryPool<OsString>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: HashMap<Handle, ObjectMetadata>,

    snapshots: Vec<Snapshot>,

    // When snapshots are pushed, maintain the current state of the filesystem.
    #[serde(skip)]
    current: HashSet<FileHandle>,
}

impl Default for PackIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl PackIndex {
    pub fn new() -> Self {
        Self {
            path_pool: EntryPool::new(),
            object_pool: EntryPool::new(),
            object_metadata: HashMap::new(),
            snapshots: Vec::new(),
            current: HashSet::new(),
        }
    }

    pub fn object_size_total(&self) -> u64 {
        self.object_metadata.values().map(|x| x.size).sum()
    }
    pub fn object_checksums(&self) -> impl ExactSizeIterator<Item = &ObjectChecksum> {
        self.object_pool.iter()
    }
    pub fn object_metadata(&self, checksum: &ObjectChecksum) -> &ObjectMetadata {
        let handle = self.object_pool.get(checksum).unwrap();
        self.object_metadata.get(&handle).unwrap()
    }
    pub(crate) fn objects_partitioned_by_size<'l>(
        &self,
        partitions: u32,
        handles: &'l [Handle],
    ) -> Vec<&'l [Handle]> {
        partition_by_u64(handles, partitions, |handle| {
            self.object_metadata.get(handle).unwrap().size
        })
    }
    pub(crate) fn compute_object_offsets_and_ordering(mut self) -> (Self, Vec<Handle>) {
        let mut size_handle = self
            .object_metadata
            .iter()
            .map(|(handle, metadata)| (metadata.size, *handle))
            .collect::<Vec<_>>();

        // Heuristic for good compression: Sort objects by size. This happens to
        // put similar objects next to each other. Can use a faster unstable
        // sort because 'ties' in size are broken according to the order objects
        // were added to the object pool.
        size_handle.sort_unstable();

        // Update object metadata to reference new offsets.
        let mut offset = 0;
        for (size, handle) in &size_handle {
            self.object_metadata.get_mut(handle).unwrap().offset = offset;
            offset += size;
        }

        let handles = size_handle
            .into_iter()
            .map(|(_size, handle)| handle)
            .collect();

        (self, handles)
    }

    pub fn handle_to_checksum(&self, h: Handle) -> &ObjectChecksum {
        self.object_pool.lookup(h).unwrap()
    }
    pub fn handle_to_entry(&self, handle: &FileHandle) -> Result<FileEntry, PackError> {
        Ok(FileEntry {
            path: self
                .path_pool
                .lookup(handle.path)
                .ok_or(PackError::PathNotFound(handle.path))?
                .clone(),
            checksum: *self
                .object_pool
                .lookup(handle.object)
                .ok_or(PackError::ObjectNotFound)?,
            metadata: *self.object_metadata.get(&handle.object).unwrap(),
        })
    }
    pub fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError> {
        let object_handle = self.object_pool.get_or_insert(&entry.checksum);
        self.object_metadata.insert(object_handle, entry.metadata);
        Ok(FileHandle {
            path: self.path_pool.get_or_insert(&entry.path),
            object: object_handle,
        })
    }

    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    /// Returns true if the pack index is empty.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    pub fn find_snapshot(&self, tag: &str) -> Option<Snapshot> {
        let mut current = HashSet::new();
        for snapshot in &self.snapshots {
            Snapshot::apply_changes(&mut current, &snapshot.list);
            if snapshot.tag == tag {
                return Some(Snapshot {
                    tag: tag.into(),
                    list: ChangeSet::from_iter(current),
                });
            }
        }

        None
    }
    pub fn entries_from_snapshot(&self, tag: &str) -> Result<Vec<FileEntry>, PackError> {
        let snapshot = self
            .find_snapshot(tag)
            .ok_or_else(|| PackError::SnapshotNotFound(tag.to_owned()))?;

        assert_eq!(0, snapshot.list.removed.len());
        snapshot
            .list
            .added
            .iter()
            .map(|h| self.handle_to_entry(h))
            .collect::<Result<Vec<_>, _>>()
    }

    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    pub fn push_snapshot(
        &mut self,
        tag: &str,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError> {
        if self.snapshots().iter().any(|s| s.tag == tag) {
            return Err(PackError::SnapshotAlreadyExists(
                "<unknown>".into(),
                tag.to_owned(),
            ));
        }

        let files = input
            .into_iter()
            .into_iter()
            .map(|e| self.entry_to_handle(&e))
            .collect::<Result<_, _>>()?;

        // Compute delta against last pushed snapshot (temporary implementation).
        let changeset = Snapshot::get_changes(&mut self.current, &files);
        self.current = files;
        self.snapshots.push(Snapshot::new(tag, changeset));
        Ok(())
    }
}
