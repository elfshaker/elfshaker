//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Recoverable replacement of a pack/index pair under the repository lock.
//!
//! Once the prepared directory is renamed to JOURNAL, recovery must roll forward.
//! Each source disappears only when its destination has been replaced. Readers
//! acquire the shared repository lock before checking for an unfinished update.

use super::{fs::create_temp_path, PackId};
use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

const JOURNAL: &str = ".append-transaction";

pub(super) fn pending(data: &Path) -> io::Result<bool> {
    data.join(JOURNAL).try_exists()
}

pub(super) fn check_complete(data: &Path) -> io::Result<()> {
    if pending(data)? {
        return Err(io::Error::other(
            "unfinished append; reopen the repository to recover it",
        ));
    }
    Ok(())
}

pub(super) fn validate_target(name: &str) -> io::Result<()> {
    // Path::components ignores trailing separators, but appending an extension
    // in recovery must address the same file as Repository::get_pack_path.
    if name.is_empty()
        || name.ends_with('/')
        || name.starts_with("loose/")
        || PackId::from_str(name).is_err()
        || !Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid append target",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn temporary_directory(data: &Path) -> PathBuf {
    let random = create_temp_path(data);
    data.join(format!(
        ".append-{}",
        random.file_name().unwrap().to_string_lossy()
    ))
}

pub(super) struct Prepared {
    pub directory: PathBuf,
    data: PathBuf,
}

impl Prepared {
    pub fn new(data: &Path) -> io::Result<Self> {
        check_complete(data)?;
        let directory = temporary_directory(data);
        fs::create_dir(&directory)?;
        Ok(Self {
            directory,
            data: data.to_owned(),
        })
    }

    pub fn commit(self, target: &str) -> io::Result<()> {
        validate_target(target)?;
        fs::write(self.directory.join("target"), target)?;
        for name in ["target", "new.pack", "new.pack.idx"] {
            fs::OpenOptions::new()
                .write(true)
                .open(self.directory.join(name))?
                .sync_all()?;
        }
        sync_directory(&self.directory)?;
        // No destination has changed before this durable intent record exists.
        fs::rename(&self.directory, self.data.join(JOURNAL))?;
        sync_directory(&self.data)?;
        recover(&self.data)
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        // After commit the directory has moved; never remove an active journal.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Requires the exclusive repository lock, including when called during open.
pub(super) fn recover(data: &Path) -> io::Result<()> {
    let journal = data.join(JOURNAL);
    let target = match fs::read_to_string(journal.join("target")) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !pending(data)? => return Ok(()),
        Err(e) => return Err(e),
    };
    validate_target(&target)?;
    for extension in ["pack", "pack.idx"] {
        let source = journal.join(format!("new.{extension}"));
        let destination = data.join("packs").join(format!("{target}.{extension}"));
        match fs::metadata(&source) {
            Ok(_) => fs::rename(&source, &destination)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound && destination.is_file() => (),
            Err(e) => return Err(e),
        }
        // Persist both ends of the rename before proceeding to the next file.
        sync_directory(destination.parent().unwrap())?;
        sync_directory(&journal)?;
    }
    // Removing entries one by one would leave an unreadable journal if killed.
    // Retire it atomically, then clean up the retired directory best-effort.
    let retired = temporary_directory(data);
    fs::rename(&journal, &retired)?;
    sync_directory(data)?;
    let _ = fs::remove_dir_all(retired);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_publication_blocks_reads_until_recovery_succeeds() {
        let data = create_temp_path(&std::env::temp_dir());
        fs::create_dir_all(data.join("packs/base.pack.idx")).unwrap();
        fs::write(data.join("packs/base.pack"), b"old pack").unwrap();
        let prepared = Prepared::new(&data).unwrap();
        fs::write(prepared.directory.join("new.pack"), b"new pack").unwrap();
        fs::write(prepared.directory.join("new.pack.idx"), b"new index").unwrap();
        // The pack rename succeeds; a directory obstructs the index rename.
        assert!(prepared.commit("base").is_err());
        assert!(check_complete(&data).is_err());
        assert_eq!(fs::read(data.join("packs/base.pack")).unwrap(), b"new pack");
        assert!(data.join(JOURNAL).join("new.pack.idx").is_file());
        fs::remove_dir(data.join("packs/base.pack.idx")).unwrap();
        recover(&data).unwrap();
        check_complete(&data).unwrap();
        assert_eq!(
            fs::read(data.join("packs/base.pack.idx")).unwrap(),
            b"new index"
        );
        recover(&data).unwrap(); // Recovery is idempotent.
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn dropping_an_unpublished_update_leaves_the_target_alone() {
        let data = create_temp_path(&std::env::temp_dir());
        fs::create_dir_all(data.join("packs")).unwrap();
        fs::write(data.join("packs/base.pack"), b"old pack").unwrap();
        let prepared = Prepared::new(&data).unwrap();
        let staging = prepared.directory.clone();
        fs::write(staging.join("new.pack"), b"partial output").unwrap();
        drop(prepared);
        assert!(!staging.exists());
        assert!(!pending(&data).unwrap());
        assert_eq!(fs::read(data.join("packs/base.pack")).unwrap(), b"old pack");
        for bad in [
            "",
            "/absolute",
            "../parent",
            "loose/snapshot",
            "base/",
            "base//",
            "nested/base/",
        ] {
            assert!(validate_target(bad).is_err());
        }
        for good in ["base", "nested/base"] {
            validate_target(good).unwrap();
        }
        fs::remove_dir_all(data).unwrap();
    }
}
