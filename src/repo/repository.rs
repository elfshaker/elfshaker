//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use super::{constants::*, pack::IdError};

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::SystemTime,
};

use crypto::digest::Digest;
use crypto::sha1::Sha1;
use log::{error, info, warn};
use walkdir::WalkDir;

use super::algo::run_in_parallel;
use super::constants::REPO_DIR;
use super::error::Error;
use super::fs::{
    create_file, create_temp_path, ensure_dir, get_last_modified, open_file, write_file_atomic,
    EmptyDirectoryCleanupQueue,
};
use super::pack::{write_skippable_frame, Pack, PackFrame, PackHeader, PackId, SnapshotId};
use super::remote;
use crate::packidx::{FileEntry, ObjectChecksum, PackError, PackIndex};
use crate::progress::ProgressReporter;
use crate::{
    batch,
    packidx::{ObjectMetadata, LOOSE_OBJECT_OFFSET},
};

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
    /// The working directory for this [`Repository`].
    path: PathBuf,
    /// The path for the elfshaker repository.
    data_dir: PathBuf,
    /// Since there might be multiple long running sub-tasks invoked in each
    /// macro tasks (e.g. extract snapshot includes fetching the .esi,
    /// fetching individual pack, etc.), it is useful to use a "factory",
    /// instead of argument passing for the [`ProgressReporter`].
    progress_reporter_factory: Box<dyn Fn(&str) -> ProgressReporter<'static> + Send + Sync>,
}

impl Repository {
    /// Opens the specified repository.
    ///
    /// # Arguments
    ///
    /// * `path` - The working directory for this [`Repository`].
    pub fn open<P>(path: P) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        Self::open_with_data_dir(
            std::fs::canonicalize(path)?,
            std::fs::canonicalize(REPO_DIR)?,
        )
    }

    /// Opens the specified repository.
    ///
    /// # Arguments
    ///
    /// * `path` - The working directory for this [`Repository`].
    /// * `data_dir` - The path to the elfshaker repository.
    pub fn open_with_data_dir<P1, P2>(path: P1, data_dir: P2) -> Result<Self, Error>
    where
        P1: AsRef<Path>,
        P2: AsRef<Path>,
    {
        let path = path.as_ref().to_path_buf();
        let data_dir = data_dir.as_ref().canonicalize()?;

        if !Path::exists(&data_dir) {
            error!(
                "The directory {:?} is not an elfshaker repository!",
                data_dir.parent().unwrap_or_else(|| Path::new("/"))
            );
            return Err(Error::RepositoryNotFound);
        }

        Ok(Repository {
            path,
            data_dir,
            progress_reporter_factory: Box::new(|_| ProgressReporter::dummy()),
        })
    }

    // Reads the state of HEAD. If the file does not exist, returns None values.
    // If ctime/mtime cannot be determined, returns None.
    pub fn read_head(&self) -> Result<(Option<SnapshotId>, Option<SystemTime>), Error> {
        let path = self.data_dir().join(HEAD_FILE);

        let (head, mtime) = match open_file(path) {
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
        Pack::open(self.data_dir(), pack)
    }

    pub fn packs(&self) -> Result<Vec<PackId>, Error> {
        let root = self.data_dir().join(PACKS_DIR);
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
                .data_dir()
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
                self.load_index_snapshots(&pack_id)
                    .map(|idx| idx.iter().any(|x| x == snapshot).then(|| pack_id))
                    .transpose()
            })
            .collect::<Result<Vec<PackId>, Error>>()?;

        match packs.len() {
            0 => Err(Error::PackError(PackError::SnapshotNotFound(
                snapshot.to_owned(),
            ))),
            1 => Ok(packs.into_iter().next().unwrap()),
            _ => self.disambiguate_snapshot(&packs, snapshot),
        }
    }

    pub fn is_pack(&self, pack_id: &str) -> Result<Option<PackId>, IdError> {
        let pack_index_path = self
            .data_dir()
            .join(PACKS_DIR)
            .join(pack_id)
            .with_extension(PACK_INDEX_EXTENSION);
        pack_index_path
            .exists()
            .then(|| PackId::from_str(pack_id))
            .transpose()
    }

    pub fn is_pack_loose(&self, pack_id: &PackId) -> bool {
        let PackId::Pack(pack_name) = pack_id;

        // The pack is loose if the .pack.idx is in the loose packs directory
        if !pack_name.starts_with(&(LOOSE_DIR.to_owned() + "/")) {
            return false;
        }

        let pack_index_path = self
            .data_dir()
            .join(PACKS_DIR)
            .join(pack_name)
            .with_extension(PACK_INDEX_EXTENSION);

        pack_index_path.exists()
    }

    pub fn load_index(&self, pack_id: &PackId) -> Result<PackIndex, Error> {
        let pack_index_path = match pack_id {
            PackId::Pack(name) => self
                .data_dir()
                .join(PACKS_DIR)
                .join(name)
                .with_extension(PACK_INDEX_EXTENSION),
        };
        info!("Load index {} {}", pack_id, pack_index_path.display());
        Ok(PackIndex::load(pack_index_path)?)
    }

    pub fn load_index_snapshots(&self, pack_id: &PackId) -> Result<Vec<String>, Error> {
        let pack_index_path = match pack_id {
            PackId::Pack(name) => self
                .data_dir()
                .join(PACKS_DIR)
                .join(name)
                .with_extension(PACK_INDEX_EXTENSION),
        };
        Ok(PackIndex::load_only_snapshots(pack_index_path)?)
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

        let entries = source_index
            .resolve_snapshot(snapshot_id.tag())
            .expect("failed to resolve snapshot"); // TODO: Temporary.
        let entries = source_index.entries_from_handles(entries.iter())?;

        let (new_entries, old_entries) = if opts.reset || head.is_none() {
            // Extract all, remove nothing
            (entries, vec![])
        } else if let Some(head) = head {
            // HEAD and new snapshot packs might differ
            if snapshot_id.pack() == head.pack() {
                let head_entries = source_index.entries_from_handles(
                    source_index
                        .resolve_snapshot(head.tag())
                        .expect("failed to resolve snapshot")
                        .iter(), // TODO: Temporary.
                )?;
                Self::compute_entry_diff(&head_entries, &entries)
            } else {
                let head_index = self.load_index(head.pack())?;
                let head_entries = head_index
                    .entries_from_handles(
                        head_index
                            .resolve_snapshot(head.tag())
                            .expect("failed to resolve snapshot")
                            .iter(), // TODO: Temporary.
                    )
                    .map_err(|e| {
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
            let mut removed: Vec<_> = old_paths.difference(&new_paths).copied().collect();
            // The reason we sort this list is that EmptyDirectoryCleanupQueue
            // uses some heuristics which make the removal of empty directories
            // more efficient in this case.
            removed.sort();
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

        let mut dir_queue = EmptyDirectoryCleanupQueue::new();

        for path in &removed_paths {
            path_buf.clear();
            path_buf.push(&self.path);
            path_buf.push(path);
            // Delete the file
            match fs::remove_file(&path_buf) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            }?;

            dir_queue.enqueue(path_buf.parent().unwrap(), self.path.clone())?;
        }

        // Process the enqueued directories.
        dir_queue.process()?;

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
        if self.is_pack_loose(pack_id) {
            self.copy_loose_entries(entries, path.as_ref(), opts.verify())
        } else if let Ok(pack) = self.open_pack(pack_id) {
            pack.extract_entries(entries, path.as_ref(), opts.verify(), opts.num_workers())
        } else {
            info!("Pack not available locally! Fetching from remote...");
            self.update_remote_pack(pack_id)?;
            self.open_pack(pack_id).and_then(|pack| {
                pack.extract_entries(entries, path.as_ref(), opts.verify(), opts.num_workers())
            })
        }
    }

    fn update_remote_pack(&self, pack: &PackId) -> Result<(), Error> {
        let remotes_dir = self.data_dir().join(REMOTES_DIR);
        let remotes = remote::load_remotes(&remotes_dir)?;

        let pack = match pack {
            PackId::Pack(p) => p.rsplit_once('/').map(|x| x.1).unwrap_or(p),
        };
        let pack_file_name = pack.to_string() + "." + PACK_EXTENSION;

        let agent = ureq::AgentBuilder::new().build();
        let reporter = (self.progress_reporter_factory)(&format!("Fetching {}", pack_file_name));

        for remote in remotes {
            if let Some(remote_pack) = remote.find_pack(pack) {
                info!("Found {} in {}. Updating...", pack, remote);
                let pack_path = self
                    .data_dir()
                    .join(PACKS_DIR)
                    .join(remote.name().unwrap())
                    .join(pack_file_name);

                // Immediately shows some progress, without waiting for the
                // HTTP response for the pack.
                reporter.checkpoint(0, Some(1));
                remote::update_remote_pack(&agent, remote_pack, &pack_path, &reporter)?;
                return Ok(());
            }
        }

        Err(Error::PackNotFound(pack.into()))
    }

    /// The name of the directory containing the elfshaker repository data.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn create_snapshot<I, P>(&mut self, snapshot: &SnapshotId, files: I) -> Result<(), Error>
    where
        I: Iterator<Item = P>,
        P: AsRef<Path>,
    {
        let files =
            clean_file_list(self.path.as_ref(), self.data_dir(), files)?.collect::<Vec<_>>();
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
                checksum,
                ObjectMetadata {
                    offset: LOOSE_OBJECT_OFFSET,
                    size: buf.len() as u64,
                },
            ))
        })
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;

        let mut index = PackIndex::new();
        index.push_snapshot(snapshot.tag().to_owned(), pack_entries)?;

        let loose_path = self.data_dir().join(PACKS_DIR).join(LOOSE_DIR);
        ensure_dir(&loose_path)?;

        index.save(
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
        index: PackIndex,
        opts: &PackOptions,
        reporter: &ProgressReporter,
    ) -> Result<(), Error> {
        let PackId::Pack(pack_name) = pack;

        // Construct output file path.
        let pack_path = {
            let mut pack_path = self.data_dir().join(PACKS_DIR);
            ensure_dir(&pack_path)?;
            pack_path.push(format!("{}.{}", pack_name, PACK_EXTENSION));
            pack_path
        };

        // Create a temporary file to use during compression.
        let temp_dir = self.temp_dir();
        ensure_dir(&temp_dir)?;
        let temp_path = create_temp_path(&temp_dir);

        let (index, ordering) = index.compute_object_offsets_and_ordering();

        // Gather a list of all objects to compress.
        let object_partitions = index.objects_partitioned_by_size(opts.num_frames, &ordering);

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
                let object_readers = objects.iter().map(|&handle| {
                    // TODO: Method of obtaining readers from packs? Or we can
                    // just assume packs first get unpacked.
                    Ok(Box::new(open_file(
                        self.loose_object_path(index.handle_to_checksum(handle)),
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
        let mut pack_writer = io::BufWriter::new(create_file(&temp_path)?);
        // Write header and frames.
        write_skippable_frame(&mut pack_writer, &header_bytes)?;
        for frame_buf in frame_bufs {
            pack_writer.write_all(&frame_buf)?;
        }
        pack_writer.flush()?;
        drop(pack_writer);

        let index_path = pack_path.with_extension(PACK_INDEX_EXTENSION);
        info!("Write index: {}", index_path.display());
        index.save(index_path)?;

        // Finally, move the .pack file itself to the packs/ dir.
        fs::rename(&temp_path, &pack_path)?;

        Ok(())
    }

    /// Deletes ALL loose snapshots and objects.
    pub fn remove_loose_all(&mut self) -> Result<(), Error> {
        let loose_dir = self.data_dir().join(LOOSE_DIR);

        match fs::remove_dir_all(&loose_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }?;
        Ok(())
    }

    /// Updates the HEAD snapshot id.
    pub fn update_head(&mut self, snapshot_id: &SnapshotId) -> Result<(), Error> {
        let snapshot_string = format!("{}\n", snapshot_id);
        ensure_dir(&self.temp_dir())?;
        write_file_atomic(
            snapshot_string.as_bytes(),
            &self.temp_dir(),
            &self.data_dir().join(HEAD_FILE),
        )?;
        Ok(())
    }

    pub fn add_remote(&mut self, name: &str, url: &str) -> Result<(), Error> {
        let mut path = self.data_dir().join(REMOTES_DIR);
        fs::create_dir_all(&path)?;
        path.push(name);
        path.set_extension("esi");

        let agent = ureq::AgentBuilder::new().build();
        let reporter = (self.progress_reporter_factory)(&format!(
            "Fetching remote repository index from {}",
            name
        ));

        reporter.checkpoint_with_detail(0, Some(1), url.to_owned());
        remote::fetch_remote(&agent, url, &path)?;
        reporter.checkpoint_with_detail(1, Some(0), url.to_owned());

        Ok(())
    }

    pub fn set_progress_reporter<F>(&mut self, factory: F)
    where
        F: 'static + Fn(&str) -> ProgressReporter<'static> + Send + Sync,
    {
        self.progress_reporter_factory = Box::new(factory);
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
            let object_path = self.loose_object_path(&entry.checksum);
            fs::copy(&object_path, &dest_path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "couldn't copy {} to {}",
                        object_path.display(),
                        dest_path.display()
                    ),
                )
            })?;
        }

        if verify {
            let checksums = batch::compute_checksums(&dest_paths)?;
            let expected_checksums = entries.iter().map(|e| &e.checksum);
            for (expected, actual) in expected_checksums.zip(checksums) {
                if *expected != actual {
                    return Err(PackError::ChecksumMismatch(*expected, actual).into());
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
            .map(|e| ((&e.path, &e.checksum), e))
            .collect();
        let to_lookup: HashMap<_, _> = to_entries
            .iter()
            .map(|e| ((&e.path, &e.checksum), e))
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
        self.data_dir().join(TEMP_DIR)
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
        // $REPO_DIR/$LOOSE
        let mut obj_path = self.data_dir().join(LOOSE_DIR);
        // $REPO_DIR/$LOOSE/FA/
        obj_path.push(&checksum_str[..2]);
        // $REPO_DIR/$LOOSE/FA/F0/
        obj_path.push(&checksum_str[2..4]);
        // $REPO_DIR/$LOOSE/FA/F0/FAF0F0F0FAFAF0F0F0FAFAF0F0
        obj_path.push(&checksum_str[4..]);
        obj_path
    }

    /// Updates all remotes and their associated .pack.idx files.
    pub fn update_remotes(&self) -> Result<(), Error> {
        let remotes_dir = self.data_dir().join(REMOTES_DIR);
        let remotes = remote::load_remotes(&remotes_dir)?;

        let agent = ureq::AgentBuilder::new().build();
        let reporter = (self.progress_reporter_factory)("Fetching pack indexes from origin");
        // Display the progress bar immediately.
        reporter.checkpoint(0, Some(1));

        for remote in remotes {
            // .path() is Some, because load_remotes guarantees it
            let remote_name = remote.path().unwrap().file_stem().unwrap();
            let mut remote_packs_dir = self.data_dir().join(PACKS_DIR);
            remote_packs_dir.push(remote_name);

            info!("Updating {}...", remote);
            let remote = remote::update_remote(&agent, &remote)?;
            fs::create_dir_all(&remote_packs_dir)?;
            remote::update_remote_pack_indexes(&agent, &remote, &remote_packs_dir, &reporter)?;
        }
        Ok(())
    }

    /// Checks whether the snapshots have the same content checksum.
    fn are_snapshots_equal(&self, packs: &[PackId], snapshot: &str) -> Result<bool, Error> {
        let mut snapshot_checksums = packs.iter().map(|pack| {
            self.load_index(pack)
                .map(|packidx| packidx.compute_snapshot_checksum(snapshot))
                .expect("failed to resolve snapshot")
        });

        let first = snapshot_checksums.next().expect("At least 1 pack expected");
        Ok(snapshot_checksums.all(|checksum| checksum == first))
    }

    /// Takes a set of packs which contain the given snapshot name. If the
    /// snapshot in each pack is identical according to content checksum,
    /// return an arbitrary pack, preferring a loose one if available.
    fn disambiguate_snapshot(&self, packs: &[PackId], snapshot: &str) -> Result<PackId, Error> {
        info!(
            "Snapshot exists in multiple packs ({:?}), verifying that checksums match...",
            packs
        );
        if self.are_snapshots_equal(packs, snapshot)? {
            // The snapshots have the same checksums, so we could use either one
            // but we prefer picking a loose one over a packed for performance.
            let loose = packs.iter().find(|pack| self.is_pack_loose(pack));
            let selected = loose.unwrap_or(&packs[0]).clone();
            info!(
                "Snapshot exists in multiple packs ({:?}), {:?} is selected",
                packs, selected
            );
            Ok(selected)
        } else {
            Err(Error::AmbiguousSnapshotMatch(
                snapshot.to_owned(),
                packs.to_vec(),
            ))
        }
    }
}

/// Cleans the list of file paths relative to the repository root,
/// and skips any paths pointing into the repository data directory.
fn clean_file_list<P>(
    repo_dir: &Path,
    data_dir: &Path,
    files: impl Iterator<Item = P>,
) -> io::Result<impl Iterator<Item = PathBuf>>
where
    P: AsRef<Path>,
{
    let data_dir_is_subdir = data_dir.starts_with(repo_dir);
    let stripped_data_dir = data_dir
        .components()
        .skip(repo_dir.components().count())
        .collect::<PathBuf>();

    let files = files
        .flat_map(|p| {
            if p.as_ref().is_relative() {
                Ok(p)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Expected a relative path, got {:?}!", p.as_ref()),
                ))
            }
        })
        .map(|p| {
            Ok(p.as_ref()
                .canonicalize()?
                .components()
                .skip(repo_dir.components().count())
                .collect::<PathBuf>())
        })
        .filter(|p| {
            !data_dir_is_subdir
                || match p {
                    Ok(p) => !p.starts_with(&stripped_data_dir),
                    _ => false,
                }
        })
        .collect::<io::Result<Vec<PathBuf>>>()?
        .into_iter();

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    static EXAMPLE_MD: ObjectMetadata = ObjectMetadata {
        size: 1,
        offset: LOOSE_OBJECT_OFFSET,
    };

    #[test]
    fn building_loose_object_paths_works() {
        let checksum = [
            0xFA, 0xF0, 0xDE, 0xAD, 0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0, 0xFA, 0xF0, 0xDE, 0xAD,
            0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0,
        ];
        let repo = Repository {
            path: "/repo".into(),
            data_dir: "/repo/elfshaker_data".into(),
            progress_reporter_factory: Box::new(|_| ProgressReporter::dummy()),
        };
        let path = repo.loose_object_path(&checksum);
        assert_eq!(
            format!(
                "{}/{}/fa/f0/deadbeefbadc0de0faf0deadbeefbadc0de0",
                repo.data_dir().to_string_lossy(),
                LOOSE_DIR
            ),
            path.to_str().unwrap(),
        );
    }

    #[test]
    fn compute_entry_diff_finds_updates() {
        let path = "/path/to/A";
        let old_checksum = [0; 20];
        let new_checksum = [1; 20];
        let old_entries = [FileEntry::new(path.into(), old_checksum, EXAMPLE_MD)];
        let new_entries = [FileEntry::new(path.into(), new_checksum, EXAMPLE_MD)];
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
            FileEntry::new(path_a.into(), path_a_old_checksum, EXAMPLE_MD),
            FileEntry::new(path_b.into(), path_b_old_checksum, EXAMPLE_MD),
        ];
        let new_entries = [FileEntry::new(
            path_a.into(),
            path_a_new_checksum,
            EXAMPLE_MD,
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
            FileEntry::new(path_a.into(), path_a_old_checksum, EXAMPLE_MD),
            FileEntry::new(path_b.into(), path_b_old_checksum, EXAMPLE_MD),
        ];
        let new_entries = [
            FileEntry::new(path_a.into(), path_a_new_checksum, EXAMPLE_MD),
            FileEntry::new(path_b.into(), path_b_new_checksum, EXAMPLE_MD),
        ];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(2, added.len());
        assert!(added
            .iter()
            .any(|e| path_a == e.path && path_a_new_checksum == e.checksum));
        assert!(added
            .iter()
            .any(|e| path_b == e.path && path_b_new_checksum == e.checksum));
        assert_eq!(2, removed.len());
        assert!(removed
            .iter()
            .any(|e| path_a == e.path && path_a_old_checksum == e.checksum));
        assert!(removed
            .iter()
            .any(|e| path_b == e.path && path_b_old_checksum == e.checksum));
    }
}
