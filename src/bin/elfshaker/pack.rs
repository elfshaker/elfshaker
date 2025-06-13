//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::Command;
use clap::{Arg, ArgMatches};
use log::error;
use log::info;
use std::{error::Error, fs, io, ops::ControlFlow, str::FromStr};

use super::utils::{create_percentage_print_reporter, open_repo_from_cwd};
use elfshaker::{
    packidx::PackIndex,
    repo::{PackId, PackOptions, Repository, SnapshotId},
};

pub(crate) const SUBCOMMAND: &str = "pack";

/// Window log is currently not configurable; We use a hopefully reasonable
/// value of 28 == 256MiB window log. A configurable window log will require the
/// user to specify the value during extract operations as well as pack
/// operations.
const DEFAULT_COMPRESSION_WINDOW_LOG: u32 = 28;

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    // Parse pack name
    let pack = matches.get_one::<String>("pack").unwrap();
    let snapshots_from = matches.get_one::<String>("snapshots-from");
    let snapshots0_from = matches.get_one::<String>("snapshots0-from");
    let pack = PackId::from_str(pack)?;
    let indexes = matches
        .get_many::<String>("indexes")
        .map(|opts| {
            opts.into_iter()
                .map(|s| s.as_str())
                .map(PackId::from_str)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;

    // Parse --compression-level
    let compression_level: i32 = matches
        .get_one::<String>("compression-level")
        .unwrap()
        .parse()?;
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

    let open = |filename| {
        if filename == "-" {
            Box::new(io::stdin()) as Box<dyn io::Read>
        } else {
            Box::new(fs::File::open(filename).unwrap()) as Box<dyn io::Read>
        }
    };
    let snapshots = match (snapshots_from, snapshots0_from, indexes) {
        (Some(s), None, None) => packs_from_list(&repo, open(s), b'\n'),
        (None, Some(s), None) => packs_from_list(&repo, open(s), b'\0'),
        (None, None, Some(s)) => Ok(s),
        (None, None, None) => repo.loose_packs(),
        _ => {
            error!("Cannot specify a combination of --snapshots-from, --snapshots0-from and <indexes>!");
            return Err("Invalid options!".into());
        }
    }?;

    // No point in creating an empty pack.
    if snapshots.is_empty() {
        return Err("There are no loose snapshots!".into());
    }

    let mut new_index = PackIndex::new();

    for pack_id in &snapshots {
        assert!(
            repo.is_pack_loose(pack_id),
            "packing non-loose indexes not yet supported"
        );
        let index = repo.load_index(pack_id)?;
        eprintln!("Packing {} {}", pack_id, index.snapshot_tags().len());
        index.for_each_snapshot(|snapshot, entries| {
            if let Err(e) = new_index.push_snapshot(snapshot.to_owned(), entries.clone()) {
                ControlFlow::Break(Result::<(), _>::Err(e))
            } else {
                ControlFlow::Continue(())
            }
        })?;
    }

    // Parse --frames
    let frames: u32 = match matches.get_one::<String>("frames").unwrap().parse()? {
        0 => {
            let loose_size = new_index.object_size_total();
            let frames = get_frame_size_hint(loose_size);
            info!("--frames=0: using suggested number of frames = {}", frames);
            frames
        }
        n => n,
    };

    // Print progress every 5%
    let reporter = create_percentage_print_reporter("Compressing objects", 5);

    eprintln!("Compressing objects...");
    // Create a pack using the ordered "loose" index.
    repo.create_pack(
        &pack,
        new_index,
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
        if snapshots.iter().any(|pack_id| head.pack() == pack_id) {
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

fn packs_from_list(
    repo: &Repository,
    mut reader: impl io::Read,
    separator: u8,
) -> Result<Vec<PackId>, elfshaker::repo::Error> {
    let mut buf = vec![];
    reader.read_to_end(&mut buf)?;

    buf.split(|c| *c == separator)
        .filter(|s| !s.is_empty())
        .map(std::str::from_utf8)
        .map(|s| match s {
            Ok(v) => repo.find_snapshot(v).map(|l| l.pack().clone()),
            Err(e) => {
                let msg = format!("Unable to decode snapshot list: {e}");
                Err(elfshaker::repo::Error::IOError(io::Error::new(
                    io::ErrorKind::InvalidData,
                    msg,
                )))
            }
        })
        .collect()
}

pub(crate) fn get_app() -> Command {
    let compression_level_range = zstd::compression_level_range();

    Command::new(SUBCOMMAND)
        .about("Packs the given snapshots into a pack file.")
        .arg(
            Arg::new("pack")
                .required(true)
                .index(1)
                .value_name("name")
                .help("Specifies the name of the pack to create."),
        )
        .arg(
            Arg::new("threads")
                .short('T')
                .long("threads")
                .help("Use the specified number of worker threads for compression. \
                      The number of threads used is proportional to the memory needed for compression.")
                .default_value("0"),
        )
        .arg(
            Arg::new("compression-level")
                .long("compression-level")
                .help(leak_static_str(format!("The ZStandard compression level to use (up to {}). Negative values enable fast compression.",
                    compression_level_range.end())))
                .default_value("10")
                .allow_hyphen_values(true)
        )
        .arg(
            Arg::new("frames")
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
            Arg::new("snapshots-from")
                .long("snapshots-from")
                .value_name("file")
                .help("Reads the list of snapshots to include in the pack from the specified file. '-' is taken to mean stdin."),
        )
        .arg(
            Arg::new("snapshots0-from")
                .long("snapshots0-from")
                .value_name("file")
                .help("Reads the NUL-separated (ASCII \\0) list of snapshots to include in the pack from the specified file. '-' is taken to mean stdin."),
        )
        .arg(
            Arg::new("indexes")
            .index(2)
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
    object_size_total.div_ceil(FRAME_PER_DATA_SIZE) as u32
}
