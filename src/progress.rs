//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

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
