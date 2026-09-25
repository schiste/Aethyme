//! `aethyme hook SessionStart` hands the agent its state and the one command
//! to run next (recovery plan P4.7), driven through the built router against a
//! real broker fixture.
//!
//! The rendering itself is unit-tested beside the hook; this suite holds what
//! only a real repository can: that an unregistered checkout, a fresh session
//! worktree, and a worktree carrying unintegrated commits each produce the
//! brief the agent should act on, inside one envelope line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ROUTER: &str = env!("CARGO_BIN_EXE_aethyme");

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

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let repo = repo.canonicalize().unwrap();
        Self { tmp, repo }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(ROUTER);
        command
            .current_dir(cwd)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("AETHYME_WORKTREE_ROOT", self.tmp.path().join("worktrees"))
            .env("AETHYME_HOST_CACHE_DIR", self.tmp.path().join("cache"))
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_UPDATE_CHECK", "off")
            .env_remove("AETHYME_REPO")
            .env_remove("AETHYME_AGENT")
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_SESSION_TAB_NAME")
            .env_remove("AETHYME_CHAU7_TAB_NAME");
        command
    }

    fn start(&self) -> (i64, PathBuf) {
        let output = self
            .command(&self.repo)
            .args(["broker", "start", "--task", "brief fixture", "--json"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "broker start: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = started["id"]
            .as_i64()
            .or_else(|| started["session"]["id"].as_i64())
            .expect("session id");
        let worktree = started["worktree_path"]
            .as_str()
            .or_else(|| started["session"]["worktree_path"].as_str())
            .expect("worktree path");
        (id, PathBuf::from(worktree))
    }

    fn session_start(&self, checkout: &Path) -> String {
        let mut child = self
            .command(checkout)
            .args(["hook", "SessionStart", "--repo"])
            .arg(checkout)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"{}").unwrap();
        context(&child.wait_with_output().unwrap())
    }
}

fn context(output: &Output) -> String {
    assert!(output.status.success(), "the hook must never fail");
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(text.lines().count(), 1, "one envelope line: {text}");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The brief proper: everything before any installation notice, which is
/// appended after a blank line and is covered by `hook_install_health_cli`.
fn brief(context: &str) -> &str {
    context.split("\n\n").next().unwrap_or_default()
}

fn next(context: &str) -> String {
    brief(context)
        .lines()
        .find_map(|line| line.strip_prefix("Next: "))
        .unwrap_or_else(|| panic!("no Next: line in {context}"))
        .to_string()
}

#[test]
fn an_unregistered_checkout_is_told_to_start_a_session() {
    let fixture = Fixture::new();
    let context = fixture.session_start(&fixture.repo);
    assert!(brief(&context).lines().count() <= 5, "{context}");
    assert!(context.contains("not a broker session"), "{context}");
    assert!(
        next(&context).starts_with("aethyme broker start --task"),
        "{context}"
    );
    // State, not rules: none of the old policy paragraph.
    assert!(
        !context.contains("coordinates concurrent agents"),
        "{context}"
    );
}

#[test]
fn a_session_worktree_gets_its_state_and_next_command() {
    let fixture = Fixture::new();
    let (id, worktree) = fixture.start();

    let context = fixture.session_start(&worktree);
    let brief_text = brief(&context);
    assert!(brief_text.lines().count() <= 5, "{context}");
    assert!(
        brief_text.starts_with(&format!("Aethyme: session {id}: brief fixture")),
        "{context}"
    );
    assert!(brief_text.contains("Worktree: "), "{context}");
    assert!(brief_text.contains("no blockers"), "{context}");
    assert!(
        next(&context).ends_with(&format!("aethyme broker submit --session {id}")),
        "{context}"
    );
    assert!(
        brief_text.contains("nothing committed to integrate"),
        "{context}"
    );

    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "work"]);
    let context = fixture.session_start(&worktree);
    assert!(
        brief(&context).contains("1 commit to integrate"),
        "{context}"
    );
    assert_eq!(
        next(&context),
        format!("aethyme broker submit --session {id}"),
        "{context}"
    );
}
