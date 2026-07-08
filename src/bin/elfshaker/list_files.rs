//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use std::error::Error;

use super::utils::{format_size, open_repo_from_cwd};
use elfshaker::packidx::{EntryMetadata, ObjectChecksum, PackPath};
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
        .about("List files recorded in a snapshot.")
        .long_about(
            "Lists file metadata for a snapshot identified by its tag or as \
             PACK:SNAPSHOT. Output lines are sorted.",
        )
        .arg(
            Arg::new("snapshot")
                .index(1)
                .required(true)
                .value_name("SNAPSHOT")
                .help("Snapshot tag or PACK:SNAPSHOT identifier to inspect."),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .default_value("%o %b %f")
                .help(concat!(
                    "Pretty-print each result using placeholders:\n",
                    "  %o - file checksum\n",
                    "  %f - file name\n",
                    "  %h - human-readable size\n",
                    "  %b - size in bytes\n",
                    "  %p - permissions, or - for a symbolic link\n",
                    "  %k - entry kind: file or symlink",
                )),
        )
}

fn format_file_row(
    fmt: &str,
    checksum: &ObjectChecksum,
    file_name: &PackPath,
    size: u64,
    metadata: EntryMetadata,
) -> String {
    let (kind, permissions) = match metadata {
        EntryMetadata::Regular { mode } => ("file", format!("{mode:o}")),
        EntryMetadata::Symlink { .. } => ("symlink", "-".to_owned()),
    };
    fmt.to_owned()
        .replace("%o", &hex::encode(checksum))
        .replace("%f", &file_name.escaped())
        .replace("%h", &format_size(size))
        .replace("%b", &size.to_string())
        .replace("%p", &permissions)
        .replace("%k", kind)
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
                entry.metadata,
            )
        })
        .collect();

    lines.sort();

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_rows_preserve_utf8_and_escape_other_bytes() {
        let checksum = [0; 20];
        let utf8 = PackPath::new("目录/résumé".as_bytes()).unwrap();
        assert!(format_file_row(
            "%f",
            &checksum,
            &utf8,
            0,
            EntryMetadata::Regular { mode: 0 }
        )
        .contains("目录/résumé"));

        let binary = PackPath::new(b"dir/control\n/non-utf8-\xff".to_vec()).unwrap();
        assert_eq!(
            format_file_row(
                "%f",
                &checksum,
                &binary,
                0,
                EntryMetadata::Regular { mode: 0 }
            ),
            "dir/control%0A/non-utf8-%FF"
        );
    }

    #[test]
    fn file_rows_expose_symlink_kind_without_permissions() {
        let checksum = [0; 20];
        let path = PackPath::new(b"link".to_vec()).unwrap();
        let row = format_file_row(
            "%k %p %f",
            &checksum,
            &path,
            6,
            EntryMetadata::Symlink {
                target_encoding: elfshaker::packidx::SymlinkTargetEncoding::PortableUtf8,
                target_kind: elfshaker::packidx::SymlinkTargetKind::Unknown,
            },
        );
        assert_eq!(row, "symlink - link");
    }
}
