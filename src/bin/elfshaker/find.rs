//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use std::error::Error;

use super::utils::{open_repo_from_cwd, print_table};

pub(crate) const SUBCOMMAND: &str = "find";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let term = matches.get_one::<String>("term").unwrap();
    let repo = open_repo_from_cwd(data_dir)?;

    let table = search(&repo, term)?
        .into_iter()
        .map(|s| IntoIterator::into_iter([s.tag().to_owned(), s.pack().to_string()]));

    let i = IntoIterator::into_iter(["SNAPSHOT".to_owned(), "PACK".to_owned()]);
    print_table(Some(i), table);
    Ok(())
}

/// Shared case-sensitive substring search. Never silently ignore broken indexes.
pub(crate) fn search(
    repo: &elfshaker::repo::Repository,
    term: &str,
) -> Result<Vec<elfshaker::repo::SnapshotId>, Box<dyn Error>> {
    let mut matches = Vec::new();
    for pack in repo.packs()? {
        for tag in repo.load_index_snapshots(&pack)? {
            if tag.contains(term) {
                matches.push(elfshaker::repo::SnapshotId::new(pack.clone(), &tag)?);
            }
        }
    }
    Ok(matches)
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Searches the repository index.")
        .arg(
            Arg::new("term")
                .required(true)
                .index(1)
                .default_value("")
                .help("The search term."),
        )
}
