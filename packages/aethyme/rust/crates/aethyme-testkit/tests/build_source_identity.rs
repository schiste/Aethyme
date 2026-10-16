//! Real Cargo builds of tiny synthetic packages using the production scripts.
//! No product binary or real-repository mutation is needed for this regression.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn checked(command: &mut Command) -> String {
    let output: Output = command.output().expect("spawn fixture command");
    assert!(
        output.status.success(),
        "command {command:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("fixture command UTF-8")
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath="])
        .args(args);
    checked(&mut command).trim().to_owned()
}

fn run_probe(manifest: &Path, target: &Path, common_cwd: &Path) -> String {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(common_cwd)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("RUSTC_WRAPPER", "")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(["run", "--offline", "--locked", "--quiet", "-j", "1"])
        .arg("--manifest-path")
        .arg(manifest);
    checked(&mut command).trim().to_owned()
}

#[test]
fn shared_cache_tracks_each_worktrees_source_identity() {
    let scripts = [
        (
            "cli",
            include_str!("../../aethyme-cli/build.rs"),
            "AETHYME_GIT_COMMIT",
        ),
        (
            "engine",
            include_str!("../../aethyme-engine/build.rs"),
            "AETHYME_GIT_COMMIT",
        ),
        (
            "broker",
            include_str!("../../aethyme-broker/build.rs"),
            "AETHYME_BROKER_GIT_COMMIT",
        ),
    ];
    // Serial nested Cargo invocations have an independent tiny target and no
    // dependencies. A common cwd also prevents PWD from hiding the regression.
    for (name, script, variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("newer");
        let older = temp.path().join("older");
        let target = temp.path().join("shared-target");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(
            repo.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(repo.join("build.rs"), script).unwrap();
        fs::write(
            repo.join("src/main.rs"),
            format!("fn main() {{ println!(\"{{}}\", env!(\"{variable}\")); }}\n"),
        )
        .unwrap();
        git(&repo, &["init", "--initial-branch=probe"]);
        git(&repo, &["config", "user.name", "Build identity fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(
            &repo,
            &["add", "Cargo.toml", "Cargo.lock", "build.rs", "src/main.rs"],
        );
        git(&repo, &["commit", "-m", "initial source"]);
        let first = git(&repo, &["rev-parse", "HEAD"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                older.to_str().unwrap(),
                &first,
            ],
        );
        fs::write(repo.join("revision-marker"), "new source\n").unwrap();
        git(&repo, &["add", "revision-marker"]);
        git(&repo, &["commit", "-m", "advance newer source"]);
        let second = git(&repo, &["rev-parse", "HEAD"]);
        assert_ne!(first, second);
        // All existing Git watches must exist. Otherwise an absent packed-refs
        // input can itself rerun the old script and conceal the cache bug.
        git(&repo, &["pack-refs", "--all", "--no-prune"]);
        for (tree, expected) in [(&repo, &second), (&older, &first), (&repo, &second)] {
            assert_eq!(
                run_probe(&tree.join("Cargo.toml"), &target, temp.path()),
                *expected,
                "{name} script reported another worktree's source at {}",
                tree.display()
            );
        }
        // A same-checkout warm build also preserves the exact identity.
        let warm_start = std::time::Instant::now();
        assert_eq!(
            run_probe(&repo.join("Cargo.toml"), &target, temp.path()),
            second
        );
        eprintln!("{name} warm tiny-probe build: {:?}", warm_start.elapsed());
        // A moving branch must still invalidate a stationary symbolic HEAD.
        fs::write(repo.join("revision-marker"), "third source\n").unwrap();
        git(&repo, &["add", "revision-marker"]);
        git(&repo, &["commit", "-m", "advance stationary branch"]);
        let third = git(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(
            run_probe(&repo.join("Cargo.toml"), &target, temp.path()),
            third,
            "{name} script missed a branch advance"
        );
    }
}
