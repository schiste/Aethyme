//! Session ids are numbered per repository and resolved against the broker of
//! the caller's directory. On 2026-10-03 `broker advanced gh --session 4 --repo
//! schiste/SP42` run from the Aethyme checkout resolved Aethyme's session 4; it
//! was refused only because that session happened to be closed. These tests
//! drive the real binary so the caller's directory is a genuine property of the
//! invocation.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_from(cwd: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn merged(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A repository whose origin identifies `schiste/Aethyme`, with one adopted
/// session worktree. Returns the session id.
fn fixture(tmp: &Path) -> (std::path::PathBuf, String) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "README.md", ".gitignore"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/schiste/Aethyme.git",
        ],
    );
    let worktree = repo.join("named-session-worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "session",
        ],
    );
    let output = run_from(&worktree, &["start", "--adopt", "--task", "repo check"]);
    let text = merged(&output);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let id = tokens
        .windows(2)
        .find(|pair| pair[0] == "session")
        .and_then(|pair| {
            pair[1]
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<i64>()
                .ok()
        })
        .unwrap_or_else(|| panic!("could not read a session id from: {text}"));
    (repo, id.to_string())
}

#[test]
fn a_repository_the_session_does_not_belong_to_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, session) = fixture(tmp.path());

    let output = run_from(
        &repo,
        &[
            "advanced",
            "git",
            "--session",
            &session,
            "--repo",
            "schiste/SP42",
            "--reason",
            "test",
            "--",
            "branch",
            "--list",
        ],
    );
    let text = merged(&output);

    assert!(!output.status.success(), "must be refused: {text}");
    assert!(
        text.contains(&format!("does not match session {session}'s repository")),
        "the refusal must name the session: {text}"
    );
    assert!(
        text.contains("origin: schiste/Aethyme"),
        "the refusal must name the session's repository: {text}"
    );
    assert!(
        text.contains("cd <that checkout>"),
        "the refusal must state the way out: {text}"
    );
}

#[test]
fn a_successful_operation_names_the_session_worktree_it_ran_in() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, session) = fixture(tmp.path());

    let output = run_from(
        &repo,
        &[
            "advanced",
            "git",
            "--session",
            &session,
            "--repo",
            "schiste/Aethyme",
            "--reason",
            "test",
            "--",
            "branch",
            "--list",
        ],
    );
    let text = merged(&output);

    assert!(output.status.success(), "must succeed: {text}");
    let line = text
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with(&format!("session {session} worktree: "))
        })
        .unwrap_or_else(|| panic!("no session worktree line: {text}"));
    assert!(
        line.trim_end().ends_with("named-session-worktree"),
        "the line must name the session worktree: {line}"
    );
    assert!(
        text.contains("on github.com/schiste/aethyme ("),
        "the line must name the resolved repository: {text}"
    );
}
