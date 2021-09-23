//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Tools for logging.
use lazy_static::lazy_static;
use log::{set_logger, set_max_level, Level, LevelFilter, Log, Metadata, Record};
use std::sync::RwLock;
use std::time::{Duration, Instant};

lazy_static! {
    static ref INIT_LOG_LEVEL: RwLock<Level> = RwLock::new(Level::Error);
    static ref LOGGER: Box<Logger> = Box::new(Logger::static_init());
}

/// The [`Log`] implementation used by elfshaker.
pub struct Logger {
    level: Level,
    init_at: Instant,
}

impl Logger {
    /// Used to initialise the static LOGGER instance.
    fn static_init() -> Logger {
        Logger {
            level: *INIT_LOG_LEVEL.read().unwrap(),
            init_at: Instant::now(),
        }
    }
    /// Initialise with the specified log level.
    /// This function should only be called once, during program initialisation.
    pub fn init(level: Level) {
        {
            let mut w = INIT_LOG_LEVEL.write().unwrap();
            *w = level;
        }
        // Trigger the lazy initialisation
        let instance = &**LOGGER;
        set_logger(instance).unwrap();
        set_max_level(LevelFilter::Info);
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level_string = match record.level() {
                Level::Error => "ERROR",
                Level::Warn => "WARN",
                Level::Info => "INFO",
                Level::Debug => "DEBUG",
                Level::Trace => "TRACE",
            };
            let elapsed = Instant::now() - self.init_at;
            let thread = std::thread::current();
            let thread_string = match thread.name() {
                Some(name) => name.to_owned(),
                None => format!("{:?}", thread.id()),
            };
            eprintln!(
                "[{} ({}) {:?}]: {}",
                level_string,
                thread_string,
                elapsed,
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

/// Measures the time it took to run the specified block.
pub fn measure<F, T>(f: F) -> (Duration, T)
where
    F: FnOnce() -> T,
{
    let start = Instant::now();
    let result = f();
    let elapsed = Instant::now() - start;
    (elapsed, result)
}

/// Measures the time it took to run the specified block.
pub fn measure_ok<F, T, E>(f: F) -> Result<(Duration, T), E>
where
    F: FnOnce() -> Result<T, E>,
{
    let start = Instant::now();
    let result = f()?;
    let elapsed = Instant::now() - start;
    Ok((elapsed, result))
}

/// Measures the time it took to run the specified block.
pub fn measure_some<F, T>(f: F) -> Option<(Duration, T)>
where
    F: FnOnce() -> Option<T>,
{
    let start = Instant::now();
    let result = f()?;
    let elapsed = Instant::now() - start;
    Some((elapsed, result))
}
