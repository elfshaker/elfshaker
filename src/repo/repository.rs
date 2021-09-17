//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use super::constants::*;

use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap, HashSet},
    ffi::OsStr,
    fs,
    fs::File,
    io,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::SystemTime,
};

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

use super::algo::{partition_by_u64, run_in_parallel};
use super::constants::REPO_DIR;
use super::error::Error;
use super::fs::{create_temp_path, ensure_dir, get_last_modified, read_or_none, write_file_atomic};
use super::pack::{write_skippable_frame, Pack, PackFrame, PackHeader, PackId, SnapshotId};
use crate::batch;
use crate::log::measure_ok;
use crate::packidx::{ObjectChecksum, ObjectIndex, PackEntry, PackError, PackIndex};
use crate::progress::ProgressReporter;

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

#[derive(Serialize, Deserialize)]
pub struct RepositoryIndex {
    snapshots: HashMap<String, Vec<PackId>>,
}

impl RepositoryIndex {
    pub fn new() -> Self {
        Self {
            snapshots: Default::default(),
        }
    }

    /// Returns the names of the packs containing the snapshot.
    pub fn find_packs(&self, snapshot: &str) -> &[PackId] {
        &self
            .snapshots
            .get(snapshot)
            .map(|x| x as &[PackId])
            .unwrap_or(&[])
    }

    /// Returns the names of the packs containing the snapshot.
    pub fn available_snapshots(&self) -> Cow<[&str]> {
        self.snapshots.keys().map(|tag| &tag as &str).collect()
    }

    /// The list of known packs containing at least 1 snapshot.
    pub fn available_packs(&self) -> Cow<Vec<PackId>> {
        // Derive the pack names from the index
        let packs: HashSet<&PackId> = self.snapshots.values().flatten().collect();
        let mut packs: Vec<PackId> = packs.into_iter().cloned().collect();
        packs.sort_unstable();
        // We might want to cache this, hence the Cow
        Cow::Owned(packs)
    }

    /// Adds a new snapshot to the index.
    pub fn add_snapshot(&mut self, snapshot: &SnapshotId) {
        match self.snapshots.entry(snapshot.tag().to_owned()) {
            Entry::Occupied(mut o) => {
                o.get_mut().push(snapshot.pack().clone());
            }
            Entry::Vacant(v) => {
                v.insert(vec![snapshot.pack().clone()]);
            }
        }
    }
}

impl Default for RepositoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Repository {
    /// The path containing the [`Repository::data_dir`] directory.
    path: PathBuf,
    /// The ID of the currently extracted snapshot or None, if nothing has been extracted yet.
    head: Option<SnapshotId>,
    /// The time HEAD was last modified.
    head_time: Option<SystemTime>,
    /// Maps (snapshot -> packs)
    index: RepositoryIndex,
}

impl Repository {
    /// Rebuilds the repository index. Use this if the repository fails to open.
    pub fn update_index(repo_dir: &Path) -> Result<RepositoryIndex, Error> {
        info!("Opening repository {:?}...", repo_dir);
        let mut packs_dir = repo_dir.to_owned();
        packs_dir.push(&*Repository::data_dir());
        packs_dir.push(PACKS_DIR);

        // File all pack index files under $REPO_DIR/packs.
        let packs = WalkDir::new(packs_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(is_pack_index)
            .collect::<Vec<_>>();

        info!("Found {} packs!", packs.len());
        let mut index = RepositoryIndex::new();

        // Construct a top-level index from the pack indexes.
        for pack in &packs {
            let pack_path = pack.path();
            // Pack names must be valid unicode.
            let pack_name = pack_path.file_name().unwrap().to_str().unwrap();
            info!("Processing {}...", pack_name);
            let pack_name: String = {
                // Skip the extension
                let mut chars = pack_name.chars();
                chars.nth_back(PACK_INDEX_EXTENSION.len()).unwrap();
                chars.collect()
            };
            let pack_index = Pack::parse_index(std::io::BufReader::new(File::open(pack_path)?))?;
            let snapshots = pack_index.snapshots();
            for snapshot in snapshots {
                let id = SnapshotId::new(PackId::Packed(pack_name.to_owned()), snapshot.tag())?;
                index.add_snapshot(&id);
            }
        }

        for snapshot in Self::read_unpacked_index(repo_dir)?.snapshots() {
            let id = SnapshotId::unpacked(snapshot.tag())?;
            index.add_snapshot(&id);
        }

        let mut index_path = std::env::current_dir()?;
        index_path.push(&*Repository::data_dir());
        index_path.push(INDEX_FILE);
        info!("Writing {:?}...", index_path);
        let mut writer = File::create(index_path)?;
        rmp_serde::encode::write(&mut writer, &index).expect("Serialization failed!");

        Ok(index)
    }

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

        let (head, head_time) = Self::read_head(&data_dir.join(HEAD_FILE))?;

        if let Some(id) = head.as_ref() {
            info!("Current HEAD: {}/{}", id.pack(), id.tag());
        } else {
            info!("Current HEAD: None");
        }

        let index: RepositoryIndex = {
            if let Some(bytes) = read_or_none(data_dir.join(INDEX_FILE))? {
                rmp_serde::decode::from_slice(&bytes).map_err(|_| Error::CorruptRepositoryIndex)?
            } else {
                return Err(Error::CorruptRepositoryIndex);
            }
        };

        Ok(Repository {
            path: path.as_ref().into(),
            head,
            head_time,
            index,
        })
    }

    // Reads the state of HEAD. If the file does not exist, returns None values.
    // If ctime/mtime cannot be determined, returns None.
    fn read_head(path: &Path) -> Result<(Option<SnapshotId>, Option<SystemTime>), Error> {
        let mut file = match File::open(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
            result => result,
        }?;

        let metadata = file.metadata()?;
        let time = get_last_modified(metadata);
        let mut buf = vec![];
        file.read_to_end(&mut buf)?;
        let text = std::str::from_utf8(&buf).map_err(|_| Error::CorruptHead)?;
        let snapshot = SnapshotId::from_str(text).map_err(|_| Error::CorruptHead)?;
        Ok((Some(snapshot), time))
    }

    /// The base path of the repository.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The currently extracted snapshot.
    pub fn head(&self) -> &Option<SnapshotId> {
        &self.head
    }
    pub fn index(&self) -> &RepositoryIndex {
        &self.index
    }
    /// Open the pack.
    pub fn open_pack(&self, pack: &str) -> Result<Pack, Error> {
        let pack_id = PackId::Packed(pack.to_owned());
        if !self.index().available_packs().iter().any(|p| *p == pack_id) {
            return Err(Error::PackNotFound);
        }
        Pack::open(&self.path, pack)
    }

    /// Checks-out the specified snapshot.
    ///
    /// # Arguments
    ///
    /// * `snapshot_id` - The snapshot to extract.
    pub fn extract(
        &mut self,
        snapshot_id: SnapshotId,
        opts: ExtractOptions,
    ) -> Result<ExtractResult, Error> {
        if self.head.is_some() && self.head_time.is_none() && !opts.force() {
            warn!("The OS/filesystem does not support file creation timestamps!");
            return Err(Error::DirtyWorkDir);
        }

        // Open the pack and find the snapshot specified in SnapshotId.
        let source_pack;
        let mut unpacked_index = None;
        let pack_index = if let PackId::Packed(name) = snapshot_id.pack() {
            source_pack = Some(self.open_pack(name)?);
            source_pack.as_ref().unwrap().index()
        } else {
            source_pack = None;
            unpacked_index = Some(self.unpacked_index()?);
            unpacked_index.as_ref().unwrap()
        };

        let entries = pack_index.entries_from_snapshot(snapshot_id.tag())?;

        let (new_entries, old_entries) = if !opts.reset && self.head().is_some() {
            let head = self.head().as_ref().unwrap();
            // HEAD and new snapshot packs might differ
            if snapshot_id.pack() == head.pack() {
                let head_entries = pack_index.entries_from_snapshot(head.tag())?;
                Self::compute_entry_diff(&head_entries, &entries)
            } else {
                let head_pack = if let PackId::Packed(name) = head.pack() {
                    Some(self.open_pack(name).map_err(|e| {
                        if matches!(e, Error::PackNotFound) {
                            Error::BrokenHeadRef
                        } else {
                            e
                        }
                    })?)
                } else {
                    None
                };
                let head_index = if let Some(ref head_pack) = head_pack {
                    head_pack.index()
                } else {
                    if unpacked_index.is_none() {
                        unpacked_index = Some(self.unpacked_index()?)
                    }
                    &unpacked_index.as_ref().unwrap()
                };
                let head_entries = head_index.entries_from_snapshot(head.tag()).map_err(|e| {
                    if matches!(e, PackError::SnapshotNotFound) {
                        Error::BrokenHeadRef
                    } else {
                        Error::PackError(e)
                    }
                })?;
                Self::compute_entry_diff(&head_entries, &entries)
            }
        } else {
            // Extract all, remove nothing
            (entries, vec![])
        };

        // There is no point in deleting files which will be overwritten by the extract, so
        // we identify and ignore them beforehand.
        let (updated_paths, removed_paths) = {
            let new_paths: HashSet<_> = new_entries.iter().map(|e| e.path()).collect();
            let old_paths: HashSet<_> = old_entries.iter().map(|e| e.path()).collect();
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
                path_buf.push(entry.path());
                self.check_for_changes(&path_buf)?;
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

        self.extract_entries(source_pack, &new_entries, self.path.clone(), opts)?;
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
    /// * `pack` - The pack containing the entries. If [`None`], entries will be
    ///     assumed to be unpacked.
    /// * `entries` - The list of entries to extract.
    /// * `path` - The destination path.
    /// * `verify` - Set to true to verify object checksums after extraction.
    pub fn extract_entries<P>(
        &mut self,
        pack: Option<Pack>,
        entries: &[PackEntry],
        path: P,
        opts: ExtractOptions,
    ) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        if let Some(pack) = pack {
            pack.extract(&entries, path.as_ref(), opts.verify(), opts.num_workers())
        } else {
            self.extract_from_unpacked(&entries, path.as_ref(), opts.verify())
        }
    }

    /// The name of the directory containing the elfshaker repository data.
    pub fn data_dir() -> Cow<'static, str> {
        Cow::Borrowed(REPO_DIR)
    }

    pub fn unpacked_index(&self) -> Result<PackIndex, Error> {
        Self::read_unpacked_index(&self.path)
    }

    fn read_unpacked_index(repo_dir: &Path) -> Result<PackIndex, Error> {
        let mut index_path = repo_dir.join(&*Repository::data_dir());
        index_path.push(UNPACKED_DIR);
        index_path.push(UNPACKED_INDEX_FILE);

        let index = read_or_none(index_path)?.map(|b| Pack::parse_index(&b[..]));
        Ok(match index {
            Some(i) => i?,
            _ => PackIndex::default(),
        })
    }

    fn update_unpacked_index(&mut self, index: &PackIndex) -> Result<(), Error> {
        let mut index_path = self.path.join(&*Repository::data_dir());
        index_path.push(UNPACKED_DIR);
        index_path.push(UNPACKED_INDEX_FILE);

        let mut buf = vec![];
        // This should not fail, unless there is an error in the implementation.
        rmp_serde::encode::write(&mut buf, &index).expect("Serialization failed!");

        write_file_atomic(buf.as_slice(), &self.temp_dir(), &index_path)?;
        Ok(())
    }

    pub fn snapshot<I, P>(&mut self, snapshot: &SnapshotId, files: I) -> Result<(), Error>
    where
        I: Iterator<Item = P>,
        P: AsRef<Path>,
    {
        assert!(matches!(snapshot.pack(), PackId::Unpacked));

        let files: Vec<_> = clean_file_list(self.path.as_ref(), files)?.collect();
        info!("Computing checksums for {} files...", files.len());
        // Checksum files
        let (duration, checksums) = measure_ok(|| batch::compute_checksums(&files))?;
        info!("Checksum computation took {:?}", duration);
        // Compare with index
        info!("Reading unpacked index...");
        let mut index = self.unpacked_index()?;
        if index.snapshots().iter().any(|s| s.tag() == snapshot.tag()) {
            return Err(PackError::SnapshotAlreadyExists.into());
        }
        // input_checksums maps checksums to the first file on disk with that checksum.
        let input_checksums: HashMap<&_, usize> =
            checksums.iter().enumerate().map(|(x, y)| (y, x)).collect();
        // unpacked_checksums contains the checksums in the unpacked.idx
        let unpacked_checksums: HashSet<&_> =
            index.objects().iter().map(|o| o.checksum()).collect();
        let new_checksums: Vec<&_> = input_checksums
            .keys()
            .filter(|&x| !unpacked_checksums.contains(x))
            .copied()
            .collect();
        info!("Need to store {} new objects!", new_checksums.len());
        // Compute the entries for the new snapshot and update the unpacked index
        let pack_entries = checksums
            .iter()
            .enumerate()
            .map(|(file_index, checksum)| {
                let file_path = &files[file_index];
                let file_size = fs::metadata(file_path)?.len();
                Ok(PackEntry::new(
                    file_path.as_ref(),
                    ObjectIndex::loose(*checksum, file_size),
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        index.push_snapshot(snapshot.tag(), &pack_entries)?;

        // Copy files
        let mut unpacked_dir = self.path.join(&*Repository::data_dir());
        unpacked_dir.push(UNPACKED_DIR);
        ensure_dir(&unpacked_dir)?;
        // Create temp dir
        let temp_dir = self.temp_dir();
        ensure_dir(&temp_dir)?;

        info!("Writing files to disk...");
        for checksum in &new_checksums {
            let file_index = input_checksums[checksum];
            let file_path = &files[file_index];
            let file = File::open(file_path)?;
            // Store object
            self.write_loose_object(file, &temp_dir, checksum)?;
        }

        info!("Updating unpacked index...");
        self.update_unpacked_index(&index)?;
        info!("Updating HEAD...");
        self.update_head(&snapshot)?;
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
        assert!(!matches!(pack, PackId::Unpacked));
        let pack_name = match pack {
            PackId::Packed(name) => name,
            PackId::Unpacked => unreachable!(),
        };

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
            partition_by_u64(index.objects(), opts.num_frames, |o| o.size() as u64);

        let workers_per_task = (opts.num_workers + object_partitions.len() as u32 - 1)
            / object_partitions.len() as u32;

        let task_opts = &batch::CompressionOptions {
            window_log: opts.compression_window_log,
            level: opts.compression_level,
            num_workers: workers_per_task,
        };

        // Keep count of done compression tasks
        let done_task_count = std::sync::atomic::AtomicUsize::new(0);
        let total_task_count = object_partitions.len();

        info!(
            "Starting {} compression tasks, processing {} at a time using {} worker threads for each...",
            total_task_count, opts.num_workers, workers_per_task
        );

        let tasks = object_partitions.into_iter().map(|objects| {
            let base_path = self.path.to_path_buf();
            let done_task_count_ref = &done_task_count;
            move || {
                let paths = objects
                    .iter()
                    .map(|o| build_loose_object_path(&base_path, o.checksum()))
                    .collect::<Vec<_>>();

                let mut buf = vec![];
                // Compress all the object files.
                let r =
                    batch::compress_files(&mut buf, &paths, &task_opts, &ProgressReporter::dummy())
                        .map(move |bytes| (bytes, buf));
                // Update done count.
                let done =
                    done_task_count_ref.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                // And report the change.
                reporter.checkpoint(done, Some(total_task_count - done));
                r
            }
        });

        let mut frames = vec![];
        let mut frame_bufs = vec![];

        for frame_result in run_in_parallel(tasks, opts.num_workers) {
            let (decompressed_size, compressed_buffer) = frame_result?;
            frames.push(PackFrame {
                frame_size: compressed_buffer.len() as u64,
                decompressed_size,
            });
            frame_bufs.push(compressed_buffer);
        }

        // Report that all compression tasks are done.
        reporter.checkpoint(total_task_count, Some(0));

        // Create and serialize header
        let header = PackHeader::new(frames);
        let header_bytes = rmp_serde::encode::to_vec(&header).expect("Serialization failed!");

        // And a writer to that temporary file.
        let mut pack_writer = io::BufWriter::new(File::create(&temp_path)?);
        // Write header and frames
        write_skippable_frame(&mut pack_writer, &header_bytes)?;
        for frame_buf in frame_bufs {
            pack_writer.write_all(&frame_buf)?;
        }
        pack_writer.flush()?;
        drop(pack_writer);

        // Serialize the index
        let mut index_bytes: Vec<u8> = vec![];
        rmp_serde::encode::write(&mut index_bytes, &index).expect("Serialization failed!");

        // And write it atomically to the packs/ dir.
        let index_path = {
            let mut index_path = pack_path.clone();
            index_path.set_file_name(format!("{}.{}", pack_name, PACK_INDEX_EXTENSION));
            index_path
        };
        write_file_atomic(index_bytes.as_slice(), &temp_dir, &index_path)?;

        // Finally, move tha .pack file itself to the packs/ dir.
        fs::rename(&temp_path, &pack_path)?;

        Ok(())
    }

    /// Deletes ALL unpacked snapshots and objects.
    pub fn remove_unpacked_all(&mut self) -> Result<(), Error> {
        let mut unpacked_dir = self.path().join(&*Repository::data_dir());
        unpacked_dir.push(UNPACKED_DIR);

        match fs::remove_dir_all(&unpacked_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }?;
        Ok(())
    }

    /// Updates the HEAD snapshot id.
    pub fn update_head(&mut self, snapshot_id: &SnapshotId) -> Result<(), Error> {
        let data_dir = self.path.join(&*Self::data_dir());
        let snapshot_string = format!("{}", snapshot_id);
        ensure_dir(&self.temp_dir())?;
        write_file_atomic(
            snapshot_string.as_bytes(),
            &self.temp_dir(),
            &data_dir.join(HEAD_FILE),
        )?;
        self.head = Some(snapshot_id.clone());
        Ok(())
    }

    fn extract_from_unpacked(
        &mut self,
        entries: &[PackEntry],
        path: &Path,
        verify: bool,
    ) -> Result<(), Error> {
        let mut dest_paths = vec![];
        let mut dest_path = PathBuf::new();
        for entry in entries {
            dest_path.clear();
            dest_path.push(path);
            dest_path.push(entry.path());
            dest_paths.push(dest_path.clone());
            fs::create_dir_all(dest_path.parent().unwrap())?;
            fs::copy(
                build_loose_object_path(&self.path, entry.object_index().checksum()),
                &dest_path,
            )?;
        }

        if verify {
            let checksums = batch::compute_checksums(&dest_paths)?;
            let expected_checksums = entries.iter().map(|e| e.object_index().checksum());
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
        from_entries: &[PackEntry],
        to_entries: &[PackEntry],
    ) -> (Vec<PackEntry>, Vec<PackEntry>) {
        // Create lookup based on path+checksum.
        // The reason we're not using a `HashSet<PackEntry>` here is that
        // we only care about the file path and checksum,
        // but not, for example, the offset of the object in the pack file.
        let from_lookup: HashMap<_, _> = from_entries
            .iter()
            .map(|e| ((e.path(), e.object_index().checksum()), e))
            .collect();
        let to_lookup: HashMap<_, _> = to_entries
            .iter()
            .map(|e| ((e.path(), e.object_index().checksum()), e))
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

    fn check_for_changes(&self, path: &Path) -> Result<(), Error> {
        assert!(
            self.head_time.is_some(),
            "check_for_changes cannot be called when HEAD time is unknown!"
        );
        let last_modified = fs::metadata(&path)
            // The modification date of the file is unknown, there is no other
            // option to fallback on, so we mark the directory as dirty.
            .map_err(|_| {
                warn!("Expected file {:?} to be present!", path);
                // If the file is missing that also means it has been modified!
                return Error::DirtyWorkDir;
            })
            // If the modification date of the file is unknown, there is no
            // other option to fallback on, so we mark the directory as dirty.
            .and_then(|metadata| get_last_modified(metadata).ok_or(Error::DirtyWorkDir))?;

        if self.head_time.unwrap() < last_modified {
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

    /// Atomically writes an object to the unpacked object store.
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
        let obj_path = build_loose_object_path(&self.path, checksum);
        if obj_path.exists() {
            // No need to do anything. Object writes are atomic, so if an object
            // with the same checksum alerady exists, there is no need to do anything.
            return Ok(());
        }

        // Write to disk
        fs::create_dir_all(obj_path.parent().unwrap())?;
        write_file_atomic(&mut reader, temp_dir, &obj_path)?;
        Ok(())
    }
}

fn is_pack_index(entry: &DirEntry) -> bool {
    entry.file_type().is_file()
        && match Path::new(entry.file_name())
            .file_name()
            .map(OsStr::to_str)
            .flatten()
        {
            Some(filename) => filename.ends_with(PACK_INDEX_EXTENSION),
            None => false,
        }
}

fn build_loose_object_path(repo_path: &Path, checksum: &ObjectChecksum) -> PathBuf {
    let checksum_str = hex::encode(&checksum[..]);
    let mut obj_path = repo_path.join(&*Repository::data_dir());
    // $REPO_DIR/$UNPACKED
    obj_path.push(UNPACKED_DIR);

    // $REPO_DIR/$UNPACKED/FA/
    obj_path.push(&checksum_str[..2]);
    // $REPO_DIR/$UNPACKED/FA/F0/
    obj_path.push(&checksum_str[2..4]);
    // $REPO_DIR/$UNPACKED/FA/F0/FAF0F0F0FAFAF0F0F0FAFAF0F0
    obj_path.push(&checksum_str[4..]);
    obj_path
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
        .filter(|p| !is_data_dir(&p));

    Ok(files)
}

/// Checks if the relative path is rooted at the data directory.
fn is_data_dir(p: &Path) -> bool {
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
        let path = super::build_loose_object_path("/repo".as_ref(), &checksum);
        assert_eq!(
            format!(
                "/repo/{}/{}/fa/f0/deadbeefbadc0de0faf0deadbeefbadc0de0",
                Repository::data_dir(),
                UNPACKED_DIR
            ),
            path.to_str().unwrap(),
        );
    }

    #[test]
    fn data_dir_detected() {
        let path = format!("{}", Repository::data_dir());
        assert!(is_data_dir(path.as_ref()));
    }
    #[test]
    fn data_dir_detected_as_parent() {
        let path = format!("{}/something", Repository::data_dir());
        assert!(is_data_dir(path.as_ref()));
    }
    #[test]
    fn data_dir_not_detected_incorrectly() {
        let path = "some/path/something";
        assert!(!is_data_dir(path.as_ref()));
    }

    #[test]
    fn compute_entry_diff_finds_updates() {
        let path = "/path/to/A";
        let old_checksum = [0; 20];
        let new_checksum = [1; 20];
        let old_entries = [PackEntry::new(
            path.as_ref(),
            ObjectIndex::loose(old_checksum, 1),
        )];
        let new_entries = [PackEntry::new(
            path.as_ref(),
            ObjectIndex::loose(new_checksum, 1),
        )];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(1, added.len());
        assert_eq!(path, added[0].path());
        assert_eq!(1, removed.len());
        assert_eq!(path, removed[0].path());
    }

    #[test]
    fn compute_entry_diff_finds_update_of_duplicated() {
        let path_a = "/path/to/A";
        let path_a_old_checksum = [0; 20];
        let path_b = "/path/to/B";
        let path_b_old_checksum = [0; 20];
        let path_a_new_checksum = [1; 20];
        let old_entries = [
            PackEntry::new(path_a.as_ref(), ObjectIndex::loose(path_a_old_checksum, 1)),
            PackEntry::new(path_b.as_ref(), ObjectIndex::loose(path_b_old_checksum, 1)),
        ];
        let new_entries = [PackEntry::new(
            path_a.as_ref(),
            ObjectIndex::loose(path_a_new_checksum, 1),
        )];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(1, added.len());
        assert_eq!(path_a, added[0].path());
        assert_eq!(2, removed.len());
        assert!(removed.iter().any(|e| path_a == e.path()));
        assert!(removed.iter().any(|e| path_b == e.path()));
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
            PackEntry::new(path_a.as_ref(), ObjectIndex::loose(path_a_old_checksum, 1)),
            PackEntry::new(path_b.as_ref(), ObjectIndex::loose(path_b_old_checksum, 1)),
        ];
        let new_entries = [
            PackEntry::new(path_a.as_ref(), ObjectIndex::loose(path_a_new_checksum, 1)),
            PackEntry::new(path_b.as_ref(), ObjectIndex::loose(path_b_new_checksum, 1)),
        ];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(2, added.len());
        assert!(added
            .iter()
            .any(|e| path_a == e.path() && path_a_new_checksum == *e.object_index().checksum()));
        assert!(added
            .iter()
            .any(|e| path_b == e.path() && path_b_new_checksum == *e.object_index().checksum()));
        assert_eq!(2, removed.len());
        assert!(removed
            .iter()
            .any(|e| path_a == e.path() && path_a_old_checksum == *e.object_index().checksum()));
        assert!(removed
            .iter()
            .any(|e| path_b == e.path() && path_b_old_checksum == *e.object_index().checksum()));
    }

    #[test]
    fn extract_requires_force_if_no_timestamps() {
        let mut repo = Repository {
            head: Some(SnapshotId::from_str("pack/snapshot").unwrap()),
            head_time: None,
            index: RepositoryIndex::default(),
            path: "/some/path".into(),
        };
        let snapshot = SnapshotId::from_str("pack/snapshot-2").unwrap();
        let err1 = repo
            .extract(snapshot.clone(), ExtractOptions::default())
            .unwrap_err();
        let err2 = repo
            .extract(
                snapshot.clone(),
                ExtractOptions {
                    force: true,
                    ..Default::default()
                },
            )
            .unwrap_err();

        assert!(matches!(err1, Error::DirtyWorkDir));
        assert!(!matches!(err2, Error::DirtyWorkDir));
    }

    #[test]
    fn extract_does_not_require_force_or_timestamp_when_none_head() {
        let mut repo = Repository {
            head: None,
            head_time: None,
            index: RepositoryIndex::default(),
            path: "/some/path".into(),
        };
        let snapshot = SnapshotId::from_str("pack/snapshot").unwrap();
        let err = repo
            .extract(
                snapshot.clone(),
                ExtractOptions {
                    force: true,
                    ..Default::default()
                },
            )
            .unwrap_err();

        assert!(!matches!(err, Error::DirtyWorkDir));
    }
}
