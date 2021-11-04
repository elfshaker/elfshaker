//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Batch file operation implementations.
use crate::packidx::ObjectChecksum;
use crate::progress::ProgressReporter;
use crate::repo::run_in_parallel;
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use std::{fs::File, io, io::Read, path::Path};
use zstd::stream::raw::CParameter;
use zstd::Encoder;

/// Computes the content checksums of the files at the listed paths.
pub fn compute_checksums<P>(paths: &[P]) -> io::Result<Vec<ObjectChecksum>>
where
    P: AsRef<Path> + Sync,
{
    run_in_parallel(num_cpus::get(), paths.iter(), |x| {
        let mut buf = vec![];
        let mut file = File::open(&x)?;
        file.read_to_end(&mut buf)?;

        let checksum_buf = &mut [0u8; 20];
        let mut hasher = Sha1::new();
        hasher.input(&buf);
        hasher.result(checksum_buf);
        Ok(*checksum_buf)
    })
    .into_iter()
    .collect::<io::Result<Vec<_>>>()
}

/// Options for the batch compression functions.
pub struct CompressionOptions {
    pub level: i32,
    pub window_log: u32,
    pub num_workers: u32,
}

/// Compresses the specified set of files using ZStandard compression and the specified options.
/// Returns the number of bytes processed (the size of the decompressed stream).
///
/// # Arguments
/// * `pack_file` - the output writer
/// * `object_paths` - the list of file paths to process
/// * `opts` - the compression options
///
///
pub fn compress_files<W, P>(
    pack_file: W,
    object_paths: &[P],
    opts: &CompressionOptions,
    reporter: &ProgressReporter,
) -> io::Result<u64>
where
    W: io::Write,
    P: AsRef<Path>,
{
    assert!(opts.num_workers > 0);
    // Initialize encoder.
    let mut encoder = Encoder::new(pack_file, opts.level)?;
    // Zstandard takes NbWorkers to mean extra compression threads (0 means on same thread as IO).
    encoder.set_parameter(CParameter::NbWorkers(opts.num_workers - 1))?;
    encoder.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
    encoder.set_parameter(CParameter::WindowLog(opts.window_log))?;

    let mut processed_bytes = 0;

    for (i, obj) in object_paths.iter().enumerate() {
        let mut file = File::open(&obj)?;
        let bytes = io::copy(&mut file, &mut encoder)?;
        processed_bytes += bytes;
        reporter.checkpoint(i, Some(object_paths.len() - i));
    }

    reporter.checkpoint(object_paths.len(), Some(0));
    // Important to call .finish()
    encoder.finish()?;
    Ok(processed_bytes)
}
