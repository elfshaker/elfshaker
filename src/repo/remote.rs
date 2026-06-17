use std::{
    ffi::OsStr,
    fs, io,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{offset::Utc, DateTime};
use sha1::{Digest, Sha1};
use ureq::Agent;
use url::Url;

use super::constants::{PACK_EXTENSION, REMOTE_INDEX_EXTENSION};
use super::error::Error;
use super::fs::{create_file, open_file};
use crate::esi::{Document, MetaVersion};
use crate::packidx::{ObjectChecksum, PackIndex};
use crate::progress::{ProgressReporter, ProgressWriter};

const HTTP_STATUS_OK: u16 = 200;
const HTTP_STATUS_NOT_MODIFIED: u16 = 304;
const HTTP_STATUS_NOT_FOUND: u16 = 404;

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
            RemoteIndexFormatError {
                message: self.message,
                source: Some(format!("{display_name}")),
            }
        } else {
            self
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
    /// Optional size of the pack index file, in bytes (v2 indices only)
    pub index_size: Option<u64>,
    /// Optional size of the pack file, in bytes (v2 indices only)
    pub pack_size: Option<u64>,
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
    meta: MetaVersion,
    url: String,
    packs: Vec<RemotePack>,
}

impl RemoteIndex {
    #[allow(dead_code)]
    pub fn new(url: String) -> RemoteIndex {
        Self {
            path: None,
            meta: MetaVersion::V1,
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

    pub fn name(&self) -> Option<String> {
        self.path()
            .and_then(|p| p.file_stem())
            .map(|s| (*s.to_string_lossy()).into())
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<RemoteIndex, Error> {
        let file = open_file(path.as_ref()).map_err(Error::IOError)?;
        let mut remote = Self::read(file).reify(path.as_ref().display())?;
        remote.path = Some(path.as_ref().into());
        Ok(remote)
    }

    pub fn find_pack(&self, pack_name: &str) -> Option<&RemotePack> {
        let file_name = pack_name.to_owned() + "." + PACK_EXTENSION;
        self.packs.iter().find(|p| p.file_name() == file_name)
    }

    pub fn read<R: Read>(mut reader: R) -> Result<RemoteIndex, Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(Error::IOError)?;
        let document = Document::parse(&bytes)
            .map_err(|error| RemoteIndexFormatError::new(error.to_string()))?;
        Self::from_document(document)
    }

    fn from_document(document: Document) -> Result<Self, Error> {
        let packs = document
            .entries()
            .map(|entry| {
                Ok(RemotePack {
                    url: document.resolved_entry_url(entry)?.into(),
                    index_checksum: entry.index_checksum,
                    pack_checksum: entry.pack_checksum,
                    pack_size: entry.pack_size,
                    index_size: entry.index_size,
                })
            })
            .collect::<Result<Vec<_>, crate::esi::FormatError>>()
            .map_err(|error| RemoteIndexFormatError::new(error.to_string()))?;

        Ok(Self {
            path: None,
            meta: document.meta(),
            url: document.url().to_owned(),
            packs,
        })
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

/// Builds an HTTP agent using explicit CA overrides or platform verification.
pub(crate) fn http_agent() -> Agent {
    let ca_file = std::env::var_os("SSL_CERT_FILE");
    let ca_dirs = std::env::var_os("SSL_CERT_DIR");
    let has_ca_dirs = ca_dirs
        .as_ref()
        .is_some_and(|paths| std::env::split_paths(paths).any(|path| !path.as_os_str().is_empty()));
    let roots = if ca_file.is_some() || has_ca_dirs {
        // Load explicit PEM locations on every platform, including macOS,
        // whose native verifier uses the Keychain rather than these variables.
        let mut loaded = rustls_native_certs::load_native_certs();
        if let Some(error) = loaded.errors.first() {
            log::warn!("Failed to load CA certificate overrides: {error}");
            // Invalid overrides must never fall back to platform roots. Empty
            // anchors reject HTTPS while leaving plain HTTP requests usable.
            loaded.certs.clear();
        } else if loaded.certs.is_empty() {
            log::warn!("No CA certificates loaded from SSL_CERT_FILE or SSL_CERT_DIR");
        }
        ureq::tls::RootCerts::Specific(std::sync::Arc::new(
            loaded
                .certs
                .iter()
                .map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned())
                .collect(),
        ))
    } else {
        ureq::tls::RootCerts::PlatformVerifier
    };
    Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build())
        .build()
        .into()
}

enum RemoteResource<R> {
    Found { content_length: usize, body: R },
    NotModified,
    NotFound,
}

/// Sends an HTTP GET request with the specified URL.
fn open_remote_resource(
    agent: &Agent,
    url: &Url,
    timeout: Option<Duration>,
    if_modified_since: Option<SystemTime>,
) -> Result<RemoteResource<Box<dyn Read>>, Error> {
    let mut request = agent.get(url.as_ref());
    request = request
        .config()
        .http_status_as_error(false)
        .timeout_global(timeout)
        .build();
    if let Some(if_modified_since) = if_modified_since {
        request = request.header("If-Modified-Since", &format_http_date(if_modified_since));
    }

    let response = request
        .call()
        .map_err(|error| Error::HttpError(error.into()))?;

    let status = response.status().as_u16();

    let content_length = response
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
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
        HTTP_STATUS_OK => Ok(RemoteResource::Found {
            content_length,
            body: Box::new(response.into_body().into_reader()),
        }),
        HTTP_STATUS_NOT_MODIFIED => Ok(RemoteResource::NotModified),
        HTTP_STATUS_NOT_FOUND => Ok(RemoteResource::NotFound),
        _ => Err(Error::HttpError(
            format!("Response status code {status} indicates failure!").into(),
        )),
    }
}

fn read_remote_resource(
    agent: &Agent,
    url: &Url,
    timeout: Duration,
    if_modified_since: Option<SystemTime>,
) -> Result<RemoteResource<Vec<u8>>, Error> {
    match open_remote_resource(agent, url, Some(timeout), if_modified_since)? {
        RemoteResource::Found {
            content_length,
            body,
        } => {
            let bytes = read_remote_body(body, content_length)?;
            Ok(RemoteResource::Found {
                content_length,
                body: bytes,
            })
        }
        RemoteResource::NotModified => Ok(RemoteResource::NotModified),
        RemoteResource::NotFound => Ok(RemoteResource::NotFound),
    }
}

fn read_remote_body<R: Read>(mut body: R, _content_length: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .map_err(|error| Error::HttpError(error.into()))?;
    Ok(bytes)
}

/// Updates the specified pack file by fetching the URL in the [`RemotePack`]
/// only when necessary.
pub fn update_remote_pack(
    agent: &Agent,
    remote_pack: &RemotePack,
    pack_path: &Path,
    reporter: &ProgressReporter,
) -> Result<(), Error> {
    let date_modified = fs::metadata(pack_path).ok().and_then(|x| x.modified().ok());

    let url = remote_pack.url.parse::<Url>().unwrap();
    match open_remote_resource(agent, &url, None, date_modified)? {
        RemoteResource::Found {
            content_length,
            mut body,
        } => {
            let mut data = vec![];

            let mut writer = ProgressWriter::with_known_size(&mut data, reporter, content_length);
            io::copy(&mut body, &mut writer)?;

            let mut hasher = Sha1::new();
            hasher.update(&data);
            let checksum: [u8; 20] = hasher.finalize().into();
            let size_matches = remote_pack
                .pack_size
                .map(|size| size == data.len() as u64)
                .unwrap_or(true);

            if checksum == remote_pack.pack_checksum && size_matches {
                create_file(pack_path, None)?.write_all(&data)?;
            } else {
                log::error!(
                    "The pack checksum or size did not match the value in the .esi! The download failed."
                );
                return Err(Error::CorruptPack);
            }
        }
        RemoteResource::NotModified => {}
        RemoteResource::NotFound => {
            return Err(Error::HttpError(
                format!("Response status code {HTTP_STATUS_NOT_FOUND} indicates failure!").into(),
            ));
        }
    }
    Ok(())
}

/// Updates all pack index files by fetching the URLs in the [`RemoteIndex`]
/// only when necessary.
pub fn update_remote_pack_indexes(
    agent: &Agent,
    remote: &RemoteIndex,
    base_dir: &Path,
    reporter: &ProgressReporter,
) -> Result<(), Error> {
    let mut done = 0usize;
    let mut remaining = remote.packs.len();

    for pack in &remote.packs {
        let url = (pack.url.to_string() + ".idx").parse::<Url>().unwrap();
        let pack_index_file_name = url.path_segments().unwrap().next_back().unwrap();
        let pack_index_path = base_dir.join(pack_index_file_name);

        if verify_checksum(&pack_index_path, &pack.index_checksum)? {
            // The file exists and the checksums match -> skip
            log::info!("{} is up to date", pack_index_path.display());
        } else {
            update_pack_index(agent, &url, &pack_index_path, pack.index_size)?;
        }

        done += 1;
        remaining -= 1;
        reporter.checkpoint_with_detail(done, Some(remaining), pack.file_name().to_owned());
    }
    Ok(())
}

/// Updates the pack index file by fetching its contents from the URL only
/// when the content at the URL is newer than what is available on-disk.
fn update_pack_index(
    agent: &Agent,
    url: &Url,
    pack_index_path: &Path,
    expected_size: Option<u64>,
) -> Result<(), Error> {
    let date_modified = fs::metadata(pack_index_path)
        .ok()
        .and_then(|x| x.modified().ok());

    match read_remote_resource(agent, url, Duration::from_secs(15), date_modified)? {
        RemoteResource::Found {
            body: pack_index_bytes,
            ..
        } => {
            if expected_size
                .map(|size| size != pack_index_bytes.len() as u64)
                .unwrap_or(false)
            {
                return Err(Error::CorruptPackIndex);
            }
            if let Err(e) = PackIndex::parse(pack_index_bytes.as_slice()) {
                log::error!(
                    "Failed to fetch {} from remote: The remote returned a broken .pack.idx! {}",
                    url,
                    e
                );
            } else {
                log::info!(
                    "Writing {} ({} B)...",
                    pack_index_path.display(),
                    pack_index_bytes.len()
                );
                fs::write(pack_index_path, pack_index_bytes.as_slice()).map_err(Error::IOError)?;
            }
        }
        RemoteResource::NotModified => {}
        RemoteResource::NotFound => {
            return Err(Error::HttpError(
                format!("Response status code {HTTP_STATUS_NOT_FOUND} indicates failure!").into(),
            ));
        }
    }
    Ok(())
}

/// Fetches the remote index from the server.
pub fn fetch_remote(agent: &Agent, url: &str, path: &Path) -> Result<RemoteIndex, Error> {
    let supplied_url = url
        .parse::<Url>()
        .map_err(|error| Error::HttpError(error.into()))?;
    let (fetched_url, data) = if let Some(versioned_url) = v2_sibling_url(&supplied_url) {
        match read_remote_resource(agent, &versioned_url, Duration::from_secs(15), None)? {
            RemoteResource::Found { body, .. } => (versioned_url, body),
            RemoteResource::NotFound => {
                let body = require_found(read_remote_resource(
                    agent,
                    &supplied_url,
                    Duration::from_secs(15),
                    None,
                )?)?;
                (supplied_url, body)
            }
            RemoteResource::NotModified => unreachable!(
                "Unexpected Not-Modified response from server given previously unseen resource"
            ),
        }
    } else {
        let body = require_found(read_remote_resource(
            agent,
            &supplied_url,
            Duration::from_secs(15),
            None,
        )?)?;
        (supplied_url, body)
    };

    install_remote(path, &fetched_url, data)
}

/// Fetches the new newest version of the [`RemoteIndex`] from the server and
/// overwrites its backing file only when the remote file is newer.
pub fn update_remote(agent: &Agent, remote: &RemoteIndex) -> Result<RemoteIndex, Error> {
    let path = remote
        .path
        .as_ref()
        .expect("The RemoteIndex must have a valid .path set! Use RemoteIndex::load().");

    let date_modified = fs::metadata(path).ok().and_then(|x| x.modified().ok());
    let url = remote
        .url
        .parse::<Url>()
        .map_err(|error| Error::HttpError(error.into()))?;

    if let Some(versioned_url) = v2_sibling_url(&url) {
        // The v2 probe is unconditional because the local mtime belongs to the
        // legacy resource, not the versioned sibling.
        match read_remote_resource(agent, &versioned_url, Duration::from_secs(15), None)? {
            RemoteResource::Found { body, .. } => {
                return install_remote(path, &versioned_url, body);
            }
            RemoteResource::NotFound => {}
            RemoteResource::NotModified => {
                unreachable!("Unexpected Not-Modified response from unconditional versioned probe")
            }
        }
    }

    match read_remote_resource(agent, &url, Duration::from_secs(15), date_modified)? {
        RemoteResource::Found { body, .. } => install_remote(path, &url, body),
        RemoteResource::NotModified => RemoteIndex::load(path).reify(path.display()),
        RemoteResource::NotFound => Err(Error::HttpError(
            format!("Response status code {HTTP_STATUS_NOT_FOUND} indicates failure!").into(),
        )),
    }
}

fn install_remote(path: &Path, fetched_url: &Url, data: Vec<u8>) -> Result<RemoteIndex, Error> {
    let mut remote = RemoteIndex::read(data.as_slice()).reify(fetched_url)?;
    validate_endpoint_version(fetched_url, remote.meta)?;
    create_file(path, None)
        .map_err(Error::IOError)?
        .write_all(data.as_slice())
        .map_err(Error::IOError)?;
    remote.path = Some(path.to_owned());
    Ok(remote)
}

fn require_found(resource: RemoteResource<Vec<u8>>) -> Result<Vec<u8>, Error> {
    match resource {
        RemoteResource::Found { body, .. } => Ok(body),
        RemoteResource::NotModified => unreachable!(
            "Unexpected Not-Modified response from server given previously unseen resource"
        ),
        RemoteResource::NotFound => Err(Error::HttpError(
            format!("Response status code {HTTP_STATUS_NOT_FOUND} indicates failure!").into(),
        )),
    }
}

fn validate_endpoint_version(url: &Url, meta: MetaVersion) -> Result<(), Error> {
    if is_versioned_endpoint(url) {
        match endpoint_version(url) {
            Some(expected) if expected == meta => {}
            Some(expected) => {
                return Err(RemoteIndexFormatError::new(format!(
                    "Endpoint '{url}' requires meta {expected}, found meta {meta}"
                ))
                .into());
            }
            None => {
                return Err(RemoteIndexFormatError::new(format!(
                    "Unsupported versioned ESI endpoint '{url}'"
                ))
                .into());
            }
        }
    } else if url.path().ends_with(".esi") && meta != MetaVersion::V1 {
        return Err(RemoteIndexFormatError::new(format!(
            "Unversioned endpoint '{url}' requires meta v1, found meta {meta}"
        ))
        .into());
    }
    Ok(())
}

fn endpoint_version(url: &Url) -> Option<MetaVersion> {
    let file_name = url.path_segments()?.next_back()?;
    let stem = file_name.strip_suffix(".esi")?;
    let (_, version) = stem.rsplit_once(".v")?;
    match version {
        "1" => Some(MetaVersion::V1),
        "2" => Some(MetaVersion::V2),
        _ => None,
    }
}

fn v2_sibling_url(url: &Url) -> Option<Url> {
    if is_versioned_endpoint(url) {
        return None;
    }
    let path = url.path();
    let stem = path.strip_suffix(".esi")?;
    let mut sibling = url.clone();
    sibling.set_path(&format!("{stem}.v2.esi"));
    Some(sibling)
}

fn is_versioned_endpoint(url: &Url) -> bool {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|file_name| file_name.strip_suffix(".esi"))
        .and_then(|stem| stem.rsplit_once(".v"))
        .map(|(_, version)| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
        .unwrap_or(false)
}

/// A convenience function which verifies the checksum of the file
/// and coerces ENOENT to false.
fn verify_checksum(path: &Path, checksum: &ObjectChecksum) -> io::Result<bool> {
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
mod remote_index_tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn test_format_http_date_works() {
        assert_eq!(
            format_http_date(SystemTime::UNIX_EPOCH),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }

    #[test]
    fn remote_body_does_not_reserve_untrusted_content_length() {
        let bytes = read_remote_body(Cursor::new(b"body"), usize::MAX).unwrap();
        assert_eq!(bytes, b"body");
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

        assert_eq!(r.meta, MetaVersion::V1);
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

        assert_eq!(r.meta, MetaVersion::V1);
        assert_eq!(
            r.url,
            "https://github.com/elfshaker/releases/download/index.esi"
        );
        assert_eq!(r.packs.len(), 0);

        Ok(())
    }

    // Helper: create a 20-byte (40 hex chars) checksum string of repeated ch.
    fn hex20(ch: char) -> String {
        std::iter::repeat_n(ch, 40).collect()
    }

    #[test]
    fn parse_v1_no_sizes() {
        let idx = format!(
            "meta v1\n\
                 url\thttp://example.com/index.esi\n\
                 {ic} {pc} pack1.pack\n",
            ic = hex20('a'),
            pc = hex20('b'),
        );
        let remote = RemoteIndex::read(Cursor::new(idx)).unwrap();
        assert_eq!(remote.meta, MetaVersion::V1);
        assert_eq!(remote.packs.len(), 1);
        let p = &remote.packs[0];
        // v1 should not set sizes
        assert_eq!(p.index_size, None);
        assert_eq!(p.pack_size, None);
        // URL should join base path
        assert_eq!(p.url, "http://example.com/pack1.pack");
    }

    #[test]
    fn parse_v2_with_sizes() {
        let idx = format!(
            "meta v2\n\
                 url http://example.org/path/index.esi\n\
                 {ic}\t{pc}\t../p/pk.pack\t100\t200\n",
            ic = hex20('c'),
            pc = hex20('d'),
        );
        let remote = RemoteIndex::read(Cursor::new(idx)).unwrap();
        assert_eq!(remote.meta, MetaVersion::V2);
        assert_eq!(remote.packs.len(), 1);
        let p = &remote.packs[0];
        // sizes parsed correctly
        assert_eq!(p.pack_size, Some(100));
        assert_eq!(p.index_size, Some(200));
        // relative URL should resolve correctly
        assert_eq!(p.url, "http://example.org/p/pk.pack");
    }

    #[test]
    fn parse_too_many_values_v2() {
        let idx = format!(
            "meta v2\nurl http://example.org/index.esi\n{ic} {pc} pack.pack 1 2 extra\n",
            ic = hex20('e'),
            pc = hex20('f'),
        );
        let err = RemoteIndex::read(Cursor::new(idx)).unwrap_err();
        match err {
            Error::BadRemoteIndexFormat(e) => {
                // Expect message about too many values
                assert!(e.message.contains("Too many values"));
            }
            other => panic!("Unexpected error variant: {:?}", other),
        }
    }

    #[test]
    fn test_remote_index_v1_read_works() -> Result<(), Error> {
        let input = concat!(
            "meta\tv1\n",
            "url\thttp://example.com/index.esi\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\t",
            "http://example.com/A.pack\t",
        );
        let r = RemoteIndex::read(BufReader::new(input.as_bytes()))?;
        // One pack entry parsed
        assert_eq!(r.packs.len(), 1);
        let p = &r.packs[0];
        // Checksums
        assert_eq!(
            p.index_checksum.to_vec(),
            hex::decode("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
        );
        assert_eq!(
            p.pack_checksum.to_vec(),
            hex::decode("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap()
        );
        // URL
        assert_eq!(p.url, "http://example.com/A.pack");
        // Sizes
        assert_eq!(p.pack_size, None);
        assert_eq!(p.index_size, None);
        Ok(())
    }

    #[test]
    fn test_remote_index_v2_read_works() -> Result<(), Error> {
        let input = concat!(
            "meta\tv2\n",
            "url\thttp://example.com/index.esi\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\t",
            "http://example.com/A.pack\t",
            "1234\t",
            "567\n"
        );
        let r = RemoteIndex::read(BufReader::new(input.as_bytes()))?;
        // One pack entry parsed
        assert_eq!(r.packs.len(), 1);
        let p = &r.packs[0];
        // Checksums
        assert_eq!(
            p.index_checksum.to_vec(),
            hex::decode("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
        );
        assert_eq!(
            p.pack_checksum.to_vec(),
            hex::decode("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap()
        );
        // URL
        assert_eq!(p.url, "http://example.com/A.pack");
        // Sizes
        assert_eq!(p.pack_size, Some(1234));
        assert_eq!(p.index_size, Some(567));
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

    #[test]
    fn derives_v2_only_for_unversioned_esi_urls() {
        let unversioned = Url::parse("https://example.test/releases/index.esi?token=yes").unwrap();
        assert_eq!(
            v2_sibling_url(&unversioned).unwrap().as_str(),
            "https://example.test/releases/index.v2.esi?token=yes"
        );

        for url in [
            "https://example.test/index.v1.esi",
            "https://example.test/index.v2.esi",
            "https://example.test/index.v12.esi",
            "https://example.test/index",
        ] {
            assert!(v2_sibling_url(&Url::parse(url).unwrap()).is_none());
        }
    }

    #[test]
    fn versioned_endpoint_requires_matching_meta() {
        let v2 = Url::parse("https://example.test/index.v2.esi").unwrap();
        assert!(validate_endpoint_version(&v2, MetaVersion::V2).is_ok());
        assert!(validate_endpoint_version(&v2, MetaVersion::V1).is_err());
        let future = Url::parse("https://example.test/index.v3.esi").unwrap();
        assert!(validate_endpoint_version(&future, MetaVersion::V2).is_err());
    }
}
