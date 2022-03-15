use std::{
    convert::TryInto,
    ffi::OsStr,
    fs, io,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{offset::Utc, DateTime};
use ureq::AgentBuilder;
use url::Url;

use super::constants::{PACK_INDEX_EXTENSION, REMOTE_INDEX_EXTENSION, REMOTE_INDEX_SIZE_LIMIT};
use super::error::Error;
use super::fs::{create_file, open_file};
use crate::packidx::{ObjectChecksum, PackIndex};

const HTTP_STATUS_OK: u16 = 200;
const HTTP_STATUS_NOT_MODIFIED: u16 = 304;

#[derive(Debug)]
pub struct RemotePack {
    pub checksum: ObjectChecksum,
    pub url: String,
}

#[derive(Debug)]
pub struct RemoteIndex {
    // Set when open by load().
    path: Option<PathBuf>,
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

    pub fn path(&self) -> Option<&Path> {
        self.path.as_ref().map(|p| p.as_path())
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<RemoteIndex, Error> {
        let file = open_file(path.as_ref()).map_err(Error::IOError)?;
        let reader = BufReader::new(file);
        let mut ridx = Self::read(reader)?;
        ridx.path = Some(path.as_ref().into());
        Ok(ridx)
    }

    pub fn read<R: BufRead>(reader: R) -> Result<RemoteIndex, Error> {
        let mut lines = reader.lines();
        let mut line_no = 1;
        let meta = Self::read_keyed_line(&mut lines, "meta")?;
        line_no += 1;
        let url = Self::read_keyed_line(&mut lines, "url")?;
        line_no += 1;

        let mut packs = vec![];
        for line in lines {
            line_no += 1;
            let line = line.map_err(Error::IOError)?;
            let mut parts = line.split('\t');
            let checksum = parts.next().ok_or_else(|| {
                Error::BadRemoteIndexFormat(format!(
                    "Expected pack checksum, reached end of line {}",
                    line_no
                ))
            })?;
            let url = parts.next().ok_or_else(|| {
                Error::BadRemoteIndexFormat(format!(
                    "Expected pack URL, reached end of line {}",
                    line_no
                ))
            })?;

            if url.parse::<Url>().is_err() {
                return Err(Error::BadRemoteIndexFormat(format!(
                    "Expected a valid pack URL, found {} on line {}",
                    url, line_no
                )));
            }

            if parts.next().is_some() {
                panic!("Too many values");
            }

            let checksum = hex::decode(checksum)
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!("Bad checksum format on line {}", line_no))
                })?
                .try_into()
                .map_err(|_| {
                    Error::BadRemoteIndexFormat(format!(
                        "The value for checksum on line {} is not the right length",
                        line_no
                    ))
                })?;

            packs.push(RemotePack {
                url: url.to_owned(),
                checksum,
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

/// Loads all remote index (.esi) files from the target directory.
pub fn load_remotes(base_path: &Path) -> Result<Vec<RemoteIndex>, Error> {
    let paths = fs::read_dir(base_path).unwrap();
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
/// * `size_limit` - the maximum size of the response body, before the
///   connection is terminated
/// * `if_modified_since` - Sets the value of the `If-Modified-Since` HTTP
///   header
fn read_remote_resource(
    url: &Url,
    timeout: Duration,
    size_limit: u64,
    if_modified_since: Option<SystemTime>,
) -> Result<Option<Vec<u8>>, Error> {
    let agent = AgentBuilder::new().timeout(timeout).build();

    let mut request = agent.get(url.as_ref());
    if let Some(if_modified_since) = if_modified_since {
        request = request.set("If-Modified-Since", &format_http_date(if_modified_since));
    }

    let response = request.call().map_err(|e| Error::HttpError(e.into()))?;

    let status = response.status();

    log::info!("HTTP GET {} -> {}", url, status);

    match status {
        HTTP_STATUS_OK => {
            let mut body = vec![];
            response
                .into_reader()
                // A malicious third-party could overwhelm might send a very
                // large response.
                .take(size_limit)
                .read_to_end(&mut body)
                .map_err(|e| Error::HttpError(e.into()))?;

            Ok(Some(body))
        }
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

pub fn update_remote_packs(ridx: &RemoteIndex, base_dir: &Path) -> Result<(), Error> {
    for pack in &ridx.packs {
        let url = (pack.url.to_string() + ".idx").parse::<Url>().unwrap();
        let pack_index_file_name = url.path_segments().unwrap().last().unwrap();
        let pack_index_path = base_dir.join(pack_index_file_name);

        let date_modified = fs::metadata(&pack_index_path)
            .ok()
            .map(|x| x.modified().ok())
            .flatten();

        let pack_index_bytes = read_remote_resource(
            &url,
            Duration::from_secs(15),
            REMOTE_INDEX_SIZE_LIMIT,
            date_modified,
        )?;

        if let Some(pack_index_bytes) = pack_index_bytes {
            if let Err(e) = PackIndex::parse(pack_index_bytes.as_slice()) {
                log::error!(
                    "Failed to fetch {}.idx from remote: The remote returned a broken .pack.idx! {}",
                    url,
                    e
                );
            } else {
                log::info!(
                    "Writing {} ({} B)...",
                    pack_index_path.display(),
                    pack_index_bytes.len()
                );
                fs::write(&pack_index_path, pack_index_bytes.as_slice())
                    .map_err(|e| Error::IOError(e))?;
            }
        }
    }
    Ok(())
}

/// Fetches the new newest version of the [`RemoteIndex`] from the server and
/// overwrites its backing file.
pub fn update_remote(ridx: &RemoteIndex) -> Result<RemoteIndex, Error> {
    let path = ridx
        .path
        .as_ref()
        .expect("The RemoteIndex must have a valid .path set! Use RemoteIndex::load().");

    // Read the modification date of the .esi.
    let date_modified = fs::metadata(path).ok().map(|x| x.modified().ok()).flatten();
    let url = ridx.url.parse::<Url>().unwrap();
    let response = read_remote_resource(
        &url,
        Duration::from_secs(15),
        REMOTE_INDEX_SIZE_LIMIT,
        date_modified,
    )?;

    match response {
        // The local version is up-to-date.
        None => RemoteIndex::load(path),
        Some(data) => {
            let mut ridx = RemoteIndex::read(BufReader::new(data.as_slice()))?;
            // Update the .esi
            create_file(path)
                .map_err(Error::IOError)?
                .write_all(data.as_slice())
                .map_err(Error::IOError)?;
            // And return the parsed index
            ridx.path = Some(path.clone());
            Ok(ridx)
        }
    }
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
90765d432f15eda9b42e0ed747ceaa9b5f8237de\thttps://github.com/elfshaker/releases/download/A.pack
f3a50129b7ac872b63585f884f1a73e013d51f85\tB.pack"
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
            "https://github.com/elfshaker/releases/download/A.pack"
        );
        assert_eq!(
            r.packs[0].checksum.to_vec(),
            hex::decode(b"90765d432f15eda9b42e0ed747ceaa9b5f8237de").unwrap()
        );
        assert_eq!(r.packs[1].url, "B.pack");
        assert_eq!(
            r.packs[1].checksum.to_vec(),
            hex::decode(b"f3a50129b7ac872b63585f884f1a73e013d51f85").unwrap()
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
