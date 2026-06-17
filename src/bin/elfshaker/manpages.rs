//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::Command;
use clap_mangen::roff::{bold, line_break, roman, Roff};
use std::path::Path;
use std::{fs, io};

const MANYCLANGS_ESI_URL: &str =
    "https://github.com/elfshaker/manyclangs/releases/download/v0.9.0/aarch64-ubuntu2004.esi";
const MANYCLANGS_PROJECT_URL: &str = "https://github.com/elfshaker/manyclangs";

const TERMINOLOGY: &[(&str, &str)] = &[
    (
        "Working directory",
        "The directory containing the files that elfshaker stores and restores. \
         It is normally the directory from which you run elfshaker.",
    ),
    (
        "Data directory",
        "The directory where elfshaker keeps snapshots and its other state, \
         including packs, indexes, loose objects, remotes, and HEAD. It defaults \
         to elfshaker_data and can be selected with --data-dir.",
    ),
    (
        "Repository",
        "A working directory together with its associated data directory.",
    ),
    (
        "Snapshot",
        "A record of selected files from the working directory at a point in time.",
    ),
    (
        "Snapshot tag",
        "The name of a snapshot. You choose the tag when you store a snapshot and \
         use it later to list, inspect, or restore that snapshot. PACK:SNAPSHOT \
         identifies a snapshot in a specific pack.",
    ),
    (
        "Loose snapshot",
        "A snapshot stored as a loose index and loose objects in the data \
         directory. The store command creates this form. The pack command can \
         combine loose snapshots into a pack.",
    ),
    (
        "Pack",
        "A compressed file containing the data used by one or more snapshots. A \
         companion pack index records the snapshots and files available from \
         the pack.",
    ),
    (
        "Remote index (.esi)",
        "A small catalog of packs published by a remote source. It records where \
         each pack can be downloaded and the checksums of the pack and its pack \
         index. The clone and update commands fetch remote indexes and pack \
         indexes. Pack data is downloaded only when needed.",
    ),
    (
        "HEAD",
        "The snapshot elfshaker uses as the starting point for the next \
         incremental extraction. The store and extract commands update HEAD.",
    ),
];

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
    if command.get_name() == "elfshaker" {
        insert_terminology(&output_path)?;
    }
    if command.get_name() == "clone" {
        append_clone_example(&output_path)?;
    }
    format_placeholder_list(command, &output_path)?;
    mark_variadic_positionals(command, &output_path)?;
    Ok(())
}

fn insert_terminology(output_path: &Path) -> io::Result<()> {
    let mut page = fs::read_to_string(output_path)?;
    let insertion_point = page
        .find(".SH OPTIONS\n")
        .or_else(|| page.find(".SH SUBCOMMANDS\n"))
        .or_else(|| page.find(".SH VERSION\n"))
        .ok_or_else(|| io::Error::other("no section follows DESCRIPTION"))?;

    let mut section = Roff::new();
    section.control("SH", ["TERMINOLOGY"]);
    for (term, definition) in TERMINOLOGY {
        section.control("TP", []);
        section.text([bold(*term)]);
        section.text([roman(*definition)]);
    }

    page.insert_str(insertion_point, &section.to_roff());
    fs::write(output_path, page)
}

fn append_clone_example(output_path: &Path) -> io::Result<()> {
    let mut page = fs::read_to_string(output_path)?;
    let mut sections = Roff::new();

    sections.control("SH", ["EXAMPLES"]);
    sections.text([roman(
        "Clone the public manyclangs remote, which provides snapshots of prebuilt LLVM versions:",
    )]);
    sections.control("PP", []);
    sections.control("nf", []);
    sections.text([
        bold("elfshaker clone \\"),
        line_break(),
        bold(format!("  {MANYCLANGS_ESI_URL} \\")),
        line_break(),
        bold("  manyclangs"),
    ]);
    sections.control("fi", []);
    sections.control("PP", []);
    sections.text([roman(
        "This creates a working directory named manyclangs and downloads its pack indexes. \
         Pack data is fetched when a snapshot is needed.",
    )]);

    sections.control("SH", ["SEE ALSO"]);
    sections.text([roman("manyclangs project:")]);
    sections.control("UR", [MANYCLANGS_PROJECT_URL]);
    sections.text([roman(MANYCLANGS_PROJECT_URL)]);
    sections.control("UE", []);

    page.push_str(&sections.to_roff());
    fs::write(output_path, page)
}

fn format_placeholder_list(command: &Command, output_path: &Path) -> io::Result<()> {
    let Some(argument) = command
        .get_arguments()
        .find(|argument| argument.get_id() == "format")
    else {
        return Ok(());
    };
    let Some(help) = argument
        .get_long_help()
        .or_else(|| argument.get_help())
        .map(ToString::to_string)
    else {
        return Ok(());
    };

    let mut lines = help.lines();
    let Some(introduction) = lines.next() else {
        return Ok(());
    };
    let placeholders = lines
        .filter_map(|line| line.trim().split_once(" - "))
        .collect::<Vec<_>>();
    if placeholders.is_empty() {
        return Ok(());
    }

    let mut original = Roff::new();
    original.text([roman(&help)]);

    let mut replacement = Roff::new();
    replacement.text([roman(introduction)]);
    replacement.control("RS", ["4"]);
    let mut entries = Vec::new();
    for (index, (placeholder, meaning)) in placeholders.into_iter().enumerate() {
        if index > 0 {
            entries.push(line_break());
        }
        entries.extend([bold(placeholder), roman(" - "), roman(meaning)]);
    }
    replacement.text(entries);
    replacement.control("RE", []);

    let page = fs::read_to_string(output_path)?;
    let original = original.to_roff();
    let replaced = page.replacen(&original, &replacement.to_roff(), 1);
    if replaced == page {
        return Err(io::Error::other(format!(
            "could not find --format help in {}",
            output_path.display()
        )));
    }
    fs::write(output_path, replaced)
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
