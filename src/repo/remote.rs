use std::{
    convert::TryInto,
    ffi::OsStr,
    fs, io,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{offset::Utc, DateTime};
use sha1::{Digest, Sha1};
use ureq::Agent;
use url::Url;

use super::constants::{PACK_EXTENSION, REMOTE_INDEX_EXTENSION};
use super::error::Error;
use super::fs::{create_file, create_temp_path, open_file, write_file_atomic};
use crate::packidx::{ObjectChecksum, PackIndex};
use crate::progress::{ProgressReporter, ProgressWriter};

const HTTP_STATUS_OK: u16 = 200;
const HTTP_STATUS_NOT_MODIFIED: u16 = 304;

/// The .esi file is corrupted.
#[derive(Debug)]
pub struct RemoteIndexFormatError {
    message: String,
    source: Option<String>,
}

impl RemoteIndexFormatError {
    fn new(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }

    fn reify<D: std::fmt::Display>(self, display_name: D) -> Self {
        if self.source.is_none() {
            self
        } else {
            RemoteIndexFormatError {
                message: self.message,
                source: Some(format!("{display_name}")),
            }
        }
    }
}

impl std::error::Error for RemoteIndexFormatError {}

impl std::fmt::Display for RemoteIndexFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} (source={})",
            self.message,
            self.source.as_deref().unwrap_or("(unspecified)")
        )
    }
}

trait ReifyRemoteResult {
    fn reify<D: std::fmt::Display>(self, display_name: D) -> Self;
}

impl<T> ReifyRemoteResult for Result<T, Error> {
    fn reify<D: std::fmt::Display>(self, display_name: D) -> Result<T, Error> {
        match self {
            Err(Error::BadRemoteIndexFormat(e)) => {
                Err(Error::BadRemoteIndexFormat(e.reify(display_name)))
            }
            _ => self,
        }
    }
}

#[derive(Debug)]
pub struct RemotePack {
    pub index_checksum: ObjectChecksum,
    pub pack_checksum: ObjectChecksum,
    pub url: String,
}

impl RemotePack {
    pub fn file_name(&self) -> &str {
        self.url.rsplit_once('/').unwrap().1
    }
}

#[derive(Debug)]
pub struct RemoteIndex {
    // Set when open by load().
    path: Option<PathBuf>,
    #[allow(dead_code)]
    meta: String,
    url: String,
    packs: Vec<RemotePack>,
}

impl RemoteIndex {
    #[allow(dead_code)]
    pub fn new(url: String) -> RemoteIndex {
        Self {
            path: None,
            meta: "v1".to_owned(),
            url,
            packs: vec![],
        }
    }

    #[allow(dead_code)]
    pub fn packs(&self) -> &[RemotePack] {
        &self.packs
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<RemoteIndex, Error> {
        let file = open_file(path.as_ref()).map_err(Error::IOError)?;
        let reader = BufReader::new(file);
        let mut remote = Self::read(reader).reify(path.as_ref().display())?;
        remote.path = Some(path.as_ref().into());
        Ok(remote)
    }

    pub fn find_pack(&self, pack_name: &str) -> Option<&RemotePack> {
        let file_name = pack_name.to_owned() + "." + PACK_EXTENSION;
        self.packs.iter().find(|p| p.file_name() == file_name)
    }

    pub fn read<R: BufRead>(reader: R) -> Result<RemoteIndex, Error> {
        let mut lines = reader.lines();
        let mut line_no = 1;
        let meta = Self::read_keyed_line(&mut lines, "meta")?;
        line_no += 1;
        let url = Self::read_keyed_line(&mut lines, "url")?;
        let base_url = url.parse::<Url>().map_err(|_| {
            RemoteIndexFormatError::new(format!(
                "Expected a valid elfshaker index URL, found {url} on line {line_no}"
            ))
        })?;
        line_no += 1;

        let mut packs = vec![];
        for line in lines {
            line_no += 1;
            let line = line.map_err(Error::IOError)?;
            if line.is_empty() {
                continue;
            }
            let mut parts = line
                .split(Self::is_field_separator)
                .filter(|&p| !p.is_empty());
            let index_checksum = parts.next().ok_or_else(|| {
                RemoteIndexFormatError::new(format!(
                    "Expected pack index checksum, reached end of line {line_no}"
                ))
            })?;

            let pack_checksum = parts.next().ok_or_else(|| {
                RemoteIndexFormatError::new(format!(
                    "Expected pack checksum, reached end of line {line_no}"
                ))
            })?;

            // Pack URL (possibly relative)
            let relative_url = parts.next().ok_or_else(|| {
                RemoteIndexFormatError::new(format!(
                    "Expected pack URL, reached end of line {line_no}"
                ))
            })?;

            // Always provide the index URL as base URL. If the pack URL is
            // absolute, the `join` will use it as-is.
            let absolute_url = match base_url.join(relative_url) {
                Ok(url) => url,
                Err(_) => {
                    return Err(RemoteIndexFormatError::new(format!(
                        "Expected a valid pack URL, found {url} on line {line_no}"
                    ))
                    .into());
                }
            };

            if parts.next().is_some() {
                return Err(RemoteIndexFormatError::new(format!(
                    "Too many values on line {line_no}"
                ))
                .into());
            }

            let index_checksum = hex::decode(index_checksum)
                .map_err(|_| {
                    RemoteIndexFormatError::new(format!(
                        "Bad pack index checksum format on line {line_no}"
                    ))
                })?
                .try_into()
                .map_err(|_| {
                    RemoteIndexFormatError::new(format!(
                        "The value for pack index checksum on line {line_no} is not the right length"
                    ))
                })?;

            let pack_checksum = hex::decode(pack_checksum)
                .map_err(|_| {
                    RemoteIndexFormatError::new(format!(
                        "Bad pack checksum format on line {line_no}"
                    ))
                })?
                .try_into()
                .map_err(|_| {
                    RemoteIndexFormatError::new(format!(
                        "The value for pack checksum on line {line_no} is not the right length"
                    ))
                })?;

            packs.push(RemotePack {
                url: absolute_url.as_str().to_owned(),
                index_checksum,
                pack_checksum,
            });
        }

        Ok(Self {
            path: None,
            meta,
            url,
            packs,
        })
    }

    fn read_keyed_line<R: BufRead>(lines: &mut io::Lines<R>, key: &str) -> Result<String, Error> {
        let line = match lines.next() {
            None => Err(RemoteIndexFormatError::new(format!(
                "Expected '{key} ...', but end of file was reached!"
            ))
            .into()),
            Some(Err(e)) => Err(Error::IOError(e)),
            Some(Ok(line)) => Ok(line),
        }?;

        let mut parts = line
            .split(Self::is_field_separator)
            .filter(|&p| !p.is_empty());

        if parts.next() != Some(key) {
            return Err(
                RemoteIndexFormatError::new(format!("Expected '{key} ...': '{line}'")).into(),
            );
        }

        let value = parts.next().ok_or_else(|| {
            RemoteIndexFormatError::new(format!("Expected a value after '{key}': '{line}'"))
        })?;

        if parts.next().is_some() {
            return Err(RemoteIndexFormatError::new(format!("Too many values: '{line}'")).into());
        }

        Ok(value.to_string())
    }

    fn is_field_separator(ch: char) -> bool {
        ch == ' ' || ch == '\t'
    }
}

impl std::fmt::Display for RemoteIndex {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Some(stem) = self.path().and_then(|p| p.file_stem()) {
            write!(fmt, "{} ({})", stem.to_string_lossy(), self.url)?;
        } else {
            let stem = self.url.rsplit_once('/').unwrap().0;
            write!(fmt, "{} ({})", stem, self.url)?;
        }
        Ok(())
    }
}

/// Loads all remote index (.esi) files from the target directory.
pub fn load_remotes(base_path: &Path) -> Result<Vec<RemoteIndex>, Error> {
    let paths = fs::read_dir(base_path).map_err(Error::IOError)?;
    paths
        .filter_map(|e| e.ok())
        .filter(|p| p.path().extension() == Some(OsStr::new(REMOTE_INDEX_EXTENSION)))
        .map(|p| RemoteIndex::load(p.path()))
        .collect()
}

/// Sends an HTTP GET request with the specified URL.
///
/// # Arguments
/// * `url` - the URL to fetch
/// * `timeout` - the timeout for the whole of the request
/// * `if_modified_since` - Sets the value of the `If-Modified-Since` HTTP
///   header
fn open_remote_resource(
    agent: &Agent,
    url: &Url,
    timeout: Option<Duration>,
    if_modified_since: Option<SystemTime>,
) -> Result<Option<(usize, impl Read)>, Error> {
    let mut request = agent.get(url.as_ref());
    // Alternatively, we could have used Duration::MAX to indicate no timeout.
    // Unfortunately, the ureq crashes at some unwrap() somewhere then MAX is
    // provided.
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    if let Some(if_modified_since) = if_modified_since {
        request = request.set("If-Modified-Since", &format_http_date(if_modified_since));
    }

    let response = request.call().map_err(|e| Error::HttpError(e.into()))?;

    let status = response.status();

    let content_length = response
        .header("Content-Length")
        .unwrap_or("0")
        .parse::<usize>()
        .unwrap_or(0);

    log::info!(
        "HTTP GET {} -> {} (Content-Length: {})",
        url,
        status,
        content_length
    );

    match status {
        HTTP_STATUS_OK => Ok(Some((content_length, response.into_reader()))),
        HTTP_STATUS_NOT_MODIFIED => Ok(None),
        _ => Err(Error::HttpError(
            format!(
                "Response status code {} indicates failure!",
                response.status()
            )
            .into(),
        )),
    }
}

fn read_remote_resource(
    agent: &Agent,
    url: &Url,
    timeout: Duration,
    if_modified_since: Option<SystemTime>,
) -> Result<Option<Vec<u8>>, Error> {
    open_remote_resource(agent, url, Some(timeout), if_modified_since).and_then(|opt_reader| {
        opt_reader
            .map(|mut reader| {
                let mut body: Vec<u8> = vec![];
                reader
                    .1
                    .read_to_end(&mut body)
                    .map_err(|e| Error::HttpError(e.into()))
                    .map(|_| body)
            })
            .map_or(Ok(None), |v| v.map(Some))
    })
}

/// Downloads and verifies a payload before atomically making it available.
pub fn update_remote_pack(
    agent: &Agent,
    remote_pack: &RemotePack,
    pack_path: &Path,
    reporter: &ProgressReporter,
) -> Result<(), Error> {
    let url = remote_pack.url.parse::<Url>().unwrap();
    let (content_length, mut reader) =
        open_remote_resource(agent, &url, None, None)?.ok_or_else(|| {
            Error::HttpError("Unexpected HTTP 304 for an unconditional download".into())
        })?;
    let parent = pack_path.parent().unwrap();
    let temp_path = create_temp_path(parent);
    let result = (|| {
        let mut file = create_file(&temp_path, None)?;
        let mut writer = if content_length == 0 {
            ProgressWriter::new(&mut file, reporter)
        } else {
            ProgressWriter::with_known_size(&mut file, reporter, content_length)
        };
        io::copy(&mut reader, &mut writer)?;
        file.sync_all()?;
        if !verify_checksum(&temp_path, &remote_pack.pack_checksum)? {
            return Err(Error::CorruptPack);
        }
        fs::rename(&temp_path, pack_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

/// Fetches a manifest without using local mtimes as HTTP cache validators.
fn download_remote(agent: &Agent, url: &str, path: &Path) -> Result<(RemoteIndex, Vec<u8>), Error> {
    let url = url.parse::<Url>().map_err(|e| Error::HttpError(e.into()))?;
    let data =
        read_remote_resource(agent, &url, Duration::from_secs(15), None)?.ok_or_else(|| {
            Error::HttpError("Unexpected HTTP 304 for an unconditional download".into())
        })?;
    let mut remote = RemoteIndex::read(BufReader::new(data.as_slice())).reify(url)?;
    remote.path = Some(path.to_owned());
    Ok((remote, data))
}

/// Fetches and atomically installs a new remote registration.
pub fn fetch_remote(agent: &Agent, url: &str, path: &Path) -> Result<RemoteIndex, Error> {
    let (remote, data) = download_remote(agent, url, path)?;
    write_file_atomic(data.as_slice(), path.parent().unwrap(), path)?;
    Ok(remote)
}

/// Validates all candidate indexes before changing the installed catalog.
/// The caller must hold the repository's exclusive lock. The manifest is
/// installed last; readers reject indexes that do not match it after interruption.
pub fn update_remote(
    agent: &Agent,
    current: &RemoteIndex,
    base_dir: &Path,
    reporter: &ProgressReporter,
) -> Result<(), Error> {
    let path = current.path().expect("loaded remote has a path");
    let (remote, data) = download_remote(agent, &current.url, path)?;
    let mut indexes = Vec::new();
    for (done, pack) in remote.packs.iter().enumerate() {
        let index_path = base_dir.join(format!("{}.idx", pack.file_name()));
        if !verify_checksum(&index_path, &pack.index_checksum)? {
            let url = format!("{}.idx", pack.url).parse::<Url>().unwrap();
            let bytes = read_remote_resource(agent, &url, Duration::from_secs(15), None)?
                .ok_or_else(|| {
                    Error::HttpError("Unexpected HTTP 304 for an unconditional download".into())
                })?;
            let checksum: ObjectChecksum = Sha1::digest(&bytes).into();
            if checksum != pack.index_checksum {
                return Err(Error::CorruptPackIndex);
            }
            PackIndex::parse(bytes.as_slice())?;
            indexes.push((index_path, bytes));
        }
        reporter.checkpoint_with_detail(
            done + 1,
            Some(remote.packs.len() - done - 1),
            pack.file_name().to_owned(),
        );
    }

    // Check every cached payload, including caches made by older clients. Do
    // this even if the index hash is unchanged: payload hashes are independent.
    for pack in &remote.packs {
        let pack_path = base_dir.join(pack.file_name());
        if !verify_checksum(&pack_path, &pack.pack_checksum)? {
            match fs::remove_file(pack_path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
    }
    for (index_path, bytes) in indexes {
        write_file_atomic(bytes.as_slice(), base_dir, &index_path)?;
    }
    // On Unix, make invalidations and index renames durable before the new
    // manifest can become durable in its separate directory.
    #[cfg(unix)]
    fs::File::open(base_dir)?.sync_all()?;
    write_file_atomic(data.as_slice(), path.parent().unwrap(), path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// A convenience function which verifies the checksum of the file
/// and coerces ENOENT to false.
pub(super) fn verify_checksum(path: &Path, checksum: &ObjectChecksum) -> io::Result<bool> {
    match compute_checksum(path) {
        Ok(actual_checksum) => Ok(actual_checksum == *checksum),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn compute_checksum(path: &Path) -> io::Result<ObjectChecksum> {
    let mut reader = io::BufReader::new(fs::File::open(path)?);
    let mut sha1 = Sha1::new();
    loop {
        let buf = reader.fill_buf()?;
        let len = buf.len();
        if len == 0 {
            break;
        }
        sha1.update(buf);
        reader.consume(len);
    }

    Ok(sha1.finalize().into())
}

/// Formats a [`SystemTime`] as an HTTP date string. HTTP dates are always in
/// GMT, never in local time. The format is specified in RFC 5322.
///
/// HTTP date format: `<day-name>, <day> <month> <year> <hour>:<minute>:<second>
/// GMT`
fn format_http_date(t: SystemTime) -> String {
    let datetime: DateTime<Utc> = t.into();
    format!("{}", datetime.format("%a, %d %h %Y %H:%M:%S GMT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_http_date_works() {
        assert_eq!(
            format_http_date(SystemTime::UNIX_EPOCH),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }

    #[test]
    fn test_remote_index_read_works() -> Result<(), Error> {
        let r = RemoteIndex::read(BufReader::new(
            "\
meta\tv1
url   https://github.com/elfshaker/releases/download/index.esi
90765d432f15eda9b42e0ed747ceaa9b5f8237de 3fc6c1b427b19217cdd9c4eecf0c74943fa4adb2\thttps://gitlab.com/elfshaker/releases/download/A.pack
f3a50129b7ac872b63585f884f1a73e013d51f85\td8a41c1859d6276f0dd74fdd7e4513d89f68600f  B.pack"
                .as_bytes(),
        ))?;

        assert_eq!(r.meta, "v1");
        assert_eq!(
            r.url,
            "https://github.com/elfshaker/releases/download/index.esi"
        );
        assert_eq!(r.packs.len(), 2);
        assert_eq!(
            r.packs[0].url,
            "https://gitlab.com/elfshaker/releases/download/A.pack"
        );
        assert_eq!(
            r.packs[0].index_checksum.to_vec(),
            hex::decode(b"90765d432f15eda9b42e0ed747ceaa9b5f8237de").unwrap()
        );
        assert_eq!(
            r.packs[0].pack_checksum.to_vec(),
            hex::decode(b"3fc6c1b427b19217cdd9c4eecf0c74943fa4adb2").unwrap()
        );
        assert_eq!(
            r.packs[1].url,
            "https://github.com/elfshaker/releases/download/B.pack"
        );
        assert_eq!(
            r.packs[1].index_checksum.to_vec(),
            hex::decode(b"f3a50129b7ac872b63585f884f1a73e013d51f85").unwrap()
        );
        assert_eq!(
            r.packs[1].pack_checksum.to_vec(),
            hex::decode(b"d8a41c1859d6276f0dd74fdd7e4513d89f68600f").unwrap()
        );

        Ok(())
    }

    #[test]
    fn test_remote_index_no_packs_read_works() -> Result<(), Error> {
        let r = RemoteIndex::read(BufReader::new(
            "\
meta\tv1
url\thttps://github.com/elfshaker/releases/download/index.esi"
                .as_bytes(),
        ))?;

        assert_eq!(r.meta, "v1");
        assert_eq!(
            r.url,
            "https://github.com/elfshaker/releases/download/index.esi"
        );
        assert_eq!(r.packs.len(), 0);

        Ok(())
    }

    #[test]
    fn test_remote_index_bad_meta_read_fails() {
        let _no_meta = RemoteIndex::read(BufReader::new(
            "url\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .expect_err("no meta");
        let _no_meta_value = RemoteIndex::read(BufReader::new(
            "meta\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .expect_err("no meta value");
        let _bad_meta_delimiter = RemoteIndex::read(BufReader::new(
            "meta_v1\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .expect_err("no meta delimiter");
    }

    #[test]
    fn test_remote_index_bad_url_read_fails() {
        let _no_url = RemoteIndex::read(BufReader::new("meta\tv1".as_bytes())).unwrap_err();
        let _no_url_value =
            RemoteIndex::read(BufReader::new("meta\tv1\nurl".as_bytes())).unwrap_err();
        // The bad delimiter case is covered by the meta delimiter test.
    }

    #[test]
    fn test_remote_index_bad_checksum_fails() {
        let _bad_checksum = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nCH3kS0M\thttps://asd".as_bytes(),
        ))
        .unwrap_err();
    }

    #[test]
    fn test_remote_index_bad_pack_url_fails() {
        let _no_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85\t".as_bytes(),
        ))
        .unwrap_err();
        let _bad_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85\thttps://"
                .as_bytes(),
        ))
        .unwrap_err();
    }

    #[test]
    fn test_remote_index_bad_pack_delimiter_fails() {
        let _no_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85 https://".as_bytes(),
        ))
        .unwrap_err();
    }
}
