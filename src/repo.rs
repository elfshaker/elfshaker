//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    fmt::Display,
    fs,
    fs::File,
    io,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use crypto::digest::Digest;
use crypto::sha1::Sha1;

use log::{info, warn};

use zstd::stream::raw::DParameter;
use zstd::Decoder;

use crate::log::measure_ok;
use crate::packidx::{ObjectChecksum, PackEntry, PackError, PackIndex, PackedFileList};

/// Use [`Repository::data_dir`] instead of REPO_DIR
/// This will make is easier to make this value user-configurable
/// in the future.
const REPO_DIR: &str = "elfshaker_data";
/// The top-level index file path.
pub const INDEX_FILE: &str = "index";
/// A pointer to the extracted snapshot.
pub const HEAD_FILE: &str = "HEAD";
/// A directory containing a list of .pack and .pack.idx files
pub const PACKS_DIR: &str = "packs";
/// The file extension of a pack file.
pub const PACK_EXTENSION: &str = "pack";
/// The file extension of a pack index file.
pub const PACK_INDEX_EXTENSION: &str = "pack.idx";

#[derive(Debug)]
/// Error used when parsing a [`SnapshotId`] fails.
pub struct ParseSnapshotIdError {}

impl Display for ParseSnapshotIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Failed to parse the snapshot spec!")
    }
}

impl std::error::Error for ParseSnapshotIdError {}

/// Identifies a snapshot using a pack base filename [`Path::file_stem`] and a snapshot tag.
#[derive(PartialEq, Clone, Debug)]
pub struct SnapshotId {
    pack: String,
    tag: String,
}

impl SnapshotId {
    /// Creates a [`SnapshotId`] from a pack and a snapshot tag
    pub fn new(pack: &str, tag: &str) -> Self {
        Self {
            pack: pack.to_owned(),
            tag: tag.to_owned(),
        }
    }

    pub fn pack(&self) -> &str {
        &self.pack
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }
}

/// [`SnapshotId`] equality is a full equivalence relation.
impl Eq for SnapshotId {}

/// Prints the canonical form for [`SnapshotId`].
impl Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}/{}", self.pack, self.tag)
    }
}

/// Parses a [`SnapshotId`] from a canonical form string.
impl FromStr for SnapshotId {
    type Err = ParseSnapshotIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('/').collect();

        // Neither the snapshot pack filename nor the snapshot tag are
        // allowed to contain forward slashes.
        if parts.len() != 2 {
            return Err(ParseSnapshotIdError {});
        }

        let pack = parts[0].to_owned();
        if pack.is_empty() {
            return Err(ParseSnapshotIdError {});
        }
        // Whitespace at the end is ignored!
        let tag = parts[1].trim_end().to_owned();
        if tag.is_empty() {
            return Err(ParseSnapshotIdError {});
        }

        Ok(SnapshotId { pack, tag })
    }
}

/// The type of error used by repository operations.
#[derive(Debug)]
pub enum Error {
    IOError(io::Error),
    PackError(PackError),
    /// Bad elfshaker_data/inex
    CorruptRepositoryIndex,
    /// Bad elfshaker_data/HEAD (missing HEAD is okay and means that nothing has been extracted so far)
    CorruptHead,
    /// The .pack.idx is corrupt
    CorruptPackIndex,
    /// Multiple or none snapshots match the specified description
    AmbiguousSnapshotMatch,
    /// The working directory contains unexpected files
    DirtyWorkDir,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::IOError(ioerr) => ioerr.fmt(f),
            Self::PackError(packerr) => packerr.fmt(f),
            Self::CorruptHead => write!(f, "HEAD is corrupt!"),
            Self::CorruptRepositoryIndex => {
                write!(f, "The repository index is corrupt or missing!")
            }
            Self::CorruptPackIndex => write!(f, "The pack index is corrupt!"),
            Self::AmbiguousSnapshotMatch => {
                write!(f, "Found multiple snapshots matching the description!")
            }
            Self::DirtyWorkDir => write!(
                f,
                "Some files in the repository have been removed or modified unexpectedly!"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl std::convert::From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::IOError(err)
    }
}

impl std::convert::From<PackError> for Error {
    fn from(err: PackError) -> Self {
        Self::PackError(err)
    }
}

/// Represents an pack file.
pub struct Pack {
    /// The base filename ([`Path::file_stem`]) of the pack.
    name: String,
    /// The index for the pack.
    index: PackIndex,
    /// The unidirectional stream of data stored in the pack.
    reader: Decoder<'static, BufReader<File>>,
}

impl Pack {
    /// Opens a pack file and its corresponding index.
    ///
    /// # Arguments
    ///
    /// * `pack_name` - The base filename ([`Path::file_stem`]) of the pack.
    pub fn open<P>(repo: P, pack_name: &str) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let mut packs_data = repo.as_ref().join(&*Repository::data_dir());
        packs_data.push(PACKS_DIR);

        let mut pack_index_path = packs_data.join(pack_name);
        pack_index_path.set_extension(PACK_INDEX_EXTENSION);

        let mut pack_path = packs_data.join(pack_name);
        pack_path.set_extension(PACK_EXTENSION);

        info!("Reading pack index {:?}...", pack_index_path);
        let pack_index = Self::parse_index(File::open(pack_index_path)?)?;

        info!("Opening pack file {:?}...", pack_path);
        let reader = File::open(pack_path)?;
        let mut reader = Decoder::new(reader)?;
        // 2^30 = 1024MiB window log
        reader.set_parameter(DParameter::WindowLogMax(30))?;

        Ok(Pack {
            name: pack_name.to_owned(),
            index: pack_index,
            reader,
        })
    }

    /// Parses the data fom the reader into a [`PackIndex`].
    pub fn parse_index<R>(pack_index: R) -> Result<PackIndex, Error>
    where
        R: Read,
    {
        let index: PackIndex = rmp_serde::decode::from_read(pack_index).map_err(|e| match e {
            // Bubble IO errors up.
            rmp_serde::decode::Error::InvalidMarkerRead(e)
            | rmp_serde::decode::Error::InvalidDataRead(e) => Error::IOError(e),
            // Everything else indicates a broken pack index and is non-actionable.
            _ => Error::CorruptPackIndex,
        })?;
        Ok(index)
    }

    /// The base filename ([`Path::file_stem`]) of the pack.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A reference to the pack index.
    pub fn index(&self) -> &PackIndex {
        &self.index
    }

    /// Extracts the specified entries from the pack into the specified directory.
    /// This operation consumes the pack, since [`Pack`] objects contain a unidirectional
    /// data stream that becomes unusable after it is read.
    ///
    /// # Arguments
    ///
    /// * `entries` - The list of entries to extract. These *must* be entries contained in the pack index.
    /// * `output_dir` - The directory relative to which the files will be extracted.
    /// * `verify` - Enable/disable checksum verification.
    fn extract<P>(mut self, entries: &[PackEntry], output_dir: P, verify: bool) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        let mut indices: Vec<_> = (0..entries.len()).collect();
        // Sort objects to allow for forward-only seeking
        indices.sort_by(|x, y| {
            let offset_x = entries[*x].object_index().offset;
            let offset_y = entries[*y].object_index().offset;
            offset_x.cmp(&offset_y)
        });

        info!(
            "Checksum verification is {}!",
            if verify { "on (slower)" } else { "off" }
        );

        info!("Decompressing {} files...", entries.len());
        // Used for timing
        let mut seek_time: std::time::Duration = Default::default();
        let mut object_time: std::time::Duration = Default::default();
        let mut verify_time: std::time::Duration = Default::default();
        let mut write_time: std::time::Duration = Default::default();
        let (total_time, _) = measure_ok(|| -> Result<(), Error> {
            // Decompression buffer
            let mut buf = vec![];
            let mut path_buf = PathBuf::new();
            let mut pos = 0;

            for index in indices.into_iter() {
                let entry = &entries[index];
                let path = entry.path();
                let object = entry.object_index();

                // Seek forward
                let discard_bytes = object.offset - pos;
                // Check if we need to read a new object.
                // The current position in stream can be AFTER the object offset only
                // if the previous and this object are the same. This is because the objects
                // are sorted by offset, and the current position is set to the offset at the
                // end of each object, after that object is consumed.
                if pos <= object.offset {
                    seek_time += measure_ok(|| self.seek(discard_bytes))?.0;
                    // Resize buf
                    if buf.len() < object.size as usize {
                        buf.resize(object.size as usize, 0);
                    }
                    // Read object
                    object_time +=
                        measure_ok(|| self.reader.read_exact(&mut buf[..object.size as usize]))?.0;
                    pos = object.offset + object.size;
                    if verify {
                        verify_time += measure_ok(|| {
                            Self::verify_object(&buf[..object.size as usize], &object.checksum)
                        })?
                        .0;
                    }
                }
                // Output path
                path_buf.clear();
                path_buf.push(&output_dir);
                path_buf.push(path);

                write_time +=
                    measure_ok(|| Self::write_object(&buf[..object.size as usize], &path_buf))?.0;
            }

            Ok(())
        })?;

        // Time spent computing paths, resizing buffers, etc.
        let other_time = total_time - (seek_time + object_time + verify_time + write_time);
        // Log statistics about the decompression performance
        info!(
            "Decompression took {:?}\n\tSeeking: {:?}\n\tObject decompression: {:?}\n\tVerification: {:?}\n\tWriting to disk: {:?}\n\tOther: {:?}",
            total_time, seek_time, object_time, verify_time, write_time, other_time
        );

        Ok(())
    }

    /// Consumes the specified number of bytes from the reader.
    fn seek(&mut self, bytes: u64) -> io::Result<()> {
        io::copy(&mut self.reader.by_ref().take(bytes), &mut io::sink()).map(|_| {})
    }

    /// Verifies that the object has the expected checksum.
    fn verify_object(buf: &[u8], exp_checksum: &ObjectChecksum) -> Result<(), Error> {
        // Verify checksum
        let mut checksum = [0u8; 20];
        let mut hasher = Sha1::new();
        hasher.input(buf);
        hasher.result(&mut checksum);
        if &checksum != exp_checksum {
            return Err(PackError::ChecksumMismatch.into());
        }
        Ok(())
    }

    /// Writes the object to the specified path, taking care
    /// of adjusting file permissions.
    fn write_object(buf: &[u8], path: &Path) -> Result<(), Error> {
        fs::create_dir_all(path.parent().unwrap())?;
        let _ = ensure_file_writable(path)?;
        let mut f = File::create(path)?;
        f.write_all(&buf)?;
        set_readonly(&f, true)?;
        Ok(())
    }
}

/// Ensures that the file is writable OR does not exist!
fn ensure_file_writable(p: impl AsRef<Path>) -> io::Result<()> {
    let o = match File::open(p) {
        Ok(file) => file,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut p = o.metadata()?.permissions();
    p.set_readonly(false);
    o.set_permissions(p)?;
    Ok(())
}

/// Sets the readonly permission on the file.
fn set_readonly(f: &File, readonly: bool) -> io::Result<()> {
    let mut perm = f.metadata()?.permissions();
    perm.set_readonly(readonly);
    f.set_permissions(perm)?;
    Ok(())
}

impl fmt::Debug for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pack")
            .field("name", &self.name)
            .field("index", &self.index)
            .finish()
    }
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
        }
    }
}

pub struct Repository {
    /// The path containing the [`Repository::data_dir`] directory.
    path: PathBuf,
    /// The ID of the currently extracted snapshot or None, if nothing has been extracted yet.
    head: Option<SnapshotId>,
    /// Maps (snapshot -> packs)
    index: HashMap<String, Vec<String>>,
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
        let head = {
            if let Some(bytes) = read_or_none(data_dir.join(HEAD_FILE))? {
                let text = std::str::from_utf8(&bytes).map_err(|_| Error::CorruptHead)?;
                Some(SnapshotId::from_str(&text).map_err(|_| Error::CorruptHead)?)
            } else {
                None
            }
        };

        if let Some(id) = head.as_ref() {
            info!("Current HEAD: {}/{}", id.pack(), id.tag());
        } else {
            info!("Current HEAD: None");
        }

        let index = {
            if let Some(bytes) = read_or_none(data_dir.join(INDEX_FILE))? {
                rmp_serde::decode::from_slice(&bytes).map_err(|_| Error::CorruptRepositoryIndex)?
            } else {
                return Err(Error::CorruptRepositoryIndex);
            }
        };

        Ok(Repository {
            path: path.as_ref().into(),
            head,
            index,
        })
    }
    /// The currently extracted snapshot.
    pub fn head(&self) -> &Option<SnapshotId> {
        &self.head
    }
    /// Returns the names of the packs containing the snapshot.
    pub fn find_packs(&self, snapshot: &str) -> &[String] {
        &self
            .index
            .get(snapshot)
            .map(|x| x as &[String])
            .unwrap_or(&[])
    }
    /// Checks-out the specified snapshot.
    ///
    /// # Arguments
    ///
    /// * `snapshot_id` - The snapshot to extract.
    pub fn extract(&mut self, snapshot_id: SnapshotId, opts: ExtractOptions) -> Result<(), Error> {
        // Open the pack and find the snapshot specified in SnapshotId.
        let pack = Pack::open(&self.path, snapshot_id.pack())?;
        let entries = Self::get_entries(&pack, snapshot_id.tag())?;

        let (new_entries, old_entries) = if !opts.reset && self.head().is_some() {
            let head = self.head().as_ref().unwrap();
            // HEAD and new snapshot packs might differ
            if snapshot_id.pack() == head.pack() {
                let head_entries = Self::get_entries(&pack, head.tag())?;
                Self::compute_entry_diff(&head_entries, &entries)
            } else {
                let head_pack = Pack::open(&self.path, head.pack())?;
                let head_entries = Self::get_entries(&head_pack, head.tag())?;
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
            // Paths which will
            let updated: Vec<_> = new_paths.intersection(&old_paths).copied().collect();
            let removed: Vec<_> = old_paths.difference(&new_paths).copied().collect();
            (updated, removed)
        };

        if let Some(head) = self.head() {
            info!(
                "Extract {} -> {} (+{} files, -{} files, *{} files)",
                head,
                snapshot_id,
                new_entries.len() - updated_paths.len(),
                removed_paths.len(),
                updated_paths.len()
            );
        }

        let mut path_buf = PathBuf::new();
        for path in &removed_paths {
            path_buf.clear();
            path_buf.push(&self.path);
            path_buf.push(path);
            if !opts.force() {
                Self::ensure_safe_to_delete(&path_buf)?;
            }
            // Delete the file
            match File::open(&path_buf)
                .map(|file| set_readonly(&file, false))
                .and_then(|_| fs::remove_file(&path_buf))
            {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
                _ => {}
            };
        }
        if !opts.force() {
            for path in &updated_paths {
                path_buf.clear();
                path_buf.push(&self.path);
                path_buf.push(path);
                Self::ensure_safe_to_delete(&path_buf)?;
            }
        }

        pack.extract(&new_entries, &self.path, opts.verify())?;
        self.update_head(&snapshot_id)?;
        Ok(())
    }
    /// The name of the directory containing the elfshaker repository data.
    pub fn data_dir() -> Cow<'static, str> {
        Cow::Borrowed(REPO_DIR)
    }
    /// Updates the HEAD snapshot id.
    fn update_head(&mut self, snapshot_id: &SnapshotId) -> Result<(), Error> {
        let data_dir = self.path.join(&*Self::data_dir());
        let snapshot_string = format!("{}", snapshot_id);
        fs::write(data_dir.join(HEAD_FILE), snapshot_string)?;
        self.head = Some(snapshot_id.clone());
        Ok(())
    }
    /// Returns the full list of [`PackEntry`].
    fn get_entries(pack: &Pack, tag: &str) -> Result<Vec<PackEntry>, Error> {
        let snapshot = pack
            .index()
            .find_snapshot(tag)
            .ok_or(Error::PackError(PackError::SnapshotNotFound))?;

        // Convert the PackedFileList into a list of entries to write to disk.
        let entries = match snapshot.list {
            PackedFileList::Complete(ref list) => pack.index().entries(list.iter().cloned())?,
            _ => unreachable!(),
        };
        Ok(entries)
    }
    /// Returns the pair of lists (`added`, `removed`). `added` contains the entries from
    /// `to_entries` which are not present in `from_entries`. `removed` contains the entries
    /// from `from_entries` which are not present in `to_entries`.
    fn compute_entry_diff(
        from_entries: &[PackEntry],
        to_entries: &[PackEntry],
    ) -> (Vec<PackEntry>, Vec<PackEntry>) {
        // Object path+checksum to index in from_entries.
        let entry2fromidx: BTreeMap<_, _> = from_entries
            .iter()
            .enumerate()
            .map(|(i, e)| ((e.path(), e.object_index().checksum), i))
            .collect();
        let entry2toidx: BTreeMap<_, _> = to_entries
            .iter()
            .enumerate()
            .map(|(i, e)| ((e.path(), e.object_index().checksum), i))
            .collect();

        let mut added = vec![];
        let mut removed = vec![];

        // Check which from entries are missing in to_entries or under a different path,
        // and mark them for removal
        for (entry, &from_idx) in &entry2fromidx {
            match entry2toidx.get(entry) {
                Some(&to_idx) => {
                    if from_entries[from_idx].path() != to_entries[to_idx].path() {
                        removed.push(from_entries[from_idx].clone());
                    }
                }
                None => removed.push(from_entries[from_idx].clone()),
            }
        }
        // Check which to entries were added or moved,
        // and add them for extraction
        for (entry, &to_idx) in &entry2toidx {
            match entry2fromidx.get(entry) {
                Some(&from_idx) => {
                    if from_entries[from_idx].path() != to_entries[to_idx].path() {
                        added.push(to_entries[to_idx].clone());
                    }
                }
                None => added.push(to_entries[to_idx].clone()),
            }
        }

        (added, removed)
    }

    fn ensure_safe_to_delete(path: &Path) -> Result<(), Error> {
        if let Ok(metadata) = fs::metadata(&path) {
            if !metadata.permissions().readonly() {
                warn!("Expected file {:?} to be readonly!", path);
                // If the file is not readonly that means that the repo has been modified unexpectedly!
                return Err(Error::DirtyWorkDir);
            }
        } else {
            warn!("Expected file {:?} to be present!", path);
            // If the file is missing that also means it has been modified!
            return Err(Error::DirtyWorkDir);
        }
        Ok(())
    }
}

/// Reads a file, returning a byte slice. Returns [`None`], if the file does not exist.
fn read_or_none<P>(path: P) -> io::Result<Option<Vec<u8>>>
where
    P: AsRef<Path>,
{
    let mut buf = vec![];
    match File::open(path) {
        Ok(mut file) => {
            file.read_to_end(&mut buf)?;
            Ok(Some(buf))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packidx::ObjectIndex;

    #[test]
    fn compute_entry_diff_finds_updates() {
        let path = "/path/to/A";
        let old_checksum = [0; 20];
        let new_checksum = [1; 20];
        let old_entries = [PackEntry::new(
            path.as_ref(),
            ObjectIndex {
                checksum: old_checksum,
                offset: 0,
                size: 1,
            },
        )];
        let new_entries = [PackEntry::new(
            path.as_ref(),
            ObjectIndex {
                checksum: new_checksum,
                offset: 0,
                size: 1,
            },
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
            PackEntry::new(
                path_a.as_ref(),
                ObjectIndex {
                    checksum: path_a_old_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
            PackEntry::new(
                path_b.as_ref(),
                ObjectIndex {
                    checksum: path_b_old_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
        ];
        let new_entries = [PackEntry::new(
            path_a.as_ref(),
            ObjectIndex {
                checksum: path_a_new_checksum,
                offset: 0,
                size: 1,
            },
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
            PackEntry::new(
                path_a.as_ref(),
                ObjectIndex {
                    checksum: path_a_old_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
            PackEntry::new(
                path_b.as_ref(),
                ObjectIndex {
                    checksum: path_b_old_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
        ];
        let new_entries = [
            PackEntry::new(
                path_a.as_ref(),
                ObjectIndex {
                    checksum: path_a_new_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
            PackEntry::new(
                path_b.as_ref(),
                ObjectIndex {
                    checksum: path_b_new_checksum,
                    offset: 0,
                    size: 1,
                },
            ),
        ];
        let (added, removed) = Repository::compute_entry_diff(&old_entries, &new_entries);
        assert_eq!(2, added.len());
        assert!(added
            .iter()
            .any(|e| path_a == e.path() && path_a_new_checksum == e.object_index().checksum));
        assert!(added
            .iter()
            .any(|e| path_b == e.path() && path_b_new_checksum == e.object_index().checksum));
        assert_eq!(2, removed.len());
        assert!(removed
            .iter()
            .any(|e| path_a == e.path() && path_a_old_checksum == e.object_index().checksum));
        assert!(removed
            .iter()
            .any(|e| path_b == e.path() && path_b_old_checksum == e.object_index().checksum));
    }
}
