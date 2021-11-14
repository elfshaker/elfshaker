//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::entrypool::{EntryPool, Handle};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
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
            PackError::PathNotFound(p) => write!(f, "Corrupt pack, PathHandle {:?} not found", p),
            PackError::SnapshotNotFound(s) => write!(f, "The snapshot '{}' was not found!", s),
            PackError::SnapshotAlreadyExists(p, s) => write!(
                f,
                "A snapshot with the tag '{}' is already present in the pack '{}'!",
                s, p,
            ),
            PackError::ChecksumMismatch => write!(f, "The object checksum did not match!"),
        }
    }
}

/// [`ObjectHandle`]s can be used to get an [`ObjectEntry`] from a
/// [`PackIndex`].
#[derive(Hash, Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Copy)]
pub struct ObjectHandle(u32);

impl From<u32> for ObjectHandle {
    fn from(o: u32) -> Self {
        ObjectHandle(o)
    }
}

impl From<ObjectHandle> for u32 {
    fn from(o: ObjectHandle) -> Self {
        o.0
    }
}

/// The content checksum of an object.
pub type ObjectChecksum = [u8; 20];
/// The offset used for [`ObjectEntry::offset`], when the object is loose (not
/// in a pack file).
pub const LOOSE_OBJECT_OFFSET: u64 = std::u64::MAX;

/// Metadata for an object stored in a pack.
///
/// Contains a size and an offset (relative to the decompressed stream).
///
/// The checksum can be used to validate the contents of the object.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct ObjectEntry {
    pub checksum: ObjectChecksum,
    pub offset: u64,
    pub size: u64,
}

impl ObjectEntry {
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
}

/// A [`FileHandle`] identifies a file stored in a pack. It contains two
/// handles: a [`PathHandle`], which can be used to get the path of the file
/// from the index path_pool, and an [`ObjectHandle`], which can be used to get
/// the corresponding [`ObjectEntry`].
///
/// [`FileEntry`] and [`FileHandle`] can both be used to find the path and
/// object of a file, but [`FileHandle`] has an additional level of indirection,
/// because it stores handles, not the actual values themselves.
///
/// [`FileHandle`] is the representation that gets written to disk.
#[derive(Hash, PartialEq, Clone, Copy, Serialize, Deserialize, Debug)]
pub struct FileHandle {
    pub path: Handle,
    // Index into [`PackIndex::objects`].
    pub object: ObjectHandle,
}

impl Eq for FileHandle {}

impl FileHandle {
    pub fn new(path: Handle, object: ObjectHandle) -> Self {
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileEntry {
    pub path: OsString,
    pub object: ObjectEntry,
}

impl FileEntry {
    pub fn new(path: OsString, object: ObjectEntry) -> Self {
        Self { path, object }
    }
}

/// Contains the metadata needed to extract files from a pack file.
#[derive(Serialize, Deserialize)]
pub struct PackIndex {
    path_pool: EntryPool<OsString>,
    objects: Vec<ObjectEntry>,
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
            objects: Vec::new(),
            snapshots: Vec::new(),
            current: HashSet::new(),
        }
    }

    pub fn objects(&self) -> &[ObjectEntry] {
        &self.objects
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

    /// Applies a new ordering to the objects entries ([`PackIndex::objects`]).
    /// This operation also changes the [`FileHandle`] entries, updating the
    /// indices into the [`PackIndex::objects`] array, and the recorded offsets
    /// of the objects.
    ///
    /// # Arguments
    /// * `indices` - The new set of indices for the objects. This slice must
    ///   contain the same number of indices as the number of object entries in
    ///   the [`PackIndex`]. Any index can only appear once, otherwise, the
    ///   behavior of this function is undetermined.
    pub fn permute_objects(&mut self, indices: &[usize]) -> Result<(), PackError> {
        assert_eq!(self.objects().len(), indices.len());
        // Reorder the objects
        apply_permutation(&mut self.objects, indices);
        // Update object offsets
        let mut offset = 0;
        for object in &mut self.objects {
            object.offset = offset;
            offset += object.size;
        }

        // To preserve the identity of the objects in the snapshots, we need a
        // set of indices that permutes the object list back to the original
        // order. We can then use that to update [`FileHandle::object`] to the
        // correct new value.
        let mut reverse_indices: Vec<_> = (0..indices.len()).collect();
        reverse_indices.sort_unstable_by_key(|&i| indices[i]);
        let update_file_index = |file: &mut FileHandle| {
            // The object ID is the only part that has changed (due to
            // reordering the objects when sorting).
            file.object = ObjectHandle(reverse_indices[file.object.0 as usize] as u32);
        };

        // Process the snapshots, updating the object indices.
        map_entries_in_place(&mut self.snapshots, update_file_index);

        Ok(())
    }

    /// Creates a list of FileEntry for the given files.
    pub fn entries<I>(&self, files: I) -> Result<Vec<FileEntry>, PackError>
    where
        I: Iterator<Item = FileHandle>,
    {
        // Map the path handle and object ID to the corresponding path and
        // object entry.
        files
            .map(|x| -> Result<_, PackError> {
                let path = self
                    .path_pool
                    .lookup(x.path)
                    .ok_or(PackError::PathNotFound(x.path))?;
                let object = self
                    .objects
                    .get(x.object.0 as usize)
                    .ok_or(PackError::ObjectNotFound)?;
                Ok(FileEntry::new(path.clone(), object.clone()))
            })
            .collect::<Result<Vec<_>, PackError>>()
    }

    /// Returns the full list of [`FileEntry`].
    pub fn entries_from_snapshot(&self, tag: &str) -> Result<Vec<FileEntry>, PackError> {
        let snapshot = self
            .find_snapshot(tag)
            .ok_or_else(|| PackError::SnapshotNotFound(tag.to_owned()))?;

        assert_eq!(0, snapshot.list.removed.len());
        self.entries(snapshot.list.added.into_iter())
    }

    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    pub fn push_snapshot(&mut self, tag: &str, input: &[FileEntry]) -> Result<(), PackError> {
        if self.snapshots().iter().any(|s| s.tag == tag) {
            return Err(PackError::SnapshotAlreadyExists(
                "<unknown>".into(),
                tag.to_owned(),
            ));
        }

        let existing_objects: HashSet<&_> = self.objects.iter().map(|o| &o.checksum).collect();
        // Compute and extend with the set of objects which are not available in
        // the loose storage
        let new_objects: Vec<_> = input
            .iter()
            .filter(|x| !existing_objects.contains(&x.object.checksum))
            .map(|x| x.object.clone())
            .collect();

        // Add the new set of objects.
        self.objects.extend_from_slice(&new_objects);

        let object_indices: HashMap<_, _> = self
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.checksum, i))
            .collect();

        let files = input
            .iter()
            .map(|e| {
                FileHandle::new(
                    self.path_pool.get_or_insert(&e.path),
                    ObjectHandle(object_indices[&e.object.checksum] as u32),
                )
            })
            .collect();

        // Compute delta against last pushed snapshot (temporary implementation).
        let changeset = Snapshot::get_changes(&mut self.current, &files);
        self.current = files;
        self.snapshots.push(Snapshot::new(tag, changeset));

        Ok(())
    }
}

/// Custom debug representation
impl fmt::Debug for PackIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct DebugPackIndex {
            objects: usize,
            snapshots: usize,
        }

        let index = DebugPackIndex {
            objects: self.objects().len(),
            snapshots: self.snapshots().len(),
        };
        fmt::Debug::fmt(&index, f)
    }
}

/// Update the entries in the snapshots by applying the provided function.
fn map_entries_in_place<F>(snapshots: &mut [Snapshot], f: F)
where
    F: Fn(&mut FileHandle),
{
    for snapshot in snapshots {
        for file in &mut snapshot.list.added {
            f(file);
        }
        for file in &mut snapshot.list.removed {
            f(file);
        }
    }
}

/// Transforms the input sequence using the specified indices.
///
/// # Arguments
/// * `xs` - The input sequence of elements.
/// * `indices` - A set of indices, with `indices[i]=j` interpreted as `xs[i]` goes to `xs[j]`.
fn apply_permutation<T>(xs: &mut [T], indices: &[usize]) {
    assert_eq!(xs.len(), indices.len());
    let mut indices = indices.to_vec();
    for i in 0..indices.len() {
        let mut current = i;
        while i != indices[current] {
            let next = indices[current];
            xs.swap(current, next);
            indices[current] = current;
            current = next;
        }
        indices[current] = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_permutation_reverse_works() {
        let xs = &mut [10, 20, 30, 40];
        let indices = &[3, 2, 1, 0];
        apply_permutation(xs, indices);
        assert_eq!(&[40, 30, 20, 10], xs);
    }

    #[test]
    fn apply_permutation_identity_works() {
        let xs = &mut [10, 20, 30, 40];
        let indices = &[0, 1, 2, 3];
        apply_permutation(xs, indices);
        assert_eq!(&[10, 20, 30, 40], xs);
    }

    #[test]
    fn apply_permutation_shift_left_works() {
        let xs = &mut [10, 20, 30, 40];
        let indices = &[1, 2, 3, 0];
        apply_permutation(xs, indices);
        assert_eq!(&[20, 30, 40, 10], xs);
    }

    #[test]
    fn apply_permutation_shift_right_works() {
        let xs = &mut [10, 20, 30, 40];
        let indices = &[3, 0, 1, 2];
        apply_permutation(xs, indices);
        assert_eq!(&[40, 10, 20, 30], xs);
    }

    #[test]
    fn apply_permutation_random_works() {
        let xs = &mut [10, 20, 30, 40];
        let indices = &[2, 1, 0, 3];
        apply_permutation(xs, indices);
        assert_eq!(&[30, 20, 10, 40], xs);
    }
}
