use elfshaker::packidx::{EntryMetadata, PackIndex, SymlinkTargetEncoding, SymlinkTargetKind};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "elfshaker-windows-writer-v3-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(extension: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/windows-v3")
        .join(format!("windows-writer-v3.{extension}"))
}

fn install_fixture(tree: &TempTree) -> (PathBuf, PathBuf) {
    let checkout = tree.0.join("checkout");
    let data = tree.0.join("data");
    let packs = data.join("packs");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir_all(&packs).unwrap();
    for extension in ["pack", "pack.idx"] {
        fs::copy(
            fixture(extension),
            packs.join(format!("windows-writer-v3.{extension}")),
        )
        .unwrap();
    }
    (checkout, data)
}

fn elfshaker(root: &Path, data: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfshaker"))
        .current_dir(root)
        .arg("--data-dir")
        .arg(data)
        .args(arguments)
        .output()
        .unwrap()
}

fn succeeds(root: &Path, data: &Path, arguments: &[&str]) -> Output {
    let output = elfshaker(root, data, arguments);
    assert!(
        output.status.success(),
        "elfshaker {:?} failed\nstdout:\n{}\nstderr:\n{}",
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn windows_writer_v3_fixture_exposes_portable_and_native_target_metadata() {
    let index = PackIndex::load(fixture("pack.idx")).unwrap();
    let portable = index.resolve_snapshot("portable").unwrap();
    let portable = index.entries_from_handles(portable.iter()).unwrap();
    for (path, expected_kind) in [
        (b"file-link".as_slice(), SymlinkTargetKind::File),
        (b"directory-link".as_slice(), SymlinkTargetKind::Directory),
        (b"dangling-file".as_slice(), SymlinkTargetKind::File),
        (
            b"dangling-directory".as_slice(),
            SymlinkTargetKind::Directory,
        ),
    ] {
        let entry = portable
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

    let native = index.resolve_snapshot("native").unwrap();
    let native = index.entries_from_handles(native.iter()).unwrap();
    let link = native
        .iter()
        .find(|entry| entry.path.as_bytes() == b"absolute-link")
        .unwrap();
    assert_eq!(
        link.metadata,
        EntryMetadata::Symlink {
            target_encoding: SymlinkTargetEncoding::WindowsUtf16Le,
            target_kind: SymlinkTargetKind::File,
        }
    );
}

#[test]
fn portable_snapshot_from_windows_writer_extracts_on_this_host_requires_symlinks() {
    let tree = TempTree::new();
    let (checkout, data) = install_fixture(&tree);
    succeeds(
        &checkout,
        &data,
        &[
            "extract",
            "windows-writer-v3:portable",
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
        fs::read_link(checkout.join("dangling-file")).unwrap(),
        Path::new("missing-file")
    );
    assert_eq!(
        fs::read_link(checkout.join("dangling-directory")).unwrap(),
        Path::new("missing-directory")
    );
}

#[cfg(unix)]
#[test]
fn windows_native_target_is_rejected_before_unix_mutation() {
    let tree = TempTree::new();
    let (checkout, data) = install_fixture(&tree);
    fs::write(checkout.join("sentinel"), b"original").unwrap();
    let output = elfshaker(
        &checkout,
        &data,
        &[
            "extract",
            "windows-writer-v3:native",
            "--reset",
            "--force",
            "--verify",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not supported on this platform"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(checkout.join("sentinel")).unwrap(), b"original");
    assert!(fs::symlink_metadata(checkout.join("absolute-link")).is_err());
}

#[cfg(windows)]
#[test]
fn windows_native_target_extracts_on_windows_requires_symlinks() {
    let tree = TempTree::new();
    let (checkout, data) = install_fixture(&tree);
    succeeds(
        &checkout,
        &data,
        &[
            "extract",
            "windows-writer-v3:native",
            "--reset",
            "--force",
            "--verify",
        ],
    );
    assert_eq!(
        fs::read_link(checkout.join("absolute-link")).unwrap(),
        Path::new(r"C:\elfshaker-fixture-target")
    );
}
