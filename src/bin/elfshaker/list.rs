//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use elfshaker::repo::run_in_parallel;
use std::{error::Error, ops::ControlFlow, str::FromStr};

use super::utils::{format_size, open_repo_from_cwd};
use elfshaker::repo::{PackId, Repository};

pub(crate) const SUBCOMMAND: &str = "list";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let packs = matches.get_many::<String>("pack");
    let format = matches
        .get_one::<String>("format")
        .expect("<format> not provided");

    let repo = open_repo_from_cwd(data_dir)?;

    let packs = packs
        .map(|packs| {
            packs
                .map(|pack| PackId::from_str(pack))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or(repo.packs()?);

    print_snapshots(&repo, &packs, format)?;

    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("List snapshots in known pack indexes.")
        .long_about(
            "Lists snapshots from every known pack index, or only from the \
             selected packs when PACK is supplied. Output lines are sorted.",
        )
        .arg(
            Arg::new("pack")
                .index(1)
                .required(false)
                .num_args(0..)
                .value_name("PACK")
                .help("Pack index to list; may be repeated."),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .default_value("%s")
                .help(concat!(
                    "Pretty-print each result using placeholders:\n",
                    "  %s - fully-qualified snapshot\n",
                    "  %t - snapshot tag\n",
                    "  %h - human-readable size\n",
                    "  %b - size in bytes\n",
                    "  %n - number of files",
                )),
        )
}

fn format_snapshot_row(
    fmt: &str,
    pack_id: &PackId,
    snapshot: &str,
    size: u64,
    file_count: usize,
) -> String {
    fmt.to_owned()
        .replace("%s", &format!("{pack_id}:{snapshot}"))
        .replace("%t", snapshot)
        .replace("%h", &format_size(size))
        .replace("%b", &size.to_string())
        .replace("%n", &file_count.to_string())
}

fn is_file_size_required(fmt: &str) -> bool {
    fmt.contains("%h") || fmt.contains("%b")
}

fn print_snapshots(
    repo: &Repository,
    pack_ids: &[PackId],
    fmt: &str,
) -> Result<(), Box<dyn Error>> {
    // Process the packs in parallel
    let pack_results = run_in_parallel(
        num_cpus::get(),
        pack_ids.iter(),
        |pack_id| -> Result<_, elfshaker::repo::Error> {
            let index = repo.load_index(pack_id)?;
            let mut pack_lines = vec![];
            let mut iter = |snapshot, file_size, file_count| {
                pack_lines.push(format_snapshot_row(
                    fmt, pack_id, snapshot, file_size, file_count,
                ));
                ControlFlow::<(), ()>::Continue(())
            };

            if is_file_size_required(fmt) {
                index.for_each_snapshot(|snapshot, entries| {
                    let file_count = entries.len();
                    let file_size = entries.iter().map(|entry| entry.object_metadata.size).sum();
                    iter(snapshot, file_size, file_count)
                })?;
            } else {
                index.for_each_snapshot_file_count(|snapshot, file_count| {
                    iter(snapshot, 0, file_count as usize)
                })?;
            }

            Ok(pack_lines)
        },
    );

    // Aggregate the outputs and sort
    let mut lines = vec![];
    for pack_result in pack_results {
        let mut pack_lines = pack_result?;
        lines.append(&mut pack_lines);
    }
    lines.sort();

    for line in lines {
        println!("{line}");
    }

    Ok(())
}
