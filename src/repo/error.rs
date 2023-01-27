//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::ffi::OsString;
use std::{fmt::Display, io};

use crate::packidx::PackError;
use crate::repo::pack::IdError;
use crate::repo::remote::RemoteIndexFormatError;

use super::PackId;

/// The type of error used by repository operations.
#[derive(Debug)]
pub enum Error {
    IOError(io::Error),
    WalkDirError(walkdir::Error),
    Utf8Error(OsString),
    PackError(PackError),
    IdError(IdError),
    /// Bad elfshaker_data/HEAD (missing HEAD is okay and means that nothing has been extracted so far)
    CorruptHead,
    /// The references snapshot/pack is missing.
    BrokenHeadRef(Box<Error>),
    /// The .pack.idx is corrupt
    CorruptPackIndex,
    /// The .pack file is corrupt.
    CorruptPack,
    /// Multiple or none snapshots match the specified description
    AmbiguousSnapshotMatch(String, Vec<PackId>),
    /// The working directory contains unexpected files
    DirtyWorkDir,
    /// The .pack file is not available in packs/
    PackNotFound(String),
    /// The directory is not a repository
    RepositoryNotFound,
    // The loose object is corrupt or is in the wrong path
    BadLooseObject(String),
    /// The .esi file is corrupted.
    BadRemoteIndexFormat(RemoteIndexFormatError),
    /// A type-erased error resulting from an HTTP operation.
    HttpError(Box<dyn std::error::Error + Send + Sync>),
}

impl From<walkdir::Error> for Error {
    fn from(err: walkdir::Error) -> Self {
        Self::WalkDirError(err)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::IOError(ioerr) => {
                // Since the error message can be vague, we also print
                // the internal error kind.
                write!(f, "{} ({:?})", ioerr, ioerr.kind())
            }
            Self::PackError(packerr) => packerr.fmt(f),
            Self::IdError(iderr) => iderr.fmt(f),
            Self::WalkDirError(wderr) => wderr.fmt(f),
            Self::Utf8Error(s) => write!(f, "Unable to interpret path as utf8: {s:?}"),
            Self::CorruptHead => write!(f, "HEAD is corrupt!"),
            Self::BrokenHeadRef(e) => write!(f, "Broken HEAD: {e}"),
            Self::CorruptPack => {
                write!(f, "The pack file is corrupt!")
            }
            Self::CorruptPackIndex => write!(f, "The pack index is corrupt!"),
            Self::AmbiguousSnapshotMatch(snapshot, packs) => {
                write!(
                    f,
                    "The requested snapshot {snapshot} lives in multiple packs: {packs:?}"
                )
            }
            Self::DirtyWorkDir => write!(
                f,
                "Some files in the repository have been removed or modified unexpectedly! \
                 You can use --force to skip this check, but this might result in DATA LOSS!"
            ),
            Self::PackNotFound(p) => write!(
                f,
                "The specified pack file '{p}' could not be found in the repository index!",
            ),
            Self::RepositoryNotFound => write!(f, "The directory is not an elfshaker repository!"),
            Self::BadLooseObject(s) => write!(f, "Bad loose object: {s}"),
            Self::HttpError(e) => e.fmt(f),
            Self::BadRemoteIndexFormat(e) => e.fmt(f),
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

impl std::convert::From<RemoteIndexFormatError> for Error {
    fn from(err: RemoteIndexFormatError) -> Self {
        Self::BadRemoteIndexFormat(err)
    }
}
