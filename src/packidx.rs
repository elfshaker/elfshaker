//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::entrypool::{EntryPool, Handle};
use crate::repo::{
    fs::{create_file, open_file},
    partition_by_u64,
};

use crypto::digest::Digest;
use crypto::sha1::Sha1;
use serde::de::{SeqAccess, Visitor};
use serde::{ser::SerializeTuple, Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::Hash;
use std::io::{BufReader, BufWriter, Read, Write};
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

    fn apply_changes<T>(set: &mut HashSet<T>, changes: &ChangeSet<T>)
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

pub enum VerPackIndex {
    V1(PackIndexV1),
}

impl PackIndex for VerPackIndex {
    fn new() -> Self {
        // Use the highest supported pack index version.
        VerPackIndex::V1(PackIndexV1::new())
    }

    fn object_size_total(&self) -> u64 {
        match self {
            VerPackIndex::V1(p) => p.object_size_total(),
        }
    }

    fn object_checksums(&self) -> std::slice::Iter<ObjectChecksum> {
        match self {
            VerPackIndex::V1(p) => p.object_checksums(),
        }
    }

    fn object_metadata(&self, checksum: &ObjectChecksum) -> &ObjectMetadata {
        match self {
            VerPackIndex::V1(p) => p.object_metadata(checksum),
        }
    }

    fn objects_partitioned_by_size<'l>(
        &self,
        partitions: u32,
        handles: &'l [Handle],
    ) -> Vec<&'l [Handle]> {
        match self {
            VerPackIndex::V1(p) => p.objects_partitioned_by_size(partitions, handles),
        }
    }

    fn compute_object_offsets_and_ordering(self) -> (Self, Vec<Handle>)
    where
        Self: Sized,
    {
        match self {
            VerPackIndex::V1(p) => {
                let (p1, vec) = p.compute_object_offsets_and_ordering();
                (VerPackIndex::V1(p1), vec)
            }
        }
    }

    fn handle_to_checksum(&self, h: Handle) -> &ObjectChecksum {
        match self {
            VerPackIndex::V1(p) => p.handle_to_checksum(h),
        }
    }

    fn handle_to_entry(&self, handle: &FileHandle) -> Result<FileEntry, PackError> {
        match self {
            VerPackIndex::V1(p) => p.handle_to_entry(handle),
        }
    }

    fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError> {
        match self {
            VerPackIndex::V1(p) => p.entry_to_handle(entry),
        }
    }

    fn snapshot_tags(&self) -> &[String] {
        match self {
            VerPackIndex::V1(p) => p.snapshot_tags(),
        }
    }

    fn has_snapshot(&self, needle: &str) -> bool {
        match self {
            VerPackIndex::V1(p) => p.has_snapshot(needle),
        }
    }

    fn resolve_snapshot(&self, needle: &str) -> Option<Vec<FileHandle>> {
        match self {
            VerPackIndex::V1(p) => p.resolve_snapshot(needle),
        }
    }

    fn entries_from_handles<'l>(
        &self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntry>, PackError> {
        match self {
            VerPackIndex::V1(p) => p.entries_from_handles(handles),
        }
    }

    fn push_snapshot(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError> {
        match self {
            VerPackIndex::V1(p) => p.push_snapshot(tag, input),
        }
    }

    fn for_each_snapshot<'l, F, S>(&'l self, f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, &HashSet<FileEntry>) -> ControlFlow<S>,
    {
        match self {
            VerPackIndex::V1(p) => p.for_each_snapshot(f),
        }
    }

    fn for_each_snapshot_file_count<'l, F, S>(&'l self, f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, u64) -> ControlFlow<S>,
    {
        match self {
            VerPackIndex::V1(p) => p.for_each_snapshot_file_count(f),
        }
    }

    fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum> {
        match self {
            VerPackIndex::V1(p) => p.compute_snapshot_checksum(snapshot),
        }
    }

    fn parse<R: Read>(rd: R) -> Result<Self, PackError>
    where
        Self: Sized,
    {
        PackIndexV1::parse(rd).map(VerPackIndex::V1)
    }

    fn load<P: AsRef<Path>>(p: P) -> Result<Self, PackError>
    where
        Self: Sized,
    {
        PackIndexV1::load(p).map(VerPackIndex::V1)
    }

    fn load_only_snapshots<P: AsRef<Path>>(p: P) -> Result<Vec<String>, PackError> {
        PackIndexV1::load_only_snapshots(p)
    }

    fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), PackError> {
        match self {
            VerPackIndex::V1(p) => p.save(path),
        }
    }
}

/// Contains the metadata needed to extract files from a pack file.
pub struct PackIndexV1 {
    snapshot_tags: Vec<String>,
    snapshot_deltas: Vec<ChangeSet<FileHandle>>,

    path_pool: EntryPool<OsString>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: BTreeMap<Handle, ObjectMetadata>,

    // When snapshots are pushed, maintain the current state of the filesystem.
    // Not stored on disk.
    // TODO: Move this onto a separate builder class.
    current: HashSet<FileHandle>,
}

impl Default for PackIndexV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl PackIndex for PackIndexV1 {
    fn new() -> Self {
        Self {
            snapshot_tags: Vec::new(),
            snapshot_deltas: Vec::new(),

            path_pool: EntryPool::new(),
            object_pool: EntryPool::new(),
            object_metadata: BTreeMap::new(),

            current: HashSet::new(),
        }
    }

    fn object_size_total(&self) -> u64 {
        self.object_metadata.values().map(|x| x.size).sum()
    }
    fn object_checksums(&self) -> std::slice::Iter<ObjectChecksum> {
        self.object_pool.iter()
    }
    fn object_metadata(&self, checksum: &ObjectChecksum) -> &ObjectMetadata {
        let handle = self.object_pool.get(checksum).unwrap();
        self.object_metadata.get(&handle).unwrap()
    }
    fn objects_partitioned_by_size<'l>(
        &self,
        partitions: u32,
        handles: &'l [Handle],
    ) -> Vec<&'l [Handle]> {
        partition_by_u64(handles, partitions, |handle| {
            self.object_metadata.get(handle).unwrap().size
        })
    }
    fn compute_object_offsets_and_ordering(mut self) -> (Self, Vec<Handle>) {
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

    fn handle_to_checksum(&self, h: Handle) -> &ObjectChecksum {
        self.object_pool.lookup(h).unwrap()
    }
    fn handle_to_entry(&self, handle: &FileHandle) -> Result<FileEntry, PackError> {
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
    fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError> {
        let object_handle = self.object_pool.get_or_insert(&entry.checksum);
        self.object_metadata.insert(object_handle, entry.metadata);
        Ok(FileHandle {
            path: self.path_pool.get_or_insert(&entry.path),
            object: object_handle,
        })
    }

    fn snapshot_tags(&self) -> &[String] {
        &self.snapshot_tags
    }
    fn has_snapshot(&self, needle: &str) -> bool {
        self.snapshot_tags.iter().any(|s| s.eq(needle))
    }
    fn resolve_snapshot(&self, needle: &str) -> Option<Vec<FileHandle>> {
        let mut current = HashSet::new();
        for (tag, delta) in self.snapshot_tags.iter().zip(self.snapshot_deltas.iter()) {
            Snapshot::apply_changes(&mut current, delta);
            if tag == needle {
                return Some(current.into_iter().collect());
            }
        }
        None
    }
    fn entries_from_handles<'l>(
        &self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntry>, PackError> {
        handles
            .map(|h| self.handle_to_entry(h))
            .collect::<Result<Vec<_>, _>>()
    }
    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    fn push_snapshot(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError> {
        if self.snapshot_tags.contains(&tag) {
            return Err(PackError::SnapshotAlreadyExists("<unknown>".into(), tag));
        }

        let files = input
            .into_iter()
            .map(|e| self.entry_to_handle(&e))
            .collect::<Result<_, _>>()?;

        // Compute delta against last pushed snapshot (temporary implementation).
        let delta = Snapshot::get_changes(&mut self.current, &files);
        self.current = files;
        self.snapshot_tags.push(tag);
        self.snapshot_deltas.push(delta);
        Ok(())
    }
    // Call the closure F with materialized file entries for each snapshot.
    fn for_each_snapshot<'l, F, S>(&'l self, mut f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, &HashSet<FileEntry>) -> ControlFlow<S>,
    {
        let mut complete = HashSet::new();
        let snapshot_deltas = self.snapshot_tags.iter().zip(self.snapshot_deltas.iter());
        for (snapshot, deltas) in snapshot_deltas {
            let deltas = deltas.map(|handles| self.entries_from_handles(handles.iter()))?;
            Snapshot::apply_changes(&mut complete, &deltas);
            if let ControlFlow::Break(output) = f(snapshot, &complete) {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }
    // Call the closure F with the number of file entries for each snapshot.
    fn for_each_snapshot_file_count<'l, F, S>(
        &'l self,
        mut f: F,
    ) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, u64) -> ControlFlow<S>,
    {
        let mut file_count = 0i64;
        let snapshot_deltas = self.snapshot_tags.iter().zip(self.snapshot_deltas.iter());
        for (snapshot, deltas) in snapshot_deltas {
            file_count += deltas.added.len() as i64;
            file_count -= deltas.removed.len() as i64;
            if let ControlFlow::Break(output) = f(snapshot, file_count as u64) {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }
    /// Computes the checksum of the contents of the snapshot.
    fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum> {
        let handles = self.resolve_snapshot(snapshot)?;
        // Map FileHandle to FileEntry, which contains path and checksum
        let mut entries = self.entries_from_handles(handles.iter()).ok()?;
        entries.sort_by(|a, b| a.checksum.cmp(&b.checksum).then(a.path.cmp(&b.path)));
        let mut hasher = entries.into_iter().fold(Sha1::new(), |mut hasher, entry| {
            hasher.input(&os_str_as_bytes(&entry.path));
            hasher.input(&entry.checksum);
            hasher
        });
        let mut checksum: ObjectChecksum = [0; 20];
        hasher.result(&mut checksum);
        Some(checksum)
    }

    fn load<P: AsRef<Path>>(p: P) -> Result<Self, PackError> {
        let rd = open_file(p.as_ref())?;
        Self::parse(rd)
    }

    fn parse<R: Read>(rd: R) -> Result<Self, PackError> {
        let mut rd = BufReader::new(rd);
        Self::read_magic(&mut rd)?;

        Ok(rmp_serde::decode::from_read(rd)?)
    }

    fn load_only_snapshots<P: AsRef<Path>>(p: P) -> Result<Vec<String>, PackError> {
        let rd = open_file(p.as_ref())?;
        let mut rd = BufReader::new(rd);
        Self::read_magic(&mut rd)?;
        let mut d = rmp_serde::Deserializer::new(rd);
        Ok(Self::deserialize_only_snapshots(&mut d)?.snapshot_tags)
    }

    fn save<P: AsRef<Path>>(&self, p: P) -> Result<(), PackError> {
        // TODO: Use AtomicCreateFile.
        let wr = create_file(p.as_ref())?;
        let mut wr = BufWriter::new(wr);
        Self::write_magic(&mut wr)?;

        rmp_serde::encode::write(&mut wr, self)?;
        Ok(())
    }
}

#[cfg(unix)]
fn os_str_as_bytes(os_str: &OsStr) -> Cow<[u8]> {
    Cow::Borrowed(std::os::unix::ffi::OsStrExt::as_bytes(os_str))
}

#[cfg(not(unix))]
fn os_str_as_bytes(os_str: &OsStr) -> Cow<[u8]> {
    Cow::Owned(os_str.to_string_lossy().as_bytes())
}

impl PackIndexV1 {
    fn read_magic(rd: &mut impl Read) -> Result<(), PackError> {
        let mut magic = [0; 4];
        rd.read_exact(&mut magic)?;
        if magic.ne(&*b"ELFS") {
            return Err(PackError::BadMagic);
        }
        let mut version = [0; 4];
        rd.read_exact(&mut version)?;
        if version.gt(&[0, 0, 0, 1]) {
            return Err(PackError::BadPackVersion(version));
        }
        Ok(())
    }

    fn write_magic(wr: &mut impl Write) -> std::io::Result<()> {
        wr.write_all(&*b"ELFS")?;
        wr.write_all(&[0, 0, 0, 1])?;
        Ok(())
    }

    fn deserialize_only_snapshots<'de, D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(VisitPackIndex {
            load_mode: LoadMode::OnlySnapshots,
        })
    }
}

struct VisitPackIndex {
    load_mode: LoadMode,
}

#[derive(PartialEq)]
enum LoadMode {
    Full,
    OnlySnapshots,
}

fn next_expecting<'de, T, V, E>(seq: &mut V) -> Result<T, E>
where
    T: Deserialize<'de>,
    V: SeqAccess<'de>,
    E: serde::de::Error + std::convert::From<<V as serde::de::SeqAccess<'de>>::Error>,
{
    seq.next_element::<T>()?
        .ok_or_else(|| serde::de::Error::custom(format!("expected {}", std::any::type_name::<T>())))
}

impl<'de> Visitor<'de> for VisitPackIndex {
    type Value = PackIndexV1;

    fn visit_seq<V>(self, mut seq: V) -> Result<PackIndexV1, V::Error>
    where
        V: SeqAccess<'de>,
    {
        let mut result = PackIndexV1::new();
        result.snapshot_tags = next_expecting(&mut seq)?;
        if self.load_mode == LoadMode::OnlySnapshots {
            return Ok(result);
        }
        result.snapshot_deltas = next_expecting(&mut seq)?;
        result.path_pool = next_expecting(&mut seq)?;
        result.object_pool = next_expecting(&mut seq)?;
        let md: Vec<ObjectMetadata> = next_expecting(&mut seq)?;
        result.object_metadata = md
            .into_iter()
            .enumerate()
            .map(|(i, md)| ((i as Handle), md))
            .collect();

        Ok(result)
    }
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("PathIndex")
    }
}

impl<'de> Deserialize<'de> for PackIndexV1 {
    fn deserialize<D>(deserializer: D) -> Result<PackIndexV1, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(VisitPackIndex {
            load_mode: LoadMode::Full,
        })
    }
}

impl Serialize for PackIndexV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_tuple(5)?;
        s.serialize_element(&self.snapshot_tags)?;
        s.serialize_element(&self.snapshot_deltas)?;
        s.serialize_element(&self.path_pool)?;
        s.serialize_element(&self.object_pool)?;
        // Ordering comes from BTreeMap keys, so is for free.
        s.serialize_element(
            &self
                .object_metadata
                .values()
                .cloned()
                .collect::<Vec<ObjectMetadata>>(),
        )?;
        s.end()
    }
}
