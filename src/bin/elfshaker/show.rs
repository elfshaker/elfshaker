//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use rand::RngCore;
use std::ffi::OsString;
use std::{collections::HashMap, error::Error};

use super::utils::open_repo_from_cwd;
use elfshaker::repo::fs::open_file;
use elfshaker::repo::ExtractOptions;

pub(crate) const SUBCOMMAND: &str = "show";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let snapshot = matches.get_one::<String>("snapshot").unwrap().as_str();
    let paths: Vec<_> = matches.get_many::<String>("path").unwrap().collect();

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
        .map(|p| entries.get(OsString::from(p).as_os_str()).cloned())
        .collect();
    // And fail-fast if any are missing.
    let selected_entries = match selected_entries {
        Some(e) => e,
        None => return Err("Some of the paths did not match files in the snapshot!".into()),
    };

    let temp_dir = {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };
    let temp_dir = std::env::temp_dir().join(temp_dir);

    let mut do_extract = || -> Result<(), Box<dyn Error>> {
        let mut opts = ExtractOptions::default();
        opts.set_verify(true);

        repo.extract_entries(snapshot.pack(), &selected_entries, &temp_dir, opts)?;

        // Dump the contents of all entries to stdout.
        for e in &selected_entries {
            let path = temp_dir.join(&e.path);
            std::io::copy(&mut open_file(path)?, &mut std::io::stdout())?;
        }
        Ok(())
    };

    let extract_result = do_extract();
    std::fs::remove_dir_all(temp_dir)?;
    extract_result?;

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Shows the contents of the files in the snapshot.")
        .arg(
            Arg::new("snapshot")
                .required(true)
                .index(1)
                .help("The snapshot in which to to look for the files."),
        )
        .arg(
            Arg::new("path")
                .required(true)
                .index(2)
                .help("Specifies the path(s) to show."),
        )
}
