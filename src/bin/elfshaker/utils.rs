//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use elfshaker::log::measure;
use elfshaker::packidx::PackError;
use elfshaker::progress::ProgressReporter;
use elfshaker::repo::{Error as RepoError, PackId, Repository, RepositoryIndex, HEAD_FILE};
use log::{error, info, trace};
use std::io::Write;
use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Arc,
};

/// Prints the values as a table. The first set of values
/// is used for the table header.
pub(crate) fn print_table<R, C, I>(rows: R)
where
    R: Iterator<Item = C>,
    C: IntoIterator<Item = I>,
    I: std::fmt::Display,
{
    let strings = rows
        .map(|row| {
            row.into_iter()
                .map(|item| format!("{}", item))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    if strings.is_empty() {
        return;
    }

    let columns = strings[0].len();
    let column_sizes = (0..columns)
        .map(|i| strings.iter().map(|row| row[i].len()).max().unwrap())
        .collect::<Vec<_>>();
    for row in &strings {
        assert_eq!(columns, row.len());
        for (i, item) in row.iter().enumerate() {
            let pad = column_sizes[i] - item.len();
            print!("{} {:pad$}", item, "", pad = pad);
        }
        println!();
    }
}

/// Formats file sizes to human-readable format.
pub fn format_size(bytes: u64) -> String {
    format!("{:.3} MiB", bytes as f64 / 1024.0 / 1024.0)
}

/// Tries to find a pack that provides the snapshot using the repo index.
pub fn find_pack_with_snapshot(
    repo_index: &RepositoryIndex,
    snapshot: &str,
) -> Result<PackId, RepoError> {
    // Try to locate this snapshot.
    trace!("Looking for snapshot in top-level index...");
    let packs = repo_index.find_packs(snapshot);
    match packs.len() {
        0 => {
            error!(
                "Snapshot {} is not available in the known set of pack files! \
                Make sure that the correct pack file is available, refresh the index, and try again!",
                snapshot
            );
            Err(RepoError::PackError(PackError::SnapshotNotFound))
        }
        1 => Ok(packs[0].clone()),
        _ => {
            error!(
                "The following pack files provide snapshot {}:\n{:?}\nSpecify which one to use with -P <pack>!",
                snapshot, packs);
            Err(RepoError::AmbiguousSnapshotMatch)
        }
    }
}

/// Opens the repo from the current work directory and logs some standard
/// stats about the process.
pub fn open_repo_from_cwd() -> Result<Repository, RepoError> {
    // Open repo from cwd.
    let repo_path = std::env::current_dir()?;
    info!("Opening repository...");
    let (elapsed, open_result) = measure(|| Repository::open(&repo_path));
    info!("Opening repository took {:?}", elapsed);
    let repo = match open_result {
        Ok(repo) => repo,
        Err(RepoError::CorruptRepositoryIndex) => {
            error!("The repository index is corrupt or missing! Run update-index and try again.");
            return Err(RepoError::CorruptRepositoryIndex);
        }
        Err(RepoError::CorruptHead) => {
            let mut head_path = repo_path.join(&*Repository::data_dir());
            head_path.push(HEAD_FILE);
            error!(
                "The repository HEAD file ({:?}) is corrupt! Remove the file and try again.",
                head_path
            );
            return Err(RepoError::CorruptHead);
        }
        Err(e) => return Err(e),
    };
    Ok(repo)
}

pub fn create_percentage_print_reporter(message: &str, step: u32) -> ProgressReporter<'static> {
    assert!(step <= 100);

    let current = Arc::new(AtomicIsize::new(-100));
    let message = message.to_owned();
    ProgressReporter::new(move |checkpoint| {
        if let Some(remaining) = checkpoint.remaining {
            let percentage = (100 * checkpoint.done / (checkpoint.done + remaining)) as isize;
            if percentage - current.load(Ordering::Acquire) >= step as isize {
                current.store(percentage, Ordering::Release);
                println!("{}... {}%", message, percentage);
                std::io::stdout().flush().unwrap();
            }
        } else {
            print!("{}... {} of ?", message, checkpoint.done);
            std::io::stdout().flush().unwrap();
        }
    })
}
