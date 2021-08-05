//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{error::Error, str::FromStr};

use clap::{App, Arg, ArgMatches};
use log::{error, warn};

use super::utils::{find_pack_with_snapshot, open_repo_from_cwd};
use elfshaker::packidx::PackError;
use elfshaker::repo::{Error as RepoError, ExtractOptions, PackId, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "extract";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let snapshot = matches.value_of("snapshot").unwrap();
    let pack = matches.value_of("pack");
    let is_reset = matches.is_present("reset");
    let is_verify = matches.is_present("verify");
    let is_force = matches.is_present("force");

    let mut repo = open_repo_from_cwd()?;

    // Determine the pack file to use.
    let pack = match pack {
        Some(pack) => PackId::from_str(pack)?,
        None => find_pack_with_snapshot(repo.index(), snapshot)?,
    };

    let new_head = SnapshotId::new(pack, snapshot)?;
    if repo.head().as_ref() == Some(&new_head) && !is_reset {
        // The specified snapshot is already extracted and --reset is not specified,
        // so this is a no-op.
        warn!(
            "HEAD is already at {} and --reset is not specified. Exiting early...",
            repo.head().as_ref().unwrap()
        );
        return Ok(());
    }

    let mut opts = ExtractOptions::default();
    opts.set_verify(is_verify);
    opts.set_reset(is_reset);
    opts.set_force(is_force);

    match repo.extract(new_head.clone(), opts) {
        Ok(result) => {
            println!("A \t{} files", result.added_file_count);
            println!("D \t{} files", result.removed_file_count);
            println!("M \t{} files", result.modified_file_count);
            println!("Extracted '{}'", new_head);
        }
        Err(e) => match e {
            RepoError::PackError(PackError::SnapshotNotFound) => {
                error!(
                    "Snapshot was expected to be available in {}, but was not found \
                    in the index specific to the pack (.pack.idx)",
                    new_head.pack()
                );
            }
            RepoError::DirtyWorkDir => {
                error!("Some files in the repository have been removed or modified unexpectedly! \
                        You can use --force to skip this check, but this might result in DATA LOSS!");
            }
            e => return Err(Box::new(e)),
        },
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about("Can be used to extract a snapshot.")
        .arg(
            Arg::with_name("snapshot")
                .required(true)
                .index(1)
                .help("The tag of the snapshot to extract."),
        )
        .arg(
            Arg::with_name("pack")
                .takes_value(true)
                .short("P")
                .long("pack")
                .value_name("name")
                .help(
                    "Specifies the pack to use when extracting the snapshot. \
                        You generally only need to provide this flag when there \
                        are multiple packs providing the same snapshot.",
                ),
        )
        .arg(Arg::with_name("reset").long("reset").help(
            "Specifying this ignores the current HEAD and extract all files from the snapshot. \
            When this flag is not specified, only an incremental file update is done.",
        ))
        .arg(
            Arg::with_name("verify")
                .long("verify")
                .help("Enables SHA-1 verification of the extracted files. This has a small performance overhead."),
        )
        .arg(Arg::with_name("force")
                .long("force")
                .help("Disables certain runtime checks that aim to detect unexpected file modification and prevent data loss."))
}
