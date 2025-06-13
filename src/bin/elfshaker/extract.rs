//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::error::Error;

use clap::{Arg, ArgAction, ArgMatches, Command};
use log::{info, warn};

use super::utils::{create_percentage_print_reporter, open_repo_from_cwd};
use elfshaker::packidx::PackError;
use elfshaker::repo::{Error as RepoError, ExtractOptions};

pub(crate) const SUBCOMMAND: &str = "extract";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let snapshot = matches.get_one::<String>("snapshot").unwrap();
    let is_reset = matches.get_flag("reset");
    let is_verify = matches.get_flag("verify");
    let is_force = matches.get_flag("force");

    // Parse --threads
    let threads: u32 = match matches.get_one::<String>("threads").unwrap().parse()? {
        0 => {
            let phys_cores = num_cpus::get_physical();
            info!(
                "-T|--threads=0: defaulting to number of physical cores (OS reports {} cores)",
                phys_cores
            );
            phys_cores as u32
        }
        n => n,
    };

    let mut repo = open_repo_from_cwd(data_dir)?;
    let new_head = match repo.find_snapshot(snapshot) {
        Err(RepoError::PackError(PackError::SnapshotNotFound(_))) => {
            info!("Snapshot not available locally. Updating remotes...");
            repo.update_remotes()?;
            repo.find_snapshot(snapshot)?
        }
        r => r?,
    };

    match repo.read_head()? {
        (Some(h), _) if h == new_head && !is_reset => {
            // The specified snapshot is already extracted and --reset is not specified,
            // so this is a no-op.
            warn!(
                "HEAD is already at {} and --reset is not specified. Exiting early...",
                h,
            );
            return Ok(());
        }
        _ => {}
    };

    let mut opts = ExtractOptions::default();
    opts.set_verify(is_verify);
    opts.set_reset(is_reset);
    opts.set_force(is_force);
    opts.set_num_workers(threads);

    repo.set_progress_reporter(|msg| create_percentage_print_reporter(msg, 5));
    let result = repo.extract_snapshot(new_head.clone(), opts)?;

    eprintln!("A \t{} files", result.added_file_count);
    eprintln!("D \t{} files", result.removed_file_count);
    eprintln!("M \t{} files", result.modified_file_count);
    eprintln!("Extracted '{new_head}'");

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Can be used to extract a snapshot.")
        .arg(
            Arg::new("snapshot")
                .required(true)
                .index(1)
                .help("The tag of the snapshot to extract."),
        )
        .arg(Arg::new("reset").long("reset").help(
            "Specifying this ignores the current HEAD and extract all files from the snapshot. \
            When this flag is not specified, only an incremental file update is done.",
        ).action(ArgAction::SetTrue))
        .arg(
            Arg::new("verify")
                .long("verify")
                .help("Enables SHA-1 verification of the extracted files. This has a small performance overhead.")
                .action(ArgAction::SetTrue)
        )
        .arg(Arg::new("force")
                .long("force")
                .help("Disables certain runtime checks that aim to detect unexpected file modification and prevent data loss.")
                .action(ArgAction::SetTrue)
            )
        .arg(Arg::new("threads")
                .short('T')
                .long("threads")
                // .takes_value(true)
                .help("Use the specified number of worker threads for decompression. \
                      The number of threads used is proportional to the memory needed for decompression.")
                .default_value("0"))
}
