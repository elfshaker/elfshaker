//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{builder::OsStringValueParser, Arg, ArgMatches, Command};
use std::{collections::HashMap, error::Error, ffi::OsString, path::Path};

use super::utils::open_repo_from_cwd;
use elfshaker::packidx::PackPath;

pub(crate) const SUBCOMMAND: &str = "show";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let snapshot = matches.get_one::<String>("snapshot").unwrap().as_str();
    let paths: Vec<_> = matches.get_many::<OsString>("path").unwrap().collect();

    let mut repo = open_repo_from_cwd(data_dir)?;
    let snapshot = repo.find_snapshot(snapshot)?;
    let pack_index = repo.load_index(snapshot.pack())?;

    let entries: HashMap<_, _> = pack_index
        .entries_from_handles(
            pack_index
                .resolve_snapshot(snapshot.tag())
                .expect("failed to resolve snapshot")
                .iter(), // Temporary.
        )?
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    // Attempt to find all entries by the paths specified on the command line.
    let selected_entries: Option<Vec<_>> = paths
        .iter()
        .map(|path| {
            PackPath::from_native(Path::new(path))
                .ok()
                .and_then(|path| entries.get(&path).cloned())
        })
        .collect();
    // And fail-fast if any are missing.
    let selected_entries = match selected_entries {
        Some(e) => e,
        None => return Err("Some of the paths did not match files in the snapshot!".into()),
    };

    let objects = repo.read_entry_objects(snapshot.pack(), &selected_entries, 1)?;
    let mut stdout = std::io::stdout().lock();
    for object in objects {
        std::io::Write::write_all(&mut stdout, &object)?;
    }

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Write files from a snapshot to standard output.")
        .long_about(
            "Writes the selected snapshot files to standard output in \
             command-line order. When multiple paths are given, their contents \
             are concatenated without separators. The command fails without \
             producing output if any requested path is absent.",
        )
        .arg(
            Arg::new("snapshot")
                .required(true)
                .index(1)
                .value_name("SNAPSHOT")
                .help("Snapshot tag or PACK:SNAPSHOT identifier to read."),
        )
        .arg(
            Arg::new("path")
                .required(true)
                .index(2)
                .num_args(1..)
                .value_parser(OsStringValueParser::new())
                .value_name("PATH")
                .help("Path within the snapshot to write."),
        )
}
