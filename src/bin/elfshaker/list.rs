//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::{error::Error, io, path::Path, str::FromStr};
use walkdir::WalkDir;

use super::utils::{find_pack_with_snapshot, format_size, open_repo_from_cwd, print_table};
use elfshaker::packidx::FileHandleList;
use elfshaker::repo::{PackId, Repository, SnapshotId, LOOSE_DIR};

pub(crate) const SUBCOMMAND: &str = "list";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let pack = matches.value_of("pack");
    let snapshot = matches.value_of("snapshot");
    let bytes = matches.is_present("bytes");

    let repo = open_repo_from_cwd()?;
    if let Some(snapshot) = snapshot {
        let pack = match pack {
            Some(pack) => PackId::from_str(pack)?,
            None => find_pack_with_snapshot(repo.index(), snapshot)?,
        };
        let snapshot = SnapshotId::new(pack, snapshot)?;
        print_snapshot_summary(&repo, &snapshot)?;
    } else if let Some(pack) = pack {
        print_pack_summary(&repo, PackId::from_str(pack)?)?;
    } else {
        print_repo_summary(&repo, bytes)?;
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about(
            "Prints the list of packs available in the repository, the list of snapshots \
            available in a pack, or the list of files available in a snapshot, depending \
            on the options used.",
        )
        .arg(
            Arg::with_name("snapshot")
                .required(false)
                .index(1)
                .help("Prints the contents of the specified snapshot."),
        )
        .arg(
            Arg::with_name("pack")
                .takes_value(true)
                .short("P")
                .long("pack")
                .value_name("name")
                .help(
                    "Specifies the pack file to use. If used alone and no snapshot is specified, prints the contents of the pack file.",
                ),
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
    for pack_id in &*repo.index().available_packs() {
        match pack_id {
            PackId::Pack(pack_name) => {
                let pack = repo.open_pack(pack_name)?;
                let size_str = if bytes {
                    pack.file_size().to_string()
                } else {
                    format_size(pack.file_size())
                };
                table.push([
                    pack_name.to_owned(),
                    pack.index().snapshots().len().to_string(),
                    size_str,
                ]);
            }
            _ => {
                let loose_index = repo.loose_index()?;

                let loose_dir = repo.path().join(&*Repository::data_dir()).join(LOOSE_DIR);
                let size_str = if bytes {
                    dir_size(&loose_dir)?.to_string()
                } else {
                    format_size(dir_size(&loose_dir)?)
                };
                table.push([
                    PackId::Loose.to_string(),
                    loose_index.snapshots().len().to_string(),
                    size_str,
                ]);
            }
        };
    }

    print_table(
        Some(&["PACK".to_owned(), "SNAPSHOTS".to_owned(), "SIZE".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn print_pack_summary(repo: &Repository, pack: PackId) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    if let PackId::Pack(pack_name) = pack {
        let pack = repo.open_pack(&pack_name)?;
        for snapshot in pack.index().snapshots() {
            // Get a snapshot with a complete file list.
            let snapshot = pack.index().find_snapshot(snapshot.tag()).unwrap();
            let file_count = match snapshot.list() {
                FileHandleList::Complete(list) => list.len(),
                _ => unreachable!(),
            };
            table.push([snapshot.tag().to_owned(), file_count.to_string()]);
        }
    } else {
        let loose_index = repo.loose_index()?;
        for snapshot in loose_index.snapshots() {
            // Get a snapshot with a complete file list.
            let snapshot = loose_index.find_snapshot(snapshot.tag()).unwrap();
            let file_count = match snapshot.list() {
                FileHandleList::Complete(list) => list.len(),
                _ => unreachable!(),
            };
            table.push([snapshot.tag().to_owned(), file_count.to_string()]);
        }
    }

    print_table(
        Some(&["SNAPSHOT".to_owned(), "FILES".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn print_snapshot_summary(repo: &Repository, snapshot: &SnapshotId) -> Result<(), Box<dyn Error>> {
    let mut table = vec![];

    if let PackId::Pack(pack_name) = snapshot.pack() {
        let pack = repo.open_pack(&pack_name)?;
        // Get a snapshot with a complete file list.
        let entries = pack.index().entries_from_snapshot(snapshot.tag()).unwrap();
        for entry in entries {
            table.push([
                hex::encode(entry.object.checksum).to_string(),
                entry.object.size.to_string(),
                Path::display(entry.path.as_ref()).to_string(),
            ]);
        }
    } else {
        let loose_index = repo.loose_index()?;
        // Get a snapshot with a complete file list.
        let entries = loose_index.entries_from_snapshot(snapshot.tag()).unwrap();
        for entry in entries {
            table.push([
                hex::encode(entry.object.checksum).to_string(),
                entry.object.size.to_string(),
                Path::display(entry.path.as_ref()).to_string(),
            ]);
        }
    }

    print_table(
        Some(&["CHECKSUM".to_owned(), "SIZE".to_owned(), "FILE".to_owned()]),
        table.iter(),
    );
    Ok(())
}

fn dir_size(path: &std::path::Path) -> io::Result<u64> {
    let sizes = WalkDir::new(path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        .map(|e| Ok(e.metadata()?.len()))
        .collect::<io::Result<Vec<_>>>()?;

    Ok(sizes.iter().sum())
}
