//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use rand::RngCore;
use std::{fs, fs::File, io, io::Read, path::Path};

/// Ensures that the file is writable OR does not exist!
pub fn ensure_file_writable(p: impl AsRef<Path>) -> io::Result<()> {
    let o = match File::open(p) {
        Ok(file) => file,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut p = o.metadata()?.permissions();
    p.set_readonly(false);
    o.set_permissions(p)?;
    Ok(())
}

/// Sets the readonly permission on the file.
pub fn set_readonly(f: &File, readonly: bool) -> io::Result<()> {
    let mut perm = f.metadata()?.permissions();
    perm.set_readonly(readonly);
    f.set_permissions(perm)?;
    Ok(())
}

/// Reads a file, returning a byte slice. Returns [`None`], if the file does not exist.
pub fn read_or_none<P>(path: P) -> io::Result<Option<Vec<u8>>>
where
    P: AsRef<Path>,
{
    let mut buf = vec![];
    match File::open(path) {
        Ok(mut file) => {
            file.read_to_end(&mut buf)?;
            Ok(Some(buf))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Ensures that the directory exists.
/// Unlike [`fs::create_dir()`], this function does not return Err if the directory already exists.
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(&path) {
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
    let temp_filename = {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(&bytes)
    };
    let temp_path = temp_dir.join(temp_filename);
    {
        let mut temp_file = File::create(&temp_path)?;
        io::copy(&mut r, &mut temp_file)?;
    }
    fs::rename(temp_path, dest)
}
