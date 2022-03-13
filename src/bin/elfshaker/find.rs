//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::error::Error;

use super::utils::{open_repo_from_cwd, print_table};

pub(crate) const SUBCOMMAND: &str = "find";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let term = matches.value_of("term").unwrap();
    let repo = open_repo_from_cwd()?;

    let mut table = vec![];
    for pack_id in repo.packs()? {
        for snapshot in repo.load_index_snapshots(&pack_id)? {
            table.push([snapshot.to_string(), pack_id.to_string()])
        }
    }

    let table = repo
        .packs()?
        .into_iter()
        .flat_map(|p| {
            repo.load_index_snapshots(&p).map(|pack_index| {
                pack_index
                    .into_iter()
                    .filter_map(|s| {
                        s.contains(term)
                            .then(|| std::array::IntoIter::new([s.to_string(), p.to_string()]))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .flatten();

    let i = std::array::IntoIter::new(["SNAPSHOT".to_owned(), "PACK".to_owned()]);
    print_table(Some(i), table);
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
