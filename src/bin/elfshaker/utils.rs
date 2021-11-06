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

/// Do not print to `stderr` in case of a panic caused by a broken pipe.
///
/// A broken pipe panic happens when a println! (and friends) fails. It fails
/// when the pipe is closed by the reader (happens with elfshaker find | head
/// -n5). When that happens, the default Rust panic hook prints out the
/// following message: thread 'main' panicked at 'failed printing to stdout:
/// Broken pipe (os error 32)', src/libstd/io/stdio.rs:692:8 note: Run with
/// `RUST_BACKTRACE=1` for a backtrace.
///
/// What this function does is inject a hook earlier in the panic handling chain
/// to detect this specific panic occurring and simply not print anything.
pub(crate) fn silence_broken_pipe() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Some(message) = panic_info.payload().downcast_ref::<String>() {
            if message.contains("Broken pipe") {
                return;
            }
        }
        hook(panic_info);
    }));
}

/// Prints the values as a table. The first set of values
/// is used for the table header. The table header is always
/// printed to stderr.
pub(crate) fn print_table<R, C, I>(header: Option<C>, rows: R)
where
    R: Iterator<Item = C>,
    C: IntoIterator<Item = I>,
    I: std::fmt::Display,
{
    let format = |row: C| {
        row.into_iter()
            .map(|item| format!("{}", item))
            .collect::<Vec<_>>()
    };
    let header_strings = header.map(format);
    let strings: Vec<_> = rows.map(format).collect();

    if strings.is_empty() {
        if let Some(header) = header_strings {
            for item in header.iter() {
                eprint!("{} ", item);
            }
            eprintln!();
        }
        return;
    }

    let columns = strings[0].len();
    // Measure all columns
    let column_sizes = (0..columns)
        .map(|i| {
            let all_strings = header_strings.iter().chain(strings.iter());
            all_strings.map(|row| row[i].len()).max().unwrap()
        })
        .collect::<Vec<_>>();

    // Print header to stderr
    if let Some(header) = header_strings {
        assert_eq!(columns, header.len());
        for (i, item) in header.iter().enumerate() {
            let pad = column_sizes[i] - item.len();
            eprint!("{} {:pad$}", item, "", pad = pad);
        }
        eprintln!();
    }

    // Print rows to stdout
    for row in strings {
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
        0 => Err(RepoError::PackError(PackError::SnapshotNotFound(
            snapshot.to_owned(),
        ))),
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
                eprintln!("{}... {}%", message, percentage);
                std::io::stdout().flush().unwrap();
            }
        } else {
            eprint!("{}... {} of ?", message, checkpoint.done);
            std::io::stdout().flush().unwrap();
        }
    })
}
