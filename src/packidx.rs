//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains types and function for parsing `.pack.idx` files created by
//! elfshaker.
use crate::entrypool::{EntryPool, Handle};
use crate::legacy_path::{LegacyPathError, LegacyWirePath};
pub use crate::packpath::{PackPath, PackPathError};
use crate::repo::{
    fs::{create_file, open_file},
    partition_by_u64,
};

use serde::{ser::SerializeTuple, Deserialize, Serialize, Serializer};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;
use std::io::{BufReader, BufWriter, Read, Write};
use std::iter::FromIterator;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

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
    UnknownRequiredFeatures(u64),
    UnsupportedPathEncoding(String),
    InvalidLogicalPath(PackPathError),
    PathNotRepresentable(PackPath, PackPathError),
    DuplicateLogicalPath(PackPath),
    PathPrefixConflict(PackPath, PackPath),
    TargetPathCollision(PackPath, PackPath),
    SymlinkedDestination(PathBuf),
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
            PackError::PathNotFound(p) => write!(f, "Corrupt pack, PathHandle {p:?} not found"),
            PackError::SnapshotNotFound(s) => write!(f, "The snapshot '{s}' was not found!"),
            PackError::SnapshotAlreadyExists(p, s) => write!(
                f,
                "A snapshot with the tag '{s}' is already present in the pack '{p}'!",
            ),
            PackError::ChecksumMismatch(exp, got) => write!(
                f,
                "The object checksum did not match! exp {} got {}",
                hex::encode(exp),
                hex::encode(got)
            ),
            PackError::IOError(e) => write!(f, "Reading pack index: {e}"),
            PackError::DeserializeError(e) => {
                write!(f, "Deserialization failed, corrupt pack index: {e}")
            }
            PackError::SerializeError(e) => {
                write!(f, "Serialization failed: {e}")
            }
            PackError::BadMagic => write!(f, "Bad pack magic, expected ELFS!"),
            PackError::BadPackVersion(v) => write!(
                f,
                "Unsupported pack version {}; this elfshaker supports versions 1 through {}",
                u32::from_be_bytes(*v),
                PackIndex::MAX_VERSION
            ),
            PackError::UnknownRequiredFeatures(bits) => {
                write!(f, "Pack requires unsupported features: 0x{bits:x}")
            }
            PackError::UnsupportedPathEncoding(error) => {
                write!(f, "Unsupported legacy path encoding: {error}")
            }
            PackError::InvalidLogicalPath(error) => write!(f, "Invalid pack path: {error}"),
            PackError::PathNotRepresentable(path, error) => {
                write!(f, "Pack path '{path}' is not representable: {error}")
            }
            PackError::DuplicateLogicalPath(path) => {
                write!(f, "Pack contains duplicate path '{path}'")
            }
            PackError::PathPrefixConflict(parent, child) => write!(
                f,
                "Pack paths '{parent}' and '{child}' conflict as a file and directory"
            ),
            PackError::TargetPathCollision(first, second) => write!(
                f,
                "Pack paths '{first}' and '{second}' name the same destination path"
            ),
            PackError::SymlinkedDestination(path) => write!(
                f,
                "Destination path '{}' is or traverses a symbolic link",
                path.display()
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
pub const LOOSE_OBJECT_OFFSET: u64 = u64::MAX;

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
    pub object: Handle, // offset into object_pool
    pub metadata: EntryMetadata,
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum EntryMetadata {
    Regular { mode: u32 },
}

impl EntryMetadata {
    pub fn regular_mode(self) -> u32 {
        match self {
            Self::Regular { mode } => mode,
        }
    }
}

impl Default for EntryMetadata {
    fn default() -> Self {
        Self::Regular {
            // The default mode bits are owner can rw, everyone else can read,
            // which is the same behaviour as in version 1 of the pack format.
            mode: 0o100664,
        }
    }
}

impl Eq for FileHandle {}

impl FileHandle {
    pub fn new(path: Handle, object: Handle, metadata: EntryMetadata) -> Self {
        Self {
            path,
            object,
            metadata,
        }
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

    fn apply_changes<T>(set: &mut hashbrown::HashSet<T>, changes: &ChangeSet<T>)
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
    pub path: PackPath,
    pub checksum: ObjectChecksum,
    pub object_metadata: ObjectMetadata,
    pub metadata: EntryMetadata,
}

impl FileEntry {
    pub fn new(
        path: PackPath,
        checksum: ObjectChecksum,
        object_metadata: ObjectMetadata,
        metadata: EntryMetadata,
    ) -> Self {
        Self {
            path,
            checksum,
            object_metadata,
            metadata,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileEntryRef<'a> {
    pub path: &'a PackPath,
    pub checksum: &'a ObjectChecksum,
    pub object_metadata: &'a ObjectMetadata,
    pub metadata: EntryMetadata,
}

impl<'a> FileEntryRef<'a> {
    pub fn new(
        path: &'a PackPath,
        checksum: &'a ObjectChecksum,
        object_metadata: &'a ObjectMetadata,
        metadata: EntryMetadata,
    ) -> Self {
        Self {
            path,
            checksum,
            object_metadata,
            metadata,
        }
    }
}

impl<'a> From<FileEntryRef<'a>> for FileEntry {
    fn from(entry_ref: FileEntryRef<'a>) -> Self {
        FileEntry {
            path: entry_ref.path.clone(),
            checksum: *entry_ref.checksum,
            object_metadata: *entry_ref.object_metadata,
            metadata: entry_ref.metadata,
        }
    }
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ObjectMetadata {
    pub offset: u64,
    pub size: u64,
}

/// Contains the metadata needed to extract files from a pack file.
pub struct PackIndex {
    snapshot_tags: Vec<String>,
    snapshot_deltas: Vec<ChangeSet<FileHandle>>,

    path_pool: EntryPool<PackPath>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: BTreeMap<Handle, ObjectMetadata>,

    // When snapshots are pushed, maintain the current state of the filesystem.
    // Not stored on disk.
    // TODO: Move this onto a separate builder class.
    current: HashSet<FileHandle>,
}

/// Packs which don't
impl Default for PackIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl PackIndex {
    pub fn new() -> Self {
        Self {
            snapshot_tags: Vec::new(),
            snapshot_deltas: Vec::new(),

            path_pool: EntryPool::new(),
            object_pool: EntryPool::new(),
            object_metadata: BTreeMap::new(),

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
            .map(|(handle, om)| (om.size, *handle))
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
            object_metadata: *self.object_metadata.get(&handle.object).unwrap(),
            metadata: handle.metadata,
        })
    }
    pub fn handle_to_entry_ref<'a>(
        &'a self,
        handle: &FileHandle,
    ) -> Result<FileEntryRef<'a>, PackError> {
        Ok(FileEntryRef {
            path: self
                .path_pool
                .lookup(handle.path)
                .ok_or(PackError::PathNotFound(handle.path))?,
            checksum: self
                .object_pool
                .lookup(handle.object)
                .ok_or(PackError::ObjectNotFound)?,
            object_metadata: self.object_metadata.get(&handle.object).unwrap(),
            metadata: handle.metadata,
        })
    }
    pub fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError> {
        let object_handle = self.object_pool.get_or_insert(&entry.checksum);
        self.object_metadata
            .insert(object_handle, entry.object_metadata);
        Ok(FileHandle {
            path: self.path_pool.get_or_insert(&entry.path),
            object: object_handle,
            metadata: entry.metadata,
        })
    }

    pub fn snapshot_tags(&self) -> &[String] {
        &self.snapshot_tags
    }
    pub fn has_snapshot(&self, needle: &str) -> bool {
        self.snapshot_tags.iter().any(|s| s.eq(needle))
    }
    pub fn resolve_snapshot(&self, needle: &str) -> Option<Vec<FileHandle>> {
        let mut current = hashbrown::HashSet::new();
        for (tag, delta) in self.snapshot_tags.iter().zip(self.snapshot_deltas.iter()) {
            Snapshot::apply_changes(&mut current, delta);
            if tag == needle {
                return Some(current.into_iter().collect());
            }
        }
        None
    }
    pub fn entries_from_handles<'l>(
        &self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntry>, PackError> {
        handles
            .map(|h| self.handle_to_entry(h))
            .collect::<Result<Vec<_>, _>>()
    }
    pub fn entry_refs_from_handles<'a, 'l>(
        &'a self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntryRef<'a>>, PackError> {
        handles
            .map(|h| self.handle_to_entry_ref(h))
            .collect::<Result<Vec<_>, _>>()
    }
    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    pub fn push_snapshot<I: Into<FileEntry>>(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = I>,
    ) -> Result<(), PackError> {
        if self.snapshot_tags.contains(&tag) {
            return Err(PackError::SnapshotAlreadyExists("<unknown>".into(), tag));
        }

        let files = input
            .into_iter()
            .map(|e| self.entry_to_handle(&e.into()))
            .collect::<Result<_, _>>()?;

        // Compute delta against last pushed snapshot (temporary implementation).
        let delta = Snapshot::get_changes(&mut self.current, &files);
        self.current = files;
        self.snapshot_tags.push(tag);
        self.snapshot_deltas.push(delta);
        Ok(())
    }
    // Call the closure F with materialized file entries for each snapshot.
    pub fn for_each_snapshot<'l, F, S>(&'l self, mut f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, &hashbrown::HashSet<FileEntryRef<'l>>) -> ControlFlow<S>,
    {
        let mut complete = hashbrown::HashSet::new();
        let snapshot_deltas = self.snapshot_tags.iter().zip(self.snapshot_deltas.iter());
        for (snapshot, deltas) in snapshot_deltas {
            let deltas = deltas.map(|handles| self.entry_refs_from_handles(handles.iter()))?;
            Snapshot::apply_changes(&mut complete, &deltas);
            if let ControlFlow::Break(output) = f(snapshot, &complete) {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }
    // Call the closure F with the number of file entries for each snapshot.
    pub fn for_each_snapshot_file_count<'l, F, S>(
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
    pub fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum> {
        let handles = self.resolve_snapshot(snapshot)?;
        let mut entries = self.entry_refs_from_handles(handles.iter()).ok()?;
        entries.sort_by(|left, right| left.path.cmp(right.path));
        let mut hasher = Sha1::new();
        hasher.update(b"elfshaker-snapshot-identity\0v3\0");
        hasher.update((entries.len() as u64).to_be_bytes());
        let hasher = entries.into_iter().fold(hasher, |mut hasher, entry| {
            hasher.update((entry.path.as_bytes().len() as u64).to_be_bytes());
            hasher.update(entry.path.as_bytes());
            match entry.metadata {
                EntryMetadata::Regular { mode } => {
                    hasher.update([0]);
                    hasher.update(entry.checksum);
                    hasher.update(mode.to_be_bytes());
                }
            }
            hasher
        });
        Some(hasher.finalize().into())
    }
}

impl PackIndex {
    pub fn load<P: AsRef<Path>>(p: P) -> Result<PackIndex, PackError> {
        let rd = open_file(p.as_ref())?;
        Self::parse(rd)
    }

    pub fn parse<R: Read>(rd: R) -> Result<PackIndex, PackError> {
        let mut rd = BufReader::new(rd);
        match Self::read_magic(&mut rd)? {
            1 | 2 => Self::parse_legacy(rd, LoadMode::Full),
            3 => Self::parse_v3(rd, LoadMode::Full),
            version => Err(PackError::BadPackVersion(version.to_be_bytes())),
        }
    }

    pub fn load_only_snapshots<P: AsRef<Path>>(p: P) -> Result<Vec<String>, PackError> {
        let rd = open_file(p.as_ref())?;
        let mut rd = BufReader::new(rd);
        let index = match Self::read_magic(&mut rd)? {
            1 | 2 => Self::parse_legacy(rd, LoadMode::OnlySnapshots),
            3 => Self::parse_v3(rd, LoadMode::OnlySnapshots),
            version => Err(PackError::BadPackVersion(version.to_be_bytes())),
        }?;
        Ok(index.snapshot_tags)
    }

    pub fn save<P: AsRef<Path>>(&self, p: P) -> Result<(), PackError> {
        // TODO: Use AtomicCreateFile.
        let wr = create_file(p.as_ref(), None)?;
        let mut wr = BufWriter::new(wr);
        self.write(&mut wr)?;
        Ok(())
    }

    const MAX_VERSION: u32 = 3;
    const WRITE_VERSION: u32 = 3;
    const SUPPORTED_REQUIRED_FEATURES: u64 = 0;

    fn read_magic(rd: &mut impl Read) -> Result<u32, PackError> {
        let mut magic = [0; 4];
        rd.read_exact(&mut magic)?;
        if magic.ne(b"ELFS") {
            return Err(PackError::BadMagic);
        }
        let mut version = [0; 4];
        rd.read_exact(&mut version)?;
        if !(1..=PackIndex::MAX_VERSION).contains(&u32::from_be_bytes(version)) {
            return Err(PackError::BadPackVersion(version));
        }
        Ok(u32::from_be_bytes(version))
    }

    fn write_magic(wr: &mut impl Write) -> std::io::Result<()> {
        wr.write_all(b"ELFS")?;
        wr.write_all(&u32::to_be_bytes(PackIndex::WRITE_VERSION)[..])?;
        Ok(())
    }

    fn write(&self, wr: &mut impl Write) -> Result<(), PackError> {
        Self::write_magic(wr)?;
        rmp_serde::encode::write(wr, &V3Index::from(self))?;
        Ok(())
    }

    fn parse_legacy<R: Read>(mut rd: R, load_mode: LoadMode) -> Result<Self, PackError> {
        let fields =
            rmp::decode::read_array_len(&mut rd).map_err(rmp_serde::decode::Error::from)?;
        if fields != 5 {
            return Err(rmp_serde::decode::Error::LengthMismatch(5).into());
        }
        let snapshot_tags = rmp_serde::from_read(&mut rd)?;
        if load_mode == LoadMode::OnlySnapshots {
            return Ok(Self {
                snapshot_tags,
                ..Self::new()
            });
        }
        Self::from_legacy(LegacyIndex {
            snapshot_tags,
            snapshot_deltas: rmp_serde::from_read(&mut rd)?,
            path_pool: rmp_serde::from_read(&mut rd)?,
            object_pool: rmp_serde::from_read(&mut rd)?,
            object_metadata: rmp_serde::from_read(&mut rd)?,
        })
    }

    fn parse_v3<R: Read>(mut rd: R, load_mode: LoadMode) -> Result<Self, PackError> {
        // Read the envelope fields directly so listing leaves index tables unread
        // and unsupported features fail before their entry encoding is inspected.
        let fields =
            rmp::decode::read_array_len(&mut rd).map_err(rmp_serde::decode::Error::from)?;
        if fields < 2 {
            return Err(rmp_serde::decode::Error::LengthMismatch(2).into());
        }
        let required_features: u64 = rmp_serde::from_read(&mut rd)?;
        let snapshot_tags = rmp_serde::from_read(&mut rd)?;
        if load_mode == LoadMode::OnlySnapshots {
            return Ok(Self {
                snapshot_tags,
                ..Self::new()
            });
        }
        let unsupported = required_features & !Self::SUPPORTED_REQUIRED_FEATURES;
        if unsupported != 0 {
            return Err(PackError::UnknownRequiredFeatures(unsupported));
        }
        if fields != 7 {
            return Err(rmp_serde::decode::Error::LengthMismatch(7).into());
        }
        let wire = V3WireIndex {
            snapshot_tags,
            snapshot_deltas: rmp_serde::from_read(&mut rd)?,
            path_pool: rmp_serde::from_read(&mut rd)?,
            object_pool: rmp_serde::from_read(&mut rd)?,
            object_metadata: rmp_serde::from_read(&mut rd)?,
        };
        let _extensions: Vec<OptionalExtension> = rmp_serde::from_read(&mut rd)?;
        Ok(Self::from_v3(wire))
    }

    fn from_legacy(wire: LegacyIndex) -> Result<Self, PackError> {
        let mut path_pool = EntryPool::new();
        for path in wire.path_pool.iter() {
            let path = path.clone().decode().map_err(|error| match error {
                LegacyPathError::UnsupportedWindowsUtf16(units) => {
                    PackError::UnsupportedPathEncoding(format!("Windows UTF-16 units {units:x?}"))
                }
                LegacyPathError::InvalidLogicalPath(error) => PackError::InvalidLogicalPath(error),
            })?;
            path_pool.get_or_insert(&path);
        }
        let snapshot_deltas = wire
            .snapshot_deltas
            .into_iter()
            .map(ChangeSet::from)
            .collect();
        Ok(Self::from_parts(
            wire.snapshot_tags,
            snapshot_deltas,
            path_pool,
            wire.object_pool,
            wire.object_metadata,
        ))
    }

    fn from_v3(wire: V3WireIndex) -> Self {
        Self::from_parts(
            wire.snapshot_tags,
            wire.snapshot_deltas,
            wire.path_pool,
            wire.object_pool,
            wire.object_metadata,
        )
    }

    fn from_parts(
        snapshot_tags: Vec<String>,
        snapshot_deltas: Vec<ChangeSet<FileHandle>>,
        path_pool: EntryPool<PackPath>,
        object_pool: EntryPool<ObjectChecksum>,
        object_metadata: Vec<ObjectMetadata>,
    ) -> Self {
        Self {
            snapshot_tags,
            snapshot_deltas,
            path_pool,
            object_pool,
            object_metadata: object_metadata
                .into_iter()
                .enumerate()
                .map(|(index, metadata)| (index as Handle, metadata))
                .collect(),
            current: HashSet::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LoadMode {
    Full,
    OnlySnapshots,
}

struct LegacyIndex {
    snapshot_tags: Vec<String>,
    snapshot_deltas: Vec<ChangeSet<LegacyFileHandle>>,
    path_pool: EntryPool<LegacyWirePath>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: Vec<ObjectMetadata>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
struct LegacyFileHandle {
    path: Handle,
    object: Handle,
    #[serde(default)]
    file_metadata: LegacyFileMetadata,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
struct LegacyFileMetadata {
    mode: u32,
}

impl Default for LegacyFileMetadata {
    fn default() -> Self {
        Self { mode: 0o100664 }
    }
}

impl From<LegacyFileHandle> for FileHandle {
    fn from(handle: LegacyFileHandle) -> Self {
        Self::new(
            handle.path,
            handle.object,
            EntryMetadata::Regular {
                mode: handle.file_metadata.mode,
            },
        )
    }
}

impl From<ChangeSet<LegacyFileHandle>> for ChangeSet<FileHandle> {
    fn from(changes: ChangeSet<LegacyFileHandle>) -> Self {
        Self {
            added: changes.added.into_iter().map(Into::into).collect(),
            removed: changes.removed.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct OptionalExtension {
    kind: u32,
    payload: Vec<u8>,
}

struct V3WireIndex {
    snapshot_tags: Vec<String>,
    snapshot_deltas: Vec<ChangeSet<FileHandle>>,
    path_pool: EntryPool<PackPath>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: Vec<ObjectMetadata>,
}

struct V3Index<'a> {
    index: &'a PackIndex,
}

impl<'a> From<&'a PackIndex> for V3Index<'a> {
    fn from(index: &'a PackIndex) -> Self {
        Self { index }
    }
}

impl Serialize for V3Index<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tuple = serializer.serialize_tuple(7)?;
        tuple.serialize_element(&0u64)?;
        tuple.serialize_element(&self.index.snapshot_tags)?;
        tuple.serialize_element(&self.index.snapshot_deltas)?;
        tuple.serialize_element(&self.index.path_pool)?;
        tuple.serialize_element(&self.index.object_pool)?;
        tuple.serialize_element(
            &self
                .index
                .object_metadata
                .values()
                .copied()
                .collect::<Vec<_>>(),
        )?;
        tuple.serialize_element(&Vec::<OptionalExtension>::new())?;
        tuple.end()
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[derive(Serialize)]
    enum EncodedLegacyPath {
        Unix(Vec<u8>),
        Windows(Vec<u16>),
    }

    #[derive(Serialize)]
    struct LegacyV1Handle {
        path: Handle,
        object: Handle,
    }

    #[derive(Serialize)]
    struct LegacyV2Handle {
        path: Handle,
        object: Handle,
        file_metadata: LegacyFileMetadata,
    }

    #[derive(Serialize)]
    struct EncodedChangeSet<T> {
        added: Vec<T>,
        removed: Vec<T>,
    }

    fn with_v3_header(body: Vec<u8>) -> Vec<u8> {
        let mut bytes = b"ELFS\0\0\0\x03".to_vec();
        bytes.extend(body);
        bytes
    }

    #[test]
    fn v3_snapshot_listing_respects_array_length() {
        for fields in [0, 1] {
            let mut bytes = vec![0x90 + fields];
            rmp_serde::encode::write(&mut bytes, &0u64).unwrap();
            rmp_serde::encode::write(&mut bytes, &vec!["snapshot"]).unwrap();
            assert!(PackIndex::parse_v3(bytes.as_slice(), LoadMode::OnlySnapshots).is_err());
        }
    }

    #[test]
    fn snapshot_listing_reads_index_prefix() {
        let tags = vec!["first".to_owned(), "second".to_owned()];
        let legacy = rmp_serde::to_vec(&(
            &tags,
            Vec::<()>::new(),
            Vec::<()>::new(),
            Vec::<()>::new(),
            Vec::<()>::new(),
        ))
        .unwrap();
        let mut reader = legacy.as_slice();
        let listed = PackIndex::parse_legacy(&mut reader, LoadMode::OnlySnapshots).unwrap();
        assert_eq!(listed.snapshot_tags(), tags);
        assert!(!reader.is_empty(), "legacy index tables remain unread");

        let mut index = PackIndex::new();
        index.snapshot_tags = tags;
        let mut bytes = Vec::new();
        index.write(&mut bytes).unwrap();
        let mut reader = &bytes[8..];
        let listed = PackIndex::parse_v3(&mut reader, LoadMode::OnlySnapshots).unwrap();
        assert_eq!(listed.snapshot_tags(), index.snapshot_tags());
        assert!(!reader.is_empty(), "version 3 index tables remain unread");
    }

    #[test]
    fn empty_v3_encoding_is_golden() {
        let mut bytes = Vec::new();
        PackIndex::new().write(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            [b'E', b'L', b'F', b'S', 0, 0, 0, 3, 0x97, 0, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,]
        );
    }

    #[test]
    fn v3_round_trip_uses_binary_paths_and_regular_metadata() {
        let entry = FileEntry::new(
            PackPath::new("dir/résumé".as_bytes()).unwrap(),
            [7; 20],
            ObjectMetadata {
                offset: LOOSE_OBJECT_OFFSET,
                size: 42,
            },
            EntryMetadata::Regular { mode: 0o100640 },
        );
        let mut index = PackIndex::new();
        index
            .push_snapshot("snapshot".to_owned(), [entry.clone()])
            .unwrap();

        let mut bytes = Vec::new();
        index.write(&mut bytes).unwrap();
        assert_eq!(&bytes[..8], b"ELFS\0\0\0\x03");

        let decoded = PackIndex::parse(bytes.as_slice()).unwrap();
        let handles = decoded.resolve_snapshot("snapshot").unwrap();
        assert_eq!(
            decoded.entries_from_handles(handles.iter()).unwrap(),
            [entry]
        );
    }

    #[test]
    fn required_features_fail_before_entry_decoding_but_not_snapshot_listing() {
        let body = rmp_serde::to_vec(&(1u64, vec!["snapshot"])).unwrap();
        let bytes = with_v3_header(body);
        assert!(matches!(
            PackIndex::parse(bytes.as_slice()),
            Err(PackError::UnknownRequiredFeatures(1))
        ));

        let mut body = &bytes[8..];
        let snapshots = PackIndex::parse_v3(&mut body, LoadMode::OnlySnapshots).unwrap();
        assert_eq!(snapshots.snapshot_tags(), &["snapshot"]);
    }

    #[test]
    fn unknown_optional_extensions_are_ignored() {
        let body = rmp_serde::to_vec(&(
            0u64,
            vec!["snapshot"],
            Vec::<ChangeSet<FileHandle>>::new(),
            Vec::<PackPath>::new(),
            Vec::<ObjectChecksum>::new(),
            Vec::<ObjectMetadata>::new(),
            vec![OptionalExtension {
                kind: 77,
                payload: vec![1, 2, 3],
            }],
        ))
        .unwrap();
        let index = PackIndex::parse(with_v3_header(body).as_slice()).unwrap();
        assert_eq!(index.snapshot_tags(), &["snapshot"]);
    }

    #[test]
    fn version_three_is_rejected_by_the_legacy_header_rule() {
        let mut bytes = Vec::new();
        PackIndex::new().write(&mut bytes).unwrap();
        let version = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert!(version > 2);
    }

    #[test]
    fn version_zero_is_rejected_without_dispatching() {
        assert!(matches!(
            PackIndex::parse(b"ELFS\0\0\0\0".as_slice()),
            Err(PackError::BadPackVersion([0, 0, 0, 0]))
        ));
    }

    #[test]
    fn equivalent_legacy_unix_and_windows_snapshots_have_the_same_identity() {
        let checksum = [9; 20];
        let metadata = ObjectMetadata {
            offset: 0,
            size: 12,
        };
        let unix = rmp_serde::to_vec(&(
            vec!["snapshot"],
            vec![EncodedChangeSet {
                added: vec![LegacyV1Handle { path: 0, object: 0 }],
                removed: Vec::<LegacyV1Handle>::new(),
            }],
            vec![EncodedLegacyPath::Unix(b"dir/file".to_vec())],
            vec![checksum],
            vec![metadata],
        ))
        .unwrap();
        let windows = rmp_serde::to_vec(&(
            vec!["snapshot"],
            vec![EncodedChangeSet {
                added: vec![LegacyV2Handle {
                    path: 0,
                    object: 0,
                    file_metadata: LegacyFileMetadata::default(),
                }],
                removed: Vec::<LegacyV2Handle>::new(),
            }],
            vec![EncodedLegacyPath::Windows(
                r"dir\file".encode_utf16().collect(),
            )],
            vec![checksum],
            vec![metadata],
        ))
        .unwrap();

        let mut unix_index = b"ELFS\0\0\0\x01".to_vec();
        unix_index.extend(unix);
        let mut windows_index = b"ELFS\0\0\0\x02".to_vec();
        windows_index.extend(windows);
        let unix = PackIndex::parse(unix_index.as_slice()).unwrap();
        let windows = PackIndex::parse(windows_index.as_slice()).unwrap();
        assert_eq!(
            unix.compute_snapshot_checksum("snapshot"),
            windows.compute_snapshot_checksum("snapshot")
        );
    }

    fn identity(path: &str, checksum: ObjectChecksum, mode: u32) -> ObjectChecksum {
        let mut index = PackIndex::new();
        index
            .push_snapshot(
                "snapshot".to_owned(),
                [FileEntry::new(
                    PackPath::new(path.as_bytes()).unwrap(),
                    checksum,
                    ObjectMetadata { offset: 0, size: 1 },
                    EntryMetadata::Regular { mode },
                )],
            )
            .unwrap();
        index.compute_snapshot_checksum("snapshot").unwrap()
    }

    #[test]
    fn snapshot_identity_preserves_path_content_and_mode_distinctions() {
        let baseline = identity("path", [1; 20], 0o100644);
        assert_ne!(baseline, identity("Path", [1; 20], 0o100644));
        assert_ne!(baseline, identity("path", [2; 20], 0o100644));
        assert_ne!(baseline, identity("path", [1; 20], 0o100755));
    }

    #[test]
    fn snapshot_identity_does_not_depend_on_entry_insertion_order() {
        let make_index = |entries: [FileEntry; 2]| {
            let mut index = PackIndex::new();
            index.push_snapshot("snapshot".to_owned(), entries).unwrap();
            index.compute_snapshot_checksum("snapshot").unwrap()
        };
        let first = FileEntry::new(
            PackPath::new(b"a".to_vec()).unwrap(),
            [1; 20],
            ObjectMetadata { offset: 0, size: 1 },
            EntryMetadata::default(),
        );
        let second = FileEntry::new(
            PackPath::new(b"b".to_vec()).unwrap(),
            [2; 20],
            ObjectMetadata { offset: 1, size: 1 },
            EntryMetadata::default(),
        );
        assert_eq!(
            make_index([first.clone(), second.clone()]),
            make_index([second, first])
        );
    }
}
