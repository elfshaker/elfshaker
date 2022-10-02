use crate::entrypool::Handle;
use crate::repo::fs::open_file;

use super::packidx::{FileEntry, FileHandle, ObjectChecksum, ObjectMetadata, PackError, PackIndex};

use super::packidxv1::PackIndexV1;
use super::packidxv2::PackIndexV2;

use std::collections::HashSet;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

pub enum VerPackIndex {
    V1(PackIndexV1),
    V2(PackIndexV2),
}

impl PackIndex for VerPackIndex {
    fn new() -> Self {
        // Use the highest supported pack index version.
        VerPackIndex::V2(PackIndexV2::new())
    }

    fn object_size_total(&self) -> u64 {
        match self {
            VerPackIndex::V1(p) => p.object_size_total(),
            VerPackIndex::V2(p) => p.object_size_total(),
        }
    }

    fn object_checksums(&self) -> std::slice::Iter<ObjectChecksum> {
        match self {
            VerPackIndex::V1(p) => p.object_checksums(),
            VerPackIndex::V2(p) => p.object_checksums(),
        }
    }

    fn object_metadata(&self, checksum: &ObjectChecksum) -> &ObjectMetadata {
        match self {
            VerPackIndex::V1(p) => p.object_metadata(checksum),
            VerPackIndex::V2(p) => p.object_metadata(checksum),
        }
    }

    fn objects_partitioned_by_size<'l>(
        &self,
        partitions: u32,
        handles: &'l [Handle],
    ) -> Vec<&'l [Handle]> {
        match self {
            VerPackIndex::V1(p) => p.objects_partitioned_by_size(partitions, handles),
            VerPackIndex::V2(p) => p.objects_partitioned_by_size(partitions, handles),
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
            VerPackIndex::V2(p) => {
                let (p2, vec) = p.compute_object_offsets_and_ordering();
                (VerPackIndex::V2(p2), vec)
            }
        }
    }

    fn handle_to_checksum(&self, h: Handle) -> &ObjectChecksum {
        match self {
            VerPackIndex::V1(p) => p.handle_to_checksum(h),
            VerPackIndex::V2(p) => p.handle_to_checksum(h),
        }
    }

    fn handle_to_entry(&self, handle: &FileHandle) -> Result<FileEntry, PackError> {
        match self {
            VerPackIndex::V1(p) => p.handle_to_entry(handle),
            VerPackIndex::V2(p) => p.handle_to_entry(handle),
        }
    }

    fn entry_to_handle(&mut self, entry: &FileEntry) -> Result<FileHandle, PackError> {
        match self {
            VerPackIndex::V1(p) => p.entry_to_handle(entry),
            VerPackIndex::V2(p) => p.entry_to_handle(entry),
        }
    }

    fn snapshot_tags(&self) -> &[String] {
        match self {
            VerPackIndex::V1(p) => p.snapshot_tags(),
            VerPackIndex::V2(p) => p.snapshot_tags(),
        }
    }

    fn has_snapshot(&self, needle: &str) -> bool {
        match self {
            VerPackIndex::V1(p) => p.has_snapshot(needle),
            VerPackIndex::V2(p) => p.has_snapshot(needle),
        }
    }

    fn resolve_snapshot(&self, needle: &str) -> Option<Vec<FileHandle>> {
        match self {
            VerPackIndex::V1(p) => p.resolve_snapshot(needle),
            VerPackIndex::V2(p) => p.resolve_snapshot(needle),
        }
    }

    fn entries_from_handles<'l>(
        &self,
        handles: impl Iterator<Item = &'l FileHandle>,
    ) -> Result<Vec<FileEntry>, PackError> {
        match self {
            VerPackIndex::V1(p) => p.entries_from_handles(handles),
            VerPackIndex::V2(p) => p.entries_from_handles(handles),
        }
    }

    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    fn push_snapshot(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError> {
        match self {
            VerPackIndex::V1(p) => p.push_snapshot(tag, input),
            VerPackIndex::V2(p) => p.push_snapshot(tag, input),
        }
    }

    // Call the closure F with materialized file entries for each snapshot.
    fn for_each_snapshot<'l, F, S>(&'l self, f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, &HashSet<FileEntry>) -> ControlFlow<S>,
    {
        match self {
            VerPackIndex::V1(p) => p.for_each_snapshot(f),
            VerPackIndex::V2(p) => p.for_each_snapshot(f),
        }
    }

    fn for_each_snapshot_file_count<'l, F, S>(&'l self, f: F) -> Result<Option<S>, PackError>
    where
        F: FnMut(&'l str, u64) -> ControlFlow<S>,
    {
        match self {
            VerPackIndex::V1(p) => p.for_each_snapshot_file_count(f),
            VerPackIndex::V2(p) => p.for_each_snapshot_file_count(f),
        }
    }

    /// Computes the checksum of the contents of the snapshot.
    fn compute_snapshot_checksum(&self, snapshot: &str) -> Option<ObjectChecksum> {
        match self {
            VerPackIndex::V1(p) => p.compute_snapshot_checksum(snapshot),
            VerPackIndex::V2(p) => p.compute_snapshot_checksum(snapshot),
        }
    }

    fn parse<R: Read>(mut rd: R) -> Result<Self, PackError>
    where
        Self: Sized,
    {
        let mut magic = [0; 8];
        rd.read_exact(&mut magic)?;

        if PackIndexV2::read_magic(&mut &magic[..]).is_ok() {
            PackIndexV2::parse(rd).map(VerPackIndex::V2)
        } else {
            PackIndexV1::read_magic(&mut &magic[..])?;
            PackIndexV1::parse(rd).map(VerPackIndex::V1)
        }
    }

    fn parse_only_snapshots<R: Read>(mut rd: R) -> Result<Vec<String>, PackError>
    where
        Self: Sized,
    {
        let mut magic = [0; 8];
        rd.read_exact(&mut magic)?;

        if PackIndexV2::read_magic(&mut &magic[..]).is_ok() {
            PackIndexV2::parse_only_snapshots(rd)
        } else {
            PackIndexV1::read_magic(&mut &magic[..])?;
            PackIndexV1::parse_only_snapshots(rd)
        }
    }

    fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), PackError> {
        match self {
            VerPackIndex::V1(p) => p.save(path),
            VerPackIndex::V2(p) => p.save(path),
        }
    }
}

impl VerPackIndex {
    pub fn load<P: AsRef<Path>>(p: P) -> Result<Self, PackError>
    where
        Self: Sized,
    {
        let rd = open_file(p.as_ref())?;
        Self::parse(rd)
    }

    pub fn load_only_snapshots<P: AsRef<Path>>(p: P) -> Result<Vec<String>, PackError> {
        let rd = open_file(p.as_ref())?;
        Self::parse_only_snapshots(rd)
    }
}
