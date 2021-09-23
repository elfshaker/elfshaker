//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use log::{error, info};
use std::{error::Error, ffi::OsStr, fs, io, path::PathBuf};
use walkdir::WalkDir;

use super::utils::open_repo_from_cwd;
use elfshaker::repo::{Repository, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "store";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let files_from = matches.value_of("files-from");
    let files_from0 = matches.value_of("files-from0");
    let snapshot = matches.value_of("snapshot").unwrap();
    let snapshot = SnapshotId::loose(snapshot)?;
    let is_update_supressed = matches.is_present("no-update-index");

    if files_from.is_some() && files_from0.is_some() {
        error!("Cannot specify both --files-from and --files-from0!");
        return Err("Invalid options!".into());
    }

    let files_from_and_delim = files_from
        .map(|file| (file, b'\n'))
        .or_else(|| files_from0.map(|file| (file, b'\0')));

    let files: Vec<_> = match files_from_and_delim {
        Some(("-", delim)) => read_files_list(std::io::stdin(), delim)?,
        Some((file, delim)) => read_files_list(&*fs::read(file)?, delim)?,
        _ => find_files(),
    };

    let mut repo = open_repo_from_cwd()?;
    repo.create_snapshot(&snapshot, files.into_iter())?;

    if is_update_supressed {
        eprintln!("Snapshot {} created successfully! Remember to run update-index to update the repository index!", snapshot);
    } else {
        info!("Updating the repository index...");
        Repository::update_index(repo.path())?;
        eprintln!("Snapshot {} created successfully!", snapshot);
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about(
            "Stores the current state of the repository in a snapshot. The snapshots \
            created in this way can later be packed in a .pack file using the pack \
            command."
        )
        .arg(
            Arg::with_name("snapshot")
                .required(true)
                .index(1)
                .help("The tag for the newly created snapshot."),
        )
        .arg(
            Arg::with_name("files-from")
                .takes_value(true)
                .long("files-from")
                .value_name("file")
                .help("Reads the list of files to include in the snapshot from the specified file. '-' is taken to mean stdin."),
        )
        .arg(
            Arg::with_name("files-from0")
                .takes_value(true)
                .long("files-from0")
                .value_name("file")
                .help("Reads the NUL-separated (ASCII \\0) list of files to include in the snapshot from the specified file. '-' is taken to mean stdin."),
        )
        .arg(
            Arg::with_name("no-update-index")
                .long("no-update-index")
                .help("Does not update the repository index automatically."),
        )
}

#[cfg(unix)]
fn to_os_str(buf: &[u8]) -> Result<&OsStr, std::str::Utf8Error> {
    Ok(std::os::unix::ffi::OsStrExt::from_bytes(buf))
}

#[cfg(not(unix))]
fn to_os_str(buf: &[u8]) -> Result<&OsStr, std::str::Utf8Error> {
    // On Windows (and everything else) we will expect well-formed UTF-8 and pray
    Ok(std::str::from_utf8(buf))
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
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        .map(|e| e.path().into())
        .collect()
}
