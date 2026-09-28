//! Public CLI certification coverage: this route is top-level, not broker-only.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_testkit::{aethyme_bin, tmp_dir};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
}

/// Resolve the `git` the shim below delegates to, skipping wrapper scripts.
///
/// A `git` that re-resolves itself through `PATH` would recurse into the
/// shim. Only a real executable terminates that chain.
fn first_real_git_on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("git"))
        .find(|candidate| {
            let Ok(mut file) = std::fs::File::open(candidate) else {
                return false;
            };
            let mut magic = [0u8; 2];
            file.read_exact(&mut magic).is_ok() && &magic != b"#!"
        })
}

#[test]
fn certify_names_a_path_git_shim_that_decorates_known_empty_output() {
    let tmp = tmp_dir();
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    init_repo(&repo);

    let real_git = first_real_git_on_path().expect("git on PATH");
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$1\" = status ]; then\n  \"$REAL_GIT\" \"$@\"\n  result=$?\n  [ \"$result\" -eq 0 ] && printf 'ok \\342\\234\\223'\n  exit \"$result\"\nfi\nexec \"$REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();

    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(aethyme_bin())
        .arg("certify")
        .current_dir(&repo)
        .env_remove("AETHYME_ROOT")
        .env_remove("AETHYME_BROKER_DB")
        .env("XDG_CONFIG_HOME", tmp.path().join("empty-config"))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REAL_GIT", real_git)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Certification routes around a git wrapper for its own probes and gate
    // commands, while still naming the wrapper because operator shells retain it.
    assert!(
        output.status.success(),
        "stdout={stdout:?}\nstderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("certify.git-output"), "{stdout}");
    assert!(stdout.contains("emitted 6 bytes"), "{stdout}");
    assert!(stdout.contains(&shim.display().to_string()), "{stdout}");
    assert!(
        stdout.contains("keeps the wrapper out of gate PATH"),
        "certify must state that gates are covered: {stdout}"
    );
    assert!(
        stdout.contains("your own shell still resolves it"),
        "and must not imply the machine is repaired: {stdout}"
    );
}
