//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, ArgMatches};
use std::error::Error;

use elfshaker::repo::Repository;

pub(crate) const SUBCOMMAND: &str = "update-index";

pub(crate) fn run(_matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let repo_dir = std::env::current_dir()?;
    Repository::update_index(&repo_dir)?;
    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND).about("Updates the repository index.")
}
