//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

/// Batch file operation implementations
use crate::packidx::ObjectChecksum;
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use rayon::prelude::*;
use std::{cell::RefCell, fs::File, io, io::Read, path::Path};
use thread_local::ThreadLocal;

/// Computes the content checksums of the files at the listed paths.
pub fn compute_checksums<P>(paths: &[P]) -> io::Result<Vec<ObjectChecksum>>
where
    P: AsRef<Path> + Sync,
{
    let tls_buf = ThreadLocal::new();
    paths
        .par_iter()
        .map(|x| {
            let mut buf = tls_buf.get_or(|| RefCell::new(vec![])).borrow_mut();
            buf.clear();

            let mut file = File::open(&x)?;
            file.read_to_end(&mut buf)?;

            let checksum_buf = &mut [0u8; 20];
            let mut hasher = Sha1::new();
            hasher.input(&buf);
            hasher.result(checksum_buf);
            Ok(*checksum_buf)
        })
        .collect::<io::Result<Vec<_>>>()
}
