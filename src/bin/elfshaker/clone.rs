//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2022 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{Arg, ArgMatches, Command};
use rand::RngCore;
use std::error::Error;
use std::{
    fs,
    path::{Path, PathBuf},
};

use super::utils::create_percentage_print_reporter;
use elfshaker::repo::Repository;

pub(crate) const SUBCOMMAND: &str = "clone";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let data_dir = std::path::Path::new(matches.get_one::<String>("data_dir").unwrap());
    let origin_url = matches.get_one::<String>("repository").unwrap();
    let directory = matches.get_one::<String>("directory").unwrap();

    let original_cwd = std::env::current_dir()?;
    if Path::new(directory).exists() {
        return Err(format!("'{directory}' already exists!").into());
    }

    let temp_directory = temporary_directory(Path::new(directory))?;
    if let Err(e) = do_clone(&temp_directory, data_dir, origin_url) {
        let _ = fs::remove_dir_all(temp_directory);
        return Err(e);
    };
    fs::rename(
        original_cwd.join(temp_directory),
        original_cwd.join(directory),
    )?;

    Ok(())
}

fn do_clone(work_dir: &Path, data_dir: &Path, origin_url: &str) -> Result<(), Box<dyn Error>> {
    // Resolve the data_dir relative to the work_dir.
    let data_dir = if data_dir.is_relative() {
        work_dir.join(data_dir)
    } else {
        data_dir.into()
    };

    fs::create_dir(work_dir)?;
    fs::create_dir_all(&data_dir)?;

    let mut repo = Repository::open_with_data_dir(work_dir, &data_dir)?;

    repo.set_progress_reporter(|msg| create_percentage_print_reporter(msg, 5));
    repo.add_remote("origin", origin_url)?;
    repo.update_remotes()?;
    Ok(())
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .about("Create a working directory configured from a remote index.")
        .long_about(
            "Creates a new working directory, configures the supplied .esi \
             index as the `origin` remote, and downloads its pack indexes. Pack \
             payloads are fetched on demand. The destination must not exist.",
        )
        .arg(
            Arg::new("repository")
                .required(true)
                .index(1)
                .value_name("URL")
                .help("URL of the remote .esi index."),
        )
        .arg(
            Arg::new("directory")
                .required(true)
                .index(2)
                .value_name("DIRECTORY")
                .help("New working directory to create."),
        )
}

fn temporary_directory(directory: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let name = directory
        .file_name()
        .ok_or("clone destination must have a directory name")?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!("-{}", create_random_name()));
    Ok(directory.with_file_name(temporary_name))
}

#[test]
fn temporary_directory_is_a_sibling() {
    for destination in ["mc", "parent/mc", "/tmp/parent/mc"] {
        let destination = Path::new(destination);
        let temporary = temporary_directory(destination).unwrap();
        assert_eq!(temporary.parent(), destination.parent());
        assert!(temporary
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".mc-"));
    }
}

fn create_random_name() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
