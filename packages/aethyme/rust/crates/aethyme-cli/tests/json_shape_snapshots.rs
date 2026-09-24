//! `--json` shape snapshots for the session lifecycle: `broker start`,
//! `adopt`, `submit`, `status` and `finish`, driven through the real router.
//!
//! Only the type skeleton is stored (keys and leaf types), so the snapshot
//! is stable across runs while any added, removed or retyped field shows up
//! as a diff under `tests/snapshots/json-shapes/`.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use support::snapshots::{assert_snapshots, json_shape};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn aethyme(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(cwd)
        .env_remove("AETHYME_REPO")
        .env_remove("AETHYME_AGENT")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme")
}

fn json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let output = aethyme(cwd, args);
    assert!(
        output.status.success(),
        "aethyme {args:?} failed ({:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "aethyme {args:?} did not print JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn shape(value: &serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&json_shape(value)).expect("serialize shape")
    )
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    tmp
}

#[test]
fn session_lifecycle_json_shapes_match_snapshots() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let mut entries = Vec::new();

    let started = json(&repo, &["broker", "start", "--task", "shape fixture", "--json"]);
    entries.push(("start".to_string(), shape(&started)));
    let session = started["id"]
        .as_i64()
        .or_else(|| started["session"]["id"].as_i64())
        .unwrap_or_else(|| panic!("start --json carries a session id: {started}"));
    let worktree = PathBuf::from(
        started["worktree_path"]
            .as_str()
            .or_else(|| started["session"]["worktree_path"].as_str())
            .unwrap_or_else(|| panic!("start --json carries a worktree: {started}")),
    );
    let session_arg = session.to_string();

    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "done"]);

    let submitted = json(
        &worktree,
        &["broker", "submit", "--session", &session_arg, "--json"],
    );
    entries.push(("submit".to_string(), shape(&submitted)));

    let status = json(&repo, &["broker", "status", "--json"]);
    entries.push(("status".to_string(), shape(&status)));

    let finished = json(
        &repo,
        &["broker", "finish", "--session", &session_arg, "--json"],
    );
    entries.push(("finish".to_string(), shape(&finished)));

    let adopted_path = tmp.path().join("adopted");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted",
            adopted_path.to_str().unwrap(),
        ],
    );
    let adopted = json(
        &adopted_path,
        &["broker", "adopt", "--task", "adopt fixture", "--json"],
    );
    entries.push(("adopt".to_string(), shape(&adopted)));

    assert_snapshots("json-shapes", &entries);
}
