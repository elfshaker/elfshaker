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
    UNPACKED_ID,
};
use super::error::Error;
use super::fs::{ensure_file_writable, set_readonly};
use super::repository::Repository;
use crate::log::measure_ok;
use crate::packidx::{ObjectChecksum, ObjectIndex, PackEntry, PackError, PackIndex};

/// Pack and snapshots IDs can contain latin letter, digits or the following characters.
const EXTRA_ID_CHARS: &[char] = &['-', '_'];

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
                "Invalid pack identifier '{}'! Latin letters, digits, - and _ are allowed!",
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackId {
    Packed(String),
    Unpacked,
}

impl PackId {
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
        if s == UNPACKED_ID {
            Ok(PackId::Unpacked)
        } else {
            Ok(PackId::Packed(s.to_owned()))
        }
    }
}

impl Display for PackId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            PackId::Packed(s) => write!(f, "{}", s),
            PackId::Unpacked => write!(f, "unpacked"),
        }
    }
}

impl std::convert::From<Option<&str>> for PackId {
    fn from(opt: Option<&str>) -> Self {
        match opt {
            Some(s) => Self::Packed(s.to_owned()),
            None => Self::Unpacked,
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

    pub fn unpacked(tag: &str) -> Result<Self, IdError> {
        if !Self::is_valid(tag) {
            return Err(IdError::InvalidSnapshot(tag.to_owned()));
        }
        Ok(Self {
            pack: PackId::Unpacked,
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
        write!(f, "{}/{}", self.pack, self.tag)
    }
}

/// Parses a [`SnapshotId`] from a canonical form string.
impl FromStr for SnapshotId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('/').collect();

        // Neither the snapshot pack filename nor the snapshot tag are
        // allowed to contain forward slashes.
        if parts.len() != 2 {
            return Err(IdError::BadFormat(s.to_owned()));
        }

        let pack = parts[0].to_owned();
        if pack.is_empty() {
            return Err(IdError::InvalidPack(pack));
        }
        // Whitespace at the end is ignored!
        let tag = parts[1].trim_end().to_owned();
        if tag.is_empty() {
            return Err(IdError::InvalidSnapshot(tag));
        }

        Ok(SnapshotId {
            pack: PackId::from_str(&pack)?,
            tag,
        })
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
        let pack_index = Self::parse_index(std::io::BufReader::new(File::open(pack_index_path)?))?;

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
        let file = File::open(&pack_path)?;
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
                let mut reader = File::open(&pack_path)?;
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
        let file = File::open(&pack_path)?;
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
    pub(crate) fn extract<P>(
        mut self,
        entries: &[PackEntry],
        output_dir: P,
        verify: bool,
    ) -> Result<(), Error>
    where
        P: AsRef<Path>,
    {
        if entries.is_empty() {
            return Ok(());
        }

        let num_frames = self.header.frames.len();
        assert_ne!(0, num_frames);

        // Assign entries to the frames they reside in.
        // The resulting entries will have offsets relative to their containing frame.
        let frames = assign_to_frames(&self.header.frames, entries)?;

        // Compute and log total amount of seeking and decompression needed.
        let bytes_to_decompress = frames
            .iter()
            .flat_map(|entries| {
                entries
                    .iter()
                    .map(|e| e.object_index().offset() + e.object_index().size())
                    .max()
            })
            .sum::<u64>();
        info!(
            "Decompressing {:.3} MiB of data...",
            bytes_to_decompress as f64 / 1024f64 / 1024f64
        );

        // Record start time
        let start_time = std::time::Instant::now();
        // Process the frames
        let results = self
            .frame_readers
            .into_iter()
            .zip(frames.into_iter())
            // Skip frames we are not interested in
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(frame_reader, entries)| {
                let output_dir: PathBuf = output_dir.as_ref().into();
                extract_files(frame_reader, &entries, output_dir, verify)
            });
        // Collect stats
        let stats = results.sum::<Result<ExtractStats, Error>>()?;
        // Log statistics about the decompression performance
        let real_time = std::time::Instant::now() - start_time;
        info!(
            "Decompression statistics ({:?})\n\tSeeking: {:.1}s\n\tObject decompression: {:.1}s\n\tVerification: {:.1}s\n\tWriting to disk: {:.1}s\n\tOther: {:.1}s",
            real_time, stats.seek_time, stats.object_time, stats.verify_time, stats.write_time, stats.other_time(),
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

impl fmt::Debug for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pack")
            .field("name", &self.name)
            .field("index", &self.index)
            .finish()
    }
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

/// Returns a list of the data offsets, computed
/// using the order and decompressed sizes of the given frames.
fn compute_data_offsets(frames: &[PackFrame]) -> Vec<u64> {
    let mut data_offsets: Vec<_> = vec![0; frames.len()];
    for i in 1..data_offsets.len() {
        data_offsets[i] = frames[i - 1].decompressed_size + data_offsets[i - 1];
    }
    data_offsets
}

/// Groups and transforms the list of [`PackEntry`]-s taken from a pack index (and with absolute offsets into the decompressed stream)
/// into sets of entries per frame, with adjusted (relative) offsets to that corresponding Zstandard frame.
/// Objects are assumed to not be split across two frames.
fn assign_to_frames(
    frames: &[PackFrame],
    entries: &[PackEntry],
) -> Result<Vec<Vec<PackEntry>>, Error> {
    let data_offsets: Vec<_> = compute_data_offsets(frames);

    // Figure out frame belonging of the objects,
    // using the frame offset and the object offset.
    let mut frames: Vec<Vec<PackEntry>> = (0..frames.len()).map(|_| vec![]).collect();
    for entry in entries {
        let frame_index = data_offsets
            .iter()
            // Find the index of the frame containing the object (objects are assumed to not be split across two frames)
            .rposition(|&x| x <= entry.object_index().offset())
            .ok_or(Error::CorruptPack)?;
        // Compute the offset relative to that frame.
        let local_offset = entry.object_index().offset() - data_offsets[frame_index];
        let local_entry = PackEntry::new(
            entry.path(),
            ObjectIndex::new(
                *entry.object_index().checksum(),
                // Replace the offset
                local_offset,
                entry.object_index().size(),
            ),
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
    entries: &[PackEntry],
    output_dir: impl AsRef<Path>,
    verify: bool,
) -> Result<ExtractStats, Error> {
    let mut entries: Vec<PackEntry> = entries.to_vec();
    // Sort objects to allow for forward-only seeking
    entries.sort_by(|x, y| {
        let offset_x = x.object_index().offset();
        let offset_y = y.object_index().offset();
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
            let path = entry.path();
            let object = entry.object_index();
            // Seek forward
            let discard_bytes = object.offset() - pos;
            // Check if we need to read a new object.
            // The current position in stream can be AFTER the object offset only
            // if the previous and this object are the same. This is because the objects
            // are sorted by offset, and the current position is set to the offset at the
            // end of each object, after that object is consumed.
            if pos <= object.offset() {
                stats.seek_time += measure_ok(|| reader.seek(discard_bytes))?.0.as_secs_f64();
                // Resize buf
                buf.resize(object.size() as usize, 0);
                // Read object
                stats.object_time += measure_ok(|| reader.read_exact(&mut buf[..]))?
                    .0
                    .as_secs_f64();
                pos = object.offset() + object.size();
                if verify {
                    stats.verify_time += measure_ok(|| verify_object(&buf[..], object.checksum()))?
                        .0
                        .as_secs_f64();
                }
            }
            // Output path
            path_buf.clear();
            path_buf.push(&output_dir);
            path_buf.push(path);
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

    #[test]
    fn unpacked_snapshot_id_parses() {
        assert_eq!(
            SnapshotId::unpacked("my-snapshot").unwrap(),
            SnapshotId::from_str("unpacked/my-snapshot").unwrap()
        );
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
            PackEntry::new("A".as_ref(), ObjectIndex::new([0; 20], 50, 200)),
            PackEntry::new("B".as_ref(), ObjectIndex::new([1; 20], 50, 200)),
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
            PackEntry::new("A".as_ref(), ObjectIndex::new([0; 20], 800, 200)),
            PackEntry::new("B".as_ref(), ObjectIndex::new([1; 20], 1200, 200)),
        ];
        let frame_1_entries = [
            // Offset is same
            PackEntry::new("A".as_ref(), ObjectIndex::new([0; 20], 800, 200)),
        ];
        let frame_2_entries = [
            // Offset 1200 -> 200
            PackEntry::new("B".as_ref(), ObjectIndex::new([1; 20], 200, 200)),
        ];
        let result = assign_to_frames(&frames, &entries).unwrap();
        assert_eq!(2, result.len());
        assert_eq!(&frame_1_entries, result[0].as_slice());
        assert_eq!(&frame_2_entries, result[1].as_slice());
    }
}
