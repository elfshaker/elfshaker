//! Canonical repository-relative paths stored in pack indexes.

use std::borrow::Borrow;
use std::convert::TryFrom;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// A canonical path inside an elfshaker snapshot.
///
/// Components are separated by `/`. Component bytes are otherwise preserved.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackPath(Vec<u8>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackPathError {
    Empty,
    Absolute,
    EmptyComponent,
    CurrentDirectory,
    ParentDirectory,
    Nul,
    NativePrefix,
    NonUnicodeWindowsComponent,
    UnrepresentableComponent(Vec<u8>),
}

impl fmt::Display for PackPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "pack path is empty"),
            Self::Absolute => write!(f, "pack path is absolute"),
            Self::EmptyComponent => write!(f, "pack path contains an empty component"),
            Self::CurrentDirectory => write!(f, "pack path contains a '.' component"),
            Self::ParentDirectory => write!(f, "pack path contains a '..' component"),
            Self::Nul => write!(f, "pack path contains NUL"),
            Self::NativePrefix => write!(f, "native path contains a root or platform prefix"),
            Self::NonUnicodeWindowsComponent => {
                write!(f, "Windows path component is not valid Unicode")
            }
            Self::UnrepresentableComponent(component) => write!(
                f,
                "pack path component '{}' is not representable on this platform",
                escape_bytes(component)
            ),
        }
    }
}

impl std::error::Error for PackPathError {}

impl PackPath {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, PackPathError> {
        let bytes = bytes.into();
        validate(&bytes)?;
        Ok(Self(bytes))
    }

    pub fn from_native(path: &Path) -> Result<Self, PackPathError> {
        let mut bytes = Vec::new();
        for component in path.components() {
            let component = match component {
                Component::Normal(component) => component,
                Component::CurDir => return Err(PackPathError::CurrentDirectory),
                Component::ParentDir => return Err(PackPathError::ParentDirectory),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PackPathError::NativePrefix)
                }
            };
            if !bytes.is_empty() {
                bytes.push(b'/');
            }
            append_native_component(&mut bytes, component)?;
        }
        Self::new(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.0.split(|byte| *byte == b'/')
    }

    pub fn to_native(&self) -> Result<PathBuf, PackPathError> {
        let mut path = PathBuf::new();
        for component in self.components() {
            path.push(native_component(component)?);
        }
        Ok(path)
    }

    pub fn escaped(&self) -> String {
        escape_bytes(&self.0)
    }
}

impl AsRef<[u8]> for PackPath {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Borrow<[u8]> for PackPath {
    fn borrow(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Display for PackPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.escaped())
    }
}

impl fmt::Debug for PackPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PackPath").field(&self.escaped()).finish()
    }
}

impl TryFrom<Vec<u8>> for PackPath {
    type Error = PackPathError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for PackPath {
    type Error = PackPathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value.as_bytes())
    }
}

fn validate(bytes: &[u8]) -> Result<(), PackPathError> {
    if bytes.is_empty() {
        return Err(PackPathError::Empty);
    }
    if bytes[0] == b'/' {
        return Err(PackPathError::Absolute);
    }
    if bytes.contains(&0) {
        return Err(PackPathError::Nul);
    }
    for component in bytes.split(|byte| *byte == b'/') {
        match component {
            [] => return Err(PackPathError::EmptyComponent),
            b"." => return Err(PackPathError::CurrentDirectory),
            b".." => return Err(PackPathError::ParentDirectory),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn append_native_component(output: &mut Vec<u8>, component: &OsStr) -> Result<(), PackPathError> {
    use std::os::unix::ffi::OsStrExt;
    output.extend_from_slice(component.as_bytes());
    Ok(())
}

#[cfg(windows)]
fn append_native_component(output: &mut Vec<u8>, component: &OsStr) -> Result<(), PackPathError> {
    output.extend_from_slice(
        component
            .to_str()
            .ok_or(PackPathError::NonUnicodeWindowsComponent)?
            .as_bytes(),
    );
    Ok(())
}

#[cfg(unix)]
fn native_component(component: &[u8]) -> Result<OsString, PackPathError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(component.to_vec()))
}

#[cfg(windows)]
fn native_component(component: &[u8]) -> Result<OsString, PackPathError> {
    let component = std::str::from_utf8(component)
        .map_err(|_| PackPathError::UnrepresentableComponent(component.to_vec()))?;
    if component.contains(['\\', '/']) {
        return Err(PackPathError::UnrepresentableComponent(
            component.as_bytes().to_vec(),
        ));
    }
    Ok(OsString::from(component))
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut output = String::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                push_escaped_str(&mut output, valid);
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                push_escaped_str(
                    &mut output,
                    std::str::from_utf8(&remaining[..valid_len]).unwrap(),
                );
                let invalid_len = error.error_len().unwrap_or(remaining.len() - valid_len);
                for byte in &remaining[valid_len..valid_len + invalid_len] {
                    output.push_str(&format!("%{byte:02X}"));
                }
                remaining = &remaining[valid_len + invalid_len..];
            }
        }
    }
    output
}

fn push_escaped_str(output: &mut String, value: &str) {
    for character in value.chars() {
        if character == '%' || character.is_control() {
            let mut bytes = [0; 4];
            for byte in character.encode_utf8(&mut bytes).as_bytes() {
                output.push_str(&format!("%{byte:02X}"));
            }
        } else {
            output.push(character);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_structural_forms() {
        for (path, error) in [
            (b"".as_slice(), PackPathError::Empty),
            (b"/a", PackPathError::Absolute),
            (b"a/", PackPathError::EmptyComponent),
            (b"a//b", PackPathError::EmptyComponent),
            (b".", PackPathError::CurrentDirectory),
            (b"a/./b", PackPathError::CurrentDirectory),
            (b"..", PackPathError::ParentDirectory),
            (b"a/../b", PackPathError::ParentDirectory),
            (b"a\0b", PackPathError::Nul),
        ] {
            assert_eq!(PackPath::new(path).unwrap_err(), error);
        }
    }

    #[test]
    fn orders_by_canonical_bytes() {
        let mut paths = ["z", "a/β", "a/a"].map(|path| PackPath::try_from(path).unwrap());
        paths.sort();
        assert_eq!(paths.map(|path| path.escaped()), ["a/a", "a/β", "z"]);
    }

    #[test]
    fn escaped_display_is_reversible_and_readable() {
        assert_eq!(
            PackPath::new(b"ordinary/r\xC3\xA9sum\xC3\xA9".to_vec())
                .unwrap()
                .escaped(),
            "ordinary/résumé"
        );
        assert_eq!(
            PackPath::new(b"percent%/control\n/invalid\xFF".to_vec())
                .unwrap()
                .escaped(),
            "percent%25/control%0A/invalid%FF"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_native_conversion_preserves_component_bytes_and_backslashes() {
        use std::os::unix::ffi::OsStrExt;

        let native = Path::new(OsStr::from_bytes(b"dir/non-utf8-\xFF/back\\slash"));
        let logical = PackPath::from_native(native).unwrap();
        assert_eq!(logical.as_bytes(), b"dir/non-utf8-\xFF/back\\slash");
        assert_eq!(logical.to_native().unwrap(), native);
    }

    #[test]
    fn unicode_components_round_trip() {
        let native = Path::new("目录/emoji-😀");
        let logical = PackPath::from_native(native).unwrap();
        assert_eq!(logical.as_bytes(), "目录/emoji-😀".as_bytes());
        assert_eq!(logical.to_native().unwrap(), native);
    }
}
