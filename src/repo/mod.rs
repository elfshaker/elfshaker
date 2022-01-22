//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Contains core types for interfacing with elfshaker repositories.
mod algo;
mod constants;
mod error;
#[doc(hidden)]
pub mod fs;
mod pack;
mod repository;

#[doc(hidden)]
pub use algo::{partition_by_u64, run_in_parallel};
pub use constants::{
    HEAD_FILE, INDEX_FILE, LOOSE_DIR, PACKS_DIR, PACK_EXTENSION, PACK_INDEX_EXTENSION, REPO_DIR,
};
pub use error::Error;
#[doc(hidden)]
pub use pack::write_skippable_frame;
pub use pack::{Pack, PackFrame, PackHeader, PackId, SnapshotId};
pub use repository::{ExtractOptions, PackOptions, Repository};
