use crate::entrypool::{EntryPool, Handle};
use crate::repo::{fs::create_file, partition_by_u64};

use super::packidx::{
    ChangeSet, FileEntry, FileHandle, ObjectChecksum, ObjectMetadata, PackError, PackIndex,
    Snapshot,
};

use serde::de::{SeqAccess, Visitor};
use serde::{ser::SerializeTuple, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{BufReader, BufWriter, Read, Write};
use std::ops::ControlFlow;
use std::path::Path;

/// Contains the metadata needed to extract files from a pack file.
pub struct PackIndexV2 {
    snapshot_tags: Vec<String>,
    snapshot_checksums: Vec<ObjectChecksum>,
    snapshot_deltas: Vec<ChangeSet<FileHandle>>,

    path_pool: EntryPool<String>,
    object_pool: EntryPool<ObjectChecksum>,
    object_metadata: BTreeMap<Handle, ObjectMetadata>,

    // When snapshots are pushed, maintain the current state of the filesystem.
    // Not stored on disk.
    // TODO: Move this onto a separate builder class.
    current: HashSet<FileHandle>,
}

impl Default for PackIndexV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl PackIndex for PackIndexV2 {
    fn new() -> Self {
        Self {
            snapshot_tags: Vec::new(),
            snapshot_checksums: Vec::new(),
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
        self.snapshot_checksums.push(todo!());
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
            file_count += deltas.added().len() as i64;
            file_count -= deltas.removed().len() as i64;
            if let ControlFlow::Break(output) = f(snapshot, file_count as u64) {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }
    /// Computes the checksum of the contents of the snapshot.
    fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum> {
        let index = self.snapshot_tags.iter().position(|x| x == snapshot)?;
        Some(self.snapshot_checksums[index])
    }

    fn parse<R: Read>(rd: R) -> Result<Self, PackError> {
        let rd = BufReader::new(rd);

        Ok(rmp_serde::decode::from_read(rd)?)
    }

    fn parse_only_snapshots<R: Read>(rd: R) -> Result<Vec<String>, PackError> {
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

impl PackIndexV2 {
    pub fn read_magic(rd: &mut impl Read) -> Result<(), PackError> {
        let mut magic = [0; 4];
        rd.read_exact(&mut magic)?;
        if magic.ne(&*b"ELFS") {
            return Err(PackError::BadMagic);
        }
        let mut version = [0; 4];
        rd.read_exact(&mut version)?;
        if version.ne(&[0, 0, 0, 2]) {
            return Err(PackError::BadPackVersion(version));
        }
        Ok(())
    }

    fn write_magic(wr: &mut impl Write) -> std::io::Result<()> {
        wr.write_all(&*b"ELFS")?;
        wr.write_all(&[0, 0, 0, 2])?;
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

#[derive(Serialize, Deserialize)]
struct ObjectMetadataV2 {
    pub offset: u64,
    pub size: u64,
    pub mode_bits: u32,
}

impl From<ObjectMetadata> for ObjectMetadataV2 {
    fn from(md: ObjectMetadata) -> Self {
        Self {
            offset: md.offset,
            size: md.size,
            mode_bits: md.mode_bits,
        }
    }
}

impl From<ObjectMetadataV2> for ObjectMetadata {
    fn from(md: ObjectMetadataV2) -> Self {
        Self {
            offset: md.offset,
            size: md.size,
            mode_bits: md.mode_bits,
        }
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
    type Value = PackIndexV2;

    fn visit_seq<V>(self, mut seq: V) -> Result<PackIndexV2, V::Error>
    where
        V: SeqAccess<'de>,
    {
        let mut result = PackIndexV2::new();
        result.snapshot_tags = next_expecting(&mut seq)?;
        if self.load_mode == LoadMode::OnlySnapshots {
            return Ok(result);
        }
        result.snapshot_checksums = next_expecting(&mut seq)?;
        result.snapshot_deltas = next_expecting(&mut seq)?;
        result.path_pool = next_expecting(&mut seq)?;
        result.object_pool = next_expecting(&mut seq)?;
        let md: Vec<ObjectMetadataV2> = next_expecting(&mut seq)?;
        result.object_metadata = md
            .into_iter()
            .enumerate()
            .map(|(i, md)| ((i as Handle), md.into()))
            .collect();

        Ok(result)
    }
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("PathIndex")
    }
}

impl<'de> Deserialize<'de> for PackIndexV2 {
    fn deserialize<D>(deserializer: D) -> Result<PackIndexV2, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(VisitPackIndex {
            load_mode: LoadMode::Full,
        })
    }
}

impl Serialize for PackIndexV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_tuple(5)?;
        s.serialize_element(&self.snapshot_tags)?;
        s.serialize_element(&self.snapshot_checksums)?;
        s.serialize_element(&self.snapshot_deltas)?;
        s.serialize_element(&self.path_pool)?;
        s.serialize_element(&self.object_pool)?;
        // Ordering comes from BTreeMap keys, so is for free.
        s.serialize_element(
            &self
                .object_metadata
                .values()
                .map(|x| x.clone().into())
                .collect::<Vec<ObjectMetadataV2>>(),
        )?;
        s.end()
    }
}
