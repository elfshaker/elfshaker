//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, Arg, ArgMatches};
use std::error::Error;

use super::utils::{format_size, open_repo_from_cwd};
use elfshaker::repo::{PackId, Repository};

pub(crate) const SUBCOMMAND: &str = "list-packs";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.value_of("data_dir").unwrap());
    let format = matches
        .value_of_lossy("format")
        .expect("<format> not provided");

    let repo = open_repo_from_cwd(data_dir)?;

    print_packs(&repo, &format)?;

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about("Prints the list of packs available in the repository.")
        .arg(
            Arg::with_name("format")
                .long("format")
                .default_value("%p")
                .help(
                    "Pretty-print each result in the given format, where \
                    <format> is a string containing one or more of the \
                    following placeholders:\n\
                    \t%p - pack\n\
                    \t%h - human-readable size\n\
                    \t%b - size in bytes\n\
                    \t%c - number of snapshots\n",
                ),
        )
}

fn format_pack_row(fmt: &str, pack_id: &PackId, snapshot_count: usize, size: u64) -> String {
    fmt.to_owned()
        .replace("%p", &format!("{pack_id}"))
        .replace("%c", &snapshot_count.to_string())
        .replace("%b", &size.to_string())
        .replace("%h", &format_size(size))
}

fn print_packs(repo: &Repository, fmt: &str) -> Result<(), Box<dyn Error>> {
    let mut lines = vec![];

    for pack_id in repo.packs()? {
        let snapshot_count = repo.load_index_snapshots(&pack_id)?.len();
        let pack_size = repo
            .open_pack(&pack_id)
            .map(|pack| pack.file_size())
            .unwrap_or(0);
        lines.push(format_pack_row(fmt, &pack_id, snapshot_count, pack_size));
    }

    lines.sort();

    for line in lines {
        println!("{line}");
    }

    Ok(())
}
