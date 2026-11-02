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
            "elfshaker-collision-{}-{nonce}",
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
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfshaker"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
fn succeeds(root: &Path, args: &[&str]) {
    let output = cli(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn extraction_refuses_untracked_collisions_before_mutating_tree_or_head() {
    for packed in [false, true] {
        for reset in [false, true] {
            let tree = TempTree::new();
            let root = &tree.0;
            fs::write(root.join("common"), b"a").unwrap();
            fs::write(root.join("removed"), b"keep until successful extraction").unwrap();
            succeeds(root, &["store", "a"]);
            fs::write(root.join("common"), b"b").unwrap();
            fs::remove_file(root.join("removed")).unwrap();
            fs::write(root.join("notes.txt"), b"snapshot contents").unwrap();
            succeeds(root, &["store", "b"]);
            if packed {
                succeeds(root, &["pack", "test", "--threads", "1"]);
                succeeds(root, &["gc", "--loose-snapshots", "--loose-objects"]);
            }
            succeeds(root, &["extract", "a", "--verify"]);
            fs::write(root.join("notes.txt"), b"my local notes").unwrap();
            let head_path = root.join("elfshaker_data/HEAD");
            let head = fs::read(&head_path).unwrap();
            let mut args = vec!["extract", "b", "--verify"];
            if reset {
                args.push("--reset");
            }
            let output = cli(root, &args);
            assert!(
                !output.status.success(),
                "packed={}, reset={}: untracked file overwritten",
                packed,
                reset
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("notes.txt"));
            assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"my local notes");
            assert_eq!(fs::read(root.join("common")).unwrap(), b"a");
            assert_eq!(
                fs::read(root.join("removed")).unwrap(),
                b"keep until successful extraction"
            );
            assert_eq!(fs::read(&head_path).unwrap(), head);
            args.push("--force");
            succeeds(root, &args);
            assert_eq!(
                fs::read(root.join("notes.txt")).unwrap(),
                b"snapshot contents"
            );
        }
    }
}

#[test]
fn extraction_without_head_protects_existing_files() {
    let tree = TempTree::new();
    let root = &tree.0;
    fs::write(root.join("notes.txt"), b"snapshot contents").unwrap();
    succeeds(root, &["store", "b"]);
    fs::remove_file(root.join("elfshaker_data/HEAD")).unwrap();
    fs::write(root.join("notes.txt"), b"my local notes").unwrap();
    for args in [vec!["extract", "b"], vec!["extract", "b", "--reset"]] {
        let output = cli(root, &args);
        assert!(!output.status.success());
        assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"my local notes");
        assert!(!root.join("elfshaker_data/HEAD").exists());
    }
    succeeds(root, &["extract", "b", "--force", "--verify"]);
    assert_eq!(
        fs::read(root.join("notes.txt")).unwrap(),
        b"snapshot contents"
    );
}

#[test]
fn replacing_tracked_directory_protects_untracked_children() {
    let tree = TempTree::new();
    let root = &tree.0;
    fs::create_dir(root.join("node")).unwrap();
    fs::write(root.join("node/tracked"), b"tracked").unwrap();
    succeeds(root, &["store", "directory"]);
    fs::remove_file(root.join("node/tracked")).unwrap();
    fs::remove_dir(root.join("node")).unwrap();
    fs::write(root.join("node"), b"replacement").unwrap();
    succeeds(root, &["store", "file"]);
    succeeds(root, &["extract", "directory", "--verify"]);
    fs::write(root.join("node/untracked"), b"my local notes").unwrap();
    let head = fs::read(root.join("elfshaker_data/HEAD")).unwrap();
    let output = cli(root, &["extract", "file", "--verify"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("untracked"));
    assert_eq!(fs::read(root.join("node/tracked")).unwrap(), b"tracked");
    assert_eq!(
        fs::read(root.join("node/untracked")).unwrap(),
        b"my local notes"
    );
    assert_eq!(fs::read(root.join("elfshaker_data/HEAD")).unwrap(), head);
    fs::remove_file(root.join("node/untracked")).unwrap();
    succeeds(root, &["extract", "file", "--verify"]);
    assert_eq!(fs::read(root.join("node")).unwrap(), b"replacement");
}

#[cfg(unix)]
#[test]
fn extraction_protects_untracked_dangling_symlinks() {
    let tree = TempTree::new();
    let root = &tree.0;
    fs::write(root.join("common"), b"common").unwrap();
    succeeds(root, &["store", "a"]);
    fs::write(root.join("notes.txt"), b"snapshot contents").unwrap();
    succeeds(root, &["store", "b"]);
    succeeds(root, &["extract", "a", "--verify"]);
    std::os::unix::fs::symlink("missing", root.join("notes.txt")).unwrap();
    let head = fs::read(root.join("elfshaker_data/HEAD")).unwrap();
    let output = cli(root, &["extract", "b", "--verify"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("notes.txt"));
    assert_eq!(
        fs::read_link(root.join("notes.txt")).unwrap(),
        Path::new("missing")
    );
    assert_eq!(fs::read(root.join("elfshaker_data/HEAD")).unwrap(), head);
    succeeds(root, &["extract", "b", "--verify", "--force"]);
    assert_eq!(
        fs::read(root.join("notes.txt")).unwrap(),
        b"snapshot contents"
    );
}

#[test]
fn forced_reset_can_recover_a_missing_head_index() {
    let tree = TempTree::new();
    let root = &tree.0;
    fs::write(root.join("common"), b"a").unwrap();
    succeeds(root, &["store", "a"]);
    fs::write(root.join("common"), b"b").unwrap();
    succeeds(root, &["store", "b"]);
    succeeds(root, &["extract", "a", "--verify"]);
    fs::remove_file(root.join("elfshaker_data/packs/loose/a.pack.idx")).unwrap();
    let output = cli(root, &["extract", "b", "--reset", "--verify"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(root.join("common")).unwrap(), b"a");
    succeeds(root, &["extract", "b", "--reset", "--force", "--verify"]);
    assert_eq!(fs::read(root.join("common")).unwrap(), b"b");
}
