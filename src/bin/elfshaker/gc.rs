//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use log::{error, info};
use std::error::Error;

use super::utils::{format_size, open_repo_from_cwd};
use elfshaker::repo::PackId;

pub(crate) const SUBCOMMAND: &str = "gc";

/// We use a mark-and-sweep approach to implement garbage collection and we
/// have two different types of GC for the two kinds of "objects" we have: loose
/// snapshots and loose objects.
///
/// We GC the loose snapshots first and then use the remaining loose snapshots
/// as the roots in the "object graph" for reachability analysis.
pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let dry_run = matches.get_flag("dry_run");
    let loose_snapshots = matches.get_flag("loose_snapshots");
    let loose_objects = matches.get_flag("loose_objects");

    if !loose_snapshots && !loose_objects {
        error!(
            "Specify what to GC (--loose-snapshots, --loose-objects). \
            Nothing to do!"
        );
        return Err("Invalid options!".into());
    }

    let repo = open_repo_from_cwd(data_dir)?;
    let mut remaining_roots = repo.loose_packs()?;

    let mut freed_bytes = 0;

    // GC the snapshots
    if loose_snapshots {
        // Mark loose snapshots/packs for removal
        let loose_packs = repo.find_redundant_loose_packs()?;
        for loose_pack in loose_packs {
            let index = remaining_roots
                .iter()
                .position(|x| *x == loose_pack)
                .unwrap();
            remaining_roots.remove(index);

            freed_bytes += repo.get_pack_disk_stats(&loose_pack)?.len;

            let PackId::Pack(ref name) = &loose_pack;
            println!("Deleting pack {name:?}");

            // Delete if requested
            if !dry_run {
                repo.delete_pack(&loose_pack)?;
            }
        }
    }

    // GC the loose objects
    if loose_objects {
        info!("Remaining roots: {:?}", remaining_roots);
        // Mark loose objects for removal
        let loose_objects = repo.find_unreferenced_objects(remaining_roots.into_iter())?;
        println!("Deleting {} objects", loose_objects.len());
        for loose_object in loose_objects {
            freed_bytes += repo.get_object_disk_stats(&loose_object)?.len;
            // Delete if requested
            if !dry_run {
                repo.delete_object(&loose_object)?;
            }
        }
    }

    // Print the size of the files that were removed (or would be removed if
    // --dry-run).
    println!("GC: Freed {}", format_size(freed_bytes));

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about(
            "Cleanup redundant snapshots and unreferenced objects. \
            This frees up disk space after creating a pack.",
        )
        .arg(
            Arg::new("dry_run")
                .long("dry-run")
                .help("Do not actually delete anything.")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("loose_snapshots")
                .short('s')
                .long("loose-snapshots")
                .help("Delete redundant loose snapshots which exist an a pack.")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("loose_objects")
                .short('o')
                .long("loose-objects")
                .help(
                    "Delete unreferenced loose objects (which are not needed \
                    by any snapshot).",
                )
                .action(clap::ArgAction::SetTrue),
        )
}
