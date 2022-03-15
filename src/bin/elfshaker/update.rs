//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, ArgMatches};
use std::error::Error;

use super::utils::open_repo_from_cwd;

pub(crate) const SUBCOMMAND: &str = "update";

pub(crate) fn run(_matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let repo = open_repo_from_cwd()?;
    repo.update_remotes()?;
    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND).about("Updates the remote indexes.")
}
