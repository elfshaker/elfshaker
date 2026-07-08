use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::packidx::{PackError, PackPath, SymlinkTargetEncoding, SymlinkTargetKind};

use super::fs::{create_file, create_temp_path, FileMode};

pub(crate) fn encode_target(target: &Path) -> io::Result<(Vec<u8>, SymlinkTargetEncoding)> {
    if let Some(portable) = portable_native_target(target) {
        return Ok((portable, SymlinkTargetEncoding::PortableUtf8));
    }
    encode_native_target(target)
}

pub(crate) fn encode_target_as(
    entry_path: &PackPath,
    target: &Path,
    encoding: SymlinkTargetEncoding,
) -> Result<Vec<u8>, PackError> {
    match encoding {
        SymlinkTargetEncoding::PortableUtf8 => portable_native_target(target).ok_or_else(|| {
            PackError::InvalidSymlinkTarget(
                entry_path.clone(),
                "materialized target is not portable",
            )
        }),
        SymlinkTargetEncoding::UnixBytes => encode_unix_native_target(entry_path, target),
        SymlinkTargetEncoding::WindowsUtf16Le => encode_windows_native_target(entry_path, target),
    }
}

fn portable_native_target(target: &Path) -> Option<Vec<u8>> {
    let text = target.to_str()?;
    if text.is_empty() || target.is_absolute() {
        return None;
    }

    #[cfg(windows)]
    let text = text.replace('\\', "/");
    #[cfg(not(windows))]
    let text = {
        if text.contains('\\') {
            return None;
        }
        text.to_owned()
    };

    let bytes = text.as_bytes();
    let windows_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if text.starts_with('/') || windows_prefix {
        None
    } else {
        Some(bytes.to_vec())
    }
}

#[cfg(unix)]
fn encode_native_target(target: &Path) -> io::Result<(Vec<u8>, SymlinkTargetEncoding)> {
    use std::os::unix::ffi::OsStrExt;
    Ok((
        target.as_os_str().as_bytes().to_vec(),
        SymlinkTargetEncoding::UnixBytes,
    ))
}

#[cfg(windows)]
fn encode_native_target(target: &Path) -> io::Result<(Vec<u8>, SymlinkTargetEncoding)> {
    Ok((
        windows_target_bytes(target),
        SymlinkTargetEncoding::WindowsUtf16Le,
    ))
}

#[cfg(unix)]
fn encode_unix_native_target(_entry_path: &PackPath, target: &Path) -> Result<Vec<u8>, PackError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(target.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn encode_unix_native_target(entry_path: &PackPath, _target: &Path) -> Result<Vec<u8>, PackError> {
    Err(PackError::UnsupportedSymlinkTargetEncoding(
        entry_path.clone(),
        SymlinkTargetEncoding::UnixBytes,
    ))
}

#[cfg(windows)]
fn windows_target_bytes(target: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    target
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn encode_windows_native_target(
    _entry_path: &PackPath,
    target: &Path,
) -> Result<Vec<u8>, PackError> {
    Ok(windows_target_bytes(target))
}

#[cfg(not(windows))]
fn encode_windows_native_target(
    entry_path: &PackPath,
    _target: &Path,
) -> Result<Vec<u8>, PackError> {
    Err(PackError::UnsupportedSymlinkTargetEncoding(
        entry_path.clone(),
        SymlinkTargetEncoding::WindowsUtf16Le,
    ))
}

#[cfg(unix)]
pub(crate) fn target_kind(path: &Path, _metadata: &fs::Metadata) -> SymlinkTargetKind {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => SymlinkTargetKind::Directory,
        Ok(_) => SymlinkTargetKind::File,
        Err(_) => SymlinkTargetKind::Unknown,
    }
}

#[cfg(windows)]
pub(crate) fn target_kind(_path: &Path, metadata: &fs::Metadata) -> SymlinkTargetKind {
    use std::os::windows::fs::FileTypeExt;
    let file_type = metadata.file_type();
    if file_type.is_symlink_dir() {
        SymlinkTargetKind::Directory
    } else if file_type.is_symlink_file() {
        SymlinkTargetKind::File
    } else {
        SymlinkTargetKind::Unknown
    }
}

pub(crate) fn decode_target(
    entry_path: &PackPath,
    bytes: &[u8],
    encoding: SymlinkTargetEncoding,
) -> Result<PathBuf, PackError> {
    let target = match encoding {
        SymlinkTargetEncoding::PortableUtf8 => decode_portable_target(entry_path, bytes)?,
        SymlinkTargetEncoding::UnixBytes => decode_unix_target(entry_path, bytes)?,
        SymlinkTargetEncoding::WindowsUtf16Le => decode_windows_target(entry_path, bytes)?,
    };
    Ok(target)
}

fn decode_portable_target(entry_path: &PackPath, bytes: &[u8]) -> Result<PathBuf, PackError> {
    let target = std::str::from_utf8(bytes)
        .map_err(|_| PackError::InvalidSymlinkTarget(entry_path.clone(), "invalid UTF-8"))?;
    if target.is_empty() || target.contains('\0') || target.contains('\\') {
        return Err(PackError::InvalidSymlinkTarget(
            entry_path.clone(),
            "invalid portable target",
        ));
    }
    let bytes = target.as_bytes();
    let windows_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let path = PathBuf::from(target);
    if target.starts_with('/') || path.is_absolute() || windows_prefix {
        return Err(PackError::InvalidSymlinkTarget(
            entry_path.clone(),
            "portable target is absolute",
        ));
    }
    Ok(path)
}

#[cfg(unix)]
fn decode_unix_target(entry_path: &PackPath, bytes: &[u8]) -> Result<PathBuf, PackError> {
    use std::os::unix::ffi::OsStringExt;
    if bytes.contains(&0) {
        return Err(PackError::InvalidSymlinkTarget(
            entry_path.clone(),
            "Unix target contains NUL",
        ));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn decode_unix_target(entry_path: &PackPath, _bytes: &[u8]) -> Result<PathBuf, PackError> {
    Err(PackError::UnsupportedSymlinkTargetEncoding(
        entry_path.clone(),
        SymlinkTargetEncoding::UnixBytes,
    ))
}

#[cfg(windows)]
fn decode_windows_target(entry_path: &PackPath, bytes: &[u8]) -> Result<PathBuf, PackError> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() % 2 != 0 {
        return Err(PackError::InvalidSymlinkTarget(
            entry_path.clone(),
            "Windows target has an odd byte length",
        ));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    if units.contains(&0) {
        return Err(PackError::InvalidSymlinkTarget(
            entry_path.clone(),
            "Windows target contains NUL",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(windows))]
fn decode_windows_target(entry_path: &PackPath, _bytes: &[u8]) -> Result<PathBuf, PackError> {
    Err(PackError::UnsupportedSymlinkTargetEncoding(
        entry_path.clone(),
        SymlinkTargetEncoding::WindowsUtf16Le,
    ))
}

pub(crate) fn materialize(
    entry_path: &PackPath,
    bytes: &[u8],
    encoding: SymlinkTargetEncoding,
    kind: SymlinkTargetKind,
    destination: &Path,
) -> Result<(), PackError> {
    let target = decode_target(entry_path, bytes, encoding)?;
    let parent = destination.parent().ok_or_else(|| {
        PackError::InvalidSymlinkTarget(entry_path.clone(), "destination has no parent")
    })?;
    fs::create_dir_all(parent).map_err(PackError::IOError)?;
    let temporary = create_temp_path(parent);
    create_native(&target, &temporary, kind).map_err(|error| {
        PackError::SymlinkCreationUnsupported(destination.to_owned(), error.to_string())
    })?;
    if let Err(error) = replace(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(PackError::IOError(error));
    }
    Ok(())
}

pub(crate) fn materialize_regular(
    mut reader: impl Read,
    destination: &Path,
    mode: u32,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = create_temp_path(parent);
    let mut file = create_file(&temporary, Some(FileMode(mode)))?;
    if let Err(error) = io::copy(&mut reader, &mut file) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn replace(temporary: &Path, destination: &Path) -> io::Result<()> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(_error) if fs::symlink_metadata(destination).is_ok() => {
            remove_leaf(destination)?;
            fs::rename(temporary, destination)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn replace(temporary: &Path, destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => remove_leaf(destination)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(temporary, destination)
}

pub(crate) fn remove_leaf(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        remove_native_symlink(path, &file_type)
    } else if file_type.is_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(unix)]
fn remove_native_symlink(path: &Path, _file_type: &fs::FileType) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_native_symlink(path: &Path, file_type: &fs::FileType) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if file_type.is_symlink_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

pub(crate) fn verify_materialized(
    entry_path: &PackPath,
    destination: &Path,
    encoding: SymlinkTargetEncoding,
    kind: SymlinkTargetKind,
    expected_checksum: &[u8; 20],
) -> Result<(), PackError> {
    use sha1::{Digest, Sha1};

    let target = fs::read_link(destination).map_err(PackError::IOError)?;
    let bytes = encode_target_as(entry_path, &target, encoding)?;
    let actual: [u8; 20] = Sha1::digest(&bytes).into();
    if &actual != expected_checksum {
        return Err(PackError::ChecksumMismatch(*expected_checksum, actual));
    }
    verify_native_kind(destination, kind)?;
    Ok(())
}

#[cfg(unix)]
fn verify_native_kind(_destination: &Path, _kind: SymlinkTargetKind) -> Result<(), PackError> {
    Ok(())
}

#[cfg(windows)]
fn verify_native_kind(destination: &Path, kind: SymlinkTargetKind) -> Result<(), PackError> {
    use std::os::windows::fs::FileTypeExt;
    let file_type = fs::symlink_metadata(destination)
        .map_err(PackError::IOError)?
        .file_type();
    let matches = match kind {
        SymlinkTargetKind::File => file_type.is_symlink_file(),
        SymlinkTargetKind::Directory => file_type.is_symlink_dir(),
        SymlinkTargetKind::Unknown => false,
    };
    if matches {
        Ok(())
    } else {
        Err(PackError::SymlinkCreationUnsupported(
            destination.to_owned(),
            "materialized link has the wrong Windows target kind".to_owned(),
        ))
    }
}

pub(crate) fn probe(
    destination: &Path,
    kinds: impl IntoIterator<Item = SymlinkTargetKind>,
) -> Result<(), PackError> {
    let mut kinds = kinds.into_iter().collect::<Vec<_>>();
    kinds.sort_unstable();
    kinds.dedup();
    if kinds.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(destination).map_err(PackError::IOError)?;
    for kind in kinds {
        let probe = create_temp_path(destination);
        create_native(Path::new("elfshaker-symlink-probe-target"), &probe, kind).map_err(
            |error| {
                PackError::SymlinkCreationUnsupported(destination.to_owned(), error.to_string())
            },
        )?;
        remove_leaf(&probe).map_err(PackError::IOError)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_native(target: &Path, link: &Path, _kind: SymlinkTargetKind) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_native(target: &Path, link: &Path, kind: SymlinkTargetKind) -> io::Result<()> {
    match kind {
        SymlinkTargetKind::File => std::os::windows::fs::symlink_file(target, link),
        SymlinkTargetKind::Directory => std::os::windows::fs::symlink_dir(target, link),
        SymlinkTargetKind::Unknown => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows symlink target kind is unknown",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_path() -> PackPath {
        PackPath::new(b"link".to_vec()).unwrap()
    }

    #[test]
    fn portable_targets_must_be_relative_utf8() {
        assert_eq!(
            decode_target(
                &pack_path(),
                b"../target",
                SymlinkTargetEncoding::PortableUtf8
            )
            .unwrap(),
            PathBuf::from("../target")
        );
        assert!(decode_target(
            &pack_path(),
            b"/absolute",
            SymlinkTargetEncoding::PortableUtf8
        )
        .is_err());
        assert!(decode_target(
            &pack_path(),
            b"bad\\separator",
            SymlinkTargetEncoding::PortableUtf8
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_windows_native_targets_on_unix() {
        assert!(matches!(
            decode_target(
                &pack_path(),
                &[b't', 0, b'a', 0, b'r', 0, b'g', 0, b'e', 0, b't', 0],
                SymlinkTargetEncoding::WindowsUtf16Le,
            ),
            Err(PackError::UnsupportedSymlinkTargetEncoding(
                _,
                SymlinkTargetEncoding::WindowsUtf16Le
            ))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unix_native_targets_on_windows() {
        assert!(matches!(
            decode_target(&pack_path(), b"target", SymlinkTargetEncoding::UnixBytes),
            Err(PackError::UnsupportedSymlinkTargetEncoding(
                _,
                SymlinkTargetEncoding::UnixBytes
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn materializes_a_dangling_link_without_following_it() {
        use std::os::unix::ffi::OsStrExt;

        let root =
            std::env::temp_dir().join(format!("elfshaker-symlink-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("link");
        materialize(
            &pack_path(),
            b"missing-\xff",
            SymlinkTargetEncoding::UnixBytes,
            SymlinkTargetKind::Unknown,
            &destination,
        )
        .unwrap();
        assert_eq!(
            fs::read_link(&destination).unwrap().as_os_str().as_bytes(),
            b"missing-\xff"
        );
        fs::remove_file(destination).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
