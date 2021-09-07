//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use rand::RngCore;
use std::{collections::HashMap, error::Error, str::FromStr};

use super::utils::{find_pack_with_snapshot, open_repo_from_cwd};
use elfshaker::repo::{ExtractOptions, PackId, SnapshotId};

pub(crate) const SUBCOMMAND: &str = "show";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let pack = matches.value_of("pack");
    let snapshot = matches.value_of("snapshot").unwrap();
    let paths: Vec<_> = matches.values_of_os("path").unwrap().collect();

    let mut repo = open_repo_from_cwd()?;
    let pack = match pack {
        Some(pack) => PackId::from_str(pack)?,
        None => find_pack_with_snapshot(repo.index(), snapshot)?,
    };
    let snapshot = SnapshotId::new(pack, snapshot)?;
    let source_pack;
    let unpacked_index;
    let pack_index = if let PackId::Packed(name) = snapshot.pack() {
        source_pack = Some(repo.open_pack(name)?);
        source_pack.as_ref().unwrap().index()
    } else {
        source_pack = None;
        unpacked_index = Some(repo.unpacked_index()?);
        unpacked_index.as_ref().unwrap()
    };

    let entries: HashMap<_, _> = pack_index
        .entries_from_snapshot(snapshot.tag())?
        .into_iter()
        .map(|e| (e.path().to_owned(), e))
        .collect();

    // Attempt to find all entries by the paths specified on the command line.
    let selected_entries: Option<Vec<_>> = paths
        .iter()
        .map(|p| entries.get(p.to_owned()).cloned())
        .collect();
    // And fail-fast if any are missing.
    let selected_entries = match selected_entries {
        Some(e) => e,
        None => return Err("Some of the paths did not match files in the snapshot!".into()),
    };

    let temp_dir = {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(&bytes)
    };
    let temp_dir = std::env::temp_dir().join(temp_dir);

    let do_extract = || -> Result<(), Box<dyn Error>> {
        let mut opts = ExtractOptions::default();
        opts.set_verify(true);

        repo.extract_entries(source_pack, &selected_entries, &temp_dir, opts)?;

        // Dump the contents of all entries to stdout.
        for e in &selected_entries {
            let path = temp_dir.join(e.path());
            std::io::copy(&mut std::fs::File::open(path)?, &mut std::io::stdout())?;
        }
        Ok(())
    };

    let extract_result = do_extract();
    std::fs::remove_dir_all(temp_dir)?;
    extract_result?;

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about("Shows the contents of the files in the snapshot.")
        .arg(
            Arg::with_name("snapshot")
                .required(true)
                .index(1)
                .help("The snapshot in which to to look for the files."),
        )
        .arg(
            Arg::with_name("pack")
                .takes_value(true)
                .short("P")
                .long("pack")
                .value_name("name")
                .help("Specifies the pack file to use."),
        )
        .arg(
            Arg::with_name("path")
                .takes_value(true)
                .multiple(true)
                .required(true)
                .help("Specifies the path(s) to show."),
        )
}
