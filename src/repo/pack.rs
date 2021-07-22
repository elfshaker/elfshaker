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

use super::constants::{PACKS_DIR, PACK_EXTENSION, PACK_INDEX_EXTENSION, UNPACKED_ID};
use super::error::Error;
use super::fs::{ensure_file_writable, set_readonly};
use super::repository::Repository;
use crate::log::measure_ok;
use crate::packidx::{ObjectChecksum, PackEntry, PackError, PackIndex};

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

/// Represents an pack file.
pub struct Pack {
    /// The base filename ([`Path::file_stem`]) of the pack.
    name: String,
    /// The index for the pack.
    index: PackIndex,
    /// The unidirectional stream of data stored in the pack.
    reader: Decoder<'static, BufReader<File>>,
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
        let reader = File::open(pack_path)?;
        let file_size = reader.metadata()?.len();

        let mut reader = Decoder::new(reader)?;
        // 2^30 = 1024MiB window log
        reader.set_parameter(DParameter::WindowLogMax(30))?;

        Ok(Pack {
            name: pack_name.to_owned(),
            index: pack_index,
            reader,
            file_size,
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
        let mut indices: Vec<_> = (0..entries.len()).collect();
        // Sort objects to allow for forward-only seeking
        indices.sort_by(|x, y| {
            let offset_x = entries[*x].object_index().offset();
            let offset_y = entries[*y].object_index().offset();
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
                let discard_bytes = object.offset() - pos;
                // Check if we need to read a new object.
                // The current position in stream can be AFTER the object offset only
                // if the previous and this object are the same. This is because the objects
                // are sorted by offset, and the current position is set to the offset at the
                // end of each object, after that object is consumed.
                if pos <= object.offset() {
                    seek_time += measure_ok(|| self.seek(discard_bytes))?.0;
                    // Resize buf
                    if buf.len() < object.size() as usize {
                        buf.resize(object.size() as usize, 0);
                    }
                    // Read object
                    object_time +=
                        measure_ok(|| self.reader.read_exact(&mut buf[..object.size() as usize]))?
                            .0;
                    pos = object.offset() + object.size();
                    if verify {
                        verify_time += measure_ok(|| {
                            Self::verify_object(&buf[..object.size() as usize], object.checksum())
                        })?
                        .0;
                    }
                }
                // Output path
                path_buf.clear();
                path_buf.push(&output_dir);
                path_buf.push(path);
                write_time +=
                    measure_ok(|| Self::write_object(&buf[..object.size() as usize], &path_buf))?.0;
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

impl fmt::Debug for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pack")
            .field("name", &self.name)
            .field("index", &self.index)
            .finish()
    }
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
}
