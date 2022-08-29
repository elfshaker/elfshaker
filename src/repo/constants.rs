//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

/// The default repository directory.
pub const REPO_DIR: &str = "elfshaker_data";
/// The top-level index file path.
pub const INDEX_FILE: &str = "index";
/// A pointer to the extracted snapshot.
pub const HEAD_FILE: &str = "HEAD";
/// A directory containing a list of .pack and .pack.idx files
pub const PACKS_DIR: &str = "packs";
/// A directory containing the .esi files
pub const REMOTES_DIR: &str = "remotes";
/// The file extension of a pack file.
pub const PACK_EXTENSION: &str = "pack";
/// The file extension of a pack index file.
pub const PACK_INDEX_EXTENSION: &str = "pack.idx";
pub const DOT_PACK_INDEX_EXTENSION: &str = ".pack.idx";
/// The file extension of remote elfshaker indexes.
pub const REMOTE_INDEX_EXTENSION: &str = "esi";
/// A directory containing the object files from all loose snapshots
pub const LOOSE_DIR: &str = "loose";
/// A directory used during store/extract operations. Can be deleted safely
/// at anytime when there is no elfshaker operation executing.
pub const TEMP_DIR: &str = "trash";
// 2^30 = 1024MiB window log
pub const DEFAULT_WINDOW_LOG_MAX: u32 = 30;
/// Valid pack headers have this value set in the [`PackHeader::magic`] field.
pub const PACK_HEADER_MAGIC: u64 = 848629801635942891;
