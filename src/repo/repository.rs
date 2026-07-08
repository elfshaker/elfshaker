//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use super::{constants::*, pack::IdError};

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use fs2::FileExt;
use log::{error, info, warn};
use walkdir::WalkDir;

use super::algo::run_in_parallel;
use super::constants::REPO_DIR;
use super::error::Error;
use super::fs::{
    create_file, create_temp_path, ensure_dir, get_last_modified, open_file, remove_empty_dirs,
    write_file_atomic, write_file_atomic_or_existing, EmptyDirectoryCleanupQueue,
    MetadataFileModeExt,
};
use super::pack::{
    append_path_suffix, write_skippable_frame, Pack, PackFrame, PackHeader, PackId, SnapshotId,
};
use super::remote;
use super::symlink;
use crate::packidx::{EntryMetadata, FileEntry, ObjectChecksum, PackError, PackIndex, PackPath};
#[cfg(test)]
use crate::packidx::{SymlinkTargetEncoding, SymlinkTargetKind};
use crate::progress::ProgressReporter;
use crate::{
    batch,
    packidx::{ObjectMetadata, LOOSE_OBJECT_OFFSET},
};
use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;

fn logical_pack_id_path(path: &Path) -> Result<String, Error> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_owned()
                .into_string()
                .map_err(Error::Utf8Error)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

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

#[derive(Clone, Debug)]
pub struct PackDiskStats {
    pub len: u64,
}

#[derive(Clone, Debug)]
pub struct ObjectDiskStats {
    pub len: u64,
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
    /// The repository mutex file. This file is locked and unlocked
    /// when the repository instance is created/destroyed.
    /// None represents read-only repository.
    lock_file: Option<fs::File>,
    is_locked_exclusively: Mutex<bool>,
}

fn restore_shared_lock_on_error(
    lock_file: &fs::File,
    lock_result: io::Result<()>,
) -> io::Result<()> {
    let Err(lock_error) = lock_result else {
        return Ok(());
    };

    FileExt::lock_shared(lock_file).map_err(|restore_error| {
        io::Error::new(
            restore_error.kind(),
            format!(
                "failed to acquire exclusive repository lock ({lock_error}); \
                 failed to restore shared repository lock ({restore_error})"
            ),
        )
    })?;
    Err(lock_error)
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
        let data_dir = data_dir.as_ref();

        if !Path::exists(data_dir) {
            return Err(Error::RepositoryNotFound(data_dir.to_path_buf()));
        }
        let data_dir = data_dir.canonicalize().expect("canonicalise failed");

        let mut open_options = fs::OpenOptions::new();
        let open_options = open_options.create(true).write(true).read(true);

        let lock_file = match open_options.open(data_dir.join("mutex")) {
            Ok(lock_file) => {
                if let Err(e) = fs2::FileExt::try_lock_shared(&lock_file) {
                    if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                        warn!("Blocking until the repository mutex is unlocked...");
                        FileExt::lock_shared(&lock_file)?;
                    } else {
                        return Err(e.into());
                    }
                }

                Some(lock_file)
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => None,
            // Replace with ReadOnlyFilesystem once stabilized (io_error_more #86442).
            Err(e) if e.kind().to_string() == "read-only filesystem or storage medium" => None,
            Err(e) => Err(e)?,
        };

        Ok(Repository {
            path,
            data_dir,
            progress_reporter_factory: Box::new(|_| ProgressReporter::dummy()),
            lock_file,
            is_locked_exclusively: Mutex::new(false),
        })
    }

    /// Acquires an exclusive lock for the remaining lifetime of this repository.
    ///
    /// Multi-step mutations must acquire this lock before reading any repository
    /// state used to decide what to modify.
    pub fn lock_exclusive(&self) -> io::Result<()> {
        let mut is_locked_exclusively = self
            .is_locked_exclusively
            .lock()
            .map_err(|_| io::Error::other("repository lock state poisoned"))?;
        if *is_locked_exclusively {
            return Ok(());
        }
        let Some(lock_file) = &self.lock_file else {
            error!("Modifying readonly repository is not allowed.");
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "modifying readonly repository is not allowed",
            ));
        };

        FileExt::unlock(lock_file)?;
        let lock_result = if let Err(e) = FileExt::try_lock_exclusive(lock_file) {
            if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                warn!("Blocking until the repository mutex is unlocked...");
                FileExt::lock_exclusive(lock_file)
            } else {
                Err(e)
            }
        } else {
            Ok(())
        };
        restore_shared_lock_on_error(lock_file, lock_result)?;
        *is_locked_exclusively = true;
        Ok(())
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
                let text = text.strip_suffix('\n').unwrap_or(text);
                let text = text.strip_suffix('\r').unwrap_or(text);
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
        let mut result = vec![];
        for entry in WalkDir::new(&root) {
            let entry = entry.map_err(Error::WalkDirError)?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            let relative = logical_pack_id_path(
                path.strip_prefix(&root)
                    .expect("walked paths remain under their root"),
            )?;
            let Some(discovered) = PackId::from_index_path(relative) else {
                continue;
            };

            let pack_id = match &discovered {
                PackId::Pack(name) if name.starts_with(&(LOOSE_DIR.to_owned() + "/")) => {
                    match PackIndex::load_only_snapshots(&path)?.as_slice() {
                        [tag] => PackId::for_loose_snapshot(tag)?,
                        _ => return Err(Error::CorruptPackIndex),
                    }
                }
                _ => discovered,
            };
            result.push(pack_id);
        }
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
        let pack_index_path = self.get_pack_index_path(pack_id);
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
                    .map(|idx| idx.iter().any(|x| x == snapshot).then_some(pack_id))
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
        let pack_id = PackId::from_str(pack_id)?;
        let pack_index_path = append_path_suffix(
            self.data_dir().join(PACKS_DIR).join(pack_id.to_string()),
            DOT_PACK_INDEX_EXTENSION,
        );
        Ok(pack_index_path.exists().then_some(pack_id))
    }

    pub fn is_pack_loose(&self, pack_id: &PackId) -> bool {
        if pack_id.validate().is_err() {
            return false;
        }
        let PackId::Pack(pack_name) = pack_id;

        // The pack is loose if the .pack.idx is in the loose packs directory
        if !pack_name.starts_with(&(LOOSE_DIR.to_owned() + "/")) {
            return false;
        }

        let pack_index_path = append_path_suffix(
            self.data_dir().join(PACKS_DIR).join(pack_name),
            DOT_PACK_INDEX_EXTENSION,
        );

        pack_index_path.exists()
    }

    pub fn load_index(&self, pack_id: &PackId) -> Result<PackIndex, Error> {
        pack_id.validate()?;
        let pack_index_path = self.get_pack_index_path(pack_id);
        info!("Load index {} {}", pack_id, pack_index_path.display());
        Ok(PackIndex::load(pack_index_path)?)
    }

    pub fn load_index_snapshots(&self, pack_id: &PackId) -> Result<Vec<String>, Error> {
        pack_id.validate()?;
        let pack_index_path = self.get_pack_index_path(pack_id);
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
        preflight_path_policy(
            entries.iter().map(|entry| &entry.path),
            TargetPlatform::current(),
        )?;

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
        let removable_ancestors = removed_paths
            .iter()
            .map(|path| {
                path.to_native()
                    .map_err(|error| PackError::PathNotRepresentable((*path).clone(), error))
            })
            .collect::<Result<HashSet<_>, _>>()?;
        // Logical compatibility concerns the complete target snapshot. Filesystem
        // traversal concerns only paths this checkout will mutate.
        preflight_destination_ancestors(
            &self.path,
            new_entries
                .iter()
                .map(|entry| &entry.path)
                .chain(removed_paths.iter().copied()),
            &removable_ancestors,
        )?;
        symlink::probe(
            &self.path,
            new_entries.iter().filter_map(|entry| match entry.metadata {
                EntryMetadata::Symlink { target_kind, .. } => Some(target_kind),
                EntryMetadata::Regular { .. } => None,
            }),
        )?;
        self.preflight_symlink_targets(snapshot_id.pack(), &new_entries, opts.num_workers())?;
        let removed_paths = removed_paths
            .into_iter()
            .map(|path| {
                path.to_native()
                    .map_err(|error| PackError::PathNotRepresentable(path.clone(), error).into())
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let mut path_buf = PathBuf::new();
        if !opts.force() {
            for entry in &old_entries {
                path_buf.clear();
                path_buf.push(&self.path);
                path_buf.push(
                    entry.path.to_native().map_err(|error| {
                        PackError::PathNotRepresentable(entry.path.clone(), error)
                    })?,
                );
                self.check_entry_unchanged_since(entry, head_time.unwrap(), &path_buf)?;
            }
        }

        let mut dir_queue = EmptyDirectoryCleanupQueue::new();

        for path in &removed_paths {
            path_buf.clear();
            path_buf.push(&self.path);
            path_buf.push(path);
            // Delete the file
            let is_symlink = fs::symlink_metadata(&path_buf)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false);
            match if is_symlink {
                symlink::remove_leaf(&path_buf)
            } else {
                fs::remove_file(&path_buf)
            } {
                // Nothing to remove.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                // It's a directory, delete recursively.
                Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => {
                    fs::remove_dir_all(&path_buf)
                }
                r => r,
            }?;

            dir_queue.enqueue(path_buf.parent().unwrap(), self.path.clone())?;
        }

        // Process the enqueued directories.
        dir_queue.process()?;

        let repository_path = self.path.clone();
        self.extract_entries_preflighted(snapshot_id.pack(), &new_entries, &repository_path, opts)?;
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
        let path = path.as_ref();
        preflight_path_policy(
            entries.iter().map(|entry| &entry.path),
            TargetPlatform::current(),
        )?;
        preflight_destination_ancestors(
            path,
            entries.iter().map(|entry| &entry.path),
            &HashSet::new(),
        )?;
        symlink::probe(
            path,
            entries.iter().filter_map(|entry| match entry.metadata {
                EntryMetadata::Symlink { target_kind, .. } => Some(target_kind),
                EntryMetadata::Regular { .. } => None,
            }),
        )?;
        self.preflight_symlink_targets(pack_id, entries, opts.num_workers())?;
        self.extract_entries_preflighted(pack_id, entries, path, opts)
    }

    pub fn read_entry_objects(
        &mut self,
        pack_id: &PackId,
        entries: &[FileEntry],
        num_workers: u32,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let objects = if self.is_pack_loose(pack_id) {
            entries
                .iter()
                .cloned()
                .map(|entry| {
                    if entry.object_metadata.size == 0
                        && matches!(entry.metadata, EntryMetadata::Regular { .. })
                    {
                        Ok((entry, Vec::new()))
                    } else {
                        fs::read(self.loose_object_path(&entry.checksum))
                            .map(|object| (entry, object))
                    }
                })
                .collect::<io::Result<Vec<_>>>()?
        } else {
            let pack = match self.open_pack(pack_id) {
                Ok(pack) => pack,
                Err(_) => {
                    self.update_remote_pack(pack_id)?;
                    self.open_pack(pack_id)?
                }
            };
            pack.read_entry_objects(entries, num_workers)?
        };

        let objects = objects
            .into_iter()
            .map(|(entry, object)| {
                if entry.object_metadata.size != 0 || entry.metadata.is_symlink() {
                    let checksum: ObjectChecksum = Sha1::digest(&object).into();
                    if checksum != entry.checksum {
                        return Err(PackError::ChecksumMismatch(entry.checksum, checksum));
                    }
                }
                Ok(((entry.path, entry.checksum, entry.metadata), object))
            })
            .collect::<Result<HashMap<_, _>, PackError>>()?;

        entries
            .iter()
            .map(|entry| {
                objects
                    .get(&(entry.path.clone(), entry.checksum, entry.metadata))
                    .cloned()
                    .ok_or_else(|| PackError::ObjectNotFound.into())
            })
            .collect()
    }

    fn extract_entries_preflighted(
        &mut self,
        pack_id: &PackId,
        entries: &[FileEntry],
        path: &Path,
        opts: ExtractOptions,
    ) -> Result<(), Error> {
        if self.is_pack_loose(pack_id) {
            self.copy_loose_entries(entries, path, opts.verify(), opts.num_workers())
        } else if let Ok(pack) = self.open_pack(pack_id) {
            pack.extract_entries(entries, path, opts.verify(), opts.num_workers())
        } else {
            info!("Pack not available locally! Fetching from remote...");
            self.update_remote_pack(pack_id)?;
            self.open_pack(pack_id).and_then(|pack| {
                pack.extract_entries(entries, path, opts.verify(), opts.num_workers())
            })
        }
    }

    fn preflight_symlink_targets(
        &mut self,
        pack_id: &PackId,
        entries: &[FileEntry],
        num_workers: u32,
    ) -> Result<(), Error> {
        let symlinks = entries
            .iter()
            .filter(|entry| entry.metadata.is_symlink())
            .cloned()
            .collect::<Vec<_>>();
        if symlinks.is_empty() {
            return Ok(());
        }

        let targets = if self.is_pack_loose(pack_id) {
            symlinks
                .into_iter()
                .map(|entry| {
                    fs::read(self.loose_object_path(&entry.checksum)).map(|target| (entry, target))
                })
                .collect::<io::Result<Vec<_>>>()?
        } else {
            let pack = match self.open_pack(pack_id) {
                Ok(pack) => pack,
                Err(_) => {
                    self.update_remote_pack(pack_id)?;
                    self.open_pack(pack_id)?
                }
            };
            pack.read_entry_objects(&symlinks, num_workers)?
        };

        for (entry, target) in targets {
            if let EntryMetadata::Symlink {
                target_encoding, ..
            } = entry.metadata
            {
                symlink::decode_target(&entry.path, &target, target_encoding)?;
            }
        }
        Ok(())
    }

    fn update_remote_pack(&self, pack: &PackId) -> Result<(), Error> {
        let remotes_dir = self.data_dir().join(REMOTES_DIR);
        let remotes = remote::load_remotes(&remotes_dir)?;

        let pack = match pack {
            PackId::Pack(p) => p.rsplit_once('/').map(|x| x.1).unwrap_or(p),
        };
        let pack_file_name = pack.to_string() + "." + PACK_EXTENSION;

        let agent = remote::http_agent();
        let reporter = (self.progress_reporter_factory)(&format!("Fetching {pack_file_name}"));

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
        let files = clean_file_list(self.path.as_ref(), self.data_dir(), files)?
            .map(|native| {
                PackPath::from_native(&native)
                    .map(|logical| (native, logical))
                    .map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid snapshot path: {error}"),
                        )
                    })
            })
            .collect::<io::Result<Vec<_>>>()?;
        info!("Computing checksums for {} files...", files.len());

        let temp_dir = self.temp_dir();
        ensure_dir(&temp_dir)?;

        let threads = num_cpus::get();

        let pack_entries = run_in_parallel(threads, files.into_iter(), |(file_path, pack_path)| {
            let source_path = self.path.join(&file_path);
            let metadata = fs::symlink_metadata(&source_path)?;
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&source_path)?;
                let (buf, target_encoding) = symlink::encode_target(&target)?;
                let target_kind = symlink::target_kind(&source_path, &metadata);
                let checksum: [u8; 20] = Sha1::digest(&buf).into();
                self.write_loose_object(&*buf, &temp_dir, &checksum)?;
                return Ok(FileEntry::new(
                    pack_path,
                    checksum,
                    ObjectMetadata {
                        offset: LOOSE_OBJECT_OFFSET,
                        size: buf.len() as u64,
                    },
                    EntryMetadata::Symlink {
                        target_encoding,
                        target_kind,
                    },
                ));
            }

            let mut fd = match File::open(&source_path) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    let md = fs::metadata(&source_path)?;
                    if md.len() != 0 {
                        return Err(e);
                    }
                    // Special case file of zero byte length with no
                    // permission to read. We can still represent this,
                    // and it has uses (e.g. for testing)
                    return Ok(FileEntry::new(
                        pack_path,
                        [0; 20],
                        ObjectMetadata {
                            offset: LOOSE_OBJECT_OFFSET,
                            size: 0,
                        },
                        EntryMetadata::Regular {
                            mode: md.file_mode().0,
                        },
                    ));
                }
                r => r,
            }?;
            let (buf, mode) = {
                let mut buf = vec![];
                fd.read_to_end(&mut buf)?;
                let mode = fd.metadata()?.file_mode();
                (buf, mode)
            };
            let mut hasher = Sha1::new();
            hasher.update(&buf);
            let checksum: [u8; 20] = hasher.finalize().into();
            self.write_loose_object(&*buf, &temp_dir, &checksum)?;

            Ok(FileEntry::new(
                pack_path,
                checksum,
                ObjectMetadata {
                    offset: LOOSE_OBJECT_OFFSET,
                    size: buf.len() as u64,
                },
                EntryMetadata::Regular { mode: mode.0 },
            ))
        })
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;

        let mut index = PackIndex::new();
        index.push_snapshot(snapshot.tag().to_owned(), pack_entries)?;

        self.lock_exclusive()?;
        let loose_path = self.data_dir().join(PACKS_DIR).join(LOOSE_DIR);
        let index_path =
            append_path_suffix(loose_path.join(snapshot.tag()), DOT_PACK_INDEX_EXTENSION);
        ensure_dir(index_path.parent().unwrap())?;

        if index_path.exists() {
            let existing_tags = PackIndex::load_only_snapshots(&index_path)?;
            match existing_tags.as_slice() {
                [existing] if existing == snapshot.tag() => {}
                [existing] => {
                    return Err(Error::LooseSnapshotPathCollision {
                        path: index_path,
                        requested: snapshot.tag().to_owned(),
                        existing: existing.to_owned(),
                    });
                }
                _ => return Err(Error::CorruptPackIndex),
            }
        }

        index.save(index_path)?;

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
        pack.validate_regular()?;
        let PackId::Pack(pack_name) = pack;

        // Construct output file path.
        let pack_path = {
            let mut pack_path = self.data_dir().join(PACKS_DIR);
            ensure_dir(&pack_path)?;
            pack_path.push(format!("{pack_name}.{PACK_EXTENSION}"));
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
                let object_readers = objects.iter().map(|&handle| -> io::Result<Box<dyn Read>> {
                    // TODO: Method of obtaining readers from packs? Or we can
                    // just assume packs first get unpacked.
                    let checksum = index.handle_to_checksum(handle);
                    // All zero checksum is special case of empty object, which
                    // is not represented on disk.
                    if checksum.iter().all(|x| *x == 0) {
                        return Ok(Box::new(&[] as &[u8])); // Empty reader.
                    }

                    Ok(Box::new(open_file(self.loose_object_path(checksum))?))
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
        let mut pack_writer = io::BufWriter::new(create_file(&temp_path, None)?);
        // Write header and frames.
        write_skippable_frame(&mut pack_writer, &header_bytes)?;
        for frame_buf in frame_bufs {
            pack_writer.write_all(&frame_buf)?;
        }
        pack_writer.flush()?;
        drop(pack_writer);

        let index_path = append_path_suffix(pack_path.clone(), ".idx");
        info!("Write index: {}", index_path.display());
        index.save(index_path)?;

        // Finally, move the .pack file itself to the packs/ dir.
        fs::rename(&temp_path, &pack_path)?;

        Ok(())
    }

    /// Extracts all objects from the specified pack into the loose
    /// object store.
    ///
    /// This function takes a `pack_id` identifying a pack file and
    /// unpacks all of its contained objects, writing each object as a
    /// separate file into the repository's loose object storage. This
    /// operation is useful for scenarios where individual object access
    /// is required, or for maintenance tasks such as repacking or
    /// garbage collection. The function is currently unimplemented and
    /// serves as a placeholder for future development.
    ///
    /// # Arguments
    ///
    /// * `pack_id` - The identifier of the pack to be exploded into
    ///   loose objects.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails, such as if the pack
    /// cannot be read or objects cannot be written.
    ///
    /// # Example
    ///
    /// ```ignore
    /// repo.explode_pack(&pack_id)?;
    /// ```
    /// * `pack_id` - The pack to explode.
    pub fn explode_pack(&mut self, pack_id: &PackId) -> Result<(), Error> {
        let index = self.load_index(pack_id)?;
        let entries = index
            .object_checksums()
            .map(|checksum| {
                let object_metadata = index.object_metadata(checksum);
                let checksum_hex = hex::encode(checksum);
                let path = PackPath::new(
                    format!(
                        "{}/{}/{}",
                        &checksum_hex[..2],
                        &checksum_hex[2..4],
                        &checksum_hex[4..]
                    )
                    .into_bytes(),
                )
                .expect("checksum paths are valid pack paths");
                FileEntry::new(
                    path,
                    *checksum,
                    *object_metadata,
                    EntryMetadata::Regular { mode: 0o644 },
                )
            })
            .collect::<Vec<_>>();
        let mut opts = ExtractOptions::default();
        opts.set_verify(false);
        opts.set_num_workers(8);
        let loose_dir = self.data_dir().join(LOOSE_DIR);
        self.extract_entries(pack_id, &entries, loose_dir, opts)?;

        Ok(())
    }

    /// Deletes ALL loose snapshots and objects.
    pub fn remove_loose_all(&mut self) -> Result<(), Error> {
        let loose_dir = self.data_dir().join(LOOSE_DIR);

        match fs::remove_dir_all(loose_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }?;
        Ok(())
    }

    /// Updates the HEAD snapshot id.
    pub fn update_head(&mut self, snapshot_id: &SnapshotId) -> Result<(), Error> {
        // Readonly, do not update HEAD.
        if self.lock_file.is_none() {
            return Ok(());
        }

        let snapshot_string = format!("{snapshot_id}\n");
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

        let agent = remote::http_agent();
        let reporter = (self.progress_reporter_factory)(&format!(
            "Fetching remote repository index from {name}"
        ));

        reporter.checkpoint_with_detail(0, Some(1), url.to_owned());
        remote::fetch_remote(&agent, url, &path)?;
        reporter.checkpoint_with_detail(1, Some(0), url.to_owned());

        Ok(())
    }

    /// Identifies duplicate snapshots in the given packs.
    /// (two snapshots are equal if their checksums computed by compute_snapshot_checksum are equal).
    ///
    /// Returns a mapping of snapshot checksum to `SnapshotId`s with that checksum (>= 1).
    ///
    /// # Algorithm
    /// 1. Load up all packs indexes
    /// 2. Compute snapshot checksums and store in Checksum -> SnapshotId map
    fn find_duplicate_snapshots(
        &self,
        packs: &[PackId],
    ) -> Result<HashMap<ObjectChecksum, Vec<SnapshotId>>, Error> {
        // Deduplicate the snapshots in all packs (group by checksum)
        let checksum_to_group =
            Arc::new(Mutex::new(HashMap::<ObjectChecksum, Vec<SnapshotId>>::new()));

        let checksum_to_group_clone = checksum_to_group.clone();
        // Calculate the checksums of all snapshots in all packs
        run_in_parallel(
            num_cpus::get(),
            packs.iter().by_ref(),
            |pack_id| -> Result<_, Error> {
                let pack = self.load_index(pack_id)?;
                // Task local map of snapshot -> checksum that will be merged
                // into the results map.
                let snapshot_checksums: HashMap<String, _> = pack
                    .snapshot_tags()
                    .iter()
                    .map(|tag| {
                        (
                            tag.clone(),
                            pack.compute_snapshot_checksum(tag)
                                .expect("failed to resolve snapshot"),
                        )
                    })
                    .collect();

                // Lock the results map and add the checksums in this pack
                let mut checksum_to_group = checksum_to_group_clone.lock().unwrap();
                for (tag, checksum) in snapshot_checksums {
                    let entry = checksum_to_group.entry(checksum).or_default();
                    entry.push(SnapshotId::new(pack_id.clone(), &tag).unwrap());
                }
                Ok(())
            },
        )
        .into_iter()
        .collect::<Result<(), _>>()?;

        let mut results = checksum_to_group.lock().unwrap();
        Ok(std::mem::take(&mut *results))
    }

    /// Identifies loose packs that are redundant -- have been packed.
    ///
    /// # Algorithm
    /// 1. Find duplicate snapshots according to snapshot checksums
    /// 2. Filter out non-loose snapshots (identify what can be removed)
    pub fn find_redundant_loose_packs(&self) -> Result<Vec<PackId>, Error> {
        let is_any_non_loose =
            |snapshots: &[SnapshotId]| snapshots.iter().any(|s| !self.is_pack_loose(s.pack()));
        let filter_out_packed_snapshots = |snapshots: Vec<SnapshotId>| {
            snapshots
                .into_iter()
                .filter(|s| self.is_pack_loose(s.pack()))
                .collect::<Vec<_>>()
        };

        // 1. Find duplicate snapshots according to snapshot checksums
        let packs = self.packs()?;
        let duplicate_snapshots = self.find_duplicate_snapshots(&packs)?;
        // 2. Filter out non-loose snapshots (identify what can be removed)
        let loose_snapshots_present_in_packs = duplicate_snapshots
            .into_iter()
            // Loose with at least one copy in some non-loose pack
            .filter(|(_, snapshots)| snapshots.len() > 1 && is_any_non_loose(snapshots))
            .flat_map(|(_, snapshots)| filter_out_packed_snapshots(snapshots));

        // Loose snapshot ID -> loose pack ID
        let packs_to_remove: Vec<PackId> = loose_snapshots_present_in_packs
            .map(|loose_snapshot| {
                // Some sanity checking that we only have loose snapshots here
                let PackId::Pack(pack_id) = loose_snapshot.pack();
                assert_eq!(pack_id, &format!("{}/{}", LOOSE_DIR, loose_snapshot.tag()));

                loose_snapshot.pack().clone()
            })
            .collect();

        Ok(packs_to_remove)
    }

    /// Identifies loose objects that are not referenced from any of the packs.
    ///
    /// # Arguments
    /// * `roots` - the set of packs to consider as the sole referees to objects
    ///
    /// # Algorithm
    /// 1. Read the loose object checksums from disk
    /// 2. Load the indexes for the packs acting as roots in the object graph
    /// 3. Tracing: Process the roots and mark loose objects as reachable
    /// 4. Return the unreachable set of objects
    pub fn find_unreferenced_objects(
        &self,
        roots: impl ExactSizeIterator<Item = PackId>,
    ) -> Result<Vec<ObjectChecksum>, Error> {
        // 1. Read the loose object checksums from disk
        let loose_dir = self.data_dir().join(LOOSE_DIR);
        let objects_on_disk = WalkDir::new(loose_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| self.loose_object_checksum(&e.into_path()))
            .collect::<Result<HashSet<ObjectChecksum>, _>>()?;

        info!(
            "find_unreferenced_objects: Found {} objects on disk",
            objects_on_disk.len()
        );

        let unreferenced_objects = Arc::new(Mutex::new(objects_on_disk));
        let unreferenced_objects_clone = unreferenced_objects.clone();
        run_in_parallel(num_cpus::get(), roots, |pack_id| -> Result<_, Error> {
            // 2. Load the indexes for the packs acting as roots in the graph
            let pack = self.load_index(&pack_id)?;

            // 3. Tracing: Process the roots and mark loose objects as reachable
            let mut unreferenced_objects = unreferenced_objects_clone.lock().unwrap();
            for checksum in pack.object_checksums() {
                unreferenced_objects.remove(checksum);
            }
            Ok(())
        })
        .into_iter()
        .collect::<Result<Vec<()>, _>>()?;

        // 4. Return the unreachable set of objects
        let results = unreferenced_objects.lock().unwrap();
        info!(
            "find_unreferenced_objects: {} objects are not referenced from any of the roots",
            results.len()
        );
        Ok(results.iter().copied().collect())
    }

    /// Deletes the files related to the pack on disk. Deleting a non-existent pack is an error.
    pub fn delete_pack(&self, pack_id: &PackId) -> io::Result<()> {
        self.lock_exclusive()?;
        pack_id
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

        let pack_path = self.get_pack_path(pack_id);
        let pack_idx_path = self.get_pack_index_path(pack_id);

        let is_loose = self.is_pack_loose(pack_id);
        let pack_exsits = pack_path.exists();
        if is_loose && pack_exsits {
            return Err(io::Error::other(format!(
                "Unexpected .pack for loose {pack_id:?}"
            )));
        } else if !is_loose && !pack_exsits {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{pack_path:?} not found"),
            ));
        }

        fs::remove_file(&pack_idx_path)?;
        if !is_loose {
            fs::remove_file(&pack_path)?;
        } else if let Some(parent) = pack_idx_path.parent() {
            remove_empty_dirs(parent, self.data_dir().join(PACKS_DIR).join(LOOSE_DIR))?;
        }

        info!("Deleted pack index {:?}", pack_idx_path);
        Ok(())
    }

    /// Deletes the loose object identified by its checksum. Deleting a non-existent object is an error.
    pub fn delete_object(&self, checksum: &ObjectChecksum) -> io::Result<()> {
        self.lock_exclusive()?;

        let path = self.loose_object_path(checksum);
        fs::remove_file(&path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{:?}: {} object ({:?})",
                    e.kind(),
                    hex::encode(checksum),
                    path
                ),
            )
        })
    }

    pub fn get_pack_disk_stats(&self, pack_id: &PackId) -> io::Result<PackDiskStats> {
        pack_id
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let pack_idx_stats = fs::metadata(self.get_pack_index_path(pack_id))?;
        let pack_len = if self.is_pack_loose(pack_id) {
            0
        } else {
            fs::metadata(self.get_pack_path(pack_id))
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        };

        Ok(PackDiskStats {
            len: pack_len + pack_idx_stats.len(),
        })
    }

    pub fn get_object_disk_stats(&self, checksum: &ObjectChecksum) -> io::Result<ObjectDiskStats> {
        fs::metadata(self.loose_object_path(checksum)).map(|x| ObjectDiskStats { len: x.len() })
    }

    pub fn set_progress_reporter<F>(&mut self, factory: F)
    where
        F: 'static + Fn(&str) -> ProgressReporter<'static> + Send + Sync,
    {
        self.progress_reporter_factory = Box::new(factory);
    }

    /// Copies loose file entries from the repository's loose object
    /// store to the specified destination path.
    ///
    /// For each `FileEntry` in `entries`, this function:
    /// - Ensures the destination directory exists.
    /// - Copies the corresponding loose object file to the destination
    ///   path.
    /// - Sets the file mode (permissions) to match the original entry.
    /// - Optionally verifies the checksum of the copied file if
    ///   `verify` is true.
    ///
    /// # Arguments
    ///
    /// * `entries` - A slice of `FileEntry` objects representing the
    ///   files to copy.
    /// * `path` - The destination directory where files should be
    ///   copied.
    /// * `verify` - If true, verifies the checksum of each copied file
    ///   after copying.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be copied, if directory
    /// creation fails, if setting file permissions fails, or if
    /// checksum verification fails.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// repo.copy_loose_entries(&entries, Path::new("/destination"), true)?;
    /// ```
    fn copy_loose_entries(
        &mut self,
        entries: &[FileEntry],
        path: &Path,
        verify: bool,
        nthread: u32,
    ) -> Result<(), Error> {
        let regular_destinations = run_in_parallel(
            nthread as usize,
            entries.iter(),
            |entry| -> Result<_, Error> {
                let mut dest_path = PathBuf::new();
                dest_path.push(path);
                dest_path.push(
                    entry.path.to_native().map_err(|error| {
                        PackError::PathNotRepresentable(entry.path.clone(), error)
                    })?,
                );
                fs::create_dir_all(dest_path.parent().unwrap())?;
                let object_path = self.loose_object_path(&entry.checksum);
                match entry.metadata {
                    EntryMetadata::Symlink {
                        target_encoding,
                        target_kind,
                    } => {
                        let target = fs::read(&object_path)?;
                        if verify {
                            let checksum: ObjectChecksum = Sha1::digest(&target).into();
                            if checksum != entry.checksum {
                                return Err(
                                    PackError::ChecksumMismatch(entry.checksum, checksum).into()
                                );
                            }
                        }
                        symlink::materialize(
                            &entry.path,
                            &target,
                            target_encoding,
                            target_kind,
                            &dest_path,
                        )?;
                        if verify {
                            symlink::verify_materialized(
                                &entry.path,
                                &dest_path,
                                target_encoding,
                                target_kind,
                                &entry.checksum,
                            )?;
                        }
                        return Ok(None);
                    }
                    EntryMetadata::Regular { mode } => {
                        if entry.object_metadata.size == 0 {
                            symlink::materialize_regular(io::empty(), &dest_path, mode)?;
                        } else {
                            symlink::materialize_regular(
                                open_file(&object_path)?,
                                &dest_path,
                                mode,
                            )?;
                        }
                    }
                }
                Ok(Some((dest_path, entry.checksum)))
            },
        )
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        if verify {
            let (dest_paths, expected_checksums): (Vec<_>, Vec<_>) =
                regular_destinations.into_iter().unzip();
            let checksums = batch::compute_checksums(&dest_paths)?;
            for (expected, actual) in expected_checksums.into_iter().zip(checksums) {
                if expected != actual {
                    return Err(PackError::ChecksumMismatch(expected, actual).into());
                }
            }
        }

        Ok(())
    }

    fn get_pack_path(&self, pack_id: &PackId) -> PathBuf {
        match pack_id {
            PackId::Pack(name) => append_path_suffix(
                self.data_dir().join(PACKS_DIR).join(name),
                &format!(".{PACK_EXTENSION}"),
            ),
        }
    }

    fn get_pack_index_path(&self, pack_id: &PackId) -> PathBuf {
        match pack_id {
            PackId::Pack(name) => append_path_suffix(
                self.data_dir().join(PACKS_DIR).join(name),
                DOT_PACK_INDEX_EXTENSION,
            ),
        }
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
            .map(|e| ((&e.path, &e.checksum, &e.metadata), e))
            .collect();
        let to_lookup: HashMap<_, _> = to_entries
            .iter()
            .map(|e| ((&e.path, &e.checksum, &e.metadata), e))
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

    fn check_entry_unchanged_since(
        &self,
        entry: &FileEntry,
        head_time: SystemTime,
        path: &Path,
    ) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            warn!("Expected entry {:?} to be present!", path);
            Error::DirtyWorkDir
        })?;
        let kind_matches = match entry.metadata {
            EntryMetadata::Regular { .. } => metadata.is_file(),
            EntryMetadata::Symlink {
                target_encoding,
                target_kind,
            } => {
                metadata.file_type().is_symlink()
                    && symlink::verify_materialized(
                        &entry.path,
                        path,
                        target_encoding,
                        target_kind,
                        &entry.checksum,
                    )
                    .is_ok()
            }
        };
        if !kind_matches {
            warn!("Entry {} has changed kind or symlink target", entry.path);
            return Err(Error::DirtyWorkDir);
        }

        let last_modified = get_last_modified(metadata).ok_or(Error::DirtyWorkDir)?;

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
        fs::create_dir_all(obj_path.parent().unwrap())?;
        write_file_atomic_or_existing(&mut reader, temp_dir, &obj_path)
    }

    pub fn loose_object_checksum(&self, path: &Path) -> Result<ObjectChecksum, Error> {
        let bad_object_error = || Error::BadLooseObject(path.to_string_lossy().into());
        let num_components = path.components().count();
        let hex0_1 = path
            .components()
            .nth(num_components - 3)
            .ok_or_else(bad_object_error)?;
        let hex2_3 = path.components().nth(num_components - 2).unwrap();
        let hex4_19 = path.components().next_back().unwrap();

        let bytes0_1 =
            hex::decode(&*hex0_1.as_os_str().to_string_lossy()).map_err(|_| bad_object_error())?;
        let bytes2_3 =
            hex::decode(&*hex2_3.as_os_str().to_string_lossy()).map_err(|_| bad_object_error())?;
        let bytes4_19 =
            hex::decode(&*hex4_19.as_os_str().to_string_lossy()).map_err(|_| bad_object_error())?;

        if bytes0_1.len() + bytes2_3.len() + bytes4_19.len() != 20 {
            return Err(bad_object_error());
        }

        // Copy into array (try_into not supported by our minimum Rust version)
        let mut result: ObjectChecksum = [0; 20];
        for (i, byte) in bytes0_1
            .into_iter()
            .chain(bytes2_3)
            .chain(bytes4_19)
            .enumerate()
        {
            result[i] = byte;
        }
        Ok(result)
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

        let agent = remote::http_agent();
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // All variants are exercised by host-independent policy tests.
enum TargetPlatform {
    Unix,
    Windows,
    MacOs,
}

impl TargetPlatform {
    fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Unix
        }
    }
}

fn preflight_destination_ancestors<'a>(
    destination: &Path,
    paths: impl IntoIterator<Item = &'a PackPath>,
    removable_ancestors: &HashSet<PathBuf>,
) -> Result<(), Error> {
    for path in paths {
        let native = path
            .to_native()
            .map_err(|error| PackError::PathNotRepresentable(path.clone(), error))?;
        check_destination_ancestors(destination, &native, removable_ancestors)?;
    }
    Ok(())
}

fn preflight_path_policy<'a>(
    paths: impl IntoIterator<Item = &'a PackPath>,
    platform: TargetPlatform,
) -> Result<(), PackError> {
    let mut exact = HashSet::new();
    let mut target_paths = Vec::new();

    for path in paths {
        if !exact.insert(path) {
            return Err(PackError::DuplicateLogicalPath(path.clone()));
        }
        target_paths.push((target_path_key(path, platform)?, path));
    }

    target_paths.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for pair in target_paths.windows(2) {
        let (left, left_path) = &pair[0];
        let (right, right_path) = &pair[1];
        if left == right {
            return Err(PackError::TargetPathCollision(
                (*left_path).clone(),
                (*right_path).clone(),
            ));
        }
        if is_component_prefix(left, right) {
            return Err(PackError::PathPrefixConflict(
                (*left_path).clone(),
                (*right_path).clone(),
            ));
        }
    }
    Ok(())
}

fn target_path_key(path: &PackPath, platform: TargetPlatform) -> Result<Vec<Vec<u8>>, PackError> {
    path.components()
        .map(|component| match platform {
            TargetPlatform::Unix => Ok(component.to_vec()),
            TargetPlatform::Windows => {
                let component = target_utf8_component(path, component)?;
                validate_windows_component(path, component)?;
                Ok(component.to_lowercase().into_bytes())
            }
            TargetPlatform::MacOs => {
                let component = target_utf8_component(path, component)?;
                Ok(component
                    .nfd()
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
                    .into_bytes())
            }
        })
        .collect()
}

fn target_utf8_component<'a>(path: &PackPath, component: &'a [u8]) -> Result<&'a str, PackError> {
    std::str::from_utf8(component).map_err(|_| {
        PackError::PathNotRepresentable(
            path.clone(),
            crate::packpath::PackPathError::UnrepresentableComponent(component.to_vec()),
        )
    })
}

fn validate_windows_component(path: &PackPath, component: &str) -> Result<(), PackError> {
    let invalid_character = component
        .chars()
        .any(|character| character <= '\u{1f}' || r#"<>:"/\|?*"#.contains(character));
    let trailing = component.ends_with(['.', ' ']);
    let stem = component.split('.').next().unwrap().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(is_windows_device_suffix);
    if invalid_character || trailing || reserved {
        return Err(PackError::PathNotRepresentable(
            path.clone(),
            crate::packpath::PackPathError::UnrepresentableComponent(component.as_bytes().to_vec()),
        ));
    }
    Ok(())
}

fn is_windows_device_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn is_component_prefix(prefix: &[Vec<u8>], path: &[Vec<u8>]) -> bool {
    prefix.len() < path.len() && path.starts_with(prefix)
}

fn check_destination_ancestors(
    destination: &Path,
    relative: &Path,
    removable_ancestors: &HashSet<PathBuf>,
) -> Result<(), PackError> {
    let mut current = destination.to_owned();
    let mut current_relative = PathBuf::new();
    for component in relative.parent().into_iter().flat_map(Path::components) {
        current.push(component.as_os_str());
        current_relative.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    && !removable_ancestors.contains(&current_relative) =>
            {
                return Err(PackError::SymlinkedDestination(current));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                break
            }
            Err(error) => return Err(PackError::IOError(error)),
        }
    }
    Ok(())
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
    let data_dir = data_dir
        .strip_prefix(repo_dir)
        .ok()
        .map(normalize_relative_path)
        .transpose()?;
    let mut seen = HashSet::new();
    let mut cleaned = Vec::new();

    for input in files {
        let input = input.as_ref();
        let path = normalize_relative_path(input)?;
        if data_dir
            .as_ref()
            .is_some_and(|data_dir| path.starts_with(data_dir))
        {
            continue;
        }
        if !seen.insert(path.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate snapshot path {:?}", path),
            ));
        }
        reject_symlink_ancestors(repo_dir, &path)?;
        cleaned.push(path);
    }

    Ok(cleaned.into_iter())
}

fn normalize_relative_path(path: &Path) -> io::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("snapshot path {:?} escapes the repository", path),
                    ));
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("expected a relative snapshot path, got {:?}", path),
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("snapshot path {:?} has no file component", path),
        ));
    }
    Ok(normalized)
}

fn reject_symlink_ancestors(repo_dir: &Path, path: &Path) -> io::Result<()> {
    let mut candidate = repo_dir.to_owned();
    for component in path.parent().into_iter().flat_map(Path::components) {
        candidate.push(component);
        if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "snapshot path '{}' traverses a symbolic link",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_path(value: &str) -> PackPath {
        PackPath::new(value.as_bytes()).unwrap()
    }

    fn temp_tree(name: &str) -> PathBuf {
        let root = create_temp_path(&std::env::temp_dir());
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(name), name.as_bytes()).unwrap();
        root
    }

    #[test]
    fn clean_file_list_normalizes_lexically_and_rejects_escapes() {
        let root = temp_tree("result");
        let data_dir = root.join("elfshaker_data");
        let cleaned = clean_file_list(&root, &data_dir, [PathBuf::from("dir/../result")].iter())
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(cleaned, [PathBuf::from("result")]);

        assert!(clean_file_list(&root, &data_dir, [PathBuf::from("../result")].iter()).is_err());
        assert!(clean_file_list(&root, &data_dir, [root.join("result")].iter()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clean_file_list_excludes_data_by_component_and_rejects_duplicates() {
        let root = temp_tree("ordinary");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("inside"), b"data").unwrap();
        fs::create_dir_all(root.join("data-other")).unwrap();
        fs::write(root.join("data-other/file"), b"other").unwrap();

        let cleaned = clean_file_list(
            &root,
            &data_dir,
            [
                PathBuf::from("data/inside"),
                PathBuf::from("data-other/file"),
            ]
            .iter(),
        )
        .unwrap()
        .collect::<Vec<_>>();
        assert_eq!(cleaned, [PathBuf::from("data-other/file")]);

        assert!(clean_file_list(
            &root,
            &data_dir,
            [PathBuf::from("ordinary"), PathBuf::from("./ordinary")].iter(),
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clean_file_list_accepts_symlink_entries_but_rejects_symlink_ancestors() {
        use std::os::unix::fs::symlink;

        let root = temp_tree("target");
        fs::create_dir(root.join("directory")).unwrap();
        fs::write(root.join("directory/file"), b"file").unwrap();
        symlink("target", root.join("link")).unwrap();
        symlink("directory", root.join("linked-directory")).unwrap();
        let data_dir = root.join("elfshaker_data");

        assert_eq!(
            clean_file_list(&root, &data_dir, [PathBuf::from("link")].iter())
                .unwrap()
                .collect::<Vec<_>>(),
            [PathBuf::from("link")]
        );
        assert!(clean_file_list(
            &root,
            &data_dir,
            [PathBuf::from("linked-directory/file")].iter()
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_snapshot_stores_symlink_targets_as_objects() -> Result<(), Error> {
        use std::os::unix::fs::symlink;

        let root = temp_tree("target");
        symlink("target", root.join("link"))?;
        symlink("missing", root.join("dangling"))?;
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let mut repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let snapshot = SnapshotId::new(PackId::for_loose_snapshot("links")?, "links")?;
        repo.create_snapshot(
            &snapshot,
            [PathBuf::from("link"), PathBuf::from("dangling")].iter(),
        )?;

        let index = repo.load_index(snapshot.pack())?;
        let handles = index.resolve_snapshot("links").unwrap();
        let mut entries = index.entries_from_handles(handles.iter())?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            fs::read(repo.loose_object_path(&entries[0].checksum))?,
            b"missing"
        );
        assert_eq!(
            entries[0].metadata,
            EntryMetadata::Symlink {
                target_encoding: SymlinkTargetEncoding::PortableUtf8,
                target_kind: SymlinkTargetKind::Unknown,
            }
        );
        assert_eq!(
            fs::read(repo.loose_object_path(&entries[1].checksum))?,
            b"target"
        );
        assert_eq!(
            entries[1].metadata,
            EntryMetadata::Symlink {
                target_encoding: SymlinkTargetEncoding::PortableUtf8,
                target_kind: SymlinkTargetKind::File,
            }
        );

        let extracted = root.join("extracted");
        repo.extract_entries(
            snapshot.pack(),
            &entries,
            &extracted,
            ExtractOptions::default(),
        )?;
        assert_eq!(
            fs::read_link(extracted.join("dangling"))?,
            Path::new("missing")
        );
        assert_eq!(fs::read_link(extracted.join("link"))?, Path::new("target"));

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn native_unix_symlink_targets_retain_their_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let target = Path::new(std::ffi::OsStr::from_bytes(b"native-\xff"));
        assert_eq!(
            symlink::encode_target(target).unwrap(),
            (b"native-\xff".to_vec(), SymlinkTargetEncoding::UnixBytes)
        );
    }

    #[cfg(unix)]
    #[test]
    fn incremental_extraction_handles_file_link_and_directory_transitions() -> Result<(), Error> {
        use std::ops::ControlFlow;
        use std::os::unix::fs::symlink;

        let root = temp_tree("target");
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;
        let mut source = Repository::open_with_data_dir(&root, &data_dir)?;

        let make_snapshot = |tag: &str| -> Result<SnapshotId, Error> {
            Ok(SnapshotId::new(PackId::for_loose_snapshot(tag)?, tag)?)
        };
        let regular = make_snapshot("regular")?;
        fs::write(root.join("node"), b"regular")?;
        source.create_snapshot(&regular, [PathBuf::from("node")].iter())?;

        let link = make_snapshot("link")?;
        fs::remove_file(root.join("node"))?;
        symlink("target", root.join("node"))?;
        source.create_snapshot(&link, [PathBuf::from("node")].iter())?;

        let directory = make_snapshot("directory")?;
        fs::remove_file(root.join("node"))?;
        fs::create_dir(root.join("node"))?;
        fs::write(root.join("node/child"), b"child")?;
        source.create_snapshot(&directory, [PathBuf::from("node/child")].iter())?;

        let regular_again = make_snapshot("regular-again")?;
        fs::remove_file(root.join("node/child"))?;
        fs::remove_dir(root.join("node"))?;
        fs::write(root.join("node"), b"again")?;
        source.create_snapshot(&regular_again, [PathBuf::from("node")].iter())?;

        let mut packed_index = PackIndex::new();
        for snapshot in [&regular, &link, &directory, &regular_again] {
            let index = source.load_index(snapshot.pack())?;
            let result = index.for_each_snapshot(|tag, entries| {
                match packed_index.push_snapshot(tag.to_owned(), entries.clone()) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(error) => ControlFlow::Break(Err(error)),
                }
            })?;
            if let Some(result) = result {
                result?;
            }
        }
        let packed = PackId::Pack("transitions".to_owned());
        source.create_pack(
            &packed,
            packed_index,
            &PackOptions {
                compression_window_log: 20,
                compression_level: 1,
                num_workers: 1,
                num_frames: 1,
            },
            &ProgressReporter::dummy(),
        )?;
        drop(source);

        for (name, snapshots) in [
            (
                "loose",
                [
                    regular.clone(),
                    link.clone(),
                    directory.clone(),
                    regular_again.clone(),
                ],
            ),
            (
                "packed",
                [
                    SnapshotId::new(packed.clone(), "regular")?,
                    SnapshotId::new(packed.clone(), "link")?,
                    SnapshotId::new(packed.clone(), "directory")?,
                    SnapshotId::new(packed.clone(), "regular-again")?,
                ],
            ),
        ] {
            let checkout_root = root.join(format!("checkout-{name}"));
            fs::create_dir(&checkout_root)?;
            let mut checkout = Repository::open_with_data_dir(&checkout_root, &data_dir)?;
            let mut reset = ExtractOptions::default();
            reset.set_reset(true);
            reset.set_force(true);
            checkout.extract_snapshot(snapshots[0].clone(), reset)?;
            assert_eq!(fs::read(checkout_root.join("node"))?, b"regular");

            checkout.extract_snapshot(snapshots[1].clone(), ExtractOptions::default())?;
            assert_eq!(
                fs::read_link(checkout_root.join("node"))?,
                Path::new("target")
            );

            fs::remove_file(checkout_root.join("node"))?;
            symlink("changed", checkout_root.join("node"))?;
            assert!(matches!(
                checkout.extract_snapshot(snapshots[2].clone(), ExtractOptions::default()),
                Err(Error::DirtyWorkDir)
            ));

            let mut force = ExtractOptions::default();
            force.set_force(true);
            checkout.extract_snapshot(snapshots[2].clone(), force)?;
            assert_eq!(fs::read(checkout_root.join("node/child"))?, b"child");

            checkout.extract_snapshot(snapshots[3].clone(), ExtractOptions::default())?;
            assert_eq!(fs::read(checkout_root.join("node"))?, b"again");
            checkout.extract_snapshot(snapshots[1].clone(), ExtractOptions::default())?;
            assert_eq!(
                fs::read_link(checkout_root.join("node"))?,
                Path::new("target")
            );
            checkout.extract_snapshot(snapshots[0].clone(), ExtractOptions::default())?;
            assert_eq!(fs::read(checkout_root.join("node"))?, b"regular");
            drop(checkout);
            fs::remove_file(checkout_root.join("node"))?;
            fs::remove_dir(checkout_root)?;
        }

        fs::remove_file(root.join("node"))?;
        fs::remove_file(root.join("target"))?;
        fs::remove_dir_all(data_dir)?;
        fs::remove_dir(root)?;
        Ok(())
    }

    #[test]
    fn destination_policy_rejects_duplicates_and_prefix_conflicts() {
        let duplicate = [pack_path("same"), pack_path("same")];
        assert!(matches!(
            preflight_path_policy(duplicate.iter(), TargetPlatform::Unix),
            Err(PackError::DuplicateLogicalPath(_))
        ));

        let prefix = [
            pack_path("unrelated"),
            pack_path("file/child"),
            pack_path("file"),
        ];
        assert!(matches!(
            preflight_path_policy(prefix.iter(), TargetPlatform::Unix),
            Err(PackError::PathPrefixConflict(_, _))
        ));
    }

    #[test]
    fn destination_policy_applies_windows_name_and_collision_rules() {
        for invalid in [
            "CON",
            "dir/aux.txt",
            "trailing.",
            "trailing ",
            "a:b",
            r"a\b",
            "COM¹",
            "com².txt",
            "LPT³",
        ] {
            assert!(matches!(
                preflight_path_policy([pack_path(invalid)].iter(), TargetPlatform::Windows),
                Err(PackError::PathNotRepresentable(_, _))
            ));
        }

        assert!(matches!(
            preflight_path_policy(
                [pack_path("Dir/File"), pack_path("dir/file")].iter(),
                TargetPlatform::Windows
            ),
            Err(PackError::TargetPathCollision(_, _))
        ));
    }

    #[test]
    fn destination_policy_applies_conservative_macos_alias_rules() {
        assert!(matches!(
            preflight_path_policy(
                [pack_path("Résumé"), pack_path("re\u{301}sume\u{301}")].iter(),
                TargetPlatform::MacOs
            ),
            Err(PackError::TargetPathCollision(_, _))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn destination_preflight_rejects_existing_symlink_ancestors() {
        use std::os::unix::fs::symlink;

        let root = temp_tree("target");
        symlink(".", root.join("linked-directory")).unwrap();
        let paths = [pack_path("linked-directory/output")];
        let error =
            preflight_destination_ancestors(&root, paths.iter(), &HashSet::new()).unwrap_err();
        assert!(matches!(
            error,
            Error::PackError(PackError::SymlinkedDestination(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_destination_preflight_does_not_mutate_the_destination() {
        let root = temp_tree("sentinel");
        let data_dir = root.join("elfshaker_data");
        fs::create_dir_all(&data_dir).unwrap();
        let mut repo = Repository::open_with_data_dir(&root, &data_dir).unwrap();
        let entries = [
            FileEntry::new(
                pack_path("file"),
                [0; 20],
                ObjectMetadata {
                    offset: LOOSE_OBJECT_OFFSET,
                    size: 0,
                },
                EntryMetadata::default(),
            ),
            FileEntry::new(
                pack_path("file/child"),
                [0; 20],
                ObjectMetadata {
                    offset: LOOSE_OBJECT_OFFSET,
                    size: 0,
                },
                EntryMetadata::default(),
            ),
        ];
        let result = repo.extract_entries(
            &PackId::Pack("missing".to_owned()),
            &entries,
            &root,
            ExtractOptions::default(),
        );
        assert!(matches!(
            result,
            Err(Error::PackError(PackError::PathPrefixConflict(_, _)))
        ));
        assert_eq!(fs::read(root.join("sentinel")).unwrap(), b"sentinel");
        assert!(!root.join("file").exists());
        drop(repo);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn invalid_symlink_target_fails_before_destination_mutation() {
        let root = temp_tree("sentinel");
        let data_dir = root.join("elfshaker_data");
        fs::create_dir_all(&data_dir).unwrap();
        let mut repo = Repository::open_with_data_dir(&root, &data_dir).unwrap();
        let target = b"/not-portable";
        let checksum: ObjectChecksum = Sha1::digest(target).into();
        let temp_dir = repo.temp_dir();
        fs::create_dir_all(&temp_dir).unwrap();
        repo.write_loose_object(&target[..], &temp_dir, &checksum)
            .unwrap();
        let entry = FileEntry::new(
            pack_path("link"),
            checksum,
            ObjectMetadata {
                offset: LOOSE_OBJECT_OFFSET,
                size: target.len() as u64,
            },
            EntryMetadata::Symlink {
                target_encoding: SymlinkTargetEncoding::PortableUtf8,
                target_kind: SymlinkTargetKind::File,
            },
        );
        let pack_id = PackId::for_loose_snapshot("invalid-target").unwrap();
        let mut index = PackIndex::new();
        index
            .push_snapshot("invalid-target".to_owned(), [entry.clone()])
            .unwrap();
        let index_path = repo.get_pack_index_path(&pack_id);
        fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        index.save(index_path).unwrap();

        let result = repo.extract_entries(&pack_id, &[entry], &root, ExtractOptions::default());
        assert!(
            matches!(
                result,
                Err(Error::PackError(PackError::InvalidSymlinkTarget(_, _)))
            ),
            "unexpected result: {:?}",
            result
        );
        assert_eq!(fs::read(root.join("sentinel")).unwrap(), b"sentinel");
        assert!(!root.join("link").exists());
        fs::remove_dir_all(root).unwrap();
    }

    static EXAMPLE_MD: ObjectMetadata = ObjectMetadata {
        size: 1,
        offset: LOOSE_OBJECT_OFFSET,
    };

    #[test]
    fn repository_lock_upgrades_to_exclusive() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let repo = Repository::open_with_data_dir(&root, &data_dir)?;
        repo.lock_exclusive()?;

        let competing_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(data_dir.join("mutex"))?;
        let error = FileExt::try_lock_shared(&competing_lock).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_repository_lock_upgrade_restores_shared_lock() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let lock_file = repo.lock_file.as_ref().unwrap();
        FileExt::unlock(lock_file)?;

        let error = restore_shared_lock_on_error(
            lock_file,
            Err(io::Error::other("injected exclusive lock failure")),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "injected exclusive lock failure");

        let competing_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(data_dir.join("mutex"))?;
        let error = FileExt::try_lock_exclusive(&competing_lock).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn building_loose_object_paths_works() {
        let checksum = [
            0xFA, 0xF0, 0xDE, 0xAD, 0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0, 0xFA, 0xF0, 0xDE, 0xAD,
            0xBE, 0xEF, 0xBA, 0xDC, 0x0D, 0xE0,
        ];
        let test_lock = std::env::temp_dir().join("elfshaker_test_lock");
        let repo = Repository {
            path: "/repo".into(),
            data_dir: "/repo/elfshaker_data".into(),
            progress_reporter_factory: Box::new(|_| ProgressReporter::dummy()),
            lock_file: Some(fs::File::create(&test_lock).unwrap()),
            is_locked_exclusively: Mutex::new(false),
        };
        fs::remove_file(&test_lock).unwrap();
        let path = repo.loose_object_path(&checksum);
        assert_eq!(
            repo.data_dir()
                .join(LOOSE_DIR)
                .join("fa")
                .join("f0")
                .join("deadbeefbadc0de0faf0deadbeefbadc0de0"),
            path,
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_ascii_device_name_snapshot_remains_readable() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        let loose = data_dir.join(PACKS_DIR).join(LOOSE_DIR);
        fs::create_dir_all(&loose)?;
        let mut index = PackIndex::new();
        index.push_snapshot("CON".to_owned(), std::iter::empty::<FileEntry>())?;
        index.save(loose.join("CON.pack.idx"))?;
        fs::write(data_dir.join(HEAD_FILE), "loose/CON:CON\n")?;

        let repo = Repository::open_with_data_dir(&root, &data_dir)?;
        assert_eq!(repo.packs()?, vec![PackId::Pack("loose/CON".into())]);
        let snapshot = repo.find_snapshot("CON")?;
        assert_eq!(snapshot.tag(), "CON");
        assert_eq!(repo.read_head()?.0, Some(snapshot));

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn create_snapshot_preserves_dotted_hierarchical_tag_on_disk() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let mut repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let snapshot =
            SnapshotId::new(PackId::for_loose_snapshot("release/v1.2")?, "release/v1.2")?;
        repo.create_snapshot(&snapshot, std::iter::empty::<PathBuf>())?;

        assert!(data_dir
            .join(PACKS_DIR)
            .join(LOOSE_DIR)
            .join("release")
            .join("v1.2.pack.idx")
            .is_file());
        assert_eq!(
            fs::read_to_string(data_dir.join(HEAD_FILE))?,
            "loose/release/v1.2:release/v1.2\n"
        );
        assert!(repo
            .load_index(snapshot.pack())?
            .has_snapshot(snapshot.tag()));
        assert_eq!(repo.read_head()?.0, Some(snapshot));

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn physical_pack_paths_use_logical_id_separators() -> Result<(), Error> {
        let path = Path::new("remote").join("pack.pack.idx");

        assert_eq!(logical_pack_id_path(&path)?, "remote/pack.pack.idx");
        Ok(())
    }

    #[test]
    fn create_snapshot_does_not_overwrite_a_colliding_tag() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        let index_path = data_dir
            .join(PACKS_DIR)
            .join(LOOSE_DIR)
            .join("release")
            .join("v1.2.pack.idx");
        fs::create_dir_all(index_path.parent().unwrap())?;

        let mut existing = PackIndex::new();
        existing.push_snapshot("different-tag".to_owned(), std::iter::empty::<FileEntry>())?;
        existing.save(&index_path)?;
        let original = fs::read(&index_path)?;

        let mut repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let requested =
            SnapshotId::new(PackId::for_loose_snapshot("release/v1.2")?, "release/v1.2")?;
        let error = repo
            .create_snapshot(&requested, std::iter::empty::<PathBuf>())
            .unwrap_err();

        assert!(matches!(
            error,
            Error::LooseSnapshotPathCollision {
                requested,
                existing,
                ..
            } if requested == "release/v1.2" && existing == "different-tag"
        ));
        assert_eq!(fs::read(&index_path)?, original);

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn packs_reconstructs_loose_id_from_the_stored_tag() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        let index_path = data_dir
            .join(PACKS_DIR)
            .join(LOOSE_DIR)
            .join("filesystem-spelling.pack.idx");
        fs::create_dir_all(index_path.parent().unwrap())?;

        let mut index = PackIndex::new();
        index.push_snapshot(
            "logical/spelling".to_owned(),
            std::iter::empty::<FileEntry>(),
        )?;
        index.save(index_path)?;

        let repo = Repository::open_with_data_dir(&root, &data_dir)?;
        assert_eq!(
            repo.packs()?,
            vec![PackId::for_loose_snapshot("logical/spelling")?]
        );

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn deleting_nested_loose_snapshot_prunes_empty_tag_directories() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let mut repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let snapshot =
            SnapshotId::new(PackId::for_loose_snapshot("release/v1.2")?, "release/v1.2")?;
        repo.create_snapshot(&snapshot, std::iter::empty::<PathBuf>())?;
        repo.delete_pack(snapshot.pack())?;

        let loose_dir = data_dir.join(PACKS_DIR).join(LOOSE_DIR);
        assert!(loose_dir.is_dir());
        assert!(!loose_dir.join("release").exists());

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn repository_rejects_unchecked_traversing_pack_id() -> Result<(), Error> {
        let root = create_temp_path(&std::env::temp_dir());
        let data_dir = root.join(REPO_DIR);
        fs::create_dir_all(&data_dir)?;

        let repo = Repository::open_with_data_dir(&root, &data_dir)?;
        let error = match repo.load_index(&PackId::Pack("../outside".to_owned())) {
            Ok(_) => panic!("traversing PackId unexpectedly loaded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Invalid pack identifier"));

        drop(repo);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn loose_object_write_accepts_competing_writer() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        struct CompetingReader {
            contents: &'static [u8],
            destination: PathBuf,
            destination_dir: PathBuf,
            offset: usize,
        }

        impl Read for CompetingReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.offset == 0 {
                    fs::write(&self.destination, self.contents)?;
                    fs::set_permissions(&self.destination_dir, fs::Permissions::from_mode(0o555))?;
                }

                let remaining = &self.contents[self.offset..];
                let len = remaining.len().min(buf.len());
                buf[..len].copy_from_slice(&remaining[..len]);
                self.offset += len;
                Ok(len)
            }
        }

        struct Cleanup(PathBuf);

        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let root = create_temp_path(&std::env::temp_dir());
        fs::create_dir(&root)?;
        let _cleanup = Cleanup(root.clone());
        let data_dir = root.join(REPO_DIR);
        let temp_dir = data_dir.join(TEMP_DIR);
        fs::create_dir_all(&temp_dir)?;

        let repo = Repository {
            path: root,
            data_dir,
            progress_reporter_factory: Box::new(|_| ProgressReporter::dummy()),
            lock_file: None,
            is_locked_exclusively: Mutex::new(false),
        };

        let checksum: [u8; 20] = Sha1::digest(b"same contents").into();
        let destination = repo.loose_object_path(&checksum);
        let destination_dir = destination.parent().unwrap().to_path_buf();
        fs::create_dir_all(&destination_dir)?;

        let mut reader = CompetingReader {
            contents: b"same contents",
            destination: destination.clone(),
            destination_dir: destination_dir.clone(),
            offset: 0,
        };
        let result = repo.write_loose_object(&mut reader, &temp_dir, &checksum);

        fs::set_permissions(&destination_dir, fs::Permissions::from_mode(0o755))?;
        result?;
        assert_eq!(b"same contents", fs::read(destination)?.as_slice());
        assert!(temp_dir.read_dir()?.next().is_none());
        Ok(())
    }

    #[test]
    fn compute_entry_diff_finds_updates() {
        let path = pack_path("path/to/A");
        let old_checksum = [0; 20];
        let new_checksum = [1; 20];
        let old_entries = [FileEntry::new(
            path.clone(),
            old_checksum,
            EXAMPLE_MD,
            Default::default(),
        )];
        let new_entries = [FileEntry::new(
            path.clone(),
            new_checksum,
            EXAMPLE_MD,
            Default::default(),
        )];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(1, added.len());
        assert_eq!(path, added[0].path);
        assert_eq!(1, removed.len());
        assert_eq!(path, removed[0].path);
    }

    #[test]
    fn compute_entry_diff_finds_update_of_duplicated() {
        let path_a = pack_path("path/to/A");
        let path_a_old_checksum = [0; 20];
        let path_b = pack_path("path/to/B");
        let path_b_old_checksum = [0; 20];
        let path_a_new_checksum = [1; 20];
        let old_entries = [
            FileEntry::new(
                path_a.clone(),
                path_a_old_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
            FileEntry::new(
                path_b.clone(),
                path_b_old_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
        ];
        let new_entries = [FileEntry::new(
            path_a.clone(),
            path_a_new_checksum,
            EXAMPLE_MD,
            Default::default(),
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
        let path_a = pack_path("path/to/A");
        let path_a_old_checksum = [0; 20];
        let path_a_new_checksum = [1; 20];
        let path_b = pack_path("path/to/B");
        let path_b_old_checksum = [1; 20];
        let path_b_new_checksum = [0; 20];
        let old_entries = [
            FileEntry::new(
                path_a.clone(),
                path_a_old_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
            FileEntry::new(
                path_b.clone(),
                path_b_old_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
        ];
        let new_entries = [
            FileEntry::new(
                path_a.clone(),
                path_a_new_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
            FileEntry::new(
                path_b.clone(),
                path_b_new_checksum,
                EXAMPLE_MD,
                Default::default(),
            ),
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
