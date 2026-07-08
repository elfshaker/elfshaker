use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempTree(PathBuf);

impl TempTree {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("elfshaker-{name}-{}-{nonce}", std::process::id()));
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

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
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
fn loose_and_packed_symlinks_round_trip_through_the_cli() {
    let tree = TempTree::new("symlink-pack");
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

#[cfg(unix)]
#[test]
fn absolute_and_non_unicode_unix_targets_round_trip_natively() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let tree = TempTree::new("native-unix-links");
    let source = tree.path().join("source");
    let checkout = tree.path().join("checkout");
    let data = tree.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&checkout).unwrap();
    let absolute_target = tree.path().join("outside");
    fs::write(&absolute_target, b"outside").unwrap();
    symlink_file(&absolute_target, &source.join("absolute"));
    let native_target = std::ffi::OsString::from_vec(b"missing-\xff".to_vec());
    symlink_file(Path::new(&native_target), &source.join("non-unicode"));

    succeeds(&source, &data, &["store", "snapshot"]);
    succeeds(
        &checkout,
        &data,
        &["extract", "snapshot", "--reset", "--force", "--verify"],
    );
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
