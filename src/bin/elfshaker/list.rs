//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::{error::Error, path::Path};

use super::utils::{format_size, open_repo_from_cwd, print_table};
use elfshaker::packidx::FileHandleList;
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
            print_snapshot_summary(&repo, &snapshot)?;
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
            Arg::with_name("b")
                .long("--bytes")
                .value_name("bytes")
                .help("Print the sizes in bytes."),
        )
}

fn print_repo_summary(repo: &Repository, bytes: bool) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    for pack_id in repo.packs()? {
        let pack_index = repo.load_index(&pack_id)?;

        // TODO(peterwaller-arm): Some size calculation.
        let size_str = if bytes { "0".into() } else { format_size(1) };
        table.push([
            match pack_id {
                PackId::Pack(s) => s,
            },
            pack_index.snapshots().len().to_string(),
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

    let pack_index = repo.load_index(&pack)?;
    for snapshot in pack_index.snapshots() {
        // TODO: Provide an snapshot iterator instead.
        let snapshot = pack_index.find_snapshot(snapshot.tag()).unwrap();
        // Get a snapshot with a complete file list.
        let file_count = match snapshot.list() {
            FileHandleList::Complete(list) => list.len(),
            _ => unreachable!(),
        };
        table.push([snapshot.tag().to_owned(), file_count.to_string()]);
    }

    print_table(
        Some(&["SNAPSHOT".to_owned(), "FILES".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn print_snapshot_summary(repo: &Repository, snapshot: &SnapshotId) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    // Get a snapshot with a complete file list.
    let pack_index = repo.load_index(snapshot.pack())?;
    let entries = pack_index.entries_from_snapshot(snapshot.tag()).unwrap();
    for entry in entries {
        table.push([
            hex::encode(entry.object.checksum).to_string(),
            entry.object.size.to_string(),
            Path::display(entry.path.as_ref()).to_string(),
        ]);
    }

    print_table(
        Some(&["CHECKSUM".to_owned(), "SIZE".to_owned(), "FILE".to_owned()]),
        table.iter(),
    );
    Ok(())
}
