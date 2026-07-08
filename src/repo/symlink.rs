use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::packidx::{PackError, PackPath, SymlinkTargetEncoding, SymlinkTargetKind};

use super::fs::create_temp_path;

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
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(PackError::IOError(error));
    }
    Ok(())
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
        fs::remove_file(&probe).map_err(PackError::IOError)?;
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
