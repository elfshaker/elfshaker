//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use log::error;
use std::{
    error::Error,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

use super::utils::open_repo_from_cwd;
use elfshaker::repo::{PackId, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "store";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let files_from = matches.get_one::<String>("files-from");
    let files0_from = matches.get_one::<String>("files0-from");
    let snapshot = matches.get_one::<String>("snapshot").unwrap();
    // Use snapshot name as pack name.
    let pack_id = PathBuf::from(format!("loose/{snapshot}"));
    let pack_id = PackId::Pack(pack_id.to_str().unwrap().to_owned());
    let snapshot = SnapshotId::new(pack_id, snapshot)?;

    if files_from.is_some() && files0_from.is_some() {
        error!("Cannot specify both --files-from and --files0-from!");
        return Err("Invalid options!".into());
    }

    let files_from_and_delim = files_from
        .map(|file| (file.as_str(), b'\n'))
        .or_else(|| files0_from.map(|file| (file.as_str(), b'\0')));

    let files: Vec<_> = match files_from_and_delim {
        Some(("-", delim)) => read_files_list(std::io::stdin(), delim)?,
        Some((file, delim)) => read_files_list(&*fs::read(file)?, delim)?,
        _ => find_files(),
    };

    fs::create_dir_all(data_dir)?;

    let mut repo = open_repo_from_cwd(data_dir)?;
    repo.create_snapshot(&snapshot, files.into_iter())?;

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about(
            "Stores the current state of the repository in a snapshot. The snapshots \
            created in this way can later be packed in a .pack file using the pack \
            command."
        )
        .arg(
            Arg::new("snapshot")
                .required(true)
                .index(1)
                .help("The tag for the newly created snapshot."),
        )
        .arg(
            Arg::new("files-from")
                .long("files-from")
                .value_name("file")
                .help("Reads the list of files to include in the snapshot from the specified file. '-' is taken to mean stdin."),
        )
        .arg(
            Arg::new("files0-from")
                .long("files0-from")
                .value_name("file")
                .help("Reads the NUL-separated (ASCII \\0) list of files to include in the snapshot from the specified file. '-' is taken to mean stdin."),
        )
}

#[cfg(unix)]
fn to_os_str(buf: &[u8]) -> Result<&OsStr, std::str::Utf8Error> {
    Ok(std::os::unix::ffi::OsStrExt::from_bytes(buf))
}

#[cfg(not(unix))]
fn to_os_str(buf: &[u8]) -> Result<&OsStr, std::str::Utf8Error> {
    // On Windows (and everything else) we will expect well-formed UTF-8 and pray
    Ok(OsStr::new(std::str::from_utf8(buf)?))
}

fn read_files_list(mut reader: impl io::Read, separator: u8) -> io::Result<Vec<PathBuf>> {
    let mut buf = vec![];
    reader.read_to_end(&mut buf)?;

    buf.split(|c| *c == separator)
        .filter(|s| !s.is_empty())
        .map(|s| {
            to_os_str(s)
                .map(PathBuf::from)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        })
        .collect()
}

fn find_files() -> Vec<PathBuf> {
    WalkDir::new(".")
        .follow_links(true)
        .into_iter()
        .filter_entry(|p| {
            p.path()
                .components()
                .nth(1)
                .map(|p| p.as_os_str() != Path::new("elfshaker_data"))
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        .map(|e| e.path().into())
        .collect()
}
