//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{error::Error, ffi::OsString, path::Path, process::ExitStatus, str::FromStr};

use clap::{builder::OsStringValueParser, Arg, ArgMatches, Command};
use elfshaker::{progress::ProgressReporter, repo::SnapshotId};

use super::{extract, find, utils::open_repo_from_cwd};

pub(crate) const SUBCOMMAND: &str = "run";

pub(crate) fn run(matches: &ArgMatches) -> Result<ExitStatus, Box<dyn Error>> {
    let data_dir = Path::new(matches.get_one::<String>("data_dir").unwrap());
    let term = matches.get_one::<String>("snapshot").unwrap();
    let mut repo = open_repo_from_cwd(data_dir)?;
    repo.lock_exclusive()?;
    let snapshot = if term.contains(':') {
        let snapshot = SnapshotId::from_str(term)?;
        if !repo
            .load_index_snapshots(snapshot.pack())?
            .iter()
            .any(|s| s == snapshot.tag())
        {
            return Err(format!("No snapshot matches {term:?}").into());
        }
        snapshot
    } else {
        let mut found = find::search(&repo, term)?;
        match found.len() {
            0 => {
                return Err(format!(
                    "No snapshot matches {term:?}; run elfshaker update to refresh the catalog"
                )
                .into())
            }
            1 => found.remove(0),
            _ => {
                return Err(format!(
                    "Multiple snapshots match {term:?}:\n{}",
                    found
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
                .into())
            }
        }
    };
    repo.set_progress_reporter(|message| {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let message = message.to_owned();
        let last_step = AtomicUsize::new(0);
        eprintln!("{message}...");
        ProgressReporter::new(move |progress| {
            if let Some(remaining) = progress.remaining {
                let percent =
                    progress.done.saturating_mul(100) / (progress.done + remaining).max(1);
                let step = percent / 5;
                if step > last_step.fetch_max(step, Ordering::Relaxed) {
                    eprintln!("{message}: {percent}%");
                }
            }
        })
    });
    extract::extract(&mut repo, matches, snapshot)?;
    let mut args = matches.get_many::<OsString>("command").unwrap();
    let mut command = std::process::Command::new(args.next().unwrap());
    command.args(args).current_dir(repo.path());
    // Keep repo (and its exclusive lock) alive until the command finishes.
    #[cfg(unix)]
    let status = signals::wait(&mut command)?;
    #[cfg(not(unix))]
    let status = command.status()?;
    drop(repo);
    Ok(status)
}

pub(crate) fn get_app() -> Command {
    extract::get_app()
        .name(SUBCOMMAND)
        .mut_arg("snapshot", |arg| arg.help("A unique substring of a snapshot tag, or PACK:SNAPSHOT."))
        .about("Extracts a unique snapshot match and runs a command in the working directory.")
        .after_help("Searches the cached catalog using find's substring matching, or accepts PACK:SNAPSHOT.\nRun update explicitly to refresh. Setup failures return 125; command status is preserved.\nThe repository stays locked until COMMAND exits. Nested elfshaker commands on the same store can block.")
        .arg(Arg::new("command").index(2).required(true).num_args(1..).last(true).value_parser(OsStringValueParser::new()).help("Command and arguments, passed directly after --."))
}

pub(crate) fn exit_with_status(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            // Preserve signal termination for shells and callers inspecting wait status.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                let mut signals = std::mem::zeroed();
                libc::sigemptyset(&mut signals);
                libc::sigaddset(&mut signals, signal);
                libc::sigprocmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut());
                libc::raise(signal);
            }
            std::process::exit(128 + signal);
        }
    }
    std::process::exit(125);
}

// Keep the lock while a terminated child shuts down, including when a signal
// targets only elfshaker rather than the whole foreground process group.
#[cfg(unix)]
mod signals {
    use std::{
        io,
        process::{Command, ExitStatus},
        sync::atomic::{AtomicI32, Ordering},
    };
    static CHILD: AtomicI32 = AtomicI32::new(0);
    static PENDING: AtomicI32 = AtomicI32::new(0);

    extern "C" fn forward(signal: libc::c_int) {
        PENDING.store(signal, Ordering::SeqCst);
        let pid = CHILD.load(Ordering::SeqCst);
        if pid > 0 {
            // kill is async-signal-safe. The child also receives foreground
            // process-group signals directly; forwarding is harmless there.
            unsafe {
                libc::kill(pid, signal);
            }
        }
    }

    struct Handlers(Vec<(libc::c_int, libc::sigaction)>);
    impl Drop for Handlers {
        fn drop(&mut self) {
            CHILD.store(0, Ordering::SeqCst);
            for (signal, old) in &self.0 {
                unsafe {
                    libc::sigaction(*signal, old, std::ptr::null_mut());
                }
            }
        }
    }

    pub(super) fn wait(command: &mut Command) -> io::Result<ExitStatus> {
        let mut handlers = Handlers(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = forward as *const () as usize;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                let mut old = std::mem::zeroed();
                if libc::sigaction(signal, &action, &mut old) != 0 {
                    return Err(io::Error::last_os_error());
                }
                handlers.0.push((signal, old));
            }
        }
        // Caught signal dispositions revert to defaults at exec in the child.
        let mut child = command.spawn()?;
        CHILD.store(child.id() as i32, Ordering::SeqCst);
        let pending = PENDING.swap(0, Ordering::SeqCst);
        if pending != 0 {
            unsafe {
                libc::kill(child.id() as i32, pending);
            }
        }
        child.wait()
    }
}
