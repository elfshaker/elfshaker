//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::Command;
use std::path::Path;
use std::{fs, io};

pub(crate) fn generate(mut command: Command, output_dir: &Path) -> io::Result<()> {
    command = command.disable_help_subcommand(true);
    command.build();
    generate_command(&command, output_dir)
}

fn generate_command(command: &Command, output_dir: &Path) -> io::Result<()> {
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        generate_command(subcommand, output_dir)?;
    }

    let output_path = clap_mangen::Man::new(command.clone())
        .source(format!("elfshaker {}", env!("CARGO_PKG_VERSION")))
        .manual("elfshaker Manual")
        .generate_to(output_dir)?;
    mark_variadic_positionals(command, &output_path)?;
    Ok(())
}

fn mark_variadic_positionals(command: &Command, output_path: &Path) -> io::Result<()> {
    let mut page = fs::read_to_string(output_path)?;

    for argument in command.get_positionals() {
        let range = argument.get_num_args().expect("command is built");
        if range.max_values() <= 1 {
            continue;
        }

        let name = argument
            .get_value_names()
            .map(|names| {
                names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| argument.get_id().to_string());
        let styled_name = format!("\\fI{name}\\fR");
        let marker = if argument.is_required_set() {
            format!("<{styled_name}>")
        } else {
            format!("[{styled_name}]")
        };
        page = page.replace(&marker, &format!("{marker}..."));
    }

    fs::write(output_path, page)
}
