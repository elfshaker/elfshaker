//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, Command};

use crate::{
    clone, explode, extract, find, gc, list, list_files, list_packs, pack, show, store, update,
};

pub(crate) fn get_app() -> Command {
    Command::new("elfshaker")
        .override_usage("elfshaker [GLOBAL-OPTS] <SUBCOMMAND> [SUBCOMMAND-OPTS]")
        .version(env!("CARGO_PKG_VERSION"))
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
