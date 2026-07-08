//! Decoder for the platform-specific `OsString` representation in index
//! versions 1 and 2.
#![allow(dead_code)] // Wired into PackIndex by the version 3 envelope commit.

use crate::packpath::{PackPath, PackPathError};
use serde::Deserialize;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LegacyPathError {
    UnsupportedWindowsUtf16(Vec<u16>),
    InvalidLogicalPath(PackPathError),
}

impl fmt::Display for LegacyPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedWindowsUtf16(_) => {
                write!(f, "legacy Windows path contains invalid UTF-16")
            }
            Self::InvalidLogicalPath(error) => write!(f, "invalid logical path: {error}"),
        }
    }
}

impl std::error::Error for LegacyPathError {}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub(crate) enum LegacyWirePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl LegacyWirePath {
    pub(crate) fn decode(self) -> Result<PackPath, LegacyPathError> {
        let bytes = match self {
            Self::Unix(bytes) => bytes,
            Self::Windows(units) => decode_windows(units)?,
        };
        PackPath::new(bytes).map_err(LegacyPathError::InvalidLogicalPath)
    }
}

fn decode_windows(units: Vec<u16>) -> Result<Vec<u8>, LegacyPathError> {
    let path = String::from_utf16(&units)
        .map_err(|_| LegacyPathError::UnsupportedWindowsUtf16(units.clone()))?;
    let bytes = path.as_bytes();

    if bytes
        .first()
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
        || has_windows_prefix(bytes)
    {
        return Err(LegacyPathError::InvalidLogicalPath(PackPathError::Absolute));
    }

    Ok(bytes
        .iter()
        .map(|byte| if *byte == b'\\' { b'/' } else { *byte })
        .collect())
}

fn has_windows_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[derive(Serialize)]
    enum EncodedLegacyPath {
        Unix(Vec<u8>),
        Windows(Vec<u16>),
    }

    fn round_trip(encoded: &impl Serialize) -> LegacyWirePath {
        let bytes = rmp_serde::to_vec(encoded).unwrap();
        rmp_serde::from_slice(&bytes).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn accepts_the_serde_unix_os_string_encoding() {
        let native = OsString::from_vec(b"dir/non-utf8-\xFF/back\\slash".to_vec());
        let decoded = round_trip(&native).decode().unwrap();
        assert_eq!(decoded.as_bytes(), b"dir/non-utf8-\xFF/back\\slash");
    }

    #[test]
    fn accepts_windows_utf16_and_normalizes_native_separators() {
        let units = "dir\\emoji-😀/file".encode_utf16().collect::<Vec<_>>();
        let decoded = round_trip(&EncodedLegacyPath::Windows(units))
            .decode()
            .unwrap();
        assert_eq!(decoded.as_bytes(), "dir/emoji-😀/file".as_bytes());
    }

    #[test]
    fn separates_unsupported_encoding_from_invalid_logical_paths() {
        let unsupported = round_trip(&EncodedLegacyPath::Windows(vec![0xd800]));
        assert!(matches!(
            unsupported.decode(),
            Err(LegacyPathError::UnsupportedWindowsUtf16(_))
        ));

        let invalid = round_trip(&EncodedLegacyPath::Unix(b"a/../b".to_vec()));
        assert_eq!(
            invalid.decode(),
            Err(LegacyPathError::InvalidLogicalPath(
                PackPathError::ParentDirectory
            ))
        );
    }

    #[test]
    fn rejects_windows_roots_prefixes_and_empty_components() {
        for path in [r"C:\file", r"\\server\share", r"dir\\file"] {
            let encoded = EncodedLegacyPath::Windows(path.encode_utf16().collect());
            assert!(matches!(
                round_trip(&encoded).decode(),
                Err(LegacyPathError::InvalidLogicalPath(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn enum_shape_matches_serde_unix_variant() {
        let serde_bytes = rmp_serde::to_vec(&OsString::from("a/b")).unwrap();
        let explicit_bytes = rmp_serde::to_vec(&EncodedLegacyPath::Unix(b"a/b".to_vec())).unwrap();
        assert_eq!(serde_bytes, explicit_bytes);
    }
}
