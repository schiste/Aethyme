//! `broker git` runs coordinated commands inside the *session* worktree, so a
//! `HEAD:` refspec resolves there rather than wherever the operator stood. On
//! 2026-09-21 that published one session's commit under another branch's name
//! and reported success; the wrong content surfaced only because CI reported a
//! head SHA that did not match the local commit (#269).
//!
//! These tests drive the real binary so the caller's directory is a genuine
//! property of the invocation, which is the whole point of the guard.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn run_from(cwd: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
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

/// Repository with a session worktree, plus a separate checkout to stand in.
fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "README.md", ".gitignore"]);
    git(&repo, &["commit", "-qm", "init"]);

    // A bare remote, so a push is otherwise a legitimate command.
    let remote = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );

    let worktree = repo.join("session-worktree");
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
    // The caller stands in a *different* worktree of the same repository --
    // the exact shape of the 2026-09-21 incident, and the only shape the CLI
    // will even accept, since it resolves the repository from the cwd.
    let other = repo.join("other-worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            other.to_str().unwrap(),
            "-b",
            "other",
        ],
    );
    (tmp, repo, worktree, other)
}

fn adopt(worktree: &Path) -> String {
    // `adopt` takes the worktree from the cwd; `--path` is a lease path and
    // must be repository-relative.
    let output = run_from(worktree, &["adopt", "--task", "push guard"]);
    let text = merged(&output);
    // "Created session 574 on ..." / "Started session 575 -- ...": the id is the
    // token after the word, not a suffix of it.
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
    id.to_string()
}

#[test]
fn a_head_push_from_outside_the_session_worktree_is_refused() {
    let (_tmp, _repo, worktree, elsewhere) = fixture();
    let session = adopt(&worktree);

    let output = run_from(
        &elsewhere,
        &[
            "git",
            "--session",
            &session,
            "--reason",
            "test",
            "--",
            "push",
            "origin",
            "HEAD:refs/heads/somewhere",
        ],
    );
    let text = merged(&output);

    assert!(!output.status.success(), "the push must be refused: {text}");
    assert!(
        text.contains("worktree-relative push source"),
        "the refusal must name the cause: {text}"
    );
    assert!(
        text.contains("rev-parse HEAD"),
        "the refusal must state the way out: {text}"
    );
}

/// Branches live in the repository's shared ref store and resolve identically
/// in every worktree, so they must keep working. Refusing them would block safe
/// pushes while catching nothing.
#[test]
fn a_branch_named_push_source_is_not_refused_from_outside_the_worktree() {
    let (_tmp, _repo, worktree, elsewhere) = fixture();
    let session = adopt(&worktree);

    let output = run_from(
        &elsewhere,
        &[
            "git",
            "--session",
            &session,
            "--reason",
            "test",
            "--",
            "push",
            "origin",
            "main:refs/heads/somewhere",
        ],
    );

    assert!(
        !merged(&output).contains("worktree-relative push source"),
        "a shared branch ref must not be refused: {}",
        merged(&output)
    );
}

/// Working inside the session worktree is the ordinary case and must keep
/// working. What the push then reports is covered at library level, where a
/// push to a local remote is not subject to the CLI's `--repo owner/name`
/// requirement (`a_successful_push_reports_the_commit_it_sent`).
#[test]
fn a_head_push_from_inside_the_session_worktree_is_allowed() {
    let (_tmp, _repo, worktree, _other) = fixture();
    let session = adopt(&worktree);
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-qm", "session work"]);

    let output = run_from(
        &worktree,
        &[
            "git",
            "--session",
            &session,
            "--reason",
            "test",
            "--",
            "push",
            "origin",
            "HEAD:refs/heads/landed",
        ],
    );

    assert!(
        !merged(&output).contains("worktree-relative push source"),
        "pushing HEAD from inside the session worktree is the ordinary case: {}",
        merged(&output)
    );
}
