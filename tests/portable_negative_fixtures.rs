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

fn fixture(name: &str, extension: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/portable-negative")
        .join(format!("{name}.{extension}"))
}

fn install_fixture(tree: &TempTree, name: &str) -> (PathBuf, PathBuf) {
    let checkout = tree.path().join("checkout");
    let data = tree.path().join("data");
    let packs = data.join("packs");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir_all(&packs).unwrap();
    for extension in ["pack", "pack.idx"] {
        fs::copy(
            fixture(name, extension),
            packs.join(format!("{name}.{extension}")),
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

#[cfg(any(windows, target_os = "macos"))]
fn assert_rejected_without_mutation(name: &str, error_fragment: &str) {
    let tree = TempTree::new();
    let (checkout, data) = install_fixture(&tree, name);
    fs::write(checkout.join("sentinel"), b"original").unwrap();
    let snapshot = format!("{name}:snapshot");
    let output = elfshaker(
        &checkout,
        &data,
        &["extract", &snapshot, "--reset", "--force", "--verify"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(error_fragment),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(checkout.join("sentinel")).unwrap(), b"original");
}

#[cfg(any(target_os = "linux", windows))]
fn extract_fixture(name: &str) -> TempTree {
    let tree = TempTree::new();
    let (checkout, data) = install_fixture(&tree, name);
    let snapshot = format!("{name}:snapshot");
    succeeds(
        &checkout,
        &data,
        &["extract", &snapshot, "--reset", "--force", "--verify"],
    );
    tree
}

#[test]
fn producer_derived_negative_fixtures_can_be_inspected_on_every_host() {
    for (name, expected_path, pack_sha1, index_sha1) in [
        (
            "unix-non-utf8-v3",
            "non-utf8-%FF",
            "c1e134147cbf0739cc558bc01cf4050852cd8033",
            "3ed02f134f2164a7ed9ff5f26338125d6722050f",
        ),
        (
            "windows-case-collision-v3",
            "Case",
            "c0ccb463a216ab1e1408d016b16ee993f59874a8",
            "7aaa57db241964c4f4f3ebdb4e1b2bcdd317a60e",
        ),
        (
            "macos-unicode-collision-v3",
            "Résumé",
            "9b0c1e54690a17ac5fb902ffdc1dee08e6aedf64",
            "429af129261df247d39a0e223a1eac774428d066",
        ),
    ] {
        assert_eq!(
            hex::encode(Sha1::digest(fs::read(fixture(name, "pack")).unwrap())),
            pack_sha1
        );
        assert_eq!(
            hex::encode(Sha1::digest(fs::read(fixture(name, "pack.idx")).unwrap())),
            index_sha1
        );
        let tree = TempTree::new();
        let (checkout, data) = install_fixture(&tree, name);
        let snapshot = format!("{name}:snapshot");
        let listing = succeeds(&checkout, &data, &["list-files", &snapshot]);
        assert!(String::from_utf8_lossy(&listing.stdout).contains(expected_path));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_extracts_the_unix_only_filename_fixture_exactly() {
    use std::os::unix::ffi::OsStrExt;

    let tree = extract_fixture("unix-non-utf8-v3");
    let name = std::ffi::OsStr::from_bytes(b"non-utf8-\xff");
    assert_eq!(
        fs::read(tree.path().join("checkout").join(name)).unwrap(),
        b"native\n"
    );
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn non_utf8_unix_filename_is_rejected_before_mutation() {
    assert_rejected_without_mutation("unix-non-utf8-v3", "not representable");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_extracts_case_distinct_paths() {
    let tree = extract_fixture("windows-case-collision-v3");
    let checkout = tree.path().join("checkout");
    assert_eq!(fs::read(checkout.join("Case")).unwrap(), b"upper\n");
    assert_eq!(fs::read(checkout.join("case")).unwrap(), b"lower\n");
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn case_colliding_paths_are_rejected_before_mutation() {
    assert_rejected_without_mutation("windows-case-collision-v3", "same destination path");
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn non_macos_hosts_extract_unicode_normalization_distinctions() {
    let tree = extract_fixture("macos-unicode-collision-v3");
    let checkout = tree.path().join("checkout");
    assert_eq!(fs::read(checkout.join("Résumé")).unwrap(), b"composed\n");
    assert_eq!(
        fs::read(checkout.join("Re\u{301}sume\u{301}")).unwrap(),
        b"decomposed\n"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn unicode_normalization_collisions_are_rejected_before_mutation() {
    assert_rejected_without_mutation("macos-unicode-collision-v3", "same destination path");
}
