//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::pathidx::{PathHandle, PathPool, PathTree};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;

/// Error type used in the packidx module.
#[derive(Debug, Clone)]
pub enum PackError {
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
/// from the [`PathTree`], and an [`ObjectHandle`], which can be used to get the
/// corresponding [`ObjectEntry`].
///
/// [`FileEntry`] and [`FileHandle`] can both be used to find the path and
/// object of a file, but [`FileHandle`] has an additional level of indirection,
/// because it stores handles, not the actual values themselves.
///
/// [`FileHandle`] is the representation that gets written to disk.
#[derive(Hash, PartialEq, Clone, Serialize, Deserialize, Debug)]
pub struct FileHandle {
    pub path: PathHandle,
    // Index into [`PackIndex::objects`].
    pub object: ObjectHandle,
}

impl Eq for FileHandle {}

impl FileHandle {
    pub fn new(path: PathHandle, object: ObjectHandle) -> Self {
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
pub enum FileHandleList {
    Complete(Vec<FileHandle>),
    Delta(ChangeSet<FileHandle>),
}

/// A snapshot is identified by a string tag and specifies a list of files.
///
/// The list of files can be a complete list or a list diff.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Snapshot {
    tag: String,
    list: FileHandleList,
}

impl Snapshot {
    pub fn new(tag: &str, list: FileHandleList) -> Self {
        Self {
            tag: tag.to_owned(),
            list,
        }
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn list(&self) -> &FileHandleList {
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
        let mut complete: HashSet<FileHandle> = results.last().unwrap().create_set()?;
        for snapshot in snapshots {
            match &snapshot.list {
                FileHandleList::Complete(c) => {
                    let set: HashSet<_, _> = c.iter().cloned().collect();
                    let changes = Self::get_changes(&mut complete, &set);
                    results.push(Snapshot {
                        tag: snapshot.tag.clone(),
                        list: changes,
                    });
                }
                FileHandleList::Delta(d) => {
                    // Snapshot is already in delta format.
                    Self::apply_changes(&mut complete, d);
                    results.push(snapshot.clone());
                }
            }
        }

        Ok(results)
    }

    fn apply_changes(set: &mut HashSet<FileHandle>, changes: &ChangeSet<FileHandle>) {
        for removed in &changes.removed {
            assert!(set.remove(removed));
        }
        for added in &changes.added {
            assert!(set.insert(added.clone()));
        }
    }
    fn get_changes(set: &mut HashSet<FileHandle>, next: &HashSet<FileHandle>) -> FileHandleList {
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
            // but not in the one preceding it.
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

        FileHandleList::Delta(ChangeSet { added, removed })
    }

    fn create_set(&self) -> Result<HashSet<FileHandle>, PackError> {
        let mut set = HashSet::new();
        match &self.list {
            FileHandleList::Complete(c) => {
                for file in c {
                    assert!(set.insert(file.clone()));
                }
            }
            FileHandleList::Delta(_) => return Err(PackError::CompleteListNeeded),
        }
        Ok(set)
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
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct PackIndex {
    tree: PathTree,
    objects: Vec<ObjectEntry>,
    snapshots: Vec<Snapshot>,
}

impl PackIndex {
    pub fn new(tree: PathTree, objects: Vec<ObjectEntry>, snapshots: Vec<Snapshot>) -> Self {
        Self {
            tree,
            objects,
            snapshots,
        }
    }

    pub fn tree(&self) -> &PathTree {
        &self.tree
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
        let snapshot_idx = self.snapshots.iter().position(|s| s.tag == tag)?;
        let base_idx = match self.snapshots[0..=snapshot_idx]
            .iter()
            .rposition(|s| matches!(s.list, FileHandleList::Complete(_)))
        {
            Some(x) => x,
            None => panic!("{:?}", PackError::CompleteListNeeded),
        };

        let base_snapshot = &self.snapshots[base_idx];
        if base_snapshot.tag == tag {
            return Some(base_snapshot.clone());
        }

        let mut set = Snapshot::create_set(base_snapshot).unwrap();

        for snapshot in self.snapshots[base_idx + 1..=snapshot_idx].iter() {
            match snapshot.list {
                FileHandleList::Delta(ref d) => {
                    Snapshot::apply_changes(&mut set, d);
                }
                FileHandleList::Complete(_) => unreachable!(),
            }
        }

        Some(Snapshot {
            tag: tag.into(),
            list: FileHandleList::Complete(set.into_iter().collect()),
        })
    }

    /// Converts all snapshots' file lists (but the first) to the delta/changeset format.
    pub fn use_file_list_deltas(&mut self) -> Result<(), PackError> {
        self.snapshots = Snapshot::compute_deltas(self.snapshots().iter())?;
        Ok(())
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
        apply_permutation(&mut self.objects, &indices);
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
        let path_lookup = self.tree.create_lookup();
        // Map the path handle and object ID to the corresponding path and
        // object entry.
        files
            .map(|x| -> Result<_, PackError> {
                let path = path_lookup.get(&x.path).ok_or(PackError::PathNotFound)?;
                let object = self
                    .objects
                    .get(x.object.0 as usize)
                    .ok_or(PackError::ObjectNotFound)?;
                Ok(FileEntry::new(path.into(), object.clone()))
            })
            .collect::<Result<Vec<_>, PackError>>()
    }

    /// Returns the full list of [`FileEntry`].
    pub fn entries_from_snapshot(&self, tag: &str) -> Result<Vec<FileEntry>, PackError> {
        let snapshot = self.find_snapshot(tag).ok_or(PackError::SnapshotNotFound)?;

        // Convert the FileHandleList into a list of entries to write to disk.
        let entries = match snapshot.list {
            FileHandleList::Complete(ref list) => self.entries(list.iter().cloned())?,
            _ => unreachable!(),
        };
        Ok(entries)
    }

    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    pub fn push_snapshot(&mut self, tag: &str, input: &[FileEntry]) -> Result<(), PackError> {
        if self.snapshots().iter().any(|s| s.tag == tag) {
            return Err(PackError::SnapshotAlreadyExists);
        }

        let (new_tree, old2new) = extend_tree(self.tree.clone(), input.iter().map(|e| &e.path))?;
        let rev_lookup: HashMap<_, _> = new_tree
            .create_lookup()
            .into_iter()
            .map(|(a, b)| (b, a))
            .collect();

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

        let files: Vec<_> = input
            .iter()
            .map(|e| {
                FileHandle::new(
                    rev_lookup[&e.path],
                    ObjectHandle(object_indices[&e.object.checksum] as u32),
                )
            })
            .collect();

        // Update the path indices of the entries in the old snapshots
        map_entries_in_place(&mut self.snapshots, |f| f.path = old2new[&f.path]);
        // Finally, push the new snapshot
        self.snapshots
            .push(Snapshot::new(tag, FileHandleList::Complete(files)));
        // And replace the path tree
        self.tree = new_tree;

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

/// Update the entries in the snapshots by applying the provided function.
fn map_entries_in_place<F>(snapshots: &mut [Snapshot], f: F)
where
    F: Fn(&mut FileHandle),
{
    for snapshot in snapshots {
        match snapshot.list {
            FileHandleList::Complete(ref mut files) => {
                for file in files {
                    f(file);
                }
            }
            FileHandleList::Delta(ref mut changeset) => {
                for file in &mut changeset.added {
                    f(file);
                }
                for file in &mut changeset.removed {
                    f(file);
                }
            }
        };
    }
}

/// Adds the given paths to the [`PathTree`].
/// Also returns a [`HashMap<PathHandle, PathHandle>`] that can be used to determine how the existing indices changed.
fn extend_tree<I, P>(
    mut tree: PathTree,
    paths: I,
) -> Result<(PathTree, HashMap<PathHandle, PathHandle>), PackError>
where
    I: Iterator<Item = P>,
    P: AsRef<OsStr>,
{
    let original_lookup = tree.create_lookup();
    for path in paths {
        tree.create_file(path.as_ref());
    }
    // We need to run new_tree.commit_changes() after mutation, before any read
    // operations.
    tree.commit_changes();

    // Maps the updated paths to the new handles.
    let new_rev_lookup: HashMap<_, PathHandle> = tree
        .create_lookup()
        .into_iter()
        .map(|(i, p)| (p, i))
        .collect();

    // Now, we need to figure out how to map the old paths to new_paths.
    let old2new: HashMap<PathHandle, PathHandle> = original_lookup
        .into_iter()
        .map(|(old_idx, path)| (old_idx, new_rev_lookup[&path]))
        .collect();

    Ok((tree, old2new))
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
