//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use elfshaker::packidx::PackIndex;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = elfshaker::repo::fs::create_temp_path(&std::env::temp_dir());
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
    fn write(&self, path: &str, content: &[u8]) {
        fs::write(self.path(path), content).unwrap();
    }
    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_elfshaker"))
            .env_remove("ELFSHAKER_DATA")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn fail(&self, args: &[&str], message: &str) {
        let output = self.command(args);
        assert!(
            !output.status.success(),
            "{:?} unexpectedly succeeded",
            args
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn index(&self) -> PackIndex {
        PackIndex::load(self.path("elfshaker_data/packs/base.pack.idx")).unwrap()
    }
    fn pack(&self) -> Vec<u8> {
        fs::read(self.path("elfshaker_data/packs/base.pack")).unwrap()
    }
    fn index_bytes(&self) -> Vec<u8> {
        fs::read(self.path("elfshaker_data/packs/base.pack.idx")).unwrap()
    }
    fn extract(&self, tag: &str) {
        self.run(&["extract", "--force", "--verify", &format!("base:{tag}")]);
    }
    fn append(&self, tag: &str) {
        self.run(&[
            "pack",
            "--append",
            "-T",
            "2",
            "base",
            &format!("loose/{tag}"),
        ]);
    }
    fn setup(&self) {
        self.write("same", b"unchanged content");
        self.write("changed", b"old");
        self.write("empty", b"");
        self.run(&["store", "s1"]);
        self.run(&["pack", "-T", "2", "--frames", "3", "base", "loose/s1"]);
    }
    fn remove_old_objects(&self) {
        fs::remove_dir_all(self.path("elfshaker_data/loose")).unwrap();
    }
    fn assert_unchanged(&self, pack: &[u8], index: &[u8]) {
        assert_eq!(pack, self.pack());
        assert_eq!(index, self.index_bytes());
        assert_eq!(
            fs::read_to_string(self.path("elfshaker_data/HEAD"))
                .unwrap()
                .trim(),
            "loose/s2:s2"
        );
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn header_len(bytes: &[u8]) -> usize {
    u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize + 8
}
fn frames(bytes: &[u8]) -> Vec<&[u8]> {
    let (_, directory): (u64, Vec<elfshaker::repo::PackFrame>) =
        rmp_serde::from_slice(&bytes[8..header_len(bytes)]).unwrap();
    let mut offset = header_len(bytes);
    directory
        .into_iter()
        .map(|frame| {
            let start = offset;
            offset += frame.frame_size as usize;
            &bytes[start..offset]
        })
        .collect()
}

fn decode_frame(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut decoder = zstd::Decoder::new(bytes).unwrap();
    decoder.window_log_max(30).unwrap();
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw).unwrap();
    raw
}

fn object_path(root: &Path, checksum: &[u8; 20]) -> PathBuf {
    let checksum = hex::encode(checksum);
    root.join("elfshaker_data/loose")
        .join(&checksum[..2])
        .join(&checksum[2..4])
        .join(&checksum[4..])
}

#[test]
fn append_merges_related_frames_and_preserves_snapshots_and_deduplicates() {
    let repo = Repo::new();
    repo.setup();
    let old_index = repo.index();
    let old_pack = repo.pack();
    repo.remove_old_objects();
    repo.write("changed", b"new content of a different length");
    repo.write("copy", b"unchanged content");
    fs::remove_file(repo.path("empty")).unwrap();
    repo.run(&["store", "s2"]);
    // Store recreated old objects; remove them again. Append must only open new objects.
    for checksum in old_index.object_checksums() {
        let _ = fs::remove_file(object_path(&repo.0, checksum));
    }
    repo.append("s2");
    assert_eq!(
        fs::read_to_string(repo.path("elfshaker_data/HEAD"))
            .unwrap()
            .trim(),
        "base:s2"
    );
    let appended = repo.index();
    assert_eq!(appended.snapshot_tags(), ["s1", "s2"]);
    assert_eq!(
        appended.object_checksums().len(),
        old_index.object_checksums().len() + 1
    );
    for checksum in old_index.object_checksums() {
        assert_eq!(
            old_index.object_metadata(checksum).size,
            appended.object_metadata(checksum).size
        );
    }
    assert_eq!(
        old_index.compute_snapshot_checksum("s1"),
        appended.compute_snapshot_checksum("s1")
    );
    let new_pack = repo.pack();
    let old_frames = frames(&old_pack);
    let new_frames = frames(&new_pack);
    assert_eq!(old_frames.len(), new_frames.len());
    assert_ne!(old_frames[0], new_frames[0]); // same-path old/new content merged
    assert_eq!(old_frames.last(), new_frames.last()); // unaffected compressed bytes copied
    assert!(old_index
        .object_checksums()
        .any(|c| old_index.object_metadata(c).offset != appended.object_metadata(c).offset));
    repo.extract("s1");
    assert_eq!(fs::read(repo.path("changed")).unwrap(), b"old");
    assert!(repo.path("empty").is_file());
    assert!(!repo.path("copy").exists());
    repo.extract("s2");
    assert_eq!(
        fs::read(repo.path("changed")).unwrap(),
        b"new content of a different length"
    );
    assert_eq!(fs::read(repo.path("copy")).unwrap(), b"unchanged content");
    assert!(!repo.path("empty").exists());

    // Reopen and append a snapshot containing only old objects, with deletion,
    // rename, and a mode-only change. No new frame should be written.
    fs::rename(repo.path("copy"), repo.path("renamed")).unwrap();
    fs::remove_file(repo.path("changed")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repo.path("same"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    repo.run(&["store", "s3"]);
    repo.remove_old_objects();
    repo.append("s3");
    assert_eq!(new_pack, repo.pack());
    repo.extract("s1");
    repo.extract("s3");
    assert!(!repo.path("copy").exists());
    assert!(!repo.path("changed").exists());
    assert_eq!(
        fs::read(repo.path("renamed")).unwrap(),
        b"unchanged content"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(repo.path("same"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}

#[test]
fn append_failures_leave_pack_index_and_head_intact() {
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", b"new");
    repo.run(&["store", "s2"]);
    let before = repo.pack();
    let index = repo.index_bytes();
    repo.fail(&["pack", "--append", "base", "loose/s1"], "already present");
    repo.assert_unchanged(&before, &index);
    repo.fail(
        &["pack", "--append", "base", "loose/s2", "loose/s2"],
        "already present",
    );
    repo.assert_unchanged(&before, &index);
    repo.fail(&["pack", "--append", "base", "base"], "only loose");
    repo.fail(&["pack", "--append", "absent", "loose/s2"], "couldn't open");
    repo.fail(
        &["pack", "--append", "loose/s1", "loose/s2"],
        "loose snapshot",
    );
    repo.assert_unchanged(&before, &index);
    let new_index = PackIndex::load(repo.path("elfshaker_data/packs/loose/s2.pack.idx")).unwrap();
    let old_checksums: std::collections::HashSet<_> =
        repo.index().object_checksums().copied().collect();
    let new_checksum = new_index
        .object_checksums()
        .find(|s| !old_checksums.contains(*s))
        .unwrap();
    let object = object_path(&repo.0, new_checksum);
    fs::write(&object, b"bad").unwrap(); // same length, different hash
    repo.fail(
        &["pack", "--append", "base", "loose/s2"],
        "corrupt loose object",
    );
    repo.assert_unchanged(&before, &index);
    fs::remove_file(object).unwrap();
    repo.fail(&["pack", "--append", "base", "loose/s2"], "NotFound");
    repo.assert_unchanged(&before, &index);
}

#[test]
fn opening_recovers_each_interrupted_publication_stage() {
    let repo = Repo::new();
    repo.setup();
    let old_pack = repo.pack();
    let old_index = repo.index_bytes();
    repo.write("changed", b"new content that shifts the other objects");
    repo.run(&["store", "s2"]);
    repo.append("s2");
    let new_pack = repo.pack();
    let new_index = repo.index_bytes();
    assert_ne!(old_pack, new_pack);
    for renamed in 0..=2 {
        repo.write("elfshaker_data/packs/base.pack", &old_pack);
        repo.write("elfshaker_data/packs/base.pack.idx", &old_index);
        let journal = repo.path("elfshaker_data/.append-transaction");
        fs::create_dir(&journal).unwrap();
        fs::write(journal.join("target"), b"base").unwrap();
        fs::write(journal.join("new.pack"), &new_pack).unwrap();
        fs::write(journal.join("new.pack.idx"), &new_index).unwrap();
        if renamed >= 1 {
            fs::rename(
                journal.join("new.pack"),
                repo.path("elfshaker_data/packs/base.pack"),
            )
            .unwrap();
        }
        if renamed >= 2 {
            fs::rename(
                journal.join("new.pack.idx"),
                repo.path("elfshaker_data/packs/base.pack.idx"),
            )
            .unwrap();
        }
        repo.extract("s1");
        assert!(!journal.exists());
        assert_eq!(repo.pack(), new_pack);
        assert_eq!(repo.index_bytes(), new_index);
        assert_eq!(fs::read(repo.path("changed")).unwrap(), b"old");
        repo.extract("s2");
        assert_eq!(
            fs::read(repo.path("changed")).unwrap(),
            b"new content that shifts the other objects"
        );
    }
}

#[test]
fn append_handles_empty_snapshots_and_initially_empty_pack() {
    let repo = Repo::new();
    repo.run(&["store", "empty"]);
    repo.run(&["pack", "base", "loose/empty"]);
    repo.write("file", b"new");
    repo.run(&["store", "nonempty"]);
    repo.append("nonempty");
    fs::remove_file(repo.path("file")).unwrap();
    repo.run(&["store", "empty-again"]);
    repo.append("empty-again");
    repo.extract("nonempty");
    assert_eq!(fs::read(repo.path("file")).unwrap(), b"new");
    repo.extract("empty-again");
    assert!(!repo.path("file").exists());
    repo.extract("empty");
    assert!(!repo.path("file").exists());
}

#[test]
fn append_rejects_truncated_or_headerless_packs_without_mutation() {
    let repo = Repo::new();
    repo.setup();
    repo.run(&["store", "s2"]);
    let original = repo.pack();
    let index = repo.index_bytes();
    for damaged in [
        &original[..original.len() - 1],
        &original[header_len(&original)..],
    ] {
        repo.write("elfshaker_data/packs/base.pack", damaged);
        let result = repo.command(&["pack", "--append", "base", "loose/s2"]);
        assert!(!result.status.success());
        repo.assert_unchanged(damaged, &index);
    }
}

#[test]
fn corrupt_affected_frame_and_cross_frame_object_are_rejected() {
    use elfshaker::packidx::{FileEntry, ObjectMetadata};
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", b"new version");
    repo.run(&["store", "s2"]);
    let original = repo.pack();
    let original_index = repo.index_bytes();
    let mut damaged = original.clone();
    damaged[header_len(&original)] ^= 0xff; // Corrupt the affected frame's magic.
    repo.write("elfshaker_data/packs/base.pack", &damaged);
    assert!(!repo
        .command(&["pack", "--append", "base", "loose/s2"])
        .status
        .success());
    repo.assert_unchanged(&damaged, &original_index);

    repo.write("elfshaker_data/packs/base.pack", &original);
    let source = repo.index();
    let mut entries = source
        .entries_from_handles(source.resolve_snapshot("s1").unwrap().iter())
        .unwrap();
    let changed = entries.iter_mut().find(|e| e.path == "changed").unwrap();
    *changed = FileEntry::new(
        changed.path.clone(),
        changed.checksum,
        ObjectMetadata {
            offset: changed.object_metadata.offset,
            size: changed.object_metadata.size + 1,
        },
        changed.file_metadata,
    );
    let mut bad_index = PackIndex::new();
    bad_index.push_snapshot("s1".into(), entries).unwrap();
    fs::remove_file(repo.path("elfshaker_data/packs/base.pack.idx")).unwrap();
    bad_index
        .save(repo.path("elfshaker_data/packs/base.pack.idx"))
        .unwrap();
    let bad_bytes = repo.index_bytes();
    repo.fail(
        &["pack", "--append", "base", "loose/s2"],
        "pack index is corrupt",
    );
    repo.assert_unchanged(&original, &bad_bytes);
}

#[test]
fn compaction_from_packed_index_after_explode_preserves_snapshots() {
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", b"appended");
    repo.run(&["store", "s2"]);
    repo.append("s2");
    repo.remove_old_objects();
    repo.run(&["explode", "base"]);
    repo.run(&["pack", "compact", "base"]);
    let compact = PackIndex::load(repo.path("elfshaker_data/packs/compact.pack.idx")).unwrap();
    for tag in ["s1", "s2"] {
        assert_eq!(
            repo.index().compute_snapshot_checksum(tag),
            compact.compute_snapshot_checksum(tag)
        );
        repo.run(&[
            "extract",
            "--reset",
            "--force",
            "--verify",
            &format!("compact:{tag}"),
        ]);
    }
}

#[test]
fn extract_thousands_of_frames_with_bounded_file_handles() {
    use elfshaker::{
        packidx::{FileEntry, ObjectMetadata},
        repo::{write_skippable_frame, PackFrame, PackHeader},
    };
    use sha1::{Digest, Sha1};
    use std::io::Write;
    let repo = Repo::new();
    fs::create_dir_all(repo.path("elfshaker_data/packs")).unwrap();
    let mut frames = Vec::new();
    let mut payload = Vec::new();
    for position in 0..2048u32 {
        let data = position.to_le_bytes();
        let compressed = zstd::encode_all(&data[..], 1).unwrap();
        frames.push(PackFrame {
            frame_size: compressed.len() as u64,
            decompressed_size: 4,
        });
        payload.extend(compressed);
    }
    let mut pack = fs::File::create(repo.path("elfshaker_data/packs/base.pack")).unwrap();
    let header = rmp_serde::to_vec(&PackHeader::new(frames)).unwrap();
    write_skippable_frame(&mut pack, &header).unwrap();
    pack.write_all(&payload).unwrap();
    drop(pack);
    let mut index = PackIndex::new();
    index
        .push_snapshot(
            "s1".into(),
            [0u32, 2047].map(|n| {
                FileEntry::new(
                    format!("object-{n}").into(),
                    Sha1::digest(n.to_le_bytes()).into(),
                    ObjectMetadata {
                        offset: n as u64 * 4,
                        size: 4,
                    },
                    Default::default(),
                )
            }),
        )
        .unwrap();
    index
        .save(repo.path("elfshaker_data/packs/base.pack.idx"))
        .unwrap();
    repo.extract("s1");
    assert_eq!(
        fs::read(repo.path("object-2047")).unwrap(),
        2047u32.to_le_bytes()
    );
}

#[test]
fn concurrent_appends_keep_both_snapshots() {
    use std::process::Stdio;
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", &vec![b'A'; 1024 * 1024]);
    repo.run(&["store", "s2"]);
    repo.write("changed", &vec![b'B'; 1024 * 1024]);
    repo.run(&["store", "s3"]);
    let children: Vec<_> = ["loose/s2", "loose/s3"]
        .iter()
        .map(|snapshot| {
            Command::new(env!("CARGO_BIN_EXE_elfshaker"))
                .env_remove("ELFSHAKER_DATA")
                .current_dir(&repo.0)
                .args(["pack", "--append", "-T", "1", "base", snapshot])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let mut tags = repo.index().snapshot_tags().to_vec();
    tags.sort();
    assert_eq!(tags, ["s1", "s2", "s3"]);
    repo.extract("s1");
    repo.extract("s2");
    assert_eq!(
        fs::read(repo.path("changed")).unwrap(),
        vec![b'A'; 1024 * 1024]
    );
    repo.extract("s3");
    assert_eq!(
        fs::read(repo.path("changed")).unwrap(),
        vec![b'B'; 1024 * 1024]
    );
}

#[test]
fn append_accepts_multiple_inputs_and_snapshot_lists() {
    use std::io::Write;
    use std::process::Stdio;
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", b"second");
    repo.run(&["store", "s2"]);
    repo.write("changed", b"third");
    repo.run(&["store", "s3"]);
    repo.run(&["pack", "--append", "base", "loose/s2", "loose/s3"]);
    repo.extract("s2");
    assert_eq!(fs::read(repo.path("changed")).unwrap(), b"second");
    repo.extract("s3");
    assert_eq!(fs::read(repo.path("changed")).unwrap(), b"third");
    repo.run(&["store", "s4"]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_elfshaker"))
        .env_remove("ELFSHAKER_DATA")
        .current_dir(&repo.0)
        .args(["pack", "--append", "base", "--snapshots0-from", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"loose/s4:s4\0")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(repo.index().snapshot_tags(), ["s1", "s2", "s3", "s4"]);
    repo.extract("s1");
    repo.extract("s4");
    assert_eq!(fs::read(repo.path("changed")).unwrap(), b"third");
}

#[test]
fn overflow_splits_old_and_new_objects_and_keeps_unaffected_frames() {
    let repo = Repo::new();
    repo.write("a", &vec![b'a'; 300 * 1024]);
    repo.write("b", &vec![b'b'; 400 * 1024]);
    repo.write("stable", &vec![b's'; 900 * 1024]);
    repo.run(&["store", "s1"]);
    repo.run(&[
        "pack",
        "--frame-size",
        "1",
        "--frame-fill",
        "75",
        "base",
        "loose/s1",
    ]);
    let old = repo.pack();
    assert_eq!(frames(&old).len(), 2);
    let old_checksum = repo.index().compute_snapshot_checksum("s1");
    repo.remove_old_objects();
    repo.write("a", &vec![b'A'; 400 * 1024]);
    repo.run(&["store", "s2"]);
    for checksum in repo.index().object_checksums() {
        let _ = fs::remove_file(object_path(&repo.0, checksum));
    }
    repo.run(&[
        "pack",
        "--append",
        "--frame-size",
        "1",
        "--frame-fill",
        "75",
        "base",
        "loose/s2",
    ]);
    let new = repo.pack();
    let new_frames = frames(&new);
    assert_eq!(new_frames.len(), 3);
    assert_eq!(frames(&old).last(), new_frames.last());
    assert!(!new_frames.contains(&frames(&old)[0])); // old contents participate in splitting
    let raw_sizes: Vec<_> = new_frames.iter().map(|f| decode_frame(f).len()).collect();
    assert_eq!(raw_sizes, [700 * 1024, 400 * 1024, 900 * 1024]);
    assert_eq!(repo.index().compute_snapshot_checksum("s1"), old_checksum);
    repo.extract("s1");
    assert_eq!(fs::read(repo.path("a")).unwrap(), vec![b'a'; 300 * 1024]);
    repo.extract("s2");
    assert_eq!(fs::read(repo.path("a")).unwrap(), vec![b'A'; 400 * 1024]);
    // A later append must find its predecessor after frame numbers have shifted.
    repo.write("a", &vec![b'!'; 50 * 1024]);
    repo.run(&["store", "s3"]);
    repo.run(&["pack", "--append", "--frame-size", "1", "base", "loose/s3"]);
    repo.extract("s1");
    repo.extract("s2");
    repo.extract("s3");
    assert_eq!(fs::read(repo.path("a")).unwrap(), vec![b'!'; 50 * 1024]);
}

#[test]
fn reintroduced_path_uses_its_last_seen_frame_instead_of_size_fallback() {
    let repo = Repo::new();
    repo.write("a", &[b'a'; 100]);
    repo.write("z", &[b'z'; 10000]);
    repo.run(&["store", "s1"]);
    repo.run(&["pack", "--frames", "2", "base", "loose/s1"]);
    let before = repo.pack();
    fs::remove_file(repo.path("a")).unwrap();
    repo.run(&["store", "s2"]);
    repo.append("s2");
    assert_eq!(repo.pack(), before);
    repo.write("a", &[b'A'; 12000]);
    repo.run(&["store", "s3"]);
    repo.append("s3");
    let after = repo.pack();
    assert_ne!(frames(&before)[0], frames(&after)[0]);
    assert_eq!(frames(&before)[1], frames(&after)[1]);
    repo.extract("s1");
    repo.extract("s2");
    repo.extract("s3");
    assert_eq!(fs::read(repo.path("a")).unwrap(), [b'A'; 12000]);
}

#[test]
fn oversized_objects_remain_whole_and_bad_layout_options_are_rejected() {
    let repo = Repo::new();
    repo.setup();
    repo.write("changed", &vec![42; 2 * 1024 * 1024]);
    repo.run(&["store", "s2"]);
    for (option, value) in [
        ("--frame-size", "0"),
        ("--frame-fill", "0"),
        ("--frame-fill", "101"),
        ("--frames", "2"),
    ] {
        let result = repo.command(&["pack", "--append", option, value, "base", "loose/s2"]);
        assert!(!result.status.success());
    }
    repo.run(&["pack", "--append", "--frame-size", "1", "base", "loose/s2"]);
    assert!(frames(&repo.pack())
        .iter()
        .any(|f| decode_frame(f).len() == 2 * 1024 * 1024));
    repo.extract("s1");
    repo.extract("s2");
    assert_eq!(
        fs::read(repo.path("changed")).unwrap(),
        vec![42; 2 * 1024 * 1024]
    );
}
