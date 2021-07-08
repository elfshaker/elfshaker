//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use clap::{App, ArgMatches};
use log::{error, info};
use std::{
    collections::hash_map::Entry, collections::HashMap, error::Error, ffi::OsStr, fs::File,
    path::Path,
};
use walkdir::{DirEntry, WalkDir};

use elfshaker::repo::{Pack, Repository, INDEX_FILE, PACKS_DIR, PACK_INDEX_EXTENSION};

pub(crate) const SUBCOMMAND: &str = "update-index";

pub(crate) fn run(_matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let mut repo_dir = std::env::current_dir()?;
    repo_dir.push(&*Repository::data_dir());
    if !Path::exists(&repo_dir) {
        error!(
            "The directory {:?} is not an elfshaker repository!",
            repo_dir.parent().unwrap()
        );
        return Err(Box::new(std::io::Error::from(std::io::ErrorKind::NotFound)));
    }

    info!("Opening repository {:?}...", repo_dir);
    repo_dir.push(PACKS_DIR);
    let packs_dir = repo_dir;

    // File all pack index files under $REPO_DIR/packs.
    let packs = WalkDir::new(packs_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(is_pack_index)
        .collect::<Vec<_>>();

    info!("Found {} packs!", packs.len());
    let mut index: HashMap<String, Vec<String>> = HashMap::new();

    // Construct a top-level index from the pack indexes.
    for pack in &packs {
        let pack_path = pack.path();
        // Pack names must be valid unicode.
        let pack_name = pack_path.file_name().unwrap().to_str().unwrap();
        info!("Processing {}...", pack_name);
        let pack_name: String = {
            // Skip the extension
            let mut chars = pack_name.chars();
            chars.nth_back(PACK_INDEX_EXTENSION.len()).unwrap();
            chars.collect()
        };
        let pack_index = Pack::parse_index(File::open(pack_path)?)?;
        let snapshots = pack_index.snapshots();
        for snapshot in snapshots {
            match index.entry(snapshot.tag.clone()) {
                Entry::Occupied(mut o) => {
                    o.get_mut().push(pack_name.to_owned());
                }
                Entry::Vacant(v) => {
                    v.insert(vec![pack_name.to_owned()]);
                }
            }
        }
    }

    let mut index_path = std::env::current_dir()?;
    index_path.push(&*Repository::data_dir());
    index_path.push(INDEX_FILE);
    info!("Writing {:?}...", index_path);
    let mut writer = File::create(index_path)?;
    rmp_serde::encode::write(&mut writer, &index)?;

    Ok(())
}

fn is_pack_index(entry: &DirEntry) -> bool {
    entry.file_type().is_file()
        && match Path::new(entry.file_name())
            .file_name()
            .map(OsStr::to_str)
            .flatten()
        {
            Some(filename) => filename.ends_with(PACK_INDEX_EXTENSION),
            None => false,
        }
}

pub(crate) fn get_app() -> App<'static, 'static> {
    App::new(SUBCOMMAND)
}
