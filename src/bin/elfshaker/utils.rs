//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use elfshaker::log::measure;
use elfshaker::progress::ProgressReporter;
use elfshaker::repo::{Error as RepoError, Repository};

use lazy_static::lazy_static;
use log::info;
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
    format!("{:.3}MiB", bytes as f64 / 1024.0 / 1024.0)
}

/// Opens the repo from the current work directory and logs some standard
/// stats about the process.
pub fn open_repo_from_cwd() -> Result<Repository, RepoError> {
    // Open repo from cwd.
    let repo_path = std::env::current_dir()?;
    info!("Opening repository...");
    let (elapsed, open_result) = measure(|| Repository::open(&repo_path));
    info!("Opening repository took {:?}", elapsed);
    open_result
}

/// The default progress bar width is the same as cargo's.
const DEFAULT_PROGRESS_BAR_WIDTH: i32 = 25;

/// Builds a progress bar of the form `[===>  ]`.
///
/// # Arguments
///
/// * `percent_complete` - Integer percentage of completeness
/// * `width` - Width of the progress bar excluding the open/close bracket.
fn create_progress_bar(percent_complete: i32, width: i32) -> String {
    assert!((0..=100).contains(&percent_complete));
    let mut s = "[".to_owned();
    let full_blocks = ((percent_complete as f32 / 100f32) * width as f32).round() as i32;
    for _ in 0..full_blocks {
        s += "=";
    }
    s += ">";
    for _ in 0..(width - full_blocks) {
        s += " ";
    }
    s += "]";
    s
}

/// Checks if `stdout` outputs to a terminal with support for terminal escape
/// codes.
#[cfg(unix)]
fn is_terminal_escape_supported<Fd>(fd: Fd) -> bool
where
    Fd: std::os::unix::io::AsRawFd,
{
    extern crate libc;
    // We assume that as long as we are outputting to a terminal, that terminal
    // supports escape codes.
    unsafe { libc::isatty(fd.as_raw_fd()) != 0 }
}

#[cfg(not(unix))]
fn is_terminal_escape_supported<Fd>(_: Fd) -> bool {
    // Assume other platforms do not support ANSI escape codes
    false
}

lazy_static! {
    static ref STDOUT_TERMINAL_ESCAPE_SUPPORTED: bool =
        is_terminal_escape_supported(std::io::stdout());
}

/// Clears the current line
const ESC_CLEAR_LINE: &str = "\x1b[2K";
const ESC_BOLD_ON: &str = "\x1b[1m";
const ESC_GREEN: &str = "\x1b[32;1m";
const ESC_CYAN: &str = "\x1b[36;1m";
/// Resets all text formatting
const ESC_RESET: &str = "\x1b[32;0m";

pub fn create_percentage_print_reporter(message: &str, step: u32) -> ProgressReporter<'static> {
    assert!(step <= 100);

    let current = Arc::new(AtomicIsize::new(-100));
    let message = message.to_owned();
    ProgressReporter::new(move |checkpoint| {
        if let Some(remaining) = checkpoint.remaining {
            let is_terminal_escape_supported = *STDOUT_TERMINAL_ESCAPE_SUPPORTED;
            // Helper which returns the input if terminal codes are supported
            let if_escape = |s| {
                if is_terminal_escape_supported {
                    s
                } else {
                    ""
                }
            };

            let total = checkpoint.done + remaining;
            let percentage = (100 * checkpoint.done / std::cmp::max(1, total)) as isize;

            // Stepping helps to avoid too many lines being printed,
            // but when using terminal escape codes, we refresh the same
            // line over an over, so it is not needed.
            let needs_update = percentage - current.load(Ordering::Acquire) >= step as isize;
            if needs_update || is_terminal_escape_supported {
                current.store(percentage, Ordering::Release);
                // Clear line and reset cursor
                print!("{}\r", if_escape(ESC_CLEAR_LINE));

                if percentage == 100 {
                    // Bold green success text
                    println!(
                        "{}{}{}{} [{}/{}]",
                        if_escape(ESC_BOLD_ON),
                        if_escape(ESC_GREEN),
                        message,
                        if_escape(ESC_RESET),
                        total,
                        total
                    );
                } else {
                    // Bold blue status text with percentage
                    print!(
                        "{}{}{}{} {} {}% [{}/{}]",
                        if_escape(ESC_BOLD_ON),
                        if_escape(ESC_CYAN),
                        message,
                        if_escape(ESC_RESET),
                        create_progress_bar(percentage as i32, DEFAULT_PROGRESS_BAR_WIDTH),
                        percentage,
                        checkpoint.done,
                        total,
                    );
                    // Optional "detail" message
                    if let Some(detail) = &checkpoint.detail {
                        print!(": {}", detail);
                    }
                }
                std::io::stdout().flush().unwrap();
            }
        } else {
            println!("{}... [{}/?]", message, checkpoint.done);
        }
    })
}
