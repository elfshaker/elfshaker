use elfshaker::packidx::{
    EntryMetadata, FileEntry, ObjectMetadata, PackIndex, PackPath, SymlinkTargetEncoding,
    SymlinkTargetKind, LOOSE_OBJECT_OFFSET,
};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let path = elfshaker::repo::fs::create_temp_path(&std::env::temp_dir());
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn elfshaker(root: &Path, data_dir: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfshaker"))
        .current_dir(root)
        .arg("--data-dir")
        .arg(data_dir)
        .args(arguments)
        .output()
        .unwrap()
}

fn succeeds(root: &Path, data_dir: &Path, arguments: &[&str]) -> Output {
    let output = elfshaker(root, data_dir, arguments);
    assert!(
        output.status.success(),
        "elfshaker {:?} failed\nstdout:\n{}\nstderr:\n{}",
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fails(root: &Path, data_dir: &Path, arguments: &[&str]) -> Output {
    let output = elfshaker(root, data_dir, arguments);
    assert!(
        !output.status.success(),
        "elfshaker {:?} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(unix)]
fn remove_symlink(path: &Path) {
    fs::remove_file(path).unwrap();
}

#[cfg(windows)]
fn remove_symlink(path: &Path) {
    use std::os::windows::fs::FileTypeExt;

    let file_type = fs::symlink_metadata(path).unwrap().file_type();
    if file_type.is_symlink_dir() {
        fs::remove_dir(path).unwrap();
    } else {
        fs::remove_file(path).unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum NodeKind {
    Regular,
    EmptyRegular,
    Directory,
    FileLink,
    DirectoryLink,
}

impl NodeKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::EmptyRegular => "empty-regular",
            Self::Directory => "directory",
            Self::FileLink => "file-link",
            Self::DirectoryLink => "directory-link",
        }
    }

    fn install(self, root: &Path) {
        remove_node(&root.join("node"));
        match self {
            Self::Regular => fs::write(root.join("node"), b"regular").unwrap(),
            Self::EmptyRegular => fs::write(root.join("node"), b"").unwrap(),
            Self::Directory => {
                fs::create_dir(root.join("node")).unwrap();
                fs::write(root.join("node/child"), b"child").unwrap();
            }
            Self::FileLink => symlink_file(Path::new("target-file"), &root.join("node")),
            Self::DirectoryLink => symlink_dir(Path::new("target-dir"), &root.join("node")),
        }
    }

    fn assert_materialized(self, root: &Path) {
        match self {
            Self::Regular => assert_eq!(fs::read(root.join("node")).unwrap(), b"regular"),
            Self::EmptyRegular => assert_eq!(fs::read(root.join("node")).unwrap(), b""),
            Self::Directory => {
                assert_eq!(fs::read(root.join("node/child")).unwrap(), b"child")
            }
            Self::FileLink => {
                assert_eq!(
                    fs::read_link(root.join("node")).unwrap(),
                    Path::new("target-file")
                )
            }
            Self::DirectoryLink => {
                assert_eq!(
                    fs::read_link(root.join("node")).unwrap(),
                    Path::new("target-dir")
                )
            }
        }
    }

    fn make_dirty(self, root: &Path) {
        match self {
            Self::Regular => {
                fs::remove_file(root.join("node")).unwrap();
                fs::create_dir(root.join("node")).unwrap();
            }
            Self::EmptyRegular => {
                fs::remove_file(root.join("node")).unwrap();
                fs::create_dir(root.join("node")).unwrap();
            }
            Self::Directory => {
                fs::remove_file(root.join("node/child")).unwrap();
                symlink_file(Path::new("../target-file"), &root.join("node/child"));
            }
            Self::FileLink => {
                remove_symlink(&root.join("node"));
                symlink_file(Path::new("changed-target"), &root.join("node"));
            }
            Self::DirectoryLink => {
                remove_symlink(&root.join("node"));
                symlink_dir(Path::new("changed-target"), &root.join("node"));
            }
        }
    }
}

fn remove_node(path: &Path) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    if metadata.file_type().is_symlink() {
        remove_symlink(path);
    } else if metadata.is_dir() {
        fs::remove_dir_all(path).unwrap();
    } else {
        fs::remove_file(path).unwrap();
    }
}

fn snapshot_id(storage: &str, kind: NodeKind) -> String {
    if storage == "loose" {
        format!("loose/{0}:{0}", kind.tag())
    } else {
        format!("transitions:{}", kind.tag())
    }
}

fn loose_object_path(data_dir: &Path, checksum: &[u8; 20]) -> PathBuf {
    let checksum = hex::encode(checksum);
    data_dir
        .join("loose")
        .join(&checksum[..2])
        .join(&checksum[2..4])
        .join(&checksum[4..])
}

fn write_loose_object(data_dir: &Path, bytes: &[u8]) -> [u8; 20] {
    let checksum: [u8; 20] = Sha1::digest(bytes).into();
    let path = loose_object_path(data_dir, &checksum);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    checksum
}

fn install_synthetic_symlink_snapshot(
    data_dir: &Path,
    tag: &str,
    target: &[u8],
    encoding: SymlinkTargetEncoding,
    target_kind: SymlinkTargetKind,
) {
    let replacement = b"replacement";
    let regular_checksum = write_loose_object(data_dir, replacement);
    let target_checksum = write_loose_object(data_dir, target);
    let entries = [
        FileEntry::new(
            PackPath::new(b"sentinel".to_vec()).unwrap(),
            regular_checksum,
            ObjectMetadata {
                offset: LOOSE_OBJECT_OFFSET,
                size: replacement.len() as u64,
            },
            EntryMetadata::Regular { mode: 0o100644 },
        ),
        FileEntry::new(
            PackPath::new(b"link".to_vec()).unwrap(),
            target_checksum,
            ObjectMetadata {
                offset: LOOSE_OBJECT_OFFSET,
                size: target.len() as u64,
            },
            EntryMetadata::Symlink {
                target_encoding: encoding,
                target_kind,
            },
        ),
    ];
    let mut index = PackIndex::new();
    index.push_snapshot(tag.to_owned(), entries).unwrap();
    let index_path = data_dir.join("packs/loose").join(format!("{tag}.pack.idx"));
    fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    index.save(index_path).unwrap();
}

#[test]
fn packed_show_keeps_zero_length_objects_separate_from_cached_duplicates() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("first"), b"nonempty").unwrap();
    fs::write(source.join("second"), b"nonempty").unwrap();
    fs::write(source.join("empty"), b"").unwrap();

    succeeds(&source, &data, &["store", "snapshot"]);
    succeeds(
        &source,
        &data,
        &[
            "pack",
            "objects",
            "--compression-level",
            "1",
            "--threads",
            "1",
            "--frames",
            "1",
        ],
    );

    assert_eq!(
        succeeds(
            &source,
            &data,
            &["show", "objects:snapshot", "first", "empty"],
        )
        .stdout,
        b"nonempty"
    );
    assert_eq!(
        succeeds(
            &source,
            &data,
            &["show", "objects:snapshot", "first", "empty", "second"],
        )
        .stdout,
        b"nonemptynonempty"
    );
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link)
        .expect("native Windows file symlink creation is required");
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(target, link)
        .expect("native Windows directory symlink creation is required");
}

#[test]
fn loose_and_packed_symlinks_round_trip_through_the_cli_requires_symlinks() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let loose_checkout = tree.path().join("loose-checkout");
    let packed_checkout = tree.path().join("packed-checkout");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&loose_checkout).unwrap();
    fs::create_dir(&packed_checkout).unwrap();
    fs::create_dir(source.join("target-dir")).unwrap();
    fs::write(source.join("target-file"), b"file contents").unwrap();
    fs::write(source.join("target-dir/child"), b"directory contents").unwrap();
    fs::write(source.join("target-text"), b"target-file").unwrap();

    symlink_file(Path::new("target-file"), &source.join("file-link"));
    symlink_file(Path::new("target-file"), &source.join("duplicate-link"));
    symlink_dir(Path::new("target-dir"), &source.join("dir-link"));
    symlink_file(Path::new("missing-file"), &source.join("dangling-file"));
    symlink_dir(Path::new("missing-dir"), &source.join("dangling-dir"));

    succeeds(&source, &data, &["store", "snapshot"]);
    let listing = succeeds(
        &source,
        &data,
        &["list-files", "snapshot", "--format", "%k %p %f"],
    );
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("symlink - file-link"));
    assert!(listing.contains("symlink - dangling-file"));
    assert!(listing.contains("file 100"));
    assert_eq!(
        succeeds(&source, &data, &["show", "snapshot", "file-link"]).stdout,
        b"target-file"
    );
    assert_eq!(
        succeeds(&source, &data, &["show", "snapshot", "target-text"]).stdout,
        b"target-file"
    );

    succeeds(
        &loose_checkout,
        &data,
        &["extract", "snapshot", "--reset", "--force", "--verify"],
    );
    assert_links_restored(&loose_checkout);

    succeeds(
        &source,
        &data,
        &["pack", "links", "--compression-level", "1"],
    );
    let packed_listing = succeeds(
        &source,
        &data,
        &["list-files", "links:snapshot", "--format", "%k %p %f"],
    );
    assert!(String::from_utf8(packed_listing.stdout)
        .unwrap()
        .contains("symlink - file-link"));
    assert_eq!(
        succeeds(&source, &data, &["show", "links:snapshot", "file-link"],).stdout,
        b"target-file"
    );

    let index = PackIndex::load(data.join("packs/links.pack.idx")).unwrap();
    let handles = index.resolve_snapshot("snapshot").unwrap();
    let entries = index.entries_from_handles(handles.iter()).unwrap();
    let mut symlink_objects = entries
        .iter()
        .filter(|entry| matches!(entry.metadata, EntryMetadata::Symlink { .. }))
        .map(|entry| loose_object_path(&data, &entry.checksum))
        .collect::<Vec<_>>();
    symlink_objects.sort();
    symlink_objects.dedup();
    for object_path in &symlink_objects {
        fs::remove_file(object_path).unwrap();
    }
    succeeds(&source, &data, &["explode", "links"]);
    for object_path in symlink_objects {
        assert!(fs::symlink_metadata(&object_path).unwrap().is_file());
        let target = fs::read(&object_path).unwrap();
        assert!(matches!(
            target.as_slice(),
            b"target-file" | b"target-dir" | b"missing-file" | b"missing-dir"
        ));
    }
    succeeds(
        &packed_checkout,
        &data,
        &[
            "extract",
            "links:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );

    assert_links_restored(&packed_checkout);
}

fn assert_links_restored(checkout: &Path) {
    assert_eq!(
        fs::read_link(checkout.join("file-link")).unwrap(),
        Path::new("target-file")
    );
    assert_eq!(
        fs::read_link(checkout.join("duplicate-link")).unwrap(),
        Path::new("target-file")
    );
    assert_eq!(
        fs::read_link(checkout.join("dir-link")).unwrap(),
        Path::new("target-dir")
    );
    assert_eq!(
        fs::read_link(checkout.join("dangling-file")).unwrap(),
        Path::new("missing-file")
    );
    assert_eq!(
        fs::read_link(checkout.join("dangling-dir")).unwrap(),
        Path::new("missing-dir")
    );
    assert_eq!(
        fs::read(checkout.join("target-text")).unwrap(),
        b"target-file"
    );
}

#[test]
fn every_entry_kind_transition_works_for_loose_and_packed_snapshots_requires_symlinks() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("target-file"), b"target").unwrap();
    fs::create_dir(source.join("target-dir")).unwrap();
    fs::write(source.join("target-dir/child"), b"target child").unwrap();

    let kinds = [
        NodeKind::Regular,
        NodeKind::EmptyRegular,
        NodeKind::Directory,
        NodeKind::FileLink,
        NodeKind::DirectoryLink,
    ];
    for kind in kinds {
        kind.install(&source);
        succeeds(&source, &data, &["store", kind.tag()]);
    }
    succeeds(
        &source,
        &data,
        &["pack", "transitions", "--compression-level", "1"],
    );

    let transitions = [
        (NodeKind::Regular, NodeKind::Directory),
        (NodeKind::Directory, NodeKind::Regular),
        (NodeKind::Regular, NodeKind::FileLink),
        (NodeKind::FileLink, NodeKind::Regular),
        (NodeKind::Directory, NodeKind::FileLink),
        (NodeKind::FileLink, NodeKind::Directory),
        (NodeKind::Regular, NodeKind::DirectoryLink),
        (NodeKind::DirectoryLink, NodeKind::Regular),
        (NodeKind::DirectoryLink, NodeKind::EmptyRegular),
        (NodeKind::Directory, NodeKind::DirectoryLink),
        (NodeKind::DirectoryLink, NodeKind::Directory),
    ];
    for storage in ["loose", "packed"] {
        for force in [false, true] {
            for (from, to) in transitions {
                let checkout = tree.path().join(format!(
                    "clean-{storage}-{}-{}-{}",
                    from.tag(),
                    to.tag(),
                    if force { "force" } else { "checked" }
                ));
                fs::create_dir(&checkout).unwrap();
                let from_id = snapshot_id(storage, from);
                succeeds(
                    &checkout,
                    &data,
                    &["extract", &from_id, "--reset", "--force", "--verify"],
                );
                from.assert_materialized(&checkout);

                let to_id = snapshot_id(storage, to);
                if force {
                    succeeds(
                        &checkout,
                        &data,
                        &["extract", &to_id, "--force", "--verify"],
                    );
                } else {
                    succeeds(&checkout, &data, &["extract", &to_id, "--verify"]);
                }
                to.assert_materialized(&checkout);
            }
        }
    }

    let dirty_transitions = [
        (NodeKind::Regular, NodeKind::FileLink),
        (NodeKind::Directory, NodeKind::FileLink),
        (NodeKind::FileLink, NodeKind::Directory),
        (NodeKind::DirectoryLink, NodeKind::Regular),
    ];
    for storage in ["loose", "packed"] {
        for (from, to) in dirty_transitions {
            let checkout = tree
                .path()
                .join(format!("dirty-{storage}-{}-{}", from.tag(), to.tag()));
            fs::create_dir(&checkout).unwrap();
            let from_id = snapshot_id(storage, from);
            succeeds(
                &checkout,
                &data,
                &["extract", &from_id, "--reset", "--force", "--verify"],
            );
            from.make_dirty(&checkout);

            let to_id = snapshot_id(storage, to);
            let output = fails(&checkout, &data, &["extract", &to_id, "--verify"]);
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("removed or modified unexpectedly"),
                "unexpected stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            succeeds(
                &checkout,
                &data,
                &["extract", &to_id, "--force", "--verify"],
            );
            to.assert_materialized(&checkout);
        }
    }
}

#[test]
fn incompatible_targets_fail_before_loose_or_packed_extraction_mutates_files_requires_symlinks() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();

    #[cfg(unix)]
    let foreign_target = (
        vec![b'C', 0, b':', 0, b'\\', 0, b'x', 0],
        SymlinkTargetEncoding::WindowsUtf16Le,
    );
    #[cfg(windows)]
    let foreign_target = (b"/unix-only".to_vec(), SymlinkTargetEncoding::UnixBytes);

    let cases = vec![
        (
            "invalid-portable",
            b"/absolute".to_vec(),
            SymlinkTargetEncoding::PortableUtf8,
            SymlinkTargetKind::File,
            "invalid target",
        ),
        (
            "foreign-native",
            foreign_target.0,
            foreign_target.1,
            SymlinkTargetKind::File,
            "not supported on this platform",
        ),
    ];

    #[cfg(windows)]
    let cases = {
        let mut cases = cases;
        cases.push((
            "unknown-windows-kind",
            b"missing".to_vec(),
            SymlinkTargetEncoding::PortableUtf8,
            SymlinkTargetKind::Unknown,
            "target kind is unknown",
        ));
        cases
    };

    for (tag, target, encoding, target_kind, error_fragment) in cases {
        install_synthetic_symlink_snapshot(&data, tag, &target, encoding, target_kind);
        let pack_name = format!("{tag}-pack");
        let loose_pack = format!("loose/{tag}");
        succeeds(
            &source,
            &data,
            &["pack", &pack_name, &loose_pack, "--compression-level", "1"],
        );

        let loose_snapshot = format!("loose/{tag}:{tag}");
        let packed_snapshot = format!("{pack_name}:{tag}");
        for (storage, snapshot) in [
            ("loose", loose_snapshot.as_str()),
            ("packed", packed_snapshot.as_str()),
        ] {
            let checkout = tree.path().join(format!("{tag}-{storage}"));
            fs::create_dir(&checkout).unwrap();
            fs::write(checkout.join("sentinel"), b"original").unwrap();
            let output = fails(
                &checkout,
                &data,
                &["extract", snapshot, "--reset", "--force", "--verify"],
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(error_fragment),
                "unexpected stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read(checkout.join("sentinel")).unwrap(), b"original");
            assert!(fs::symlink_metadata(checkout.join("link")).is_err());
        }
    }
}

#[test]
fn extraction_rejects_symlinked_destination_ancestors_on_every_host_requires_symlinks() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let data = tree.path().join("data");
    fs::create_dir_all(source.join("dir")).unwrap();
    fs::write(source.join("dir/file"), b"snapshot data").unwrap();
    succeeds(&source, &data, &["store", "snapshot"]);
    succeeds(
        &source,
        &data,
        &["pack", "ancestor", "--compression-level", "1"],
    );

    for (storage, snapshot) in [
        ("loose", "loose/snapshot:snapshot"),
        ("packed", "ancestor:snapshot"),
    ] {
        let checkout = tree.path().join(format!("checkout-{storage}"));
        let outside = tree.path().join(format!("outside-{storage}"));
        fs::create_dir(&checkout).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"outside").unwrap();
        symlink_dir(&outside, &checkout.join("dir"));

        let output = fails(
            &checkout,
            &data,
            &["extract", snapshot, "--reset", "--force", "--verify"],
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("traverses a symbolic link"),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
        assert!(fs::symlink_metadata(outside.join("file")).is_err());
    }
}

fn portable_symlink_fixture(extension: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/symlink")
        .join(format!("portable-symlinks-v3.{extension}"))
}

#[test]
fn checked_in_portable_symlink_pack_extracts_on_this_host() {
    let tree = TempTree::new();
    let checkout = tree.path().join("checkout");
    let data = tree.path().join("data");
    let packs = data.join("packs");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir_all(&packs).unwrap();
    for (extension, expected_sha1) in [
        ("pack", "3e70fc7bd37c5fa4c1b1c3dc865c8382152ccec1"),
        ("pack.idx", "95d7a0d4bbde4baea16c3546a7634e7b30df05b7"),
    ] {
        let bytes = fs::read(portable_symlink_fixture(extension)).unwrap();
        assert_eq!(hex::encode(Sha1::digest(&bytes)), expected_sha1);
        fs::write(
            packs.join(format!("portable-symlinks-v3.{extension}")),
            bytes,
        )
        .unwrap();
    }
    let index = PackIndex::load(portable_symlink_fixture("pack.idx")).unwrap();
    let handles = index.resolve_snapshot("snapshot").unwrap();
    let entries = index.entries_from_handles(handles.iter()).unwrap();
    for (path, expected_kind) in [
        (b"file-link".as_slice(), SymlinkTargetKind::File),
        (b"directory-link".as_slice(), SymlinkTargetKind::Directory),
    ] {
        let entry = entries
            .iter()
            .find(|entry| entry.path.as_bytes() == path)
            .unwrap();
        assert_eq!(
            entry.metadata,
            EntryMetadata::Symlink {
                target_encoding: SymlinkTargetEncoding::PortableUtf8,
                target_kind: expected_kind,
            }
        );
    }

    let listing = succeeds(
        &checkout,
        &data,
        &[
            "list-files",
            "portable-symlinks-v3:snapshot",
            "--format",
            "%k %p %f",
        ],
    );
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("symlink - file-link"));
    assert!(listing.contains("symlink - directory-link"));
    assert_eq!(
        succeeds(
            &checkout,
            &data,
            &["show", "portable-symlinks-v3:snapshot", "file-link",],
        )
        .stdout,
        b"target-file"
    );

    if std::env::var("ELFSHAKER_TEST_NO_SYMLINKS").as_deref() == Ok("1") {
        fs::write(checkout.join("sentinel"), b"original").unwrap();
        let output = fails(
            &checkout,
            &data,
            &[
                "extract",
                "portable-symlinks-v3:snapshot",
                "--reset",
                "--force",
                "--verify",
            ],
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Cannot create symbolic links"),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(checkout.join("sentinel")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&checkout).unwrap().count(), 1);
        return;
    }

    succeeds(
        &checkout,
        &data,
        &[
            "extract",
            "portable-symlinks-v3:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );
    assert_eq!(
        fs::read_link(checkout.join("file-link")).unwrap(),
        Path::new("target-file")
    );
    assert_eq!(
        fs::read_link(checkout.join("directory-link")).unwrap(),
        Path::new("target-dir")
    );
    assert_eq!(
        fs::read(checkout.join("target-file")).unwrap(),
        b"fixture target\n"
    );
    assert_eq!(
        fs::read(checkout.join("target-dir/child")).unwrap(),
        b"fixture child\n"
    );
}

#[cfg(unix)]
#[test]
fn absolute_and_non_unicode_unix_targets_round_trip_natively_requires_symlinks() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let tree = TempTree::new();
    let source = tree.path().join("source");
    let loose_checkout = tree.path().join("loose-checkout");
    let packed_checkout = tree.path().join("packed-checkout");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&loose_checkout).unwrap();
    fs::create_dir(&packed_checkout).unwrap();
    let absolute_target = tree.path().join("outside");
    fs::write(&absolute_target, b"outside").unwrap();
    symlink_file(&absolute_target, &source.join("absolute"));
    let native_target = std::ffi::OsString::from_vec(b"missing-\xff".to_vec());
    symlink_file(Path::new(&native_target), &source.join("non-unicode"));

    succeeds(&source, &data, &["store", "snapshot"]);
    succeeds(
        &loose_checkout,
        &data,
        &["extract", "snapshot", "--reset", "--force", "--verify"],
    );
    succeeds(
        &source,
        &data,
        &["pack", "native", "--compression-level", "1"],
    );
    succeeds(
        &packed_checkout,
        &data,
        &[
            "extract",
            "native:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );

    for checkout in [&loose_checkout, &packed_checkout] {
        assert_eq!(
            fs::read_link(checkout.join("absolute")).unwrap(),
            absolute_target
        );
        assert_eq!(
            fs::read_link(checkout.join("non-unicode"))
                .unwrap()
                .as_os_str()
                .as_bytes(),
            OsStr::from_bytes(b"missing-\xff").as_bytes()
        );
    }
}

#[cfg(windows)]
#[test]
fn absolute_windows_targets_round_trip_natively_from_loose_and_packed_storage_requires_symlinks() {
    let tree = TempTree::new();
    let source = tree.path().join("source");
    let loose_checkout = tree.path().join("loose-checkout");
    let packed_checkout = tree.path().join("packed-checkout");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&loose_checkout).unwrap();
    fs::create_dir(&packed_checkout).unwrap();
    let absolute_file = tree.path().join("outside-file");
    let absolute_directory = tree.path().join("outside-directory");
    fs::write(&absolute_file, b"outside").unwrap();
    fs::create_dir(&absolute_directory).unwrap();
    symlink_file(&absolute_file, &source.join("file-link"));
    symlink_dir(&absolute_directory, &source.join("directory-link"));

    succeeds(&source, &data, &["store", "snapshot"]);
    succeeds(
        &loose_checkout,
        &data,
        &["extract", "snapshot", "--reset", "--force", "--verify"],
    );
    succeeds(
        &source,
        &data,
        &["pack", "native", "--compression-level", "1"],
    );
    succeeds(
        &packed_checkout,
        &data,
        &[
            "extract",
            "native:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );

    for checkout in [&loose_checkout, &packed_checkout] {
        assert_eq!(
            fs::read_link(checkout.join("file-link")).unwrap(),
            absolute_file
        );
        assert_eq!(
            fs::read_link(checkout.join("directory-link")).unwrap(),
            absolute_directory
        );
    }
}
