//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod extract;
mod find;
mod list;
mod pack;
mod show;
mod store;
mod utils;

use clap::{App, Arg, ArgMatches};
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
        (pack::SUBCOMMAND, Some(matches)) => pack::run(matches),
        (show::SUBCOMMAND, Some(matches)) => show::run(matches),
        (find::SUBCOMMAND, Some(matches)) => find::run(matches),
        _ => {
            app.print_long_help()?;
            println!();
            Ok(())
        }
    }
}

fn get_app() -> App<'static, 'static> {
    App::new("elfshaker")
        .subcommand(extract::get_app())
        .subcommand(store::get_app())
        .subcommand(list::get_app())
        .subcommand(pack::get_app())
        .subcommand(show::get_app())
        .subcommand(find::get_app())
        .arg(
            Arg::with_name("verbose")
                .long("verbose")
                .help("Enables verbose description of the execution process.")
                .global(true),
        )
}
