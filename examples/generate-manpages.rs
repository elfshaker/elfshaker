//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.

#![allow(dead_code)]

#[path = "../src/bin/elfshaker/cli.rs"]
mod cli;
#[path = "../src/bin/elfshaker/clone.rs"]
mod clone;
#[path = "../src/bin/elfshaker/explode.rs"]
mod explode;
#[path = "../src/bin/elfshaker/extract.rs"]
mod extract;
#[path = "../src/bin/elfshaker/find.rs"]
mod find;
#[path = "../src/bin/elfshaker/gc.rs"]
mod gc;
#[path = "../src/bin/elfshaker/list.rs"]
mod list;
#[path = "../src/bin/elfshaker/list_files.rs"]
mod list_files;
#[path = "../src/bin/elfshaker/list_packs.rs"]
mod list_packs;
#[path = "../src/bin/elfshaker/manpages.rs"]
mod manpages;
#[path = "../src/bin/elfshaker/pack.rs"]
mod pack;
#[path = "../src/bin/elfshaker/show.rs"]
mod show;
#[path = "../src/bin/elfshaker/store.rs"]
mod store;
#[path = "../src/bin/elfshaker/update.rs"]
mod update;
#[path = "../src/bin/elfshaker/utils.rs"]
mod utils;

use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: cargo run --example generate-manpages -- <output-directory>")?;

    fs::create_dir_all(&output_dir)?;
    for entry in fs::read_dir(&output_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "1") {
            fs::remove_file(path)?;
        }
    }

    manpages::generate(cli::get_app(), &output_dir)?;
    Ok(())
}
