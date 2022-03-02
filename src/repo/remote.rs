use std::{
    convert::TryInto,
    io,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use url::Url;

use super::fs::open_file;
use crate::packidx::ObjectChecksum;

#[derive(Debug)]
pub enum RemoteError {
    IOError(io::Error),
    BadIndexFormat(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::IOError(e) => write!(f, "{}", e),
            Self::BadIndexFormat(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for RemoteError {}

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

    pub fn load<P: AsRef<Path>>(path: P) -> Result<RemoteIndex, RemoteError> {
        let file = open_file(path.as_ref()).map_err(RemoteError::IOError)?;
        let reader = BufReader::new(file);
        let mut ridx = Self::read(reader)?;
        ridx.path = Some(path.as_ref().into());
        Ok(ridx)
    }

    pub fn read<R: BufRead>(reader: R) -> Result<RemoteIndex, RemoteError> {
        let mut lines = reader.lines();
        let mut line_no = 1;
        let meta = Self::read_keyed_line(&mut lines, "meta")?;
        line_no += 1;
        let url = Self::read_keyed_line(&mut lines, "url")?;
        line_no += 1;

        let mut packs = vec![];
        for line in lines {
            line_no += 1;
            let line = line.map_err(RemoteError::IOError)?;
            let mut parts = line.split('\t');
            let checksum = parts.next().ok_or_else(|| {
                RemoteError::BadIndexFormat(format!(
                    "Expected pack checksum, reached end of line {}",
                    line_no
                ))
            })?;
            let url = parts.next().ok_or_else(|| {
                RemoteError::BadIndexFormat(format!(
                    "Expected pack URL, reached end of line {}",
                    line_no
                ))
            })?;

            if url.parse::<Url>().is_err() {
                return Err(RemoteError::BadIndexFormat(format!(
                    "Expected a valid pack URL, found {} on line {}",
                    url, line_no
                )));
            }

            if parts.next().is_some() {
                panic!("Too many values");
            }

            let checksum = hex::decode(checksum)
                .map_err(|_| {
                    RemoteError::BadIndexFormat(format!("Bad checksum format on line {}", line_no))
                })?
                .try_into()
                .map_err(|_| {
                    RemoteError::BadIndexFormat(format!(
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

    fn read_keyed_line<R: BufRead>(
        lines: &mut io::Lines<R>,
        key: &str,
    ) -> Result<String, RemoteError> {
        let line = match lines.next() {
            None => Err(RemoteError::BadIndexFormat(format!(
                "Expected '{} ...', but end of file was reached!",
                key
            ))),
            Some(Err(e)) => Err(RemoteError::IOError(e)),
            Some(Ok(line)) => Ok(line),
        }?;

        if !line.starts_with(key) {
            return Err(RemoteError::BadIndexFormat(format!(
                "Expected '{} ...', but found '{}'",
                key, line
            )));
        }

        if line.as_bytes().get(key.as_bytes().len()) != Some(&b'\t') {
            return Err(RemoteError::BadIndexFormat(format!(
                "Expected a tab-delimiter '\\t' after '{}', but found '{}'",
                key,
                &line[key.as_bytes().len()..]
            )));
        }

        Ok(line[key.as_bytes().len() + 1..].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_index_read_works() -> Result<(), RemoteError> {
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
    fn test_remote_index_no_packs_read_works() -> Result<(), RemoteError> {
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
        let no_meta = RemoteIndex::read(BufReader::new(
            "url\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .unwrap_err();
        let no_meta_value = RemoteIndex::read(BufReader::new(
            "meta\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .unwrap_err();
        let bad_meta_delimiter = RemoteIndex::read(BufReader::new(
            "meta v1\nurl\thttps://github.com/elfshaker/releases/download/index.esi".as_bytes(),
        ))
        .unwrap_err();
    }

    #[test]
    fn test_remote_index_bad_url_read_fails() {
        let no_url = RemoteIndex::read(BufReader::new("meta\tv1".as_bytes())).unwrap_err();
        let no_url_value =
            RemoteIndex::read(BufReader::new("meta\tv1\nurl".as_bytes())).unwrap_err();
        // The bad delimiter case is covered by the meta delimiter test.
    }

    #[test]
    fn test_remote_index_bad_checksum_fails() {
        let bad_checksum = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nCH3kS0M\thttps://asd".as_bytes(),
        ))
        .unwrap_err();
    }

    #[test]
    fn test_remote_index_bad_pack_url_fails() {
        let no_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85\t".as_bytes(),
        ))
        .unwrap_err();
        let bad_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85\thttps://"
                .as_bytes(),
        ))
        .unwrap_err();
    }

    #[test]
    fn test_remote_index_bad_pack_delimiter_fails() {
        let no_pack_url = RemoteIndex::read(BufReader::new(
            "meta\tv1\nurl\thttps://\nf3a50129b7ac872b63585f884f1a73e013d51f85 https://".as_bytes(),
        ))
        .unwrap_err();
    }
}
