//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod extract;
mod update_index;

use clap::{App, Arg, ArgMatches};
use elfshaker::log::Logger;
use log::error;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let mut app = get_app();
    let matches = app.clone().get_matches();
    let is_verbose = matches.is_present("verbose");
    Logger::init(if is_verbose {
        log::Level::Info
    } else {
        log::Level::Warn
    });

    if let Err(e) = run_subcommand(&mut app, matches) {
        error!("*FATAL*: {:?}", e);
        return Err(e);
    }

    Ok(())
}

fn run_subcommand(app: &mut App, matches: ArgMatches) -> Result<(), Box<dyn Error>> {
    if let Some(matches) = matches.subcommand_matches(extract::SUBCOMMAND) {
        extract::run(matches)?;
    } else if let Some(matches) = matches.subcommand_matches(update_index::SUBCOMMAND) {
        update_index::run(matches)?;
    } else {
        app.print_long_help()?;
    }
    Ok(())
}

fn get_app() -> App<'static, 'static> {
    App::new("elfshaker")
        .subcommand(extract::get_app().help("Can be used to extract a snapshot."))
        .subcommand(update_index::get_app().help("Updates the repository index."))
        .arg(
            Arg::with_name("verbose")
                .long("verbose")
                .help("Enables verbose description of the execution process."),
        )
}
