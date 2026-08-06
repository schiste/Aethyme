use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("path under root")
                        .to_path_buf(),
                    fs::read(&path).expect("read snapshot file"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn certify_process_is_read_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    git(temp.path(), &["init", "-q", "-b", "main"]);
    git(temp.path(), &["config", "user.name", "Aethyme Test"]);
    git(
        temp.path(),
        &["config", "user.email", "aethyme@example.test"],
    );
    fs::write(temp.path().join("README.md"), "# Fixture\n").expect("write fixture");
    git(temp.path(), &["add", "README.md"]);
    git(temp.path(), &["commit", "-qm", "initial"]);

    let before = snapshot(temp.path());
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("certify")
        .current_dir(temp.path())
        .output()
        .expect("run aethyme certify");

    assert!(
        output.status.success(),
        "certify failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        snapshot(temp.path()),
        before,
        "certify changed repository files"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Certified (read-only — nothing written).")
    );
}
