//! `broker check-contract` on a main-branch merge commit, the way the
//! `cross-process-contract` gate runs it in `Aethyme Gates`.
//!
//! On 2026-10-04 PR #514 declared its contract decision in the PR body,
//! passed the PR check, and then failed the gate on its merge commit: the
//! gate read only commit messages. These cases run the checker against a
//! real merge commit with a fake `gh` on `PATH`, so nothing reaches GitHub.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    /// A main branch whose HEAD merges a PR that removed a tracked symbol;
    /// none of the merged commit messages declares a decision.
    fn merged_removal() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("consumers.md"), "- `tracked-entry-point`\n").unwrap();
        std::fs::write(repo.join("cli.rs"), "\"tracked-entry-point\" => run(),\n").unwrap();
        git(&repo, &["add", "consumers.md", "cli.rs"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(&repo, &["switch", "-qc", "agent/remove"]);
        std::fs::write(repo.join("cli.rs"), "\n").unwrap();
        git(&repo, &["commit", "-qam", "fix(cli): drop the entry point"]);
        git(&repo, &["switch", "-q", "main"]);
        git(
            &repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge pull request #514 from acme/agent/remove",
                "agent/remove",
            ],
        );
        Self {
            _tmp: tmp,
            repo,
            bin,
        }
    }

    /// Install a fake `gh` that runs `script`.
    fn gh(&self, script: &str) {
        let path = self.bin.join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The gate's invocation: base is the first parent, decisions come from
    /// the merged commits and, failing those, the PR GitHub associates with
    /// HEAD.
    fn check(&self) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(CLI)
            .args([
                "check-contract",
                "--base",
                "HEAD~1",
                "--commit-messages",
                "--merged-pr",
                "--consumers-doc",
                "consumers.md",
            ])
            .current_dir(&self.repo)
            .env("PATH", path)
            .output()
            .unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_decision_in_the_merged_pr_body_passes_on_main() {
    let fixture = Fixture::merged_removal();
    fixture
        .gh(r#"printf '[{"number":514,"body":"Summary\\n\\nContract decision: introduce\\n"}]'"#);
    let output = fixture.check();
    let text = text(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(
        text.contains("Contract decision in PR #514 body"),
        "the pass names its source: {text}"
    );
}

#[test]
fn no_decision_in_commits_or_pr_body_fails_on_main() {
    let fixture = Fixture::merged_removal();
    fixture.gh(r#"printf '[{"number":514,"body":"Summary\\n\\nNothing here.\\n"}]'"#);
    let output = fixture.check();
    let text = text(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("commit messages HEAD~1..HEAD: no contract decision"),
        "{text}"
    );
    assert!(text.contains("PR #514 body"), "{text}");
}

#[test]
fn an_unavailable_pr_lookup_fails_and_says_why() {
    let fixture = Fixture::merged_removal();
    fixture.gh("echo 'error connecting to api.github.com' >&2; exit 1");
    let output = fixture.check();
    let text = text(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("unavailable"), "{text}");
    assert!(
        text.contains("error connecting to api.github.com"),
        "{text}"
    );
}

#[test]
fn a_decision_in_a_commit_message_never_asks_github() {
    let fixture = Fixture::merged_removal();
    git(
        &fixture.repo,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: record",
            "-m",
            "Contract decision: soft-retire",
        ],
    );
    fixture.gh("echo 'gh must not be called' >&2; exit 1");
    let output = Command::new(CLI)
        .args([
            "check-contract",
            "--base",
            "HEAD~2",
            "--commit-messages",
            "--merged-pr",
            "--consumers-doc",
            "consumers.md",
        ])
        .current_dir(&fixture.repo)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fixture.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();
    let text = text(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(!text.contains("gh must not be called"), "{text}");
}
