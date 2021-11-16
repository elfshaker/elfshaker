//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::{error::Error, ops::ControlFlow, path::Path};

use super::utils::{format_size, open_repo_from_cwd, print_table};
use elfshaker::repo::{PackId, Repository, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "list";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let snapshot_or_pack = matches.value_of("snapshot_or_pack");
    let bytes = matches.is_present("bytes");

    let repo = open_repo_from_cwd()?;

    if let Some(snapshot_or_pack) = snapshot_or_pack {
        if let Some(pack_id) = repo.is_pack(snapshot_or_pack)? {
            print_pack_summary(&repo, pack_id)?;
        } else {
            let snapshot = repo.find_snapshot(snapshot_or_pack)?;
            print_snapshot_summary(&repo, &snapshot, bytes)?;
        }
    } else {
        print_repo_summary(&repo, bytes)?;
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about(
            "Prints the list of packs available in the repository, the list of snapshots \
             available in a pack, or the list of files available in a snapshot.",
        )
        .arg(
            Arg::with_name("snapshot_or_pack")
                .required(false)
                .index(1)
                .help("Prints the contents of the specified snapshot or pack."),
        )
        .arg(
            Arg::with_name("bytes")
                .long("--bytes")
                .help("Print the sizes in bytes."),
        )
}

fn print_repo_summary(repo: &Repository, bytes: bool) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    for pack_id in repo.packs()? {
        let pack_index = repo.load_index(&pack_id)?;

        let size_str = match repo.open_pack(&pack_id) {
            Ok(pack) => if bytes { pack.file_size().to_string() } else { format_size(pack.file_size()) },
            _ => "-".to_string()
        };

        table.push([
            match pack_id {
                PackId::Pack(s) => s,
            },
            pack_index.snapshot_tags().len().to_string(),
            size_str,
        ]);
    }

    print_table(
        Some(&["PACK".to_owned(), "SNAPSHOTS".to_owned(), "SIZE".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn print_pack_summary(repo: &Repository, pack: PackId) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    repo.load_index(&pack)?
        .for_each_snapshot(|snapshot, entries| {
            let file_count = entries.len();
            table.push([snapshot.to_owned(), file_count.to_string()]);
            ControlFlow::<(), ()>::Continue(())
        })?;

    print_table(
        Some(&["SNAPSHOT".to_owned(), "FILES".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn print_snapshot_summary(repo: &Repository, snapshot: &SnapshotId, bytes: bool) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    let idx = repo.load_index(snapshot.pack())?;
    let handles = idx
        .resolve_snapshot(snapshot.tag())
        .expect("failed to resolve snapshot"); // TODO: Temporary.

    for entry in idx.entries_from_handles(handles.iter())? {
        table.push([
            hex::encode(entry.checksum).to_string(),
            if bytes { entry.metadata.size.to_string() } else { format_size(entry.metadata.size) },
            Path::display(entry.path.as_ref()).to_string(),
        ]);
    }

    table.sort_by(|row1, row2| row1[2].cmp(&row2[2]));

    print_table(
        Some(&["CHECKSUM".to_owned(), "SIZE".to_owned(), "FILE".to_owned()]),
        table.iter(),
    );
    Ok(())
}
