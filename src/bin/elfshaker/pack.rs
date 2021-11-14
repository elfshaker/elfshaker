//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use log::info;
use std::{error::Error, str::FromStr};

use super::utils::{create_percentage_print_reporter, open_repo_from_cwd};
use elfshaker::{
    packidx::PackIndex,
    repo::{PackId, PackOptions, SnapshotId},
};

pub(crate) const SUBCOMMAND: &str = "pack";

/// Window log is currently not configurable; We use a hopefully reasonable
/// value of 28 == 256MiB window log. A configurable window log will require the
/// user to specify the value during extract operations as well as pack
/// operations.
const DEFAULT_COMPRESSION_WINDOW_LOG: u32 = 28;

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Parse pack name
    let pack = matches.value_of("pack").unwrap();
    let pack = PackId::from_str(pack)?;
    let indexes = matches
        .values_of("indexes")
        .map(|opts| {
            opts.into_iter()
                .map(|s| PackId::from_str(s))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;

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

    let mut repo = open_repo_from_cwd()?;

    let indexes = indexes
        .map(Result::Ok)
        .unwrap_or_else(|| repo.loose_packs())?;

    // No point in creating an empty pack.
    if indexes.is_empty() {
        return Err("There are no loose snapshots!".into());
    }

    let mut new_index = PackIndex::new();

    for pack_id in &indexes {
        assert!(
            repo.is_pack_loose(pack_id),
            "packing non-loose indexes not yet supported"
        );
        let index = repo.load_index(pack_id)?;
        eprintln!("Packing {} {}", pack_id, index.snapshots().len());
        for s in index.snapshots() {
            let entries = index.entries_from_snapshot(s.tag())?;
            new_index.push_snapshot(s.tag(), &entries)?;
        }
    }

    // Parse --frames
    let frames: u32 = match matches.value_of("frames").unwrap().parse()? {
        0 => {
            let loose_size = new_index.objects().iter().map(|o| o.size).sum();
            let frames = get_frame_size_hint(loose_size);
            info!("--frames=0: using suggested number of frames = {}", frames);
            frames
        }
        n => n,
    };

    // Reorder objects in a way that is suitable for compression.
    let mut object_indices: Vec<_> = (0..new_index.objects().len()).collect();
    // Sorting by object sizes has proven to be a good heuristic; we could allow
    // user-configurable heuristics in the future. Another useful heuristic
    // would be to group by name as well as size, e.g. key on (sum(size of
    // objects of given name), name).
    object_indices.sort_unstable_by_key(|&o| new_index.objects()[o].size);
    // Apply the new indices.
    new_index.permute_objects(&object_indices)?;

    // Print progress every 5%
    let reporter = create_percentage_print_reporter("Compressing objects", 5);

    eprintln!("Compressing objects...");
    // Create a pack using the ordered "loose" index.
    repo.create_pack(
        &pack,
        &new_index,
        &PackOptions {
            compression_level,
            // We don't expose the windowLog option yet.
            compression_window_log: DEFAULT_COMPRESSION_WINDOW_LOG,
            num_workers: threads,
            num_frames: frames,
        },
        &reporter,
    )?;

    if let (Some(head), _) = repo.read_head()? {
        if indexes.iter().any(|pack_id| head.pack() == pack_id) {
            info!("Updating HEAD to point to the newly-created pack...");
            // The current HEAD was referencing a snapshot an index which has
            // been packed. Update HEAD to point into the new pack.
            let new_head = SnapshotId::new(pack, head.tag()).unwrap();
            repo.update_head(&new_head)?;
        }
    }

    // TODO: New algo needs to take an exclusive repository lock and run GC.
    // // Finally, delete the loose snapshots
    // repo.remove_loose_all()?;

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    let compression_level_range = zstd::compression_level_range();

    App::new(SUBCOMMAND)
        .about("Packs the given snapshots into a pack file.")
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
            Arg::with_name("indexes")
            .index(2)
                .multiple(true)
                .help("Specify the indexes of packs to include.")
        )
}

/// Extends the lifetime of the string to 'static.
/// The memory will only be reclaimed at process exit.
fn leak_static_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// This is the built-in heuristic that tells us how many frames to use based on
/// the data size. 1 frame / 512 MiB
const FRAME_PER_DATA_SIZE: u64 = 512 * 1024 * 1024;
fn get_frame_size_hint(object_size_total: u64) -> u32 {
    // Divide by FRAME_PER_DATA_SIZE, rounding up
    ((object_size_total + FRAME_PER_DATA_SIZE - 1) / FRAME_PER_DATA_SIZE) as u32
}
