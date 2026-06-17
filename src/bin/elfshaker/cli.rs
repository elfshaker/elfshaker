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
        .about("Store and restore deduplicated snapshots of a working directory.")
        .long_about(
            "elfshaker stores snapshots of the current working directory in a \
             separate data directory. Snapshots may remain loose or be combined \
             into compressed packs.\n\nCommands that restore files write to the \
             working directory. --data-dir only selects the data directory.",
        )
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
                .help("Show informational progress and diagnostic messages.")
                .global(true)
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("data_dir")
                .long("data-dir")
                .env("ELFSHAKER_DATA")
                .hide_env_values(true)
                .value_name("PATH")
                .help("Use PATH as the elfshaker data directory.")
                .long_help(
                    "Use PATH as the data directory containing packs, indexes, \
                     loose objects, remotes, and HEAD. This does not change the \
                     working directory.",
                )
                .default_value(elfshaker::repo::REPO_DIR)
                .global(true),
        )
}
