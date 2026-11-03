//! Lossless parsing and editing of elfshaker remote indexes.

use rand::Rng;
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    convert::TryInto,
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions, Permissions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    str::FromStr,
};
use url::Url;

pub type Checksum = [u8; 20];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetaVersion {
    V1,
    V2,
}

impl MetaVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

impl fmt::Display for MetaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MetaVersion {
    type Err = FormatError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "v1" => Ok(Self::V1),
            "v2" => Ok(Self::V2),
            _ => Err(FormatError::new(format!(
                "Unsupported ESI meta version '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub index_checksum: Checksum,
    pub pack_checksum: Checksum,
    /// The URL spelling stored in the ESI document.
    pub url: String,
    pub pack_size: Option<u64>,
    pub index_size: Option<u64>,
}

impl Entry {
    fn canonical_line(&self, meta: MetaVersion) -> Result<Vec<u8>, FormatError> {
        validate_field_value(&self.url, "pack URL")?;
        let mut line = format!(
            "{}\t{}\t{}",
            hex::encode(self.index_checksum),
            hex::encode(self.pack_checksum),
            self.url
        );
        if meta == MetaVersion::V2 {
            let pack_size = self
                .pack_size
                .ok_or_else(|| FormatError::new(format!("Missing pack size for '{}'", self.url)))?;
            let index_size = self.index_size.ok_or_else(|| {
                FormatError::new(format!("Missing pack index size for '{}'", self.url))
            })?;
            line.push_str(&format!("\t{pack_size}\t{index_size}"));
        }
        Ok(line.into_bytes())
    }
}

#[derive(Clone, Debug)]
enum LineKind {
    Meta,
    Url,
    Blank,
    Entry(Entry),
}

#[derive(Clone, Debug)]
struct Line {
    content: Vec<u8>,
    ending: Vec<u8>,
    kind: LineKind,
}

#[derive(Clone, Debug)]
pub struct Document {
    lines: Vec<Line>,
    meta: MetaVersion,
    url: String,
}

impl Document {
    pub fn create(meta: MetaVersion, url: String) -> Result<Self, FormatError> {
        validate_index_url(&url)?;
        Ok(Self {
            lines: vec![
                Line {
                    content: format!("meta\t{meta}").into_bytes(),
                    ending: b"\n".to_vec(),
                    kind: LineKind::Meta,
                },
                Line {
                    content: format!("url\t{url}").into_bytes(),
                    ending: b"\n".to_vec(),
                    kind: LineKind::Url,
                },
            ],
            meta,
            url,
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut lines = split_lines(bytes);
        if lines.len() < 2 {
            return Err(FormatError::new(
                "Expected ESI headers 'meta ...' and 'url ...'",
            ));
        }

        let meta_value = parse_header(&lines[0].content, "meta", 1)?;
        let meta = MetaVersion::from_str(meta_value)?;
        lines[0].kind = LineKind::Meta;

        let url = parse_header(&lines[1].content, "url", 2)?.to_owned();
        let base_url = validate_index_url(&url)?;
        lines[1].kind = LineKind::Url;

        let mut urls = HashSet::new();
        for (index, line) in lines.iter_mut().enumerate().skip(2) {
            let line_no = index + 1;
            if line
                .content
                .iter()
                .all(|byte| *byte == b' ' || *byte == b'\t')
            {
                line.kind = LineKind::Blank;
                continue;
            }

            let entry = parse_entry(&line.content, meta, &base_url, line_no)?;
            if !urls.insert(entry.url.clone()) {
                return Err(FormatError::new(format!(
                    "Duplicate pack URL '{}' on line {line_no}",
                    entry.url
                )));
            }
            line.kind = LineKind::Entry(entry);
        }

        Ok(Self { lines, meta, url })
    }

    pub fn meta(&self) -> MetaVersion {
        self.meta
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.lines.iter().filter_map(|line| match &line.kind {
            LineKind::Entry(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn resolved_entry_url(&self, entry: &Entry) -> Result<Url, FormatError> {
        let base = validate_index_url(&self.url)?;
        validate_field_value(&entry.url, "pack URL")?;
        base.join(&entry.url).map_err(|_| {
            FormatError::new(format!("Expected a valid pack URL, found '{}'", entry.url))
        })
    }

    pub fn add(&mut self, entry: Entry, force: bool) -> Result<(), FormatError> {
        validate_field_value(&entry.url, "pack URL")?;
        validate_index_url(&self.url)?
            .join(&entry.url)
            .map_err(|_| {
                FormatError::new(format!("Expected a valid pack URL, found '{}'", entry.url))
            })?;
        // Validate the canonical representation before changing the document.
        let canonical = entry.canonical_line(self.meta)?;
        let existing = self.lines.iter().position(
            |line| matches!(&line.kind, LineKind::Entry(current) if current.url == entry.url),
        );

        if let Some(index) = existing {
            if !force {
                return Err(FormatError::new(format!(
                    "Pack URL '{}' already exists",
                    entry.url
                )));
            }
            self.lines[index].content = canonical;
            self.lines[index].kind = LineKind::Entry(entry);
            return Ok(());
        }

        let ending = self.preferred_line_ending();
        let ended_with_newline = self
            .lines
            .last()
            .map(|line| !line.ending.is_empty())
            .unwrap_or(false);
        if !ended_with_newline {
            if let Some(last) = self.lines.last_mut() {
                last.ending = ending.clone();
            }
        }
        self.lines.push(Line {
            content: canonical,
            ending: if ended_with_newline {
                ending
            } else {
                Vec::new()
            },
            kind: LineKind::Entry(entry),
        });
        Ok(())
    }

    pub fn remove(&mut self, url: &str, force: bool) -> Result<(), FormatError> {
        let existing = self
            .lines
            .iter()
            .position(|line| matches!(&line.kind, LineKind::Entry(entry) if entry.url == url));
        match existing {
            Some(index) => {
                self.lines.remove(index);
                Ok(())
            }
            None if force => Ok(()),
            None => Err(FormatError::new(format!("Pack URL '{url}' does not exist"))),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let capacity = self
            .lines
            .iter()
            .map(|line| line.content.len() + line.ending.len())
            .sum();
        let mut result = Vec::with_capacity(capacity);
        for line in &self.lines {
            result.extend_from_slice(&line.content);
            result.extend_from_slice(&line.ending);
        }
        result
    }

    fn preferred_line_ending(&self) -> Vec<u8> {
        self.lines
            .iter()
            .find_map(|line| {
                if line.ending.is_empty() {
                    None
                } else {
                    Some(line.ending.clone())
                }
            })
            .unwrap_or_else(|| b"\n".to_vec())
    }
}

fn split_lines(bytes: &[u8]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let (content_end, ending) = if index > start && bytes[index - 1] == b'\r' {
            (index - 1, b"\r\n".as_slice())
        } else {
            (index, b"\n".as_slice())
        };
        lines.push(Line {
            content: bytes[start..content_end].to_vec(),
            ending: ending.to_vec(),
            kind: LineKind::Blank,
        });
        start = index + 1;
    }
    if start < bytes.len() {
        lines.push(Line {
            content: bytes[start..].to_vec(),
            ending: Vec::new(),
            kind: LineKind::Blank,
        });
    }
    lines
}

fn fields(content: &[u8], line_no: usize) -> Result<Vec<&str>, FormatError> {
    let text = std::str::from_utf8(content)
        .map_err(|_| FormatError::new(format!("Invalid UTF-8 on line {line_no}")))?;
    Ok(text
        .split([' ', '\t'])
        .filter(|part| !part.is_empty())
        .collect())
}

fn parse_header<'a>(
    content: &'a [u8],
    expected_key: &str,
    line_no: usize,
) -> Result<&'a str, FormatError> {
    let fields = fields(content, line_no)?;
    if fields.first().copied() != Some(expected_key) {
        return Err(FormatError::new(format!(
            "Expected '{expected_key} ...' on line {line_no}"
        )));
    }
    match fields.as_slice() {
        [_, value] => Ok(value),
        [.., _] if fields.len() > 2 => Err(FormatError::new(format!(
            "Too many values on line {line_no}"
        ))),
        _ => Err(FormatError::new(format!(
            "Expected a value after '{expected_key}' on line {line_no}"
        ))),
    }
}

fn parse_entry(
    content: &[u8],
    meta: MetaVersion,
    base_url: &Url,
    line_no: usize,
) -> Result<Entry, FormatError> {
    let fields = fields(content, line_no)?;
    let expected = if meta == MetaVersion::V1 { 3 } else { 5 };
    if fields.len() < expected {
        return Err(FormatError::new(format!(
            "Expected {expected} values on line {line_no}, found {}",
            fields.len()
        )));
    }
    if fields.len() > expected {
        return Err(FormatError::new(format!(
            "Too many values on line {line_no}"
        )));
    }

    let index_checksum = parse_checksum(fields[0], "pack index", line_no)?;
    let pack_checksum = parse_checksum(fields[1], "pack", line_no)?;
    let url = fields[2].to_owned();
    validate_field_value(&url, "pack URL")?;
    base_url.join(&url).map_err(|_| {
        FormatError::new(format!(
            "Expected a valid pack URL, found '{url}' on line {line_no}"
        ))
    })?;

    let (pack_size, index_size) = if meta == MetaVersion::V2 {
        (
            Some(parse_size(fields[3], "pack", line_no)?),
            Some(parse_size(fields[4], "pack index", line_no)?),
        )
    } else {
        (None, None)
    };

    Ok(Entry {
        index_checksum,
        pack_checksum,
        url,
        pack_size,
        index_size,
    })
}

fn parse_checksum(value: &str, description: &str, line_no: usize) -> Result<Checksum, FormatError> {
    let decoded = hex::decode(value).map_err(|_| {
        FormatError::new(format!(
            "Bad {description} checksum format on line {line_no}"
        ))
    })?;
    decoded.try_into().map_err(|_| {
        FormatError::new(format!(
            "The {description} checksum on line {line_no} is not 20 bytes"
        ))
    })
}

fn parse_size(value: &str, description: &str, line_no: usize) -> Result<u64, FormatError> {
    value.parse().map_err(|_| {
        FormatError::new(format!(
            "Invalid {description} size '{value}' on line {line_no}"
        ))
    })
}

fn validate_index_url(value: &str) -> Result<Url, FormatError> {
    validate_field_value(value, "elfshaker index URL")?;
    Url::parse(value).map_err(|_| {
        FormatError::new(format!(
            "Expected a valid elfshaker index URL, found {value}"
        ))
    })
}

fn validate_field_value(value: &str, description: &str) -> Result<(), FormatError> {
    if value.is_empty() {
        return Err(FormatError::new(format!(
            "The {description} must not be empty"
        )));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(FormatError::new(format!(
            "The {description} must not contain literal whitespace; percent-encode it instead"
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub struct FormatError {
    message: String,
}

impl FormatError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FormatError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileDigest {
    pub checksum: Checksum,
    pub size: u64,
}

/// Computes a file's SHA-1 and size through a bounded streaming buffer.
pub fn digest_file(path: &Path) -> io::Result<FileDigest> {
    let mut reader = BufReader::with_capacity(64 * 1024, File::open(path)?);
    let mut hasher = Sha1::new();
    let mut size = 0u64;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        size = size
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| io::Error::other("file size overflow"))?;
        hasher.update(buffer);
        let consumed = buffer.len();
        reader.consume(consumed);
    }
    Ok(FileDigest {
        checksum: hasher.finalize().into(),
        size,
    })
}

#[derive(Debug)]
pub struct DocumentWrite {
    pub path: PathBuf,
    pub contents: Vec<u8>,
    pub backup: bool,
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Stages every output before atomically publishing any of them. If an
/// ordinary rename fails, already-published targets and backups are restored.
pub fn commit_documents(writes: Vec<DocumentWrite>) -> io::Result<()> {
    commit_documents_with(writes, replace_file)
}

fn commit_documents_with<F>(writes: Vec<DocumentWrite>, mut replace: F) -> io::Result<()>
where
    F: FnMut(&Path, &Path) -> io::Result<()>,
{
    validate_document_writes(&writes)?;

    let mut pending = Vec::with_capacity(writes.len());
    for write in writes {
        let original = read_existing(&write.path)?;
        let permissions = original.as_ref().map(|state| state.permissions.clone());
        let replacement = StagedFile::new(&write.path, &write.contents, permissions.as_ref())?;
        let original_restore = original
            .as_ref()
            .map(|state| StagedFile::new(&write.path, &state.bytes, Some(&state.permissions)))
            .transpose()?;

        let backup = if write.backup {
            if let Some(original) = original.as_ref() {
                let path = backup_path(&write.path);
                let previous = read_existing(&path)?;
                let permissions = previous
                    .as_ref()
                    .map(|state| &state.permissions)
                    .or(Some(&original.permissions));
                Some(PendingBackup {
                    replacement: StagedFile::new(&path, &original.bytes, permissions)?,
                    restore: previous
                        .as_ref()
                        .map(|state| StagedFile::new(&path, &state.bytes, Some(&state.permissions)))
                        .transpose()?,
                    existed: previous.is_some(),
                    path,
                    published: false,
                })
            } else {
                None
            }
        } else {
            None
        };

        pending.push(PendingWrite {
            path: write.path,
            replacement,
            restore: original_restore,
            existed: original.is_some(),
            published: false,
            backup,
        });
    }

    for index in 0..pending.len() {
        let result = (|| {
            let item = &mut pending[index];
            if let Some(backup) = item.backup.as_mut() {
                backup.replacement.publish(&backup.path, &mut replace)?;
                backup.published = true;
            }
            item.replacement.publish(&item.path, &mut replace)?;
            item.published = true;
            Ok(())
        })();
        if let Err(error) = result {
            let rollback = rollback(&mut pending, &mut replace);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(io::Error::other(format!(
                    "{error}; rollback failed: {rollback_error}"
                ))),
            };
        }
    }

    Ok(())
}

pub fn validate_document_writes(writes: &[DocumentWrite]) -> io::Result<()> {
    let mut seen = HashSet::new();
    for write in writes {
        if !seen.insert(path_identity(&write.path)?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate ESI target '{}'", write.path.display()),
            ));
        }
    }
    for write in writes.iter().filter(|write| write.backup) {
        let backup = backup_path(&write.path);
        if !seen.insert(path_identity(&backup)?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ESI backup destination '{}' aliases an operation target or backup",
                    backup.display()
                ),
            ));
        }
    }
    Ok(())
}

fn path_identity(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            if path.is_absolute() {
                Ok(path.to_owned())
            } else {
                Ok(std::env::current_dir()?.join(path))
            }
        } else {
            Err(error)
        }
    })
}

fn rollback<F>(pending: &mut [PendingWrite], replace: &mut F) -> io::Result<()>
where
    F: FnMut(&Path, &Path) -> io::Result<()>,
{
    let mut first_error = None;
    for item in pending.iter_mut().rev() {
        if item.published {
            let result = if let Some(restore) = item.restore.as_mut() {
                restore.publish(&item.path, replace)
            } else if item.existed {
                Ok(())
            } else {
                remove_if_exists(&item.path)
            };
            if first_error.is_none() {
                first_error = result.err();
            }
        }
        if let Some(backup) = item.backup.as_mut() {
            if backup.published {
                let result = if let Some(restore) = backup.restore.as_mut() {
                    restore.publish(&backup.path, replace)
                } else if backup.existed {
                    Ok(())
                } else {
                    remove_if_exists(&backup.path)
                };
                if first_error.is_none() {
                    first_error = result.err();
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct ExistingFile {
    bytes: Vec<u8>,
    permissions: Permissions,
}

fn read_existing(path: &Path) -> io::Result<Option<ExistingFile>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(ExistingFile {
            bytes,
            permissions: fs::metadata(path)?.permissions(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

struct PendingWrite {
    path: PathBuf,
    replacement: StagedFile,
    restore: Option<StagedFile>,
    existed: bool,
    published: bool,
    backup: Option<PendingBackup>,
}

struct PendingBackup {
    path: PathBuf,
    replacement: StagedFile,
    restore: Option<StagedFile>,
    existed: bool,
    published: bool,
}

struct StagedFile {
    path: PathBuf,
}

impl StagedFile {
    fn new(target: &Path, contents: &[u8], permissions: Option<&Permissions>) -> io::Result<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let (path, mut file) = create_staging_file(parent)?;
        file.write_all(contents)?;
        file.sync_data()?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions.clone())?;
        }
        Ok(Self { path })
    }

    fn publish<F>(&mut self, target: &Path, replace: &mut F) -> io::Result<()>
    where
        F: FnMut(&Path, &Path) -> io::Result<()>,
    {
        replace(&self.path, target)?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_staging_file(parent: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..100 {
        let mut random = [0u8; 16];
        rand::rng().fill_bytes(&mut random);
        let path = parent.join(format!(".elfshaker-esi-{}", hex::encode(random)));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique ESI staging file",
    ))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::winbase::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checksum(byte: u8) -> Checksum {
        [byte; 20]
    }

    fn entry(url: &str) -> Entry {
        Entry {
            index_checksum: checksum(0xaa),
            pack_checksum: checksum(0xbb),
            url: url.to_owned(),
            pack_size: Some(12),
            index_size: Some(34),
        }
    }

    fn temp_dir() -> PathBuf {
        let path = crate::repo::fs::create_temp_path(&std::env::temp_dir());
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn parses_v1_and_v2_relative_and_absolute_urls() {
        let v1 = format!(
            "meta v1\nurl https://example.test/releases/index.esi\n{} {} relative.pack\n",
            "aa".repeat(20),
            "bb".repeat(20)
        );
        let document = Document::parse(v1.as_bytes()).unwrap();
        let item = document.entries().next().unwrap();
        assert_eq!(
            document.resolved_entry_url(item).unwrap().as_str(),
            "https://example.test/releases/relative.pack"
        );
        assert_eq!(item.pack_size, None);

        let v2 = format!(
            "meta\tv2\nurl\thttps://example.test/index.v2.esi\n{}\t{}\thttps://cdn.test/a.pack\t5\t6\n",
            "cc".repeat(20),
            "dd".repeat(20)
        );
        let document = Document::parse(v2.as_bytes()).unwrap();
        let item = document.entries().next().unwrap();
        assert_eq!(
            document.resolved_entry_url(item).unwrap().as_str(),
            "https://cdn.test/a.pack"
        );
        assert_eq!((item.pack_size, item.index_size), (Some(5), Some(6)));
    }

    #[test]
    fn rejects_unknown_versions_and_bad_fields() {
        let cases = [
            "meta v3\nurl https://example.test/index.esi\n".to_owned(),
            format!(
                "meta v1\nurl https://example.test/index.esi\n{} {}\n",
                "aa".repeat(20),
                "bb".repeat(20)
            ),
            format!(
                "meta v2\nurl https://example.test/index.v2.esi\n{} {} a.pack 1\n",
                "aa".repeat(20),
                "bb".repeat(20)
            ),
            format!(
                "meta v1\nurl https://example.test/index.esi\nbad {} a.pack\n",
                "bb".repeat(20)
            ),
            format!(
                "meta v1\nurl https://example.test/index.esi\n{} {} a.pack\n",
                "a".repeat(39),
                "bb".repeat(20)
            ),
            format!(
                "meta v2\nurl https://example.test/index.v2.esi\n{} {} a.pack nope 2\n",
                "aa".repeat(20),
                "bb".repeat(20)
            ),
            format!(
                "meta v1\nurl https://example.test/index.esi\n{} {} a.pack extra\n",
                "aa".repeat(20),
                "bb".repeat(20)
            ),
        ];
        for case in cases {
            assert!(Document::parse(case.as_bytes()).is_err(), "{}", case);
        }
    }

    #[test]
    fn unchanged_documents_round_trip_byte_for_byte() {
        for input in [
            b"meta\tv1\r\nurl   https://example.test/index.esi\r\n\r\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  a.pack".as_slice(),
            b"meta v2\nurl\thttps://example.test/index.v2.esi\n\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb a.pack 1 2\n".as_slice(),
        ] {
            assert_eq!(Document::parse(input).unwrap().to_bytes(), input);
        }
    }

    #[test]
    fn adds_replaces_and_removes_without_touching_other_lines() {
        let input = format!(
            "meta  v1\r\nurl\thttps://example.test/index.esi\r\n\r\n{}\t{}\ta.pack\r\n{}\t{}\tb.pack",
            "01".repeat(20),
            "02".repeat(20),
            "03".repeat(20),
            "04".repeat(20)
        );
        let mut document = Document::parse(input.as_bytes()).unwrap();
        document.add(entry("c.pack"), false).unwrap();
        let added = String::from_utf8(document.to_bytes()).unwrap();
        assert!(added.starts_with(&input));
        assert!(added.ends_with(&format!(
            "\r\n{}\t{}\tc.pack",
            "aa".repeat(20),
            "bb".repeat(20)
        )));

        assert!(document.add(entry("a.pack"), false).is_err());
        document.add(entry("a.pack"), true).unwrap();
        let replaced = String::from_utf8(document.to_bytes()).unwrap();
        assert!(replaced.contains(&format!(
            "\r\n{}\t{}\ta.pack\r\n",
            "aa".repeat(20),
            "bb".repeat(20)
        )));
        assert!(replaced.contains(&format!(
            "\r\n{}\t{}\tb.pack\r\n",
            "03".repeat(20),
            "04".repeat(20)
        )));

        assert!(document.remove("missing.pack", false).is_err());
        document.remove("missing.pack", true).unwrap();
        document.remove("b.pack", false).unwrap();
        assert!(!String::from_utf8(document.to_bytes())
            .unwrap()
            .contains("b.pack"));
    }

    #[test]
    fn v2_adds_sizes_and_duplicate_documents_are_rejected() {
        let mut document = Document::create(
            MetaVersion::V2,
            "https://example.test/index.v2.esi".to_owned(),
        )
        .unwrap();
        document.add(entry("a.pack"), false).unwrap();
        assert!(String::from_utf8(document.to_bytes())
            .unwrap()
            .ends_with("a.pack\t12\t34\n"));

        let duplicate = format!(
            "meta v1\nurl https://example.test/index.esi\n{} {} a.pack\n{} {} a.pack\n",
            "aa".repeat(20),
            "bb".repeat(20),
            "cc".repeat(20),
            "dd".repeat(20)
        );
        assert!(Document::parse(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn duplicate_and_removal_matching_use_exact_stored_urls() {
        let mut document = Document::create(
            MetaVersion::V1,
            "https://example.test/releases/index.esi".to_owned(),
        )
        .unwrap();
        document.add(entry("a.pack"), false).unwrap();
        document
            .add(entry("https://example.test/releases/a.pack"), false)
            .unwrap();
        assert_eq!(document.entries().count(), 2);
        assert!(document.remove("./a.pack", false).is_err());
        document.remove("a.pack", false).unwrap();
        assert_eq!(
            document.entries().next().unwrap().url,
            "https://example.test/releases/a.pack"
        );
    }

    #[test]
    fn rejects_field_values_that_cannot_be_serialized_losslessly() {
        assert!(Document::create(MetaVersion::V1, String::new()).is_err());
        assert!(Document::create(
            MetaVersion::V1,
            "https://example.test/index with-space.esi".to_owned()
        )
        .is_err());

        let mut document =
            Document::create(MetaVersion::V1, "https://example.test/index.esi".to_owned()).unwrap();
        assert!(document.add(entry(""), false).is_err());
        assert!(document.add(entry("pack with-space.pack"), false).is_err());
        assert_eq!(
            document.to_bytes(),
            b"meta\tv1\nurl\thttps://example.test/index.esi\n"
        );
        document
            .add(entry("pack%20with-space.pack"), false)
            .unwrap();
        Document::parse(&document.to_bytes()).unwrap();
    }

    #[test]
    fn rejects_non_ascii_literal_whitespace_in_parsed_pack_urls() {
        let input = format!(
            "meta v1\nurl https://example.test/index.esi\n{} {} pack\u{a0}name.pack\n",
            "aa".repeat(20),
            "bb".repeat(20)
        );
        let error = Document::parse(input.as_bytes()).unwrap_err();
        assert!(error.message().contains("literal whitespace"));
    }

    #[test]
    fn digest_file_computes_known_sha1_and_size() {
        let dir = temp_dir();
        let path = dir.join("small");
        fs::write(&path, b"abc").unwrap();
        let digest = digest_file(&path).unwrap();
        assert_eq!(
            hex::encode(digest.checksum),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(digest.size, 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_second_publish_restores_first_target_and_backups() {
        let dir = temp_dir();
        let first = dir.join("first.esi");
        let second = dir.join("second.esi");
        fs::write(&first, b"old-first").unwrap();
        fs::write(&second, b"old-second").unwrap();
        fs::write(backup_path(&first), b"older-first").unwrap();

        let writes = vec![
            DocumentWrite {
                path: first.clone(),
                contents: b"new-first".to_vec(),
                backup: true,
            },
            DocumentWrite {
                path: second.clone(),
                contents: b"new-second".to_vec(),
                backup: true,
            },
        ];
        let mut failed = false;
        let result = commit_documents_with(writes, |source, target| {
            if target == second && !failed {
                failed = true;
                return Err(io::Error::other("injected replacement failure"));
            }
            replace_file(source, target)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&first).unwrap(), b"old-first");
        assert_eq!(fs::read(&second).unwrap(), b"old-second");
        assert_eq!(fs::read(backup_path(&first)).unwrap(), b"older-first");
        assert!(!backup_path(&second).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_destinations_cannot_alias_another_target() {
        let dir = temp_dir();
        let first = dir.join("foo.esi");
        let second = backup_path(&first);
        fs::write(&first, b"old-first").unwrap();
        fs::write(&second, b"old-second").unwrap();

        let result = commit_documents(vec![
            DocumentWrite {
                path: first.clone(),
                contents: b"new-first".to_vec(),
                backup: true,
            },
            DocumentWrite {
                path: second.clone(),
                contents: b"new-second".to_vec(),
                backup: true,
            },
        ]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&first).unwrap(), b"old-first");
        assert_eq!(fs::read(&second).unwrap(), b"old-second");
        assert!(!backup_path(&second).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
