//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{error::Error, str::FromStr};

use clap::{App, Arg, ArgMatches};
use log::{info, warn};

use super::utils::{find_pack_with_snapshot, open_repo_from_cwd};
use elfshaker::repo::{ExtractOptions, PackId, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "extract";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let snapshot = matches.value_of("snapshot").unwrap();
    let pack = matches.value_of("pack");
    let is_reset = matches.is_present("reset");
    let is_verify = matches.is_present("verify");
    let is_force = matches.is_present("force");

    // Parse --threads
    let threads: u32 = match matches.value_of("threads").unwrap().parse()? {
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
    opts.set_num_workers(threads);

    let result = repo.extract_snapshot(new_head.clone(), opts)?;

    eprintln!("A \t{} files", result.added_file_count);
    eprintln!("D \t{} files", result.removed_file_count);
    eprintln!("M \t{} files", result.modified_file_count);
    eprintln!("Extracted '{}'", new_head);

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
        .arg(Arg::with_name("threads")
                .short("T")
                .long("threads")
                .takes_value(true)
                .help("Use the specified number of worker threads for decompression. \
                      The number of threads used is proportional to the memory needed for decompression.")
                .default_value("0"))
}
