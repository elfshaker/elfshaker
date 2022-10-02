//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::entrypool::Handle;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::hash::Hash;
use std::io::Read;
use std::iter::FromIterator;
use std::ops::ControlFlow;
use std::path::Path;

/// Error type used in the packidx module.
#[derive(Debug)]
pub enum PackError {
    CompleteListNeeded,
    PathNotFound(Handle),
    ObjectNotFound,
    SnapshotNotFound(String),
    /// A snapshot with that tag is already present in the pack
    SnapshotAlreadyExists(String, String),
    ChecksumMismatch(ObjectChecksum, ObjectChecksum),
    IOError(std::io::Error),
    DeserializeError(rmp_serde::decode::Error),
    SerializeError(rmp_serde::encode::Error),
    BadMagic,
    BadPackVersion([u8; 4]),
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
            PackError::IOError(e) => write!(f, "Reading pack index: {}", e),
            PackError::DeserializeError(e) => {
                write!(f, "Deserialization failed, corrupt pack index: {}", e)
            }
            PackError::SerializeError(e) => {
                write!(f, "Serialization failed: {}", e)
            }
            PackError::BadMagic => write!(f, "Bad pack magic, expected ELFS!"),
            PackError::BadPackVersion(v) => write!(
                f,
                "Pack version is too recent ({:?}), please upgrade elfshaker!",
                v
            ),
        }
    }
}

impl From<std::io::Error> for PackError {
    fn from(err: std::io::Error) -> Self {
        Self::IOError(err)
    }
}

impl From<rmp_serde::decode::Error> for PackError {
    fn from(err: rmp_serde::decode::Error) -> Self {
        Self::DeserializeError(err)
    }
}

impl From<rmp_serde::encode::Error> for PackError {
    fn from(err: rmp_serde::encode::Error) -> Self {
        Self::SerializeError(err)
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
    pub path: Handle,   // offset into path_pool
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
    pub fn map<F, U, E>(&self, f: F) -> Result<ChangeSet<U>, E>
    where
        F: Fn(&Vec<T>) -> Result<Vec<U>, E>,
    {
        Ok(ChangeSet {
            added: f(&self.added)?,
            removed: f(&self.removed)?,
        })
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

    pub(crate) fn apply_changes<T>(set: &mut HashSet<T>, changes: &ChangeSet<T>)
    where
        T: Eq + Hash + Clone,
    {
        for removed in &changes.removed {
            assert!(set.remove(removed));
        }
        for added in &changes.added {
            assert!(set.insert(added.clone()));
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

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ObjectMetadata {
    pub offset: u64,
    pub size: u64,
}

pub trait PackIndex {
    fn new() -> Self;
    /// The total size of objects in the pack
    fn object_size_total(&self) -> u64;
    /// An iterator over all object checksums in the pack
    fn object_checksums(&self) -> std::slice::Iter<ObjectChecksum>;
    /// The metadata stored for the object
    fn object_metadata(&self, checksum: &ObjectChecksum) -> &ObjectMetadata;
    /// Partitions the objects into the specified number of partitions while
    /// maintaining the relative order of the objects and minimising the
    /// total object size in partitions.
    fn objects_partitioned_by_size<'l>(
        &self,
        partitions: u32,
        handles: &'l [Handle],
    ) -> Vec<&'l [Handle]>;
    /// Reorders the objects for maximum compressability and computes the
    /// object offsets. Note that this operation invalidates all previously
    /// held [`Handle`] and [`FileHandle`].
    fn compute_object_offsets_and_ordering(self) -> (Self, Vec<Handle>)
    where
        Self: Sized;
    /// Returns the checksum referenced by the handle.
    fn handle_to_checksum(&self, h: Handle) -> &ObjectChecksum;
    /// Expands the file handle into a file entry.`
    fn handle_to_entry(&self, handle: &FileHandle) -> Result<FileEntry, PackError>;
    /// Stores the file entry into the pack index and returns a handle to it.
    fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError>;
    /// Returns all snapshots tags stored in the pack.
    fn snapshot_tags(&self) -> &[String];
    /// Check for the presence of a snapshot.
    fn has_snapshot(&self, needle: &str) -> bool;
    /// Map the snapshot tag to the list of files store in the snapshot.
    fn resolve_snapshot(&self, needle: &str) -> Option<Vec<FileHandle>>;
    /// Dereference all file handles. This is liekly to be more efficient than
    /// calling entry_from_handle repeatedly.
    fn entries_from_handles<'l>(
        &self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntry>, PackError>;
    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    fn push_snapshot(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError>;
    /// Call the closure F with materialized file entries for each snapshot.
    fn for_each_snapshot<'l, F, S>(&'l self, f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, &HashSet<FileEntry>) -> ControlFlow<S>;
    /// Call the closure F with the file count for each snapshot.
    fn for_each_snapshot_file_count<'l, F, S>(
        &'l self,
        f: F,
    ) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, u64) -> ControlFlow<S>;
    /// Computes the checksum of the contents of the snapshot.
    fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum>;
    /// Parse from a [`Read`].
    fn parse<R: Read>(rd: R) -> Result<Self, PackError>
    where
        Self: Sized;
    /// Parse from a file.
    fn load<P: AsRef<Path>>(p: P) -> Result<Self, PackError>
    where
        Self: Sized;
    /// Parse the list of snapshot from a file.
    fn load_only_snapshots<P: AsRef<Path>>(p: P) -> Result<Vec<String>, PackError>;
    /// Serialise and write to a file.
    fn save<P: AsRef<Path>>(&self, p: P) -> Result<(), PackError>;
}
