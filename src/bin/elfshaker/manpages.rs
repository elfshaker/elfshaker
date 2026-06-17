//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::Command;
use std::io;
use std::path::Path;

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

    clap_mangen::Man::new(command.clone())
        .source(format!("elfshaker {}", env!("CARGO_PKG_VERSION")))
        .manual("elfshaker Manual")
        .generate_to(output_dir)?;
    Ok(())
}
