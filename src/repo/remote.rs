use std::{
    convert::TryInto,
    ffi::OsStr,
    fs, io,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{offset::Utc, DateTime};
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use ureq::Agent;
use url::Url;

use super::constants::{PACK_EXTENSION, REMOTE_INDEX_EXTENSION};
use super::error::Error;
use super::fs::{create_file, open_file};
use crate::packidx::{ObjectChecksum, PackIndex};
use crate::progress::{ProgressReporter, ProgressWriter};

const HTTP_STATUS_OK: u16 = 200;
const HTTP_STATUS_NOT_MODIFIED: u16 = 304;

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
    pub fn new(url: String) -> RemoteIndex {
        Self {
            path: None,
            meta: "v1".to_owned(),
            url,
            packs: vec![],
        }
    }

    pub fn packs(&self) -> &[RemotePack] {
        &self.packs
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn name(&self) -> Option<String> {
        self.path()
            .map(|p| p.file_stem())
            .flatten()
            .map(|s| (*s.to_string_lossy()).into())
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<RemoteIndex, Error> {
        let file = open_file(path.as_ref()).map_err(Error::IOError)?;
        let reader = BufReader::new(file);
        let mut remote = Self::read(reader)?;
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
            Error::BadRemoteIndexFormat(format!(
                "Expected a valid elfshaker index URL, found {} on line {}",
                url, line_no
            ))
        })?;
        line_no += 1;

        let mut packs = vec![];
        for line in lines {
            line_no += 1;
            let line = line.map_err(Error::IOError)?;
            let mut parts = line.split('\t');
            let index_checksum = parts.next().ok_or_else(|| {
                Error::BadRemoteIndexFormat(format!(
                    "Expected pack index checksum, reached end of line {}",
                    line_no
                ))
            })?;

            let pack_checksum = parts.next().ok_or_else(|| {
                Error::BadRemoteIndexFormat(format!(
                    "Expected pack checksum, reached end of line {}",
                    line_no
                ))
            })?;

            // Pack URL (possibly relative)
            let relative_url = parts.next().ok_or_else(|| {
                Error::BadRemoteIndexFormat(format!(
                    "Expected pack URL, reached end of line {}",
                    line_no
                ))
            })?;

            // Always provide the index URL as base URL. If the pack URL is
            // absolute, the `join` will use it as-is.
            let absolute_url = match base_url.join(relative_url) {
                Ok(url) => url,
                Err(_) => {
                    return Err(Error::BadRemoteIndexFormat(format!(
                        "Expected a valid pack URL, found {} on line {}",
                        url, line_no
                    )));
                }
            };

            if parts.next().is_some() {
                panic!("Too many values");
            }

            let index_checksum = hex::decode(index_checksum)
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!(
                        "Bad pack index checksum format on line {}",
                        line_no
                    ))
                })?
                .try_into()
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!(
                        "The value for pack index checksum on line {} is not the right length",
                        line_no
                    ))
                })?;

            let pack_checksum = hex::decode(pack_checksum)
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!(
                        "Bad pack checksum format on line {}",
                        line_no
                    ))
                })?
                .try_into()
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!(
                        "The value for pack checksum on line {} is not the right length",
                        line_no
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
            None => Err(Error::BadRemoteIndexFormat(format!(
                "Expected '{} ...', but end of file was reached!",
                key
            ))),
            Some(Err(e)) => Err(Error::IOError(e)),
            Some(Ok(line)) => Ok(line),
        }?;

        if !line.starts_with(key) {
            return Err(Error::BadRemoteIndexFormat(format!(
                "Expected '{} ...', but found '{}'",
                key, line
            )));
        }

        if line.as_bytes().get(key.as_bytes().len()) != Some(&b'\t') {
            return Err(Error::BadRemoteIndexFormat(format!(
                "Expected a tab-delimiter '\\t' after '{}', but found '{}'",
                key,
                &line[key.as_bytes().len()..]
            )));
        }

        Ok(line[key.as_bytes().len() + 1..].to_string())
    }
}

impl std::fmt::Display for RemoteIndex {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Some(stem) = self.path().map(|p| p.file_stem()).flatten() {
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
        opt_reader.map(|mut reader| {
            let mut body: Vec<u8> = vec![];
            reader
                .1
                .read_to_end(&mut body)
                .map_err(|e| Error::HttpError(e.into()))
                .map(|_| body)
        }).map_or(Ok(None), |v| v.map(Some))
    })
}

/// Updates the specified pack file by fetching the URL in the [`RemotePack`]
/// only when necessary.
pub fn update_remote_pack(
    agent: &Agent,
    remote_pack: &RemotePack,
    pack_path: &Path,
    reporter: &ProgressReporter,
) -> Result<(), Error> {
    let date_modified = fs::metadata(&pack_path)
        .ok()
        .map(|x| x.modified().ok())
        .flatten();

    let url = remote_pack.url.parse::<Url>().unwrap();
    if let Some((content_length, mut reader)) = open_remote_resource(agent, &url, None, date_modified)? {
        let mut data = vec![];

        let mut writer = ProgressWriter::with_known_size(&mut data, reporter, content_length);
        io::copy(&mut reader, &mut writer)?;

        let mut checksum = [0u8; 20];
        let mut hasher = Sha1::new();
        hasher.input(&data);
        hasher.result(&mut checksum);

        if checksum == remote_pack.pack_checksum {
            create_file(pack_path)?.write_all(&data)?;
        } else {
            log::error!(
                "The pack checksum did not match the one in the .esi! The download failed."
            );
            return Err(Error::CorruptPack);
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
        let pack_index_file_name = url.path_segments().unwrap().last().unwrap();
        let pack_index_path = base_dir.join(pack_index_file_name);

        if verify_checksum(&pack_index_path, &pack.index_checksum)? {
            // The file exists and the checksums match -> skip
            log::info!("{} is up to date", pack_index_path.display());
        } else {
            update_pack_index(&agent, &url, &pack_index_path)?;
        }

        done += 1;
        remaining -= 1;
        reporter.checkpoint(done, Some(remaining));
    }
    Ok(())
}

/// Updates the pack index file by fetching its contents from the URL only
/// when the content at the URL is newer than what is available on-disk.
fn update_pack_index(agent: &Agent, url: &Url, pack_index_path: &Path) -> Result<(), Error> {
    let date_modified = fs::metadata(&pack_index_path)
        .ok()
        .map(|x| x.modified().ok())
        .flatten();

    let pack_index_bytes = read_remote_resource(
        agent,
        url,
        Duration::from_secs(15),
        date_modified,
    )?;

    if let Some(pack_index_bytes) = pack_index_bytes {
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
            fs::write(&pack_index_path, pack_index_bytes.as_slice()).map_err(Error::IOError)?;
        }
    }
    Ok(())
}

/// Fetches the new newest version of the [`RemoteIndex`] from the server and
/// overwrites its backing file only when the remote file is newer.
pub fn update_remote(agent: &Agent, remote: &RemoteIndex) -> Result<RemoteIndex, Error> {
    let path = remote
        .path
        .as_ref()
        .expect("The RemoteIndex must have a valid .path set! Use RemoteIndex::load().");

    // Read the modification date of the .esi.
    let date_modified = fs::metadata(path).ok().map(|x| x.modified().ok()).flatten();
    let url = remote.url.parse::<Url>().unwrap();
    let response = read_remote_resource(
        agent,
        &url,
        Duration::from_secs(15),
        date_modified,
    )?;

    match response {
        // The local version is up-to-date.
        None => RemoteIndex::load(path),
        Some(data) => {
            let mut remote = RemoteIndex::read(BufReader::new(data.as_slice()))?;
            // Update the .esi
            create_file(path)
                .map_err(Error::IOError)?
                .write_all(data.as_slice())
                .map_err(Error::IOError)?;
            // And return the parsed index
            remote.path = Some(path.clone());
            Ok(remote)
        }
    }
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
        sha1.input(buf);
        reader.consume(len);
    }

    let mut checksum = [0u8; 20];
    sha1.result(&mut checksum);

    Ok(checksum)
}

/// Formats a [`SystemTime`] as an HTTP date string. HTTP dates are always in
/// GMT, never in local time. The format is specified in RFC 5322.
///
/// HTTP date format: `<day-name>, <day> <month> <year> <hour>:<minute>:<second>
/// GMT`
fn format_http_date(t: SystemTime) -> String {
    let datetime: DateTime<Utc> = t.into();
    return format!("{}", datetime.format("%a, %d %h %Y %H:%M:%S GMT"));
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
url\thttps://github.com/elfshaker/releases/download/index.esi
90765d432f15eda9b42e0ed747ceaa9b5f8237de\t3fc6c1b427b19217cdd9c4eecf0c74943fa4adb2\thttps://gitlab.com/elfshaker/releases/download/A.pack
f3a50129b7ac872b63585f884f1a73e013d51f85\td8a41c1859d6276f0dd74fdd7e4513d89f68600f\tB.pack"
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
        .unwrap_err();
        let _no_meta_value = RemoteIndex::read(BufReader::new(
            "meta\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .unwrap_err();
        let _bad_meta_delimiter = RemoteIndex::read(BufReader::new(
            "meta v1\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .unwrap_err();
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
