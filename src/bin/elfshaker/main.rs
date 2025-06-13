//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod clone;
mod explode;
mod extract;
mod find;
mod gc;
mod list;
mod list_files;
mod list_packs;
mod pack;
mod show;
mod store;
mod update;
mod utils;

use clap::{Arg, ArgMatches, Command};
use elfshaker::log::Logger;
use log::error;
use std::error::Error;

const ERROR_EXIT_CODE: i32 = 1;

fn main() {
    // Do not print diagnostic output when the program panics due to a broken pipe.
    // A broken pipe happens when the reader end of a pipe closes the pipe.
    // For example: elfshaker find ... | head ...
    utils::silence_broken_pipe();

    let mut app = get_app();
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

fn get_app() -> Command {
    Command::new("elfshaker")
        .override_usage("elfshaker [GLOBAL-OPTS] <SUBCOMMAND> [SUBCOMMAND-OPTS]")
        // .version(crate_version!())
        .subcommand(extract::get_app())
        .subcommand(store::get_app())
        .subcommand(list::get_app())
        .subcommand(list_packs::get_app())
        .subcommand(list_files::get_app())
        .subcommand(pack::get_app())
        .subcommand(show::get_app())
        .subcommand(find::get_app())
        .subcommand(gc::get_app())
        .subcommand(update::get_app())
        .subcommand(clone::get_app())
        .subcommand(explode::get_app())
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .help("Enables verbose description of the execution process.")
                .global(true)
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("data_dir")
                .long("data-dir")
                .env("ELFSHAKER_DATA")
                .hide_env_values(true)
                .help("Set the path to the elfshaker repository.")
                .default_value(elfshaker::repo::REPO_DIR)
                .global(true),
        )
}
