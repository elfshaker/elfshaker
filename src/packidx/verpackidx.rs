use crate::entrypool::Handle;

use super::packidx::{FileEntry, FileHandle, ObjectChecksum, ObjectMetadata, PackError, PackIndex};

use super::packidxv1::PackIndexV1;

use std::collections::HashSet;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

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

    /// Create and add a new snapshot compatible with the loose
    /// index format. The list of [`FileEntry`] is the files to record in the snapshot.
    fn push_snapshot(
        &mut self,
        tag: String,
        input: impl IntoIterator<Item = FileEntry>,
    ) -> Result<(), PackError> {
        match self {
            VerPackIndex::V1(p) => p.push_snapshot(tag, input),
        }
    }

    // Call the closure F with materialized file entries for each snapshot.
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

    /// Computes the checksum of the contents of the snapshot.
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
