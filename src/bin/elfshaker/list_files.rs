//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use std::{error::Error, ffi::OsStr};

use super::utils::{format_size, open_repo_from_cwd};
use elfshaker::packidx::ObjectChecksum;
use elfshaker::repo::{Repository, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "list-files";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let snapshot = matches
        .get_one::<String>("snapshot")
        .expect("expected snapshot");
    let format = matches
        .get_one::<String>("format")
        .expect("<format> not provided");

    let repo = open_repo_from_cwd(data_dir)?;

    let snapshot_id = repo.find_snapshot(snapshot)?;

    print_files(&repo, &snapshot_id, format)?;

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Prints the list of files available in the snapshot.")
        .arg(
            Arg::new("snapshot")
                .index(1)
                .required(true)
                .help("Prints the contents of the specified snapshot."),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .default_value("%o %b %f")
                .help(
                    "Pretty-print each result in the given format, where \
                    <format> is a string containing one or more of the \
                    following placeholders:\n\
                    \t%o - file checksum\n\
                    \t%f - file name\n\
                    \t%h - human-readable size\n\
                    \t%b - size in bytes\n\
                    \t%p - permissions",
                ),
        )
}

fn format_file_row(
    fmt: &str,
    checksum: &ObjectChecksum,
    file_name: &OsStr,
    size: u64,
    file_mode: u32,
) -> String {
    fmt.to_owned()
        .replace("%o", &hex::encode(checksum))
        .replace("%f", &file_name.to_string_lossy())
        .replace("%h", &format_size(size))
        .replace("%b", &size.to_string())
        .replace("%p", &format!("{:o}", file_mode))
}

fn print_files(
    repo: &Repository,
    snapshot_id: &SnapshotId,
    fmt: &str,
) -> Result<(), Box<dyn Error>> {
    let index = repo.load_index(snapshot_id.pack())?;
    let handles = index
        .resolve_snapshot(snapshot_id.tag())
        .expect("failed to resolve snapshot");

    let mut lines: Vec<_> = index
        .entries_from_handles(handles.iter())?
        .into_iter()
        .map(|entry| {
            format_file_row(
                fmt,
                &entry.checksum,
                &entry.path,
                entry.object_metadata.size,
                entry.file_metadata.mode,
            )
        })
        .collect();

    lines.sort();

    for line in lines {
        println!("{line}");
    }

    Ok(())
}
