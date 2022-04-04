//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{io, io::Write};

pub struct Checkpoint {
    pub done: usize,
    pub remaining: Option<usize>,
}

pub struct ProgressReporter<'a> {
    callback: Option<Box<dyn Fn(&Checkpoint) + Sync + 'a>>,
}

unsafe impl<'a> Send for ProgressReporter<'a> {}

impl<'a> ProgressReporter<'a> {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&Checkpoint) + Sync + 'a,
    {
        Self {
            callback: Some(Box::new(f)),
        }
    }

    pub fn dummy() -> Self {
        Self { callback: None }
    }

    pub fn checkpoint(&self, done: usize, remaining: Option<usize>) {
        if let Some(callback) = &self.callback {
            callback(&Checkpoint { done, remaining });
        }
    }
}

pub struct ProgressWriter<'a, T: Write> {
    reporter: &'a ProgressReporter<'a>,
    writer: T,
    remaining: Option<usize>,
    written: usize,
}

impl<'a, T: Write> ProgressWriter<'a, T> {
    pub fn new(writer: T, reporter: &'a ProgressReporter<'a>) -> Self {
        Self {
            reporter,
            writer,
            remaining: None,
            written: 0,
        }
    }

    pub fn with_known_size(writer: T, reporter: &'a ProgressReporter<'a>, size: usize) -> Self {
        Self {
            reporter,
            writer,
            remaining: Some(size),
            written: 0,
        }
    }
}

impl<'a, T: Write> Write for ProgressWriter<'a, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes_written = self.writer.write(buf)?;
        self.written += bytes_written;
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining -= bytes_written
        }
        self.reporter.checkpoint(self.written, self.remaining);
        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
