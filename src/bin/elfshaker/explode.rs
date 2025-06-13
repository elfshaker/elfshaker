//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2022 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use log::info;

use crate::utils::open_repo_from_cwd;

pub(crate) const SUBCOMMAND: &str = "explode";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let mut repo = open_repo_from_cwd(data_dir)?;
    // eprint!("Hello, explode");

    // repo.loose_packs()
    // let packs = repo.packs()?;
    let pack_name = matches.get_one::<String>("pack-name").unwrap();
    let pack_id = repo
        .is_pack(pack_name)?
        .ok_or(elfshaker::repo::Error::PackNotFound(pack_name.to_string()))?;

    info!("Exploding pack: {}", pack_id);
    repo.explode_pack(&pack_id)?;

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Explodes a pack into loose objects")
        .arg(
            Arg::new("pack-name")
                .required(true)
                .index(1)
                .help("The name of the pack to explode."),
        )
}
