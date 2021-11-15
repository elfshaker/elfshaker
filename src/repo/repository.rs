//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use super::{constants::*, pack::IdError};

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    fs::File,
    io,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::SystemTime,
};

use log::{error, info, warn};
use walkdir::WalkDir;

use super::algo::{partition_by_u64, run_in_parallel};
use super::constants::REPO_DIR;
use super::error::Error;
use super::fs::{create_temp_path, ensure_dir, get_last_modified, write_file_atomic};
use super::pack::{write_skippable_frame, Pack, PackFrame, PackHeader, PackId, SnapshotId};
use crate::batch;
use crate::packidx::{FileEntry, ObjectChecksum, ObjectEntry, PackError, PackIndex};
use crate::progress::ProgressReporter;
use crypto::digest::Digest;
use crypto::sha1::Sha1;

/// A struct specifying the the extract options.
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    /// Toggle checksum verification.
    verify: bool,
    /// Toggle reset mode on/off.
    reset: bool,
    /// Toggle checks guarding against overwriting user-modified files.
    force: bool,
    /// Number of decompression threads (this is an upper-limit).
    num_workers: u32,
}

impl ExtractOptions {
    /// Toggle checksum verification.
    pub fn verify(&self) -> bool {
        self.verify
    }
    /// Toggle checksum verification.
    pub fn set_verify(&mut self, value: bool) {
        self.verify = value;
    }
    /// Toggle reset mode on/off.
    pub fn reset(&self) -> bool {
        self.reset
    }
    /// Toggle reset mode on/off.
    pub fn set_reset(&mut self, value: bool) {
        self.reset = value;
    }
    /// Toggle checks guarding against overwriting user-modified files.
    pub fn force(&self) -> bool {
        self.force
    }
    /// Toggle checks guarding against overwriting user-modified files.
    pub fn set_force(&mut self, value: bool) {
        self.force = value;
    }
    /// Number of decompression threads (this is an upper-limit).
    pub fn num_workers(&self) -> u32 {
        self.num_workers
    }
    /// Number of decompression threads (this is an upper-limit).
    pub fn set_num_workers(&mut self, value: u32) {
        self.num_workers = value;
    }
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            // Verification might be expensive, but makes a good default to have
            // since it can help make sure that the pack contents has not been tampered with.
            verify: true,
            reset: false,
            // Safety checks on by default is safer :)
            force: false,
            // Default to single-thread decompression.
            num_workers: 1,
        }
    }
}

/// A struct specifying the the packing options.
#[derive(Clone, Debug)]
pub struct PackOptions {
    pub compression_window_log: u32,
    pub compression_level: i32,
    pub num_workers: u32,
    pub num_frames: u32,
}

#[derive(Clone, Debug)]
pub struct ExtractResult {
    pub modified_file_count: u32,
    pub added_file_count: u32,
    pub removed_file_count: u32,
}
/// Contains methods for interfacing with elfshaker repositories, including
/// methods to create snapshots and pack files, and to extract files from them.
pub struct Repository {
    /// The path containing the [`Repository::data_dir`] directory.
    path: PathBuf,
}

impl Repository {
    /// Opens the specified repository.
    ///
    /// # Arguments
    ///
    /// * `path` - The path containing the [`Repository::data_dir`] directory.
    pub fn open<P>(path: P) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let data_dir = path.as_ref().join(&*Self::data_dir());
        if !Path::exists(&data_dir) {
            error!(
                "The directory {:?} is not an elfshaker repository!",
                data_dir.parent().unwrap_or_else(|| Path::new("/"))
            );
            return Err(Error::RepositoryNotFound);
        }

        Ok(Repository {
            path: path.as_ref().to_owned(),
        })
    }

    // Reads the state of HEAD. If the file does not exist, returns None values.
    // If ctime/mtime cannot be determined, returns None.
    pub fn read_head(&self) -> Result<(Option<SnapshotId>, Option<SystemTime>), Error> {
        let path = self.path.join(&*Self::data_dir()).join(HEAD_FILE);

        let (head, mtime) = match File::open(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => return Err(e.into()),
            Ok(mut file) => {
                let metadata = file.metadata()?;
                let time = get_last_modified(metadata);
                let mut buf = vec![];
                file.read_to_end(&mut buf)?;

                let text = std::str::from_utf8(&buf).map_err(|_| Error::CorruptHead)?;
                let snapshot = SnapshotId::from_str(text).map_err(|_| Error::CorruptHead)?;
                (Some(snapshot), time)
            }
        };

        info!(
            "Current HEAD: {}",
            match &head {
                Some(head) => head.to_string(),
                None => "None".to_owned(),
            }
        );

        Ok((head, mtime))
    }

    /// The base path of the repository.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the pack.
    pub fn open_pack(&self, pack: &PackId) -> Result<Pack, Error> {
        Pack::open(&self.path, pack)
    }

    pub fn packs(&self) -> Result<Vec<PackId>, Error> {
        let root = self.path.join(REPO_DIR).join(PACKS_DIR);
        fs::create_dir_all(&root)?;
        let mut result = WalkDir::new(&root)
            .into_iter()
            .filter_map(|dirent| {
                dirent
                    .map_err(Error::WalkDirError)
                    .and_then(|e| {
                        e.into_path()
                            .strip_prefix(&root)
                            .unwrap() // has prefix by construction.
                            .as_os_str()
                            .to_owned()
                            .into_string()
                            .map_err(Error::Utf8Error)
                            .map(PackId::from_index_path)
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        result.sort();
        Ok(result)
    }

    pub fn loose_packs(&self) -> Result<Vec<PackId>, Error> {
        self.packs().map(|packs| {
            let mut result: Vec<PackId> = packs
                .into_iter()
                .filter(|p| self.is_pack_loose(p))
                .collect();
            result.sort_by_cached_key(|pack_id| {
                self.pack_index_mtime(pack_id).map_or_else(
                    |_| (SystemTime::UNIX_EPOCH, pack_id.clone()),
                    |t| (t, pack_id.clone()),
                )
            });
            result
        })
    }

    fn pack_index_mtime(&self, pack_id: &PackId) -> Result<SystemTime, Error> {
        let pack_index_path = match pack_id {
            PackId::Pack(name) => self
                .path
                .join(&*Repository::data_dir())
                .join(PACKS_DIR)
                .join(name)
                .with_extension(PACK_INDEX_EXTENSION),
        };
        Ok(pack_index_path.metadata()?.modified()?)
    }

    // Find snapshot parses a [pack_id:]snapshot_tag string, where the pack_id
    // is optional. If pack_id is specified, it is an error if the snapshot is
    // not found or is found in more than one pack.
    pub fn find_snapshot(&self, maybe_canonical_snapshot_tag: &str) -> Result<SnapshotId, Error> {
        if let Ok(s) = SnapshotId::from_str(maybe_canonical_snapshot_tag) {
            Ok(s) // Given string specified the pack.
        } else {
            // Search packs.
            let tag = maybe_canonical_snapshot_tag;
            Ok(SnapshotId::new(self.find_pack_with_snapshot(tag)?, tag)?)
        }
    }

    /// find_pack_with_snapshot searches through all packs looking for a
    /// snapshot with the given name. Returns an error if there was no snapshot
    /// with the given name, or if there is more than one pack with the given
    /// name (in which case the snapshot is ambiguous).
    pub fn find_pack_with_snapshot(&self, snapshot: &str) -> Result<PackId, Error> {
        let packs = self
            .packs()?
            .into_iter()
            .filter_map(|pack_id| {
                self.load_index(&pack_id)
                    .map(|idx| {
                        idx.snapshots()
                            .iter()
                            .any(|s| s.tag() == snapshot)
                            .then(|| pack_id)
                    })
                    .transpose()
            })
            .collect::<Result<Vec<PackId>, Error>>()?;

        match packs.len() {
            0 => Err(Error::PackError(PackError::SnapshotNotFound(
                snapshot.to_owned(),
            ))),
            1 => Ok(packs.into_iter().next().unwrap()),
            _ => Err(Error::AmbiguousSnapshotMatch(snapshot.to_owned(), packs)),
        }
    }

    pub fn is_pack(&self, pack_id: &str) -> Result<Option<PackId>, IdError> {
        let data_dir = self.path.join(&*Repository::data_dir());
        let pack_index_path = data_dir
            .join(PACKS_DIR)
            .join(pack_id)
            .with_extension(PACK_INDEX_EXTENSION);
        pack_index_path
            .exists()
            .then(|| PackId::from_str(pack_id))
            .transpose()
    }

    pub fn is_pack_loose(&self, pack_id: &PackId) -> bool {
        let data_dir = self.path.join(&*Repository::data_dir());
        let pack_index_path = match pack_id {
            PackId::Pack(name) => data_dir
                .join(PACKS_DIR)
                .join(name)
                .with_extension(PACK_EXTENSION),
        };
        !pack_index_path.exists()
    }

    pub fn load_index(&self, pack_id: &PackId) -> Result<PackIndex, Error> {
        let data_dir = self.path.join(&*Repository::data_dir());
        let pack_index_path = match pack_id {
            PackId::Pack(name) => data_dir
                .join(PACKS_DIR)
                .join(name)
                .with_extension(PACK_INDEX_EXTENSION),
        };
        info!("Load index {} {}", pack_id, pack_index_path.display());
        Pack::parse_index(std::io::BufReader::new(File::open(pack_index_path)?))
    }

    /// Checks-out the specified snapshot.
    ///
    /// # Arguments
    ///
    /// * `snapshot_id` - The snapshot to extract.
    pub fn extract_snapshot(
        &mut self,
        snapshot_id: SnapshotId,
        opts: ExtractOptions,
    ) -> Result<ExtractResult, Error> {
        let (head, head_time) = self.read_head()?;

        if head.is_some() && head_time.is_none() && !opts.force() {
            warn!("The OS/filesystem does not support file creation timestamps!");
            return Err(Error::DirtyWorkDir);
        }

        // Open the pack and find the snapshot specified in SnapshotId.
        let source_index = self.load_index(snapshot_id.pack())?;

        let entries = source_index.entries_from_snapshot(snapshot_id.tag())?;
        let (new_entries, old_entries) = if opts.reset || head.is_none() {
            // Extract all, remove nothing
            (entries, vec![])
        } else if let Some(head) = head {
            // HEAD and new snapshot packs might differ
            if snapshot_id.pack() == head.pack() {
                let head_entries = source_index.entries_from_snapshot(head.tag())?;
                Self::compute_entry_diff(&head_entries, &entries)
            } else {
                let head_index = self.load_index(head.pack())?;
                let head_entries = head_index.entries_from_snapshot(head.tag()).map_err(|e| {
                    if matches!(e, PackError::SnapshotNotFound(_)) {
                        Error::BrokenHeadRef(Box::new(Error::PackError(e)))
                    } else {
                        Error::PackError(e)
                    }
                })?;
                Self::compute_entry_diff(&head_entries, &entries)
            }
        } else {
            unreachable!();
        };

        // There is no point in deleting files which will be overwritten by the extract, so
        // we identify and ignore them beforehand.
        let (updated_paths, removed_paths) = {
            let new_paths: HashSet<_> = new_entries.iter().map(|e| &e.path).collect();
            let old_paths: HashSet<_> = old_entries.iter().map(|e| &e.path).collect();
            // Paths which will be deleted
            let updated: Vec<_> = new_paths.intersection(&old_paths).copied().collect();
            let removed: Vec<_> = old_paths.difference(&new_paths).copied().collect();
            (updated, removed)
        };

        let mut path_buf = PathBuf::new();
        if !opts.force() {
            for entry in &old_entries {
                path_buf.clear();
                path_buf.push(&self.path);
                path_buf.push(&entry.path);
                self.check_changed_since(head_time.unwrap(), &path_buf)?;
            }
        }
        for path in &removed_paths {
            path_buf.clear();
            path_buf.push(&self.path);
            path_buf.push(path);
            // Delete the file
            match fs::remove_file(&path_buf) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            }?;
        }

        self.extract_entries(snapshot_id.pack(), &new_entries, self.path.clone(), opts)?;
        self.update_head(&snapshot_id)?;

        Ok(ExtractResult {
            added_file_count: (new_entries.len() - updated_paths.len()) as u32,
            removed_file_count: removed_paths.len() as u32,
            modified_file_count: updated_paths.len() as u32,
        })
    }

    /// Extract the specified entries to a given path.
    ///
    /// # Arguments
    ///
    /// * `pack_id` - The pack_id containing the entries.
    /// * `entries` - The list of entries to extract.
    /// * `path` - The destination path.
    /// * `verify` - Set to true to verify object checksums after extraction.
    pub fn extract_entries<P>(
        &mut self,
        pack_id: &PackId,
        entries: &[FileEntry],
        path: P,
        opts: ExtractOptions,
    ) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        if let Ok(pack) = self.open_pack(pack_id) {
            pack.extract_entries(entries, path.as_ref(), opts.verify(), opts.num_workers())
        } else {
            self.copy_loose_entries(entries, path.as_ref(), opts.verify())
        }
    }

    /// The name of the directory containing the elfshaker repository data.
    pub fn data_dir() -> Cow<'static, str> {
        Cow::Borrowed(REPO_DIR)
    }

    pub fn create_snapshot<I, P>(&mut self, snapshot: &SnapshotId, files: I) -> Result<(), Error>
    where
        I: Iterator<Item = P>,
        P: AsRef<Path>,
    {
        let files = clean_file_list(self.path.as_ref(), files)?.collect::<Vec<_>>();
        info!("Computing checksums for {} files...", files.len());

        let temp_dir = self.temp_dir();
        ensure_dir(&temp_dir)?;

        let threads = num_cpus::get();

        let pack_entries = run_in_parallel(threads, files.into_iter(), |file_path| {
            let buf = fs::read(&file_path)?;
            let mut checksum = [0u8; 20];
            let mut hasher = Sha1::new();
            hasher.input(&buf);
            hasher.result(&mut checksum);
            self.write_loose_object(&*buf, &temp_dir, &checksum)?;

            Ok(FileEntry::new(
                file_path.into(),
                ObjectEntry::loose(checksum, buf.len() as u64),
            ))
        })
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;

        let mut index = PackIndex::new();
        index.push_snapshot(snapshot.tag(), &pack_entries)?;

        let loose_path = self.path().join(REPO_DIR).join(PACKS_DIR).join(LOOSE_DIR);
        ensure_dir(&loose_path)?;

        let mut buf = vec![];
        // This should not fail, unless there is an error in the implementation.
        rmp_serde::encode::write(&mut buf, &index).expect("Serialization failed!");
        write_file_atomic(
            buf.as_slice(),
            &self.temp_dir(),
            &loose_path
                .join(snapshot.tag())
                .with_extension(PACK_INDEX_EXTENSION),
        )?;

        self.update_head(snapshot)?;

        Ok(())
    }

    /// Creates a pack file.
    ///
    /// # Arguments
    ///
    /// * `pack` - The name of the pack file to create
    /// * `index` - The index for the new pack
    /// * `opts` - Additional options to use during pack creation
    pub fn create_pack(
        &mut self,
        pack: &PackId,
        index: &PackIndex,
        opts: &PackOptions,
        reporter: &ProgressReporter,
    ) -> Result<(), Error> {
        let PackId::Pack(pack_name) = pack;

        // Construct output file path.
        let pack_path = {
            let mut pack_path = self.path.to_owned();
            pack_path.push(&*Repository::data_dir());
            pack_path.push(PACKS_DIR);
            ensure_dir(&pack_path)?;
            pack_path.push(format!("{}.{}", pack_name, PACK_EXTENSION));
            pack_path
        };

        // Create a temporary file to use during compression.
        let temp_dir = self.temp_dir();
        ensure_dir(&temp_dir)?;
        let temp_path = create_temp_path(&temp_dir);

        // Gather a list of all objects to compress.
        // The `index.objects()` list should be pre-sorted for optimal compression.
        let object_partitions =
            partition_by_u64(index.objects(), opts.num_frames, |o| o.size as u64);

        let workers_per_task = (opts.num_workers + object_partitions.len() as u32 - 1)
            / std::cmp::max(1, object_partitions.len()) as u32;

        let task_opts = batch::CompressionOptions {
            window_log: opts.compression_window_log,
            level: opts.compression_level,
            num_workers: workers_per_task,
        };

        // Keep count of done compression tasks
        let done_task_count = std::sync::atomic::AtomicUsize::new(0);
        let total_task_count = object_partitions.len();

        info!("Creating {} compressed frames...", total_task_count);

        let mut frames = vec![];
        let mut frame_bufs = vec![];

        let frame_results = run_in_parallel(
            opts.num_workers as usize,
            object_partitions.into_iter(),
            |objects| {
                let object_readers = objects.iter().map(|handle| {
                    // TODO: Method of obtaining readers from packs? Or we can
                    // just assume packs first get unpacked.
                    Ok(Box::new(File::open(
                        self.loose_object_path(&handle.checksum),
                    )?))
                });

                let mut buf = vec![];
                // Compress all the object files.
                let r = batch::compress_files(
                    &mut buf,
                    object_readers,
                    &task_opts,
                    &ProgressReporter::dummy(),
                )
                .map(move |bytes| (bytes, buf));
                // Update done count.
                let done = done_task_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                // And report the change.
                reporter.checkpoint(done, Some(total_task_count - done));
                r
            },
        );

        for frame_result in frame_results {
            let (decompressed_size, compressed_buffer) = frame_result?;
            frames.push(PackFrame {
                frame_size: compressed_buffer.len() as u64,
                decompressed_size,
            });
            // Note: storing the whole file in memory at this point.
            // Could write them out, except that the header needs to be prepended.
            frame_bufs.push(compressed_buffer);
        }

        // Report that all compression tasks are done.
        reporter.checkpoint(total_task_count, Some(0));

        // Create and serialize header.
        let header = PackHeader::new(frames);
        let header_bytes = rmp_serde::encode::to_vec(&header).expect("Serialization failed!");

        // And a writer to that temporary file.
        let mut pack_writer = io::BufWriter::new(File::create(&temp_path)?);
        // Write header and frames.
        write_skippable_frame(&mut pack_writer, &header_bytes)?;
        for frame_buf in frame_bufs {
            pack_writer.write_all(&frame_buf)?;
        }
        pack_writer.flush()?;
        drop(pack_writer);

        // Serialize the index.
        let mut index_bytes: Vec<u8> = vec![];
        rmp_serde::encode::write(&mut index_bytes, &index).expect("Serialization failed!");

        let index_path = pack_path.with_extension(PACK_INDEX_EXTENSION);
        info!("Write index: {}", index_path.display());
        write_file_atomic(index_bytes.as_slice(), &temp_dir, &index_path)?;

        // Finally, move tha .pack file itself to the packs/ dir.
        fs::rename(&temp_path, &pack_path)?;

        Ok(())
    }

    /// Deletes ALL loose snapshots and objects.
    pub fn remove_loose_all(&mut self) -> Result<(), Error> {
        let mut loose_dir = self.path().join(&*Repository::data_dir());
        loose_dir.push(LOOSE_DIR);

        match fs::remove_dir_all(&loose_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }?;
        Ok(())
    }

    /// Updates the HEAD snapshot id.
    pub fn update_head(&mut self, snapshot_id: &SnapshotId) -> Result<(), Error> {
        let data_dir = self.path.join(&*Self::data_dir());
        let snapshot_string = format!("{}\n", snapshot_id);
        ensure_dir(&self.temp_dir())?;
        write_file_atomic(
            snapshot_string.as_bytes(),
            &self.temp_dir(),
            &data_dir.join(HEAD_FILE),
        )?;
        Ok(())
    }

    fn copy_loose_entries(
        &mut self,
        entries: &[FileEntry],
        path: &Path,
        verify: bool,
    ) -> Result<(), Error> {
        let mut dest_paths = vec![];
        let mut dest_path = PathBuf::new();
        for entry in entries {
            dest_path.clear();
            dest_path.push(path);
            dest_path.push(&entry.path);
            dest_paths.push(dest_path.clone());
            fs::create_dir_all(dest_path.parent().unwrap())?;
            fs::copy(self.loose_object_path(&entry.object.checksum), &dest_path)?;
        }

        if verify {
            let checksums = batch::compute_checksums(&dest_paths)?;
            let expected_checksums = entries.iter().map(|e| &e.object.checksum);
            for (expected, actual) in expected_checksums.zip(checksums) {
                if *expected != actual {
                    return Err(PackError::ChecksumMismatch.into());
                }
            }
        }

        Ok(())
    }

    /// Returns the pair of lists (`added`, `removed`). `added` contains the entries from
    /// `to_entries` which are not present in `from_entries`. `removed` contains the entries
    /// from `from_entries` which are not present in `to_entries`.
    fn compute_entry_diff(
        from_entries: &[FileEntry],
        to_entries: &[FileEntry],
    ) -> (Vec<FileEntry>, Vec<FileEntry>) {
        // Create lookup based on path+checksum.
        // The reason we're not using a `HashSet<FileEntry>` here is that
        // we only care about the file path and checksum,
        // but not, for example, the offset of the object in the pack file.
        let from_lookup: HashMap<_, _> = from_entries
            .iter()
            .map(|e| ((&e.path, &e.object.checksum), e))
            .collect();
        let to_lookup: HashMap<_, _> = to_entries
            .iter()
            .map(|e| ((&e.path, &e.object.checksum), e))
            .collect();

        let mut added = vec![];
        let mut removed = vec![];

        // Check which "from" entries are missing in to_entries and mark them as removed
        for (key, &entry) in &from_lookup {
            if !to_lookup.contains_key(key) {
                removed.push((*entry).clone());
            }
        }
        // Check which "to" entries were added and mark them as added
        for (key, &entry) in &to_lookup {
            if !from_lookup.contains_key(key) {
                added.push((*entry).clone());
            }
        }

        (added, removed)
    }

    fn check_changed_since(&self, head_time: SystemTime, path: &Path) -> Result<(), Error> {
        let last_modified = fs::metadata(&path)
            // The modification date of the file is unknown, there is no other
            // option to fallback on, so we mark the directory as dirty.
            .map_err(|_| {
                warn!("Expected file {:?} to be present!", path);
                // If the file is missing that also means it has been modified!
                Error::DirtyWorkDir
            })
            // If the modification date of the file is unknown, there is no
            // other option to fallback on, so we mark the directory as dirty.
            .and_then(|metadata| get_last_modified(metadata).ok_or(Error::DirtyWorkDir))?;

        if head_time < last_modified {
            warn!(
                "File {} is more recent than the current HEAD!",
                path.to_string_lossy()
            );
            // If the file is more recent that means that the repo has
            // been modified unexpectedly!
            return Err(Error::DirtyWorkDir);
        }

        Ok(())
    }

    fn temp_dir(&self) -> PathBuf {
        let mut temp_dir = self.path.join(&*Repository::data_dir());
        temp_dir.push(TEMP_DIR);
        temp_dir
    }

    /// Atomically writes an object to the loose object store.
    ///
    /// # Arguments
    ///
    /// * `repo_path` - The root of the repository.
    fn write_loose_object(
        &self,
        mut reader: impl Read,
        temp_dir: &Path,
        checksum: &ObjectChecksum,
    ) -> io::Result<()> {
        let obj_path = self.loose_object_path(checksum);
        if obj_path.exists() {
            // No need to do anything. Object writes are atomic, so if an object
            // with the same checksum already exists, there is no need to do anything.
            return Ok(());
        }

        // Write to disk
        fs::create_dir_all(obj_path.parent().unwrap())?;
        write_file_atomic(&mut reader, temp_dir, &obj_path)?;
        Ok(())
    }

    pub fn loose_object_path(&self, checksum: &ObjectChecksum) -> PathBuf {
        let checksum_str = hex::encode(&checksum[..]);
        let mut obj_path = self.path.join(&*Repository::data_dir());
        // $REPO_DIR/$LOOSE
        obj_path.push(LOOSE_DIR);

        // $REPO_DIR/$LOOSE/FA/
        obj_path.push(&checksum_str[..2]);
        // $REPO_DIR/$LOOSE/FA/F0/
        obj_path.push(&checksum_str[2..4]);
        // $REPO_DIR/$LOOSE/FA/F0/FAF0F0F0FAFAF0F0F0FAFAF0F0
        obj_path.push(&checksum_str[4..]);
        obj_path
    }
}

/// Cleans the list of file paths relative to the repository root,
/// and skips any paths pointing into the repository data directory.
fn clean_file_list<P>(
    repo_dir: &Path,
    files: impl Iterator<Item = P>,
) -> io::Result<impl Iterator<Item = PathBuf>>
where
    P: AsRef<Path>,
{
    let files = files
        .map(|p| {
            if p.as_ref().is_relative() {
                Ok(p)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Expected a relative path, got {:?}!", p.as_ref()),
                ))
            }
        })
        .flatten()
        .map(|p| {
            Ok(p.as_ref()
                .canonicalize()?
                .components()
                .skip(repo_dir.components().count())
                .collect::<PathBuf>())
        })
        .collect::<io::Result<Vec<PathBuf>>>()?
        .into_iter()
        .filter(|p| !is_elfshaker_data_path(p));

    Ok(files)
}

/// Checks if the relative path is rooted at the data directory.
fn is_elfshaker_data_path(p: &Path) -> bool {
    assert!(p.is_relative());
    match p.components().next() {
        Some(c) => c.as_os_str() == &*Repository::data_dir(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn building_loose_object_paths_works() {
        let checksum = [
            0xFA, 0xF0, 0xDE, 0xAD, 0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0, 0xFA, 0xF0, 0xDE, 0xAD,
            0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0,
        ];
        let repo = Repository{path: "/repo".into()};
        let path = repo.loose_object_path(&checksum);
        assert_eq!(
            format!(
                "/repo/{}/{}/fa/f0/deadbeefbadc0de0faf0deadbeefbadc0de0",
                Repository::data_dir(),
                LOOSE_DIR
            ),
            path.to_str().unwrap(),
        );
    }

    #[test]
    fn data_dir_detected() {
        let path = format!("{}", Repository::data_dir());
        assert!(is_elfshaker_data_path(path.as_ref()));
    }
    #[test]
    fn data_dir_detected_as_parent() {
        let path = format!("{}/something", Repository::data_dir());
        assert!(is_elfshaker_data_path(path.as_ref()));
    }
    #[test]
    fn data_dir_not_detected_incorrectly() {
        let path = "some/path/something";
        assert!(!is_elfshaker_data_path(path.as_ref()));
    }

    #[test]
    fn compute_entry_diff_finds_updates() {
        let path = "/path/to/A";
        let old_checksum = [0; 20];
        let new_checksum = [1; 20];
        let old_entries = [FileEntry::new(
            path.into(),
            ObjectEntry::loose(old_checksum, 1),
        )];
        let new_entries = [FileEntry::new(
            path.into(),
            ObjectEntry::loose(new_checksum, 1),
        )];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(1, added.len());
        assert_eq!(path, added[0].path);
        assert_eq!(1, removed.len());
        assert_eq!(path, removed[0].path);
    }

    #[test]
    fn compute_entry_diff_finds_update_of_duplicated() {
        let path_a = "/path/to/A";
        let path_a_old_checksum = [0; 20];
        let path_b = "/path/to/B";
        let path_b_old_checksum = [0; 20];
        let path_a_new_checksum = [1; 20];
        let old_entries = [
            FileEntry::new(path_a.into(), ObjectEntry::loose(path_a_old_checksum, 1)),
            FileEntry::new(path_b.into(), ObjectEntry::loose(path_b_old_checksum, 1)),
        ];
        let new_entries = [FileEntry::new(
            path_a.into(),
            ObjectEntry::loose(path_a_new_checksum, 1),
        )];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(1, added.len());
        assert_eq!(path_a, added[0].path);
        assert_eq!(2, removed.len());
        assert!(removed.iter().any(|e| path_a == e.path));
        assert!(removed.iter().any(|e| path_b == e.path));
    }

    #[test]
    fn compute_entry_diff_path_switch() {
        let path_a = "/path/to/A";
        let path_a_old_checksum = [0; 20];
        let path_a_new_checksum = [1; 20];
        let path_b = "/path/to/B";
        let path_b_old_checksum = [1; 20];
        let path_b_new_checksum = [0; 20];
        let old_entries = [
            FileEntry::new(path_a.into(), ObjectEntry::loose(path_a_old_checksum, 1)),
            FileEntry::new(path_b.into(), ObjectEntry::loose(path_b_old_checksum, 1)),
        ];
        let new_entries = [
            FileEntry::new(path_a.into(), ObjectEntry::loose(path_a_new_checksum, 1)),
            FileEntry::new(path_b.into(), ObjectEntry::loose(path_b_new_checksum, 1)),
        ];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(2, added.len());
        assert!(added
            .iter()
            .any(|e| path_a == e.path && path_a_new_checksum == e.object.checksum));
        assert!(added
            .iter()
            .any(|e| path_b == e.path && path_b_new_checksum == e.object.checksum));
        assert_eq!(2, removed.len());
        assert!(removed
            .iter()
            .any(|e| path_a == e.path && path_a_old_checksum == e.object.checksum));
        assert!(removed
            .iter()
            .any(|e| path_b == e.path && path_b_old_checksum == e.object.checksum));
    }
}
