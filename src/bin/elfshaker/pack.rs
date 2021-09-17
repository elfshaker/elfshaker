//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use log::info;
use std::{error::Error, str::FromStr};

use super::utils::{create_percentage_print_reporter, open_repo_from_cwd};
use elfshaker::repo::{PackId, PackOptions, Repository, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "pack";

/// Window log is currently not configurable; We use a hopefully reasonable value of 28 == 256MiB window log.
/// A configurable window log will require the user to specify the value during extract operations as well as pack operations.
const DEFAULT_COMPRESSION_WINDOW_LOG: u32 = 28;

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Parse pack name
    let pack = matches.value_of("pack").unwrap();
    let pack = PackId::from_str(pack)?;
    if matches!(pack, PackId::Unpacked) {
        return Err(format!("'{}' is a reserved name!", pack).into());
    }

    // Parse --compression-level
    let compression_level: i32 = matches.value_of("compression-level").unwrap().parse()?;
    let compression_level_range = zstd::compression_level_range();
    if !compression_level_range.contains(&compression_level) {
        return Err(format!(
            "Invalid compression level {} (value must be between {} and {})!",
            compression_level,
            compression_level_range.start(),
            compression_level_range.end(),
        )
        .into());
    }

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

    let is_update_supressed = matches.is_present("no-update-index");

    // Open the repo and unpacked index
    let mut repo = open_repo_from_cwd()?;
    let mut index = repo.unpacked_index()?;

    // Parse --frames
    let frames: u32 = match matches.value_of("frames").unwrap().parse()? {
        0 => {
            let unpacked_size = index.objects().iter().map(|o| o.size()).sum();
            let frames = get_frame_size_hint(unpacked_size);
            info!("--frames=0: using suggested number of frames = {}", frames);
            frames
        }
        n => n,
    };

    // No point in creating an empty pack.
    if index.is_empty() {
        return Err("There are no unpacked snapshots!".into());
    }

    // Here we produce the index for the resulting pack file from the unpacked index.
    // Use deltas for the differences between the snapshot file lists.
    index.use_file_list_deltas()?;
    // Reorder objects in a way that is suitable for compression.
    let mut object_indices: Vec<_> = (0..index.objects().len()).collect();
    // Sorting by object sizes has proven to be a good heuristic; we could allow
    // user-configurable heuristics in the future.
    object_indices.sort_unstable_by_key(|&o| index.objects()[o].size());
    // Apply the new indices.
    index.permute_objects(&object_indices)?;

    // Print progress every 5%
    let reporter = create_percentage_print_reporter("Compressing objects", 5);

    // Create a pack using the ordered "unpacked" index.
    repo.create_pack(
        &pack,
        &index,
        &PackOptions {
            compression_level,
            // We don't expose the windowLog option yet.
            compression_window_log: DEFAULT_COMPRESSION_WINDOW_LOG,
            num_workers: threads,
            num_frames: frames,
        },
        &reporter,
    )?;

    if let Some(head) = repo.head() {
        if *head.pack() == PackId::Unpacked {
            info!("Updating HEAD to point to the newly-created pack...");
            // The current HEAD was referencing a snapshot in the unpacked store.
            // Now that the snapshots has been packed, we need to update HEAD
            // to reference the packed snapshot.
            let new_head = SnapshotId::new(pack.clone(), head.tag()).unwrap();
            repo.update_head(&new_head)?;
        }
    }
    // Finally, delete the unpacked snapshots
    repo.remove_unpacked_all()?;

    if is_update_supressed {
        eprintln!(
            "Created pack '{}'. Remember to run update-index to update the repository index!",
            pack
        );
    } else {
        info!("Updating the repository index...");
        Repository::update_index(repo.path())?;
        eprintln!("Created pack '{}'.", pack);
    }

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    let compression_level_range = zstd::compression_level_range();

    App::new(SUBCOMMAND)
        .about("Packs all unpacked snapshots into a pack file, freeing up significant amounts of disk space.")
        .arg(
            Arg::with_name("pack")
                .takes_value(true)
                .required(true)
                .index(1)
                .value_name("name")
                .help("Specifies the name of the pack to create."),
        )
        .arg(
            Arg::with_name("threads")
                .short("T")
                .long("threads")
                .takes_value(true)
                .help("Use the specified number of worker threads for compression. \
                      The number of threads used is proportional to the memory needed for compression.")
                .default_value("0"),
        )
        .arg(
            Arg::with_name("compression-level")
                .takes_value(true)
                .long("compression-level")
                .help(leak_static_str(format!("The level of compression to use (between {} and {})",
                    compression_level_range.start(),
                    compression_level_range.end())))
                .default_value("22")
        )
        .arg(
            Arg::with_name("frames")
                .takes_value(true)
                .long("frames")
                .help(
                    "The number of frames to emit in the pack file. \
                    A lower number of frames limits the number of decompression \
                    processes that can run concurrently. A higher number of \
                    frames can result in poorer compression. Specify 0 to \
                    auto-detect the appropriate number of frames to emit.")
                .default_value("0")
        )
        .arg(
            Arg::with_name("no-update-index")
                .long("no-update-index")
                .help("Does not update the repository index automatically."),
        )
}

/// Extends the lifetime of the string to 'static.
/// The memory will only be reclaimed at process exit.
fn leak_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// This is the built-in heuristic that tells us how many frames to use based on the data size.
/// 1 frame / 512 MiB
const FRAME_PER_DATA_SIZE: u64 = 512 * 1024 * 1024;
fn get_frame_size_hint(unpacked_size: u64) -> u32 {
    // Divide by FRAME_PER_DATA_SIZE, rounding up
    ((unpacked_size + FRAME_PER_DATA_SIZE - 1) / FRAME_PER_DATA_SIZE) as u32
}
