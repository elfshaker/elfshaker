//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

mod algo;
mod constants;
mod error;
mod fs;
mod pack;
mod repository;

#[doc(hidden)]
pub use algo::partition_by_u64;
pub use constants::{
    HEAD_FILE, INDEX_FILE, PACKS_DIR, PACK_EXTENSION, PACK_INDEX_EXTENSION, REPO_DIR, UNPACKED_DIR,
    UNPACKED_INDEX_FILE,
};
pub use error::Error;
pub use pack::{write_skippable_frame, Pack, PackFrame, PackHeader, PackId, SnapshotId};
pub use repository::{ExtractOptions, PackOptions, Repository, RepositoryIndex};
