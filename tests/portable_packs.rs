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

fn elfshaker(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfshaker"))
        .current_dir(root)
        .arg("--data-dir")
        .arg(root.join("data"))
        .args(arguments)
        .output()
        .unwrap()
}

fn succeeds(root: &Path, arguments: &[&str]) -> Output {
    let output = elfshaker(root, arguments);
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
fn version_three_pack_round_trips_through_cli_operations() {
    let tree = TempTree::new();
    let source = tree.path().join("目录").join("résumé.txt");
    fs::create_dir(source.parent().unwrap()).unwrap();
    fs::write(&source, b"portable contents\n").unwrap();

    succeeds(tree.path(), &["store", "snapshot"]);
    let loose_index = tree.path().join("data/packs/loose/snapshot.pack.idx");
    assert_eq!(&fs::read(&loose_index).unwrap()[..8], b"ELFS\0\0\0\x03");

    let listing = succeeds(tree.path(), &["list-files", "snapshot"]);
    assert!(String::from_utf8(listing.stdout)
        .unwrap()
        .contains("目录/résumé.txt"));
    assert_eq!(
        succeeds(tree.path(), &["show", "snapshot", "目录/résumé.txt"]).stdout,
        b"portable contents\n"
    );

    succeeds(
        tree.path(),
        &["pack", "portable", "--compression-level", "1"],
    );
    let packed_index = tree.path().join("data/packs/portable.pack.idx");
    assert_eq!(&fs::read(&packed_index).unwrap()[..8], b"ELFS\0\0\0\x03");
    succeeds(tree.path(), &["list", "portable"]);
    succeeds(tree.path(), &["list-files", "portable:snapshot"]);
    assert_eq!(
        succeeds(
            tree.path(),
            &["show", "portable:snapshot", "目录/résumé.txt"],
        )
        .stdout,
        b"portable contents\n"
    );

    fs::remove_file(&source).unwrap();
    succeeds(
        tree.path(),
        &[
            "extract",
            "portable:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );
    assert_eq!(fs::read(source).unwrap(), b"portable contents\n");
}

#[test]
fn checked_in_version_three_pack_is_readable_on_this_host() {
    let tree = TempTree::new();
    let packs = tree.path().join("data/packs");
    fs::create_dir_all(&packs).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/portable");
    for extension in ["pack", "pack.idx"] {
        fs::copy(
            fixtures.join(format!("portable-v3.{extension}")),
            packs.join(format!("portable-v3.{extension}")),
        )
        .unwrap();
    }

    let listing = succeeds(tree.path(), &["list-files", "portable-v3:snapshot"]);
    assert!(String::from_utf8(listing.stdout)
        .unwrap()
        .contains("目录/résumé.txt"));
    assert_eq!(
        succeeds(
            tree.path(),
            &["show", "portable-v3:snapshot", "目录/résumé.txt"],
        )
        .stdout,
        b"portable fixture contents\n"
    );

    succeeds(
        tree.path(),
        &[
            "extract",
            "portable-v3:snapshot",
            "--reset",
            "--force",
            "--verify",
        ],
    );
    assert_eq!(
        fs::read(tree.path().join("目录/résumé.txt")).unwrap(),
        b"portable fixture contents\n"
    );
}
