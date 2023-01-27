//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use fs2::FileExt;
use rand::RngCore;

use std::{
    fs,
    fs::File,
    io,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Returns the most recent of [`fs::Metadata::created`] and
/// [`fs::Metadata::modified`], or [`None`], if neither succeeds.
pub fn get_last_modified(metadata: fs::Metadata) -> Option<SystemTime> {
    metadata
        .created()
        .ok()
        .into_iter()
        .chain(metadata.modified().ok().into_iter())
        .max()
}

/// Ensures that the directory exists.
/// Unlike [`fs::create_dir()`], this function does not return Err if the directory already exists.
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir_all(path) {
        Ok(_) => Ok(()),
        Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Creates a destination file [`dest`]. Uses a temporary file in [`temp_dir`] to write to and then
/// moves the file (via Posix rename() or equiv. atomic operation). If the process is killed,
/// the file in [`temp_dir`] might remain.
///
/// NOTE: [`temp_dir`] and [`dest`] must be on the same filesystem!
pub fn write_file_atomic(mut r: impl Read, temp_dir: &Path, dest: &Path) -> io::Result<()> {
    let temp_path = create_temp_path(temp_dir);
    let mut temp_file = create_file(&temp_path)?;
    // The presence of a lock on the file indicates that this tempfile is in
    // use, in case a garbage collection process wants to know which files it
    // can delete. The lock is dropped after the rename.
    temp_file.try_lock_exclusive()?;
    io::copy(&mut r, &mut temp_file)?;
    temp_file.sync_data()?;
    fs::rename(temp_path, dest)
}

/// Returns a unique path suitable for a temporary file.
pub fn create_temp_path(temp_dir: &Path) -> PathBuf {
    // Pick filename from a 128-bit random distribution.
    let temp_filename = {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };
    temp_dir.join(temp_filename)
}

/// Opens the file in read-only mode, as if by [`File::open`]. Any [`Error`]
/// returned will contain the provided [`path`] in the error message.
pub fn open_file<P: AsRef<Path>>(path: P) -> io::Result<File> {
    match File::open(&path) {
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("couldn't open {}", path.as_ref().display()),
        )),
        Ok(file) => Ok(file),
    }
}

/// Opens the file in write-only mode, as if by [`File::create`]. Any [`Error`]
/// returned will contain the provided [`path`] in the error message.
///
/// This function will create a file if it does not exist, and will truncate it if it does
pub fn create_file<P: AsRef<Path>>(path: P) -> io::Result<File> {
    match File::create(&path) {
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "couldn't create or open {} for writing",
                path.as_ref().display()
            ),
        )),
        Ok(file) => Ok(file),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
const OS_ERROR_DIR_NOT_EMPTY: i32 = 39 /* ENOTEMPTY */;
#[cfg(windows)]
const OS_ERROR_DIR_NOT_EMPTY: i32 = 145 /* ERROR_DIR_NOT_EMPTY */;
#[cfg(target_os = "macos")]
const OS_ERROR_DIR_NOT_EMPTY: i32 = 66;

/// Removes empty directories by starting at [`leaf_dir`] and bubbling up
/// until `boundary_dir` is reached.
///
/// NOTE: The part of `leaf_dir` after the `boundary_dir` cannot contain '..'
/// and should go through reference symlinks (for correctness).
///
/// # Arguments
/// * `leaf_dir` - the leaf directory from which to start the removal
/// * `boundary_dir` - stops when this directory is reached
pub fn remove_empty_dirs<P1: AsRef<Path>, P2: AsRef<Path>>(
    leaf_dir: P1,
    boundary_dir: P2,
) -> io::Result<()> {
    if let Ok(relative_path) = leaf_dir.as_ref().strip_prefix(&boundary_dir) {
        assert!(
            !contains_parent_dir_component(relative_path),
            "leaf_dir must not contain '/../'"
        );
    } else {
        panic!("leaf_dir must be a sub-directory of boundary_dir");
    }
    if leaf_dir.as_ref() == boundary_dir.as_ref() {
        return Ok(());
    }

    let current_dir = leaf_dir.as_ref().to_path_buf();
    match fs::remove_dir(current_dir) {
        Err(e) if matches!(e.raw_os_error(), Some(OS_ERROR_DIR_NOT_EMPTY)) => Ok(()),
        Ok(()) => {
            let parent_dir = leaf_dir.as_ref().parent().unwrap();
            if parent_dir != boundary_dir.as_ref() {
                remove_empty_dirs(parent_dir, boundary_dir)?
            }
            Ok(())
        }
        r => r,
    }
}

/// Checks for the existence of a '/../' component in the path.
fn contains_parent_dir_component(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "..")
}

/// A queue of directories which are removed if empty. When an empty directory is
/// removed, its parent is also considered for removal.
///
/// The current implementation uses a heuristic to avoid storing the full set of
/// paths and is able to avoid spurious system calls when the list of
/// directories is enqueued in a ascending lexicographic order.
pub struct EmptyDirectoryCleanupQueue {
    last: Option<(PathBuf, PathBuf)>,
}

impl EmptyDirectoryCleanupQueue {
    pub fn new() -> EmptyDirectoryCleanupQueue {
        EmptyDirectoryCleanupQueue { last: None }
    }

    /// Adds a directory to the queue. This operations might process some
    /// entries in certain situations, so the caller must be prepared to handle
    /// any IO errors that occur.
    pub fn enqueue<P1: Into<PathBuf> + AsRef<Path>, P2: Into<PathBuf> + AsRef<Path>>(
        &mut self,
        leaf_dir: P1,
        boundary_dir: P2,
    ) -> io::Result<()> {
        let (last_leaf, last_boundary) = match self.last.as_mut() {
            None => {
                self.last = Some((leaf_dir.into(), boundary_dir.into()));
                return Ok(());
            }
            Some(x) => x,
        };

        if *last_boundary == boundary_dir.as_ref() && last_leaf.starts_with(&leaf_dir) {
            // When scheduling a directory that is a subdirectory of the
            // last seen directory, we can simply overwrite the
            // value. remove_empty_dirs will consider the previous
            // directory for deletion when it recurses up the hierarchy.
            *last_leaf = leaf_dir.into();
            Ok(())
        } else {
            // When we schedule a directory that is not a sub-directory
            // of the last seen directory or we change the
            // boundary, we must call remove_empty_dirs, since we will
            // overwrite these values.
            remove_empty_dirs(last_leaf, last_boundary)?;
            self.last = Some((leaf_dir.into(), boundary_dir.into()));
            Ok(())
        }
    }

    /// Processes the enqueued directories.
    pub fn process(&mut self) -> io::Result<()> {
        if let Some((last_leaf, last_boundary)) = self.last.take() {
            remove_empty_dirs(last_leaf, last_boundary)?;
        }
        Ok(())
    }
}

impl Default for EmptyDirectoryCleanupQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EmptyDirectoryCleanupQueue {
    fn drop(&mut self) {
        self.process()
            .expect("Failed to remove some directories! Use process() to handle the error.");
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::iter::FromIterator;
    use std::path::PathBuf;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new<P: AsRef<Path>>(name: P) -> io::Result<TempDir> {
            let path = env::temp_dir().join(name.as_ref());
            fs::create_dir(&path)?;
            Ok(TempDir(path))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            println!(
                "Cleaning up temporary testing directory {}",
                &self.0.display()
            );
            fs::remove_dir_all(&self.0).expect("Could not cleanup!");
        }
    }

    fn is_empty_dir<P: AsRef<Path>>(path: P) -> io::Result<bool> {
        Ok(path.as_ref().read_dir()?.next().is_none())
    }

    #[test]
    fn test_remove_empty_dirs_works() -> io::Result<()> {
        let temp_dir = TempDir::new("test_remove_empty_dirs_works")?;
        let boundary_dir = temp_dir.0.join("test_root");
        let leaf_dir = boundary_dir.join(PathBuf::from_iter(&["a", "b", "c", "d"]));

        fs::create_dir_all(&leaf_dir)?;

        remove_empty_dirs(&leaf_dir, &boundary_dir)?;

        assert!(
            is_empty_dir(boundary_dir)?,
            "The directory should have been emptied!"
        );

        Ok(())
    }

    #[test]
    fn test_remove_empty_dirs_is_safe() -> Result<(), io::Error> {
        let temp_dir = TempDir::new("test_remove_empty_dirs_is_safe")?;
        let boundary_dir = temp_dir.0.join("test_root");
        let leaf_dir = boundary_dir.join(PathBuf::from_iter(&["a", "b", "c", "d"]));
        let file = leaf_dir.join("file");

        fs::create_dir_all(&leaf_dir)?;
        fs::write(&file, [])?;

        remove_empty_dirs(&leaf_dir, &boundary_dir)?;

        assert!(
            file.exists(),
            "The file was deleted when it shouldn't have been!"
        );
        Ok(())
    }

    #[test]
    fn test_cleanup_queue_works() -> io::Result<()> {
        let temp_dir = TempDir::new("test_cleanup_queue_works")?;
        let boundary_dir = temp_dir.0.join("test_root");
        let leaf_dir = boundary_dir.join(PathBuf::from_iter(&["a", "b", "c", "d"]));

        fs::create_dir_all(&leaf_dir)?;

        let mut q = EmptyDirectoryCleanupQueue::new();
        q.enqueue(leaf_dir, boundary_dir.clone())?;

        assert!(
            !is_empty_dir(&boundary_dir)?,
            "The directory should not have been emptied yet!"
        );
        q.process()?;
        assert!(
            is_empty_dir(&boundary_dir)?,
            "The directory should have been emptied!"
        );

        Ok(())
    }
}
