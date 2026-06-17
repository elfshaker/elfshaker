//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod cli;
mod clone;
mod explode;
mod extract;
mod find;
mod gc;
mod list;
mod list_files;
mod list_packs;
#[cfg(test)]
mod manpages;
mod pack;
mod show;
mod store;
mod update;
mod utils;

use clap::{ArgMatches, Command};
use elfshaker::log::Logger;
use log::error;
use std::error::Error;

const ERROR_EXIT_CODE: i32 = 1;

fn main() {
    // Do not print diagnostic output when the program panics due to a broken pipe.
    // A broken pipe happens when the reader end of a pipe closes the pipe.
    // For example: elfshaker find ... | head ...
    utils::silence_broken_pipe();

    let mut app = cli::get_app();
    let matches = app.clone().get_matches();
    let is_verbose = matches.get_flag("verbose");
    Logger::init(if is_verbose {
        log::Level::Info
    } else {
        log::Level::Warn
    });

    if let Err(e) = run_subcommand(&mut app, matches) {
        error!("*FATAL*: {}", e);
        std::process::exit(ERROR_EXIT_CODE);
    }
}

fn run_subcommand(app: &mut Command, matches: ArgMatches) -> Result<(), Box<dyn Error>> {
    match matches.subcommand() {
        Some((extract::SUBCOMMAND, matches)) => extract::run(matches),
        Some((explode::SUBCOMMAND, matches)) => explode::run(matches),
        Some((store::SUBCOMMAND, matches)) => store::run(matches),
        Some((list::SUBCOMMAND, matches)) => list::run(matches),
        Some((list_packs::SUBCOMMAND, matches)) => list_packs::run(matches),
        Some((list_files::SUBCOMMAND, matches)) => list_files::run(matches),
        Some((pack::SUBCOMMAND, matches)) => pack::run(matches),
        Some((show::SUBCOMMAND, matches)) => show::run(matches),
        Some((find::SUBCOMMAND, matches)) => find::run(matches),
        Some((gc::SUBCOMMAND, matches)) => gc::run(matches),
        Some((update::SUBCOMMAND, matches)) => update::run(matches),
        Some((clone::SUBCOMMAND, matches)) => clone::run(matches),
        _ => {
            app.print_long_help()?;
            println!();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn manpages_in(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "1"))
            .map(|path| {
                let name = path.file_name().unwrap().into();
                (name, fs::read(path).unwrap())
            })
            .collect()
    }

    #[test]
    fn generated_manpages_are_current() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let generated_dir =
            std::env::temp_dir().join(format!("elfshaker-manpages-{}-{nonce}", std::process::id()));
        fs::create_dir(&generated_dir).unwrap();

        super::manpages::generate(super::cli::get_app(), &generated_dir).unwrap();

        let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("man");
        assert_eq!(
            manpages_in(&source_dir),
            manpages_in(&generated_dir),
            "generated manpages differ; run `cargo run --example generate-manpages -- man`"
        );

        fs::remove_dir_all(generated_dir).unwrap();
    }

    #[test]
    fn show_accepts_multiple_paths() {
        let matches = super::cli::get_app()
            .try_get_matches_from(["elfshaker", "show", "snapshot", "first", "second"])
            .unwrap();
        let (_, show) = matches.subcommand().unwrap();
        let paths: Vec<_> = show.get_many::<String>("path").unwrap().cloned().collect();

        assert_eq!(paths, ["first", "second"]);
    }

    #[test]
    fn list_accepts_multiple_packs() {
        let matches = super::cli::get_app()
            .try_get_matches_from(["elfshaker", "list", "first", "second"])
            .unwrap();
        let (_, list) = matches.subcommand().unwrap();
        let packs: Vec<_> = list.get_many::<String>("pack").unwrap().cloned().collect();

        assert_eq!(packs, ["first", "second"]);
    }

    #[test]
    fn pack_accepts_multiple_indexes() {
        let matches = super::cli::get_app()
            .try_get_matches_from(["elfshaker", "pack", "combined", "first", "second"])
            .unwrap();
        let (_, pack) = matches.subcommand().unwrap();
        let indexes: Vec<_> = pack
            .get_many::<String>("indexes")
            .unwrap()
            .cloned()
            .collect();

        assert_eq!(indexes, ["first", "second"]);
    }

    #[test]
    fn store_rejects_multiple_file_list_sources() {
        let error = super::cli::get_app()
            .try_get_matches_from([
                "elfshaker",
                "store",
                "snapshot",
                "--files-from",
                "files",
                "--files0-from",
                "files0",
            ])
            .unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn pack_rejects_multiple_snapshot_sources() {
        let conflicting_arguments = [
            [
                "--snapshots-from",
                "snapshots",
                "--snapshots0-from",
                "snapshots0",
            ],
            ["--snapshots-from", "snapshots", "loose/first", ""],
            ["--snapshots0-from", "snapshots0", "loose/first", ""],
        ];

        for arguments in conflicting_arguments {
            let args = ["elfshaker", "pack", "combined"].iter().copied().chain(
                arguments
                    .iter()
                    .copied()
                    .filter(|argument| !argument.is_empty()),
            );
            let error = super::cli::get_app()
                .try_get_matches_from(args)
                .unwrap_err();

            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }
}
