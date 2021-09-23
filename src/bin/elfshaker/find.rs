//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::error::Error;

use super::utils::{open_repo_from_cwd, print_table};

pub(crate) const SUBCOMMAND: &str = "find";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let term = matches.value_of("term").unwrap();
    let repo = open_repo_from_cwd()?;
    let index = repo.index();

    let mut table = vec![];
    for snapshot in index
        .available_snapshots()
        .iter()
        .filter(|tag| tag.contains(term))
    {
        for pack in index.find_packs(snapshot) {
            table.push([snapshot.to_string(), pack.to_string()])
        }
    }

    print_table(
        Some(&["SNAPSHOT".to_owned(), "PACK".to_owned()]),
        table.iter(),
    );
    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about("Searches the repository index.")
        .arg(
            Arg::with_name("term")
                .required(true)
                .index(1)
                .default_value("")
                .help("The search term."),
        )
}
