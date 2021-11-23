//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    fs::File,
    io,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
};
use std::{fmt::Display, str::FromStr};

use crypto::digest::Digest;
use crypto::sha1::Sha1;

use log::info;

use zstd::stream::raw::DParameter;
use zstd::Decoder;

use super::constants::{
    DEFAULT_WINDOW_LOG_MAX, PACKS_DIR, PACK_EXTENSION, PACK_HEADER_MAGIC, PACK_INDEX_EXTENSION,
};
use super::error::Error;
use super::repository::Repository;
use super::{algo::run_in_parallel, constants::DOT_PACK_INDEX_EXTENSION, utils::open_file};
use crate::packidx::{FileEntry, ObjectChecksum, PackError, PackIndex};
use crate::{log::measure_ok, packidx::ObjectMetadata};

/// Pack and snapshots IDs can contain latin letter, digits or the following characters.
const EXTRA_ID_CHARS: &[char] = &['-', '_', '/'];

#[derive(Debug)]
/// Error used when parsing a [`SnapshotId`] fails.
pub enum IdError {
    BadFormat(String),
    InvalidPack(String),
    InvalidSnapshot(String),
}

impl Display for IdError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::BadFormat(s) => write!(f, "Unrecognized identifier format '{}'!", s),
            Self::InvalidPack(s) => write!(
                f,
                "Invalid pack identifier '{}'! Latin letters, digits, -, _ and / are allowed!",
                s
            ),
            Self::InvalidSnapshot(s) => write!(
                f,
                "Invalid snapshot identifier '{}'! Latin letters, digits, - and _ are allowed!",
                s
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// Identifies a pack file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackId {
    Pack(String),
}

impl PackId {
    /// from_index_path returns Some(PackId::Pack(index_path)) with the pack
    /// index extension trimmed, if the input ends in the PACK_INDEX_EXTENSION.
    pub fn from_index_path(index_path: String) -> Option<PackId> {
        index_path
            .strip_suffix(DOT_PACK_INDEX_EXTENSION)
            .map(|s| PackId::Pack(s.to_owned()))
    }

    fn is_valid(s: &str) -> bool {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || EXTRA_ID_CHARS.contains(&c))
    }
}

impl FromStr for PackId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !PackId::is_valid(s) {
            return Err(IdError::InvalidPack(s.to_owned()));
        }
        Ok(PackId::Pack(s.to_owned()))
    }
}

impl Display for PackId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            PackId::Pack(s) => write!(f, "{}", s),
        }
    }
}

/// Identifies a snapshot using a pack base filename [`Path::file_stem`] and a snapshot tag.
#[derive(PartialEq, Clone, Debug)]
pub struct SnapshotId {
    pack: PackId,
    tag: String,
}

impl SnapshotId {
    /// Creates a [`SnapshotId`] from a pack and a snapshot tag
    pub fn new(pack: PackId, tag: &str) -> Result<Self, IdError> {
        if !Self::is_valid(tag) {
            return Err(IdError::InvalidSnapshot(tag.to_owned()));
        }
        Ok(Self {
            pack,
            tag: tag.to_owned(),
        })
    }

    pub fn pack(&self) -> &PackId {
        &self.pack
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    fn is_valid(tag: &str) -> bool {
        tag.chars()
            .all(|c| c.is_ascii_alphanumeric() || EXTRA_ID_CHARS.contains(&c))
    }
}

/// [`SnapshotId`] equality is a full equivalence relation.
impl Eq for SnapshotId {}

/// Prints the canonical form for [`SnapshotId`].
impl Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}:{}", self.pack, self.tag)
    }
}

/// Parses a [`SnapshotId`] from a canonical form string 'pack_name:snapshot_name'.
impl FromStr for SnapshotId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((pack, snapshot)) = s.trim_end().rsplit_once(':') {
            if pack.is_empty() {
                return Err(IdError::InvalidPack(pack.to_owned()));
            }
            if snapshot.is_empty() {
                return Err(IdError::InvalidSnapshot(snapshot.to_owned()));
            }

            Ok(SnapshotId {
                pack: PackId::from_str(pack)?,
                tag: snapshot.to_owned(),
            })
        } else {
            Err(IdError::BadFormat(s.to_owned()))
        }
    }
}

/// A magic constant that has no use other than to indicate a custom .pack header
/// stored in a Zstandard skippable frame.
const SKIPPABLE_MAGIC_MASK: u32 = 0x184D2A50;

/// Reads a Zstandard skippable frame from reader and writes the result to `buf`.
/// Returns the size of the frame, *not* the number of bytes read.
pub fn read_skippable_frame(mut reader: impl Read, buf: &mut Vec<u8>) -> io::Result<u64> {
    fn read_u32_le(mut reader: impl Read) -> io::Result<u32> {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    // Ensure this is a skippable frame.
    let magic = read_u32_le(&mut reader)?;
    if magic & SKIPPABLE_MAGIC_MASK != SKIPPABLE_MAGIC_MASK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a Zstandard skippable frame!",
        ));
    }

    let frame_size = read_u32_le(&mut reader)?;
    buf.resize(frame_size as usize, 0);
    reader.read_exact(buf)?;
    // Compute overall frame size.
    Ok((std::mem::size_of::<u32>() * 2 + buf.len()) as u64)
}

/// Writes a Zstandard skippable frame with the given user data.
/// Returns the number of bytes written to writer (the size of the frame, including the magic number and header).
pub fn write_skippable_frame(mut writer: impl Write, buf: &[u8]) -> io::Result<u64> {
    fn write_u32_le(mut writer: impl Write, value: u32) -> io::Result<()> {
        writer.write_all(&value.to_le_bytes())
    }

    // Ensure this is a skippable frame.
    write_u32_le(&mut writer, SKIPPABLE_MAGIC_MASK)?;
    write_u32_le(&mut writer, buf.len() as u32)?;
    writer.write_all(buf)?;
    // Compute overall frame size.
    Ok((std::mem::size_of::<u32>() * 2 + buf.len()) as u64)
}

/// The unidirectional stream of data stored in the pack.
enum PackReader {
    Compressed(Decoder<'static, BufReader<File>>),
}

impl PackReader {
    /// Consumes the specified number of bytes from the reader.
    fn seek(&mut self, bytes: u64) -> io::Result<()> {
        match self {
            Self::Compressed(decoder) => {
                io::copy(&mut decoder.by_ref().take(bytes), &mut io::sink()).map(|_| {})
            }
        }
    }
    // Reads the exact number of bytes into `buf`.
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Compressed(decoder) => decoder.read_exact(buf),
        }
    }
}

/// Represents a compressed Zstandard frame in the .pack file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackFrame {
    /// The size of the frame, in bytes.
    pub frame_size: u64,
    /// The size of the data stream in the frame, once decompressed, in bytes.
    pub decompressed_size: u64,
}

/// Represents the custom header in the beginning of a .pack file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackHeader {
    /// Valid pack headers have this value set to [`PACK_HEADER_MAGIC`].
    magic: u64,
    /// The list of frames in the pack file, sorted by their byte offsets.
    frames: Vec<PackFrame>,
}

impl PackHeader {
    /// Create a new pack header.
    pub fn new(frames: Vec<PackFrame>) -> Self {
        Self {
            magic: PACK_HEADER_MAGIC,
            frames,
        }
    }
    /// Verifies the header magic.
    pub fn is_valid(&self) -> bool {
        self.magic == PACK_HEADER_MAGIC
    }
}

impl Default for PackHeader {
    fn default() -> Self {
        Self {
            magic: PACK_HEADER_MAGIC,
            frames: vec![],
        }
    }
}

/// Represents an pack file.
pub struct Pack {
    /// The base filename ([`Path::file_stem`]) of the pack.
    name: String,
    /// The index for the pack.
    index: PackIndex,
    /// The header of the pack.
    header: PackHeader,
    /// PackReader instances for each frame in the .pack file.
    frame_readers: Vec<PackReader>,
    /// The file size of the pack (in bytes).
    file_size: u64,
}

impl Pack {
    /// Opens a pack file and its corresponding index.
    ///
    /// # Arguments
    ///
    /// * `pack_name` - The base filename ([`Path::file_stem`]) of the pack.
    pub fn open<P>(repo: P, pack_name: &PackId) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        let PackId::Pack(pack_name) = pack_name;
        let mut packs_data = repo.as_ref().join(&*Repository::data_dir());
        packs_data.push(PACKS_DIR);

        let mut pack_index_path = packs_data.join(pack_name);
        pack_index_path.set_extension(PACK_INDEX_EXTENSION);

        let mut pack_path = packs_data.join(pack_name);
        pack_path.set_extension(PACK_EXTENSION);

        info!("Reading pack index {:?}...", pack_index_path);
        let pack_index = PackIndex::load(&pack_index_path)?;

        info!("Opening pack file {:?}...", pack_path);

        let (file_size, header, frame_readers) =
            Self::open_pack(&pack_path).or_else(|_| Self::open_pack_legacy(&pack_path))?;

        Ok(Pack {
            name: pack_name.to_owned(),
            index: pack_index,
            header,
            frame_readers,
            file_size,
        })
    }

    /// Opens the pack file for reading.
    fn open_pack(pack_path: &Path) -> Result<(u64, PackHeader, Vec<PackReader>), Error> {
        let file = open_file(&pack_path)?;
        let file_size = file.metadata()?.len();
        let mut reader = io::BufReader::new(file);

        let mut header = vec![];
        let header_size = read_skippable_frame(&mut reader, &mut header)?;
        let header: PackHeader =
            rmp_serde::decode::from_read(&header[..]).map_err(|_| Error::CorruptPack)?;

        drop(reader);

        if !header.is_valid() {
            return Err(Error::CorruptPack);
        }

        let frame_offsets = compute_frame_offsets(&header.frames);
        let frame_readers = frame_offsets
            .iter()
            .map(|offset| -> Result<_, Error> {
                let mut reader = open_file(&pack_path)?;
                io::Seek::seek(&mut reader, io::SeekFrom::Start(header_size + offset))?;
                let mut reader = Decoder::new(reader)?;
                reader.set_parameter(DParameter::WindowLogMax(DEFAULT_WINDOW_LOG_MAX))?;
                // Wrap in a `PackReader`.
                let reader = PackReader::Compressed(reader);
                Ok(reader)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok((file_size, header, frame_readers))
    }

    /// Backwards-compatible open_pack for the legacy pack format (no skippable frame/no header).
    fn open_pack_legacy(pack_path: &Path) -> Result<(u64, PackHeader, Vec<PackReader>), Error> {
        let file = open_file(&pack_path)?;
        let file_size = file.metadata()?.len();

        // This manufactured pack header works for the current implementation. We might
        // change how/whether we support the legacy pack format in the future...
        let header = PackHeader::new(vec![PackFrame {
            frame_size: file_size,
            decompressed_size: u64::MAX,
        }]);

        let mut reader = Decoder::new(file)?;
        reader.set_parameter(DParameter::WindowLogMax(DEFAULT_WINDOW_LOG_MAX))?;
        let frame_reader = PackReader::Compressed(reader);

        Ok((file_size, header, vec![frame_reader]))
    }

    /// The base filename ([`Path::file_stem`]) of the pack.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A reference to the pack index.
    pub fn index(&self) -> &PackIndex {
        &self.index
    }

    /// The size of the pack in bytes.
    pub fn file_size(&self) -> u64 {
        self.file_size
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
    #[allow(unused_mut)]
    #[allow(clippy::needless_collect)]
    pub(crate) fn extract_entries<P>(
        mut self,
        entries: &[FileEntry],
        output_dir: P,
        verify: bool,
        num_workers: u32,
    ) -> Result<(), Error>
    where
        P: AsRef<Path> + Sync,
    {
        if entries.is_empty() {
            return Ok(());
        }

        let num_frames = self.header.frames.len();
        assert_ne!(0, num_workers);
        assert_ne!(0, num_frames);
        if num_frames < num_workers as usize {
            info!(
                "Requested {} workers, but there are only {} frames!",
                num_workers, num_frames
            );
        }
        let num_workers = std::cmp::min(num_workers, num_frames as u32);

        // Assign entries to the frames they reside in.
        // The resulting entries will have offsets relative to their containing frame.
        let frame_to_entries = assign_to_frames(&self.header.frames, entries)?;

        // Compute and log total amount of seeking and decompression needed.
        let bytes_to_decompress = frame_to_entries
            .iter()
            .flat_map(|entries| {
                entries
                    .iter()
                    .map(|e| e.metadata.offset + e.metadata.offset)
                    .max()
            })
            .sum::<u64>();
        info!(
            "Decompressing {:.3} MiB of data...",
            bytes_to_decompress as f64 / 1024f64 / 1024f64
        );

        // Collect required for run_in_parallel ExactSizeIterator argument.
        let tasks = self
            .frame_readers
            .into_iter()
            .zip(frame_to_entries.into_iter())
            // Skip empty frames.
            .filter(|(_, entries)| !entries.is_empty())
            .collect::<Vec<_>>();

        // Record start time
        let start_time = std::time::Instant::now();
        let results = run_in_parallel(
            num_workers as usize,
            tasks.into_iter(),
            |(frame_reader, entries)| extract_files(frame_reader, &entries, &output_dir, verify),
        );
        // Collect stats
        let stats = results
            .into_iter()
            .sum::<Result<ExtractStats, Error>>()?
            // Convert the statistics into fractions, since summing the time per thread doesn't make much sense.
            .fractions();
        // Log statistics about the decompression performance
        let real_time = std::time::Instant::now() - start_time;
        info!(
            "Decompression statistics ({:?})\n\
            \tSeeking: {:.1}%\n\
            \tObject decompression: {:.1}%\n\
            \tVerification: {:.1}%\n\
            \tWriting to disk: {:.1}%\n\
            \tOther: {:.1}%",
            real_time,
            stats.seek_time * 100f64,
            stats.object_time * 100f64,
            stats.verify_time * 100f64,
            stats.write_time * 100f64,
            stats.other_time() * 100f64,
        );
        Ok(())
    }
}

/// Verifies that the object has the expected checksum.
fn verify_object(buf: &[u8], exp_checksum: &ObjectChecksum) -> Result<(), Error> {
    // Verify checksum
    let mut checksum = [0u8; 20];
    let mut hasher = Sha1::new();
    hasher.input(buf);
    hasher.result(&mut checksum);
    if &checksum != exp_checksum {
        return Err(PackError::ChecksumMismatch(*exp_checksum, checksum).into());
    }
    Ok(())
}

/// Writes the object to the specified path, taking care
/// of adjusting file permissions.
fn write_object(buf: &[u8], path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut f = File::create(path)?;
    f.write_all(buf)?;
    Ok(())
}

/// Returns a list of the frame offsets, computed
/// using the order and sizes of the given frames.
fn compute_frame_offsets(frames: &[PackFrame]) -> Vec<u64> {
    let mut frame_offsets: Vec<_> = vec![0; frames.len()];
    for i in 1..frame_offsets.len() {
        frame_offsets[i] = frames[i - 1].frame_size + frame_offsets[i - 1];
    }
    frame_offsets
}

/// Returns a list of the data offsets, computed using the order and
/// decompressed sizes of the given frames.
fn compute_frame_decompressed_offset(frames: &[PackFrame]) -> Vec<u64> {
    let mut frame_decompressed_offset: Vec<_> = vec![0; frames.len()];
    for i in 1..frame_decompressed_offset.len() {
        frame_decompressed_offset[i] =
            frames[i - 1].decompressed_size + frame_decompressed_offset[i - 1];
    }
    frame_decompressed_offset
}

/// Groups and transforms the list of [`FileEntry`]-s taken from a pack index
/// (and with absolute offsets into the decompressed stream) into sets of
/// entries per frame, with adjusted (relative) offsets to that corresponding
/// Zstandard frame. Objects are assumed to not be split across two frames.
fn assign_to_frames(
    frames: &[PackFrame],
    entries: &[FileEntry],
) -> Result<Vec<Vec<FileEntry>>, Error> {
    let frame_decompressed_offset: Vec<_> = compute_frame_decompressed_offset(frames);

    // Figure out frame belonging of the objects,
    // using the frame offset and the object offset.
    let mut frames: Vec<Vec<FileEntry>> = (0..frames.len()).map(|_| Vec::new()).collect();
    for entry in entries {
        let frame_index = frame_decompressed_offset
            .iter()
            // Find the index of the frame containing the object (objects are
            // assumed to not be split across two frames)
            .rposition(|&x| x <= entry.metadata.offset)
            .ok_or(Error::CorruptPack)?;
        // Compute the offset relative to that frame.
        let local_offset = entry.metadata.offset - frame_decompressed_offset[frame_index];
        let local_entry = FileEntry::new(
            entry.path.clone(),
            entry.checksum,
            ObjectMetadata {
                offset: local_offset, // Replace global offset -> local offset
                size: entry.metadata.size,
            },
        );
        frames[frame_index].push(local_entry);
    }

    Ok(frames)
}

/// Used for timing the different parts of the extraction process.
#[derive(Default)]
struct ExtractStats {
    total_time: f64,
    seek_time: f64,
    object_time: f64,
    verify_time: f64,
    write_time: f64,
}

impl ExtractStats {
    fn other_time(&self) -> f64 {
        self.total_time - (self.seek_time + self.object_time + self.verify_time + self.write_time)
    }
    /// Convert the statistics into fractions, taken relative to `self.total_time`.
    fn fractions(&self) -> Self {
        let norm_factor = 1f64 / self.total_time;
        Self {
            total_time: norm_factor * self.total_time,
            seek_time: norm_factor * self.seek_time,
            object_time: norm_factor * self.object_time,
            verify_time: norm_factor * self.verify_time,
            write_time: norm_factor * self.write_time,
        }
    }
}

impl std::ops::Add for ExtractStats {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            total_time: self.total_time + other.total_time,
            seek_time: self.seek_time + other.seek_time,
            object_time: self.object_time + other.object_time,
            verify_time: self.verify_time + other.verify_time,
            write_time: self.write_time + other.write_time,
        }
    }
}

impl std::iter::Sum<ExtractStats> for ExtractStats {
    fn sum<I>(iter: I) -> Self
    where
        I: Iterator<Item = ExtractStats>,
    {
        let mut acc = ExtractStats::default();
        for x in iter {
            acc = acc + x;
        }
        acc
    }
}

/// Extracts the given entries from the pack reader into the specified output directory.
/// Checksum verification can be toggled on/off.
fn extract_files(
    mut reader: PackReader,
    entries: &[FileEntry],
    output_dir: impl AsRef<Path>,
    verify: bool,
) -> Result<ExtractStats, Error> {
    let mut entries: Vec<FileEntry> = entries.to_vec();
    // Sort objects to allow for forward-only seeking
    entries.sort_by(|x, y| {
        let offset_x = x.metadata.offset;
        let offset_y = y.metadata.offset;
        offset_x.cmp(&offset_y)
    });

    // Used for timing
    let mut stats = ExtractStats::default();
    let total_time = measure_ok(|| -> Result<(), Error> {
        // Decompression buffer
        let mut buf = vec![];
        let mut path_buf = PathBuf::new();
        let mut pos = 0;
        for entry in entries {
            let metadata = &entry.metadata;
            // Seek forward
            let discard_bytes = metadata.offset - pos;
            // Check if we need to read a new object.
            // The current position in stream can be AFTER the object offset only
            // if the previous and this object are the same. This is because the objects
            // are sorted by offset, and the current position is set to the offset at the
            // end of each object, after that object is consumed.
            if pos <= metadata.offset {
                stats.seek_time += measure_ok(|| reader.seek(discard_bytes))?.0.as_secs_f64();
                // Resize buf
                buf.resize(metadata.size as usize, 0);
                // Read object
                stats.object_time += measure_ok(|| reader.read_exact(&mut buf[..]))?
                    .0
                    .as_secs_f64();
                pos = metadata.offset + metadata.size;
                if verify {
                    stats.verify_time += measure_ok(|| verify_object(&buf[..], &entry.checksum))?
                        .0
                        .as_secs_f64();
                }
            }
            // Output path
            path_buf.clear();
            path_buf.push(&output_dir);
            path_buf.push(&entry.path);
            stats.write_time += measure_ok(|| write_object(&buf[..], &path_buf))?
                .0
                .as_secs_f64();
        }

        Ok(())
    })?
    .0;
    stats.total_time = total_time.as_secs_f64();

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_md(offset: u64, size: u64) -> ObjectMetadata {
        ObjectMetadata { offset, size }
    }

    #[test]
    fn pack_id_validation_works() {
        // VALID
        assert!(PackId::is_valid("ABCD"));
        assert!(PackId::is_valid("abcd"));
        assert!(PackId::is_valid("____"));
        assert!(PackId::is_valid("----"));
        assert!(PackId::is_valid("ABCD-132_TAG"));
        // NOT VALID
        // spaces
        assert!(!PackId::is_valid("Some Text"));
        // non-latin alphabets
        assert!(!PackId::is_valid("това-е-тест"));
        // non-letter symbols other than - and _
        assert!(!PackId::is_valid("QWERTY-^!$^%^@!#"));
    }
    #[test]
    fn snapshot_tag_validation_works() {
        // VALID
        assert!(SnapshotId::is_valid("ABCD"));
        assert!(SnapshotId::is_valid("abcd"));
        assert!(SnapshotId::is_valid("____"));
        assert!(SnapshotId::is_valid("----"));
        assert!(SnapshotId::is_valid("ABCD-132_TAG"));
        // NOT VALID
        // spaces
        assert!(!SnapshotId::is_valid("Some Text"));
        // non-latin alphabets
        assert!(!SnapshotId::is_valid("това-е-тест"));
        // non-letter symbols other than - and _
        assert!(!SnapshotId::is_valid("QWERTY-^!$^%^@!#"));
    }
    #[test]
    fn assign_to_frames_single_works() {
        let frames = [PackFrame {
            frame_size: 100,
            decompressed_size: 1000,
        }];
        let entries = [
            FileEntry::new("A".into(), [0; 20], make_md(50, 1)),
            FileEntry::new("B".into(), [1; 20], make_md(50, 1)),
        ];
        let result = assign_to_frames(&frames, &entries).unwrap();
        assert_eq!(1, result.len());
        assert_eq!(&entries, result[0].as_slice());
    }
    #[test]
    fn assign_to_frames_multiple_works() {
        let frames = [
            PackFrame {
                frame_size: 100,
                decompressed_size: 1000,
            },
            PackFrame {
                frame_size: 100,
                decompressed_size: 1000,
            },
        ];
        let entries = [
            FileEntry::new("A".into(), [0; 20], make_md(800, 200)),
            FileEntry::new("B".into(), [1; 20], make_md(1200, 200)),
        ];
        let frame_1_entries = [
            // Offset is same
            FileEntry::new("A".into(), [0; 20], make_md(800, 200)),
        ];
        let frame_2_entries = [
            // Offset 1200 -> 200
            FileEntry::new("B".into(), [1; 20], make_md(200, 200)),
        ];
        let result = assign_to_frames(&frames, &entries).unwrap();
        assert_eq!(2, result.len());
        assert_eq!(&frame_1_entries, result[0].as_slice());
        assert_eq!(&frame_2_entries, result[1].as_slice());
    }
}
