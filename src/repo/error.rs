//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{fmt::Display, io};

use crate::packidx::PackError;
use crate::repo::pack::IdError;

/// The type of error used by repository operations.
#[derive(Debug)]
pub enum Error {
    IOError(io::Error),
    PackError(PackError),
    IdError(IdError),
    /// Bad elfshaker_data/inex
    CorruptRepositoryIndex,
    /// Bad elfshaker_data/HEAD (missing HEAD is okay and means that nothing has been extracted so far)
    CorruptHead,
    /// The references snapshot/pack is missing.
    BrokenHeadRef,
    /// The .pack.idx is corrupt
    CorruptPackIndex,
    /// The .pack file is corrupt.
    CorruptPack,
    /// Multiple or none snapshots match the specified description
    AmbiguousSnapshotMatch,
    /// The working directory contains unexpected files
    DirtyWorkDir,
    /// The .pack file is not available in packs/
    PackNotFound,
    /// The directory is not a repository
    RepositoryNotFound,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::IOError(ioerr) => ioerr.fmt(f),
            Self::PackError(packerr) => packerr.fmt(f),
            Self::IdError(iderr) => iderr.fmt(f),
            Self::CorruptHead => write!(f, "HEAD is corrupt!"),
            Self::BrokenHeadRef => write!(f, "The pack or snapshot referenced by HEAD is missing!"),
            Self::CorruptRepositoryIndex => {
                write!(f, "The repository index is corrupt or missing!")
            }
            Self::CorruptPack => {
                write!(f, "The pack file is corrupt!")
            }
            Self::CorruptPackIndex => write!(f, "The pack index is corrupt!"),
            Self::AmbiguousSnapshotMatch => {
                write!(f, "Found multiple snapshots matching the description!")
            }
            Self::DirtyWorkDir => write!(
                f,
                "Some files in the repository have been removed or modified unexpectedly!"
            ),
            Self::PackNotFound => write!(
                f,
                "The specified pack file could not be found in the repository index!"
            ),
            Self::RepositoryNotFound => write!(f, "The directory is not an elfshaker repository!"),
        }
    }
}

impl std::error::Error for Error {}

impl std::convert::From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::IOError(err)
    }
}

impl std::convert::From<PackError> for Error {
    fn from(err: PackError) -> Self {
        Self::PackError(err)
    }
}

impl std::convert::From<IdError> for Error {
    fn from(err: IdError) -> Self {
        Self::IdError(err)
    }
}
