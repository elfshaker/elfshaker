//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

/// Use [`Repository::data_dir`] instead of REPO_DIR
/// This will make is easier to make this value user-configurable
/// in the future.
pub const REPO_DIR: &str = "elfshaker_data";
/// The top-level index file path.
pub const INDEX_FILE: &str = "index";
/// A pointer to the extracted snapshot.
pub const HEAD_FILE: &str = "HEAD";
/// A directory containing a list of .pack and .pack.idx files
pub const PACKS_DIR: &str = "packs";
/// The file extension of a pack file.
pub const PACK_EXTENSION: &str = "pack";
/// The file extension of a pack index file.
pub const PACK_INDEX_EXTENSION: &str = "pack.idx";
/// A directory containing the object files from all unpacked snapshots
pub const UNPACKED_DIR: &str = "unpacked";
/// The pack index for the unpacked snapshots
pub const UNPACKED_INDEX_FILE: &str = "unpacked.idx";
/// A directory used during store/extract operations. Can be deleted safely
/// at anytime when there is no elfshaker operation executing.
pub const TEMP_DIR: &str = "trash";
/// Reserved pack name used to indicate the unpacked set of snapshots.
pub const UNPACKED_ID: &str = "unpacked";
