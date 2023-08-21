//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod clone;
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

use clap::{crate_version, App, Arg, ArgMatches};
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
    let is_verbose = matches.is_present("verbose");
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

fn run_subcommand(app: &mut App, matches: ArgMatches) -> Result<(), Box<dyn Error>> {
    match matches.subcommand() {
        (extract::SUBCOMMAND, Some(matches)) => extract::run(matches),
        (store::SUBCOMMAND, Some(matches)) => store::run(matches),
        (list::SUBCOMMAND, Some(matches)) => list::run(matches),
        (list_packs::SUBCOMMAND, Some(matches)) => list_packs::run(matches),
        (list_files::SUBCOMMAND, Some(matches)) => list_files::run(matches),
        (pack::SUBCOMMAND, Some(matches)) => pack::run(matches),
        (show::SUBCOMMAND, Some(matches)) => show::run(matches),
        (find::SUBCOMMAND, Some(matches)) => find::run(matches),
        (gc::SUBCOMMAND, Some(matches)) => gc::run(matches),
        (update::SUBCOMMAND, Some(matches)) => update::run(matches),
        (clone::SUBCOMMAND, Some(matches)) => clone::run(matches),
        _ => {
            app.print_long_help()?;
            println!();
            Ok(())
        }
    }
}

fn get_app() -> App<'static, 'static> {
    App::new("elfshaker")
        .version(crate_version!())
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
        .arg(
            Arg::with_name("verbose")
                .long("verbose")
                .help("Enables verbose description of the execution process.")
                .global(true),
        )
        .arg(
            Arg::with_name("data_dir")
                .long("data-dir")
                .env("ELFSHAKER_DATA")
                .hide_env_values(true)
                .help("Set the path to the elfshaker repository.")
                .default_value(elfshaker::repo::REPO_DIR)
                .global(true),
        )
}
