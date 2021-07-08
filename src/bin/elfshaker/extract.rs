//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use log::{error, info, trace, warn};
use std::error::Error;

use elfshaker::log::measure;
use elfshaker::packidx::PackError;
use elfshaker::repo::{Error as RepoError, ExtractOptions, Repository, SnapshotId, HEAD_FILE};

pub(crate) const SUBCOMMAND: &str = "extract";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let snapshot = matches.value_of("snapshot").unwrap();
    let pack = matches.value_of("pack");
    let is_reset = matches.is_present("reset");
    let is_verify = matches.is_present("verify");
    let is_force = matches.is_present("force");

    // Open repo from cwd.
    let repo_path = std::env::current_dir()?;
    let (elapsed, open_result) = measure(|| Repository::open(&repo_path));
    let mut repo = match open_result {
        Ok(repo) => repo,
        Err(RepoError::CorruptRepositoryIndex) => {
            error!("The repository index is corrupt or missing! Run update-index and try again.");
            return Err(Box::new(RepoError::CorruptRepositoryIndex));
        }
        Err(RepoError::CorruptHead) => {
            let mut head_path = repo_path.join(&*Repository::data_dir());
            head_path.push(HEAD_FILE);
            error!(
                "The repository HEAD file ({:?}) is corrupt! Remove the file and try again.",
                head_path
            );
            return Err(Box::new(RepoError::CorruptHead));
        }
        Err(e) => return Err(Box::new(e)),
    };
    info!("Opening repository {:?} took {:?}", repo_path, elapsed);

    // Determine the pack file to use.
    let pack = match pack {
        Some(pack) => pack.to_owned(),
        None => {
            // Try to locate this snapshot.
            trace!("Looking for snapshot in top-level index...");
            let packs = repo.find_packs(snapshot);
            match packs.len() {
                0 => {
                    error!(
                        "Snapshot {} is not available in the known set of pack files! \
                        Make sure that the correct pack file is available, refresh the index, and try again!",
                        snapshot
                    );
                    return Err(Box::new(RepoError::PackError(PackError::SnapshotNotFound)));
                }
                1 => packs[0].clone(),
                _ => {
                    error!(
                        "The following pack files provide snapshot {}:\n{:?}\nSpecify which one to use with -P <pack>!",
                        snapshot, packs);
                    return Err(Box::new(RepoError::AmbiguousSnapshotMatch));
                }
            }
        }
    };

    let new_head = SnapshotId::new(&pack, snapshot);
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

    if let Err(e) = repo.extract(new_head.clone(), opts) {
        match e {
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
            _ => {}
        }
        return Err(Box::new(e));
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
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
