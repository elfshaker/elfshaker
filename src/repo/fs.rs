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
    match fs::create_dir_all(&path) {
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
    let mut temp_file = File::create(&temp_path)?;
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
        hex::encode(&bytes)
    };
    temp_dir.join(temp_filename)
}
