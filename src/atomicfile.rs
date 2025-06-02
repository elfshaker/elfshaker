use fs2::FileExt;
use rand::RngCore;
use std::io::Read;
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

/// AtomicCreateFile provides an API for atomically creating a file, determining
/// if it exists before proceeding to do potentially expensive work to fill it.
/// The primitives should be used like this:
///
/// ```no_run
///     use std::path::Path;
///     let atomic_create_handle = elfshaker::atomicfile::AtomicCreateFile::new(Path::new("destination_path")).unwrap();
///     // ... on error, report that the file might already exist
///     //     (though it may not yet be ready) ...
///     // ... otherwise, do potentially long running work ...
///     use std::io::Cursor;
///     let mut reader = std::io::Cursor::new(b"foo");
///     atomic_create_handle.commit_content(&mut reader).unwrap();
///     // ... files are closed here ...
/// ```
///
/// 1. If the file exists and has an exclusive lock on it, another process is
///    approaching commit_content(), and it is an 'already exists' error.
/// 2. If the file exists, has size zero, and has no exclusive lock; the
///    original process is assumed to have crashed.
/// 3. commit_content writes its content into a randomly named file in the same
///    directory as the destination_path, and then uses rename() to the
///    destination to achieve an atomic update.
/// 4. As a convenience, if file creation fails because parent directories don't
///    exist, create them and proceed. This handily avoids the work of checking
///    if parent directories exist and creating them, saving on syscalls in the
///    success case.
///
/// The motivation is to prevent multiple processes from wasting effort creating
/// the same file, so that one process 'wins' and the others can return an
/// error. Additionally, locks are used so that it is possible to determine if a
/// process was uncleanly interrupted (even under SIGKILL), and treat the file
/// as though it doesn't yet exist. Further, stale temporary files can be
/// identified as files with the prefix .elfshakertmp_ which have no exclusive
/// lock held.
pub struct AtomicCreateFile<'l> {
    path: &'l Path,
    temp: (PathBuf, File),
    target: File,
}

/// lock_name acquires a lock on the given `fd`, and ensures that the
/// `fd` relates to the given Path. This protects against the case where
/// a file can be locked, but unlinked.
#[cfg(unix)]
fn lock_name(name: &Path, fd: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    fd.try_lock_exclusive()?;
    // Lock acquired. Ensure that the name on the filesystem corresponds
    // to the lock now held.
    let i0 = fd.metadata()?.ino();
    let name_metadata = match fs::metadata(name) {
        Ok(md) => md,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("file lock for {name:?} acquired by a different process"),
            ));
        }
        Err(e) => return Err(e),
    };
    let i1 = name_metadata.ino();
    if i0 != i1 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("file lock for {name:?} acquired by a different process"),
        ));
    }
    Ok(())
}

// Would prefer to use  std::os::windows::fs::MetadataExt, but currently
// unstable per https://github.com/rust-lang/rust/issues/63010
#[cfg(windows)]
fn get_file_id_and_serial(h: std::os::windows::io::RawHandle) -> std::io::Result<(u64, u32)> {
    use std::mem::zeroed;
    use winapi::um::fileapi::GetFileInformationByHandle;
    use winapi::um::fileapi::BY_HANDLE_FILE_INFORMATION;
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = zeroed();
        if GetFileInformationByHandle(h as *mut _, &mut info) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file_index: u64 = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
        let volume_serial = info.dwVolumeSerialNumber;
        Ok((file_index, volume_serial))
    }
}

#[cfg(windows)]
fn lock_name(path: &Path, fd: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    fd.try_lock_exclusive()?;
    // Lock acquired. Ensure that the name on the filesystem corresponds
    // to the lock now held.
    let fd2 = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("file lock for {path:?} acquired by a different process"),
            ));
        }
        Err(e) => return Err(e),
    };

    let (id1, serial1) = get_file_id_and_serial(fd.as_raw_handle())?;
    let (id2, serial2) = get_file_id_and_serial(fd2.as_raw_handle())?;

    if id1 != id2 || serial1 != serial2 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("file lock for {path:?} acquired by a different process"),
        ));
    }

    Ok(())
}

impl<'l> AtomicCreateFile<'l> {
    pub fn new(dest: &'l Path) -> io::Result<Self> {
        let mut atomic_create_for_write = OpenOptions::new();
        atomic_create_for_write.write(true).create(true);

        let parent = dest.parent().unwrap_or_else(|| Path::new("/"));
        match atomic_create_for_write.open(dest) {
            Ok(file) => {
                // Grab a lock to indicate that the use is 'live' as opposed to
                // stale. Failure to grab the lock here should be a rare race
                // condition, but some other process will have the lock and
                // proceed.
                lock_name(dest, &file)?;
                if file.metadata()?.len() > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} already exists and is non-empty", dest.display()),
                    ));
                }
                // The file exists, is empty, and has a lock on it.
                Ok(Self {
                    path: dest,
                    temp: Self::create_temp(parent)?,
                    target: file,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // NotFound during creation indicates parent directories do not
                // exist. Make them and try again.
                fs::create_dir_all(parent)?;
                Self::new(dest)
            }
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("couldn't open {} for writing", dest.display()),
            )),
        }
    }

    /// create_temp makes a temporary file in the same directory as 'dest' with
    /// the intent that it can be `rename()`d to dest as an atomic operation.
    fn create_temp(dir: &Path) -> io::Result<(PathBuf, File)> {
        let temp_path = create_temp_path(dir);
        let temp_file = match OpenOptions::new()
            .write(true)
            .create_new(true) // safety against very unlikely collisions.
            .open(&temp_path)
        {
            Ok(f) => Ok(f),
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("couldn't create temporary file {}", temp_path.display()),
            )),
        }?;
        // Take a lock for as long as the file is held open by this process.
        // Temp files without locks are stale and can be deleted with no
        // consequence. The temp file should be uniquely created by this process
        // in the lines above, failure to take the lock here is an error.
        temp_file.try_lock_exclusive()?;
        Ok((temp_path, temp_file))
    }

    /// commit_content updates the target file with the content of the reader
    /// 'r' atomically. It consumes 'self', and relinquishes any locks on the
    /// files being atomically updated.
    pub fn commit_content(mut self, mut r: impl Read) -> io::Result<()> {
        let written = io::copy(&mut r, &mut self.temp.1)?;
        assert!(
            written != 0,
            "written == 0 in commit_content; \
             AtomicCreateFile assumes non-empty files",
        );
        // Check that the data made it to disk before proceeding.
        self.temp.1.sync_data()?;

        // Windows (wine, at least) does not allow renaming a file that is
        // currently open, so we need to drop the lock on the target file
        // before renaming it. This creates a short race window.
        #[cfg(windows)]
        drop(self.target);
        fs::rename(self.temp.0, self.path)?;
        // Silence field-not-read warning, and conceptually: release the lock
        // here.
        #[cfg(not(windows))]
        drop(self.target);
        Ok(())
    }
}

/// Returns a unique path suitable for a temporary file.
fn create_temp_path<P: AsRef<Path>>(temp_dir: P) -> PathBuf {
    // Pick filename from a 128-bit random distribution.
    let mut temp_filename = String::from(".elfshakertmp_");
    temp_filename.push_str(&{
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    });
    temp_dir.as_ref().join(&temp_filename)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::repo::run_in_parallel;

    use super::*;

    #[test]
    fn test_atomic_update_api() -> Result<(), Box<dyn Error>> {
        let p = create_temp_path("/tmp/test_atomic_update_api");

        // Create an empty file with no lock on it.
        // Should succeed later.
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(&p, vec![])?;

        let content = b"non-empty" as &[u8];
        const NTHREAD: i32 = 128;
        let n_total = NTHREAD * 1000;
        // 128 threads trying 1000 times to open the same file. Only one should succeed.
        let result: i32 = run_in_parallel(NTHREAD as usize, 0..n_total, |_| {
            AtomicCreateFile::new(&p)?.commit_content(content)
        })
        .into_iter()
        .map(|r| match r {
            Ok(_) => 1,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => -1,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => -1,
            // Can arise from rename.
            #[cfg(windows)]
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => -1,
            #[cfg(windows)]
            Err(e) if e.raw_os_error.unwrap_or_default() == 33 => -1, // ERROR_LOCK_VIOLATION
            Err(e) => panic!("unexpected error: {:?}", e),
        })
        .sum();

        fs::remove_file(&p)?;
        let (n_success, n_fail) = (1, n_total - 1);
        // Test that we see exactly one success and the rest as failures.
        assert_eq!(n_success + -n_fail, result);

        Ok(())
    }
}
