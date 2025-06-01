//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2022 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, ArgMatches};
use std::error::Error;


pub(crate) const SUBCOMMAND: &str = "explode";

pub(crate) fn run(_: &ArgMatches) -> Result<(), Box<dyn Error>> {

    Ok(())
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
        .about("Explodes a pack into loose objects")
}

