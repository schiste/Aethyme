//! Sessions start from, and can catch up with, the current default branch.
//!
//! `start` cut sessions from the default branch *as last fetched*: in a
//! repository nobody had fetched for a while, a new worktree started behind
//! while reporting `origin/main`. And a session that drifted had only a
//! printed command to catch up with. `start` now refreshes the default branch
//! first, and `broker sync` catches a session up when that is safe.
//!
//! Every case runs against a local bare repository reached through a fake
//! `ssh` behind a GitHub-shaped `origin`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

const FAKE_SSH: &str = r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE" ;;
  *git-receive-pack*) exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE" ;;
esac
exit 64
"#;

const FAKE_GH: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then printf '[]\n'; exit 0; fi
exit 64
"#;

fn git_output(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn git(cwd: &Path, args: &[&str]) {
    git_output(cwd, args);
}

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    /// A verify-only repository with the push lane enabled, pushed to a bare
    /// remote, with `origin/HEAD` set so the default branch is tracked.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let bin = tmp.path().join("bin");
        for dir in [&repo, &remote, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (name, body) in [("ssh", FAKE_SSH), ("gh", FAKE_GH)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join(".gitignore"),
            ".aethyme/*\n!.aethyme/config.toml\nignored/\n",
        )
        .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(repo.join("other.txt"), "other\n").unwrap();
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/config.toml"),
            "[promote]\nmode = \"verify-only\"\n\n[delivery]\npush_session_branches = true\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/project.git"],
        );
        git(
            &repo,
            &[
                "config",
                "core.sshCommand",
                bin.join("ssh").to_str().unwrap(),
            ],
        );
        let fixture = Self {
            tmp,
            repo,
            remote,
            bin,
        };
        fixture.git_env(&["push", "-qu", "origin", "main"]);
        git(
            &fixture.repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        fixture
    }

    fn git_env(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Another clone lands a commit on the remote's default branch. This
    /// repository's `origin/main` stays at the last fetch: the stale-cache
    /// case `start` must not inherit.
    fn land_on_remote_only(&self, file: &str, body: &str) -> String {
        let other = self.tmp.path().join(format!("other-{file}-{}", body.len()));
        git(
            self.tmp.path(),
            &[
                "clone",
                "-q",
                self.remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        std::fs::write(other.join(file), body).unwrap();
        git(&other, &["add", file]);
        git(&other, &["commit", "-qm", "merged elsewhere"]);
        git(&other, &["push", "-q", "origin", "main"]);
        git_output(&other, &["rev-parse", "HEAD"])
    }

    fn run_in(&self, cwd: &Path, args: &[&str], remote: &Path) -> Output {
        Command::new(CLI)
            .args(args)
            .current_dir(cwd)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_TEST_GIT_REMOTE", remote)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo, args, &self.remote)
    }

    fn unreachable(&self) -> PathBuf {
        self.tmp.path().join("no-such-remote.git")
    }

    fn start(&self, task: &str) -> serde_json::Value {
        json(&self.run(&["start", "--task", task, "--short-name", task, "--json"]))
    }

    /// A session with one committed change to `file`.
    fn session_changing(&self, file: &str, body: &str) -> (i64, PathBuf) {
        let started = self.start("work");
        let id = started["id"].as_i64().unwrap();
        let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
        std::fs::write(worktree.join(file), body).unwrap();
        git(&worktree, &["add", file]);
        git(&worktree, &["commit", "-qm", "feat: session change"]);
        (id, worktree)
    }

    fn sync(&self, session: i64) -> Output {
        let id = session.to_string();
        self.run(&["sync", "--session", &id, "--json"])
    }
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

fn head(worktree: &Path) -> String {
    git_output(worktree, &["rev-parse", "HEAD"])
}

#[test]
fn start_begins_from_a_commit_pushed_after_the_last_fetch() {
    let fixture = Fixture::new();
    let cached = git_output(&fixture.repo, &["rev-parse", "origin/main"]);
    let landed = fixture.land_on_remote_only("other.txt", "moved on\n");

    let started = fixture.start("fresh");

    let base = &started["start_base"];
    assert_eq!(base["fetched"], true, "{started}");
    assert_ne!(base["commit"], cached, "{started}");
    assert_eq!(base["commit"], landed, "{started}");
    assert_eq!(base["behind_default_commits"], 0, "{started}");
    assert_eq!(
        started["adopted_head"], landed,
        "the session is anchored at the fresh tip"
    );
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert_eq!(head(&worktree), landed, "the worktree itself moved");
}

#[test]
fn an_offline_start_uses_the_cached_copy_and_says_how_old_it_is() {
    let fixture = Fixture::new();
    let cached = git_output(&fixture.repo, &["rev-parse", "origin/main"]);
    fixture.land_on_remote_only("other.txt", "moved on\n");

    let started = json(&fixture.run_in(
        &fixture.repo,
        &[
            "start",
            "--task",
            "offline",
            "--short-name",
            "offline",
            "--json",
        ],
        &fixture.unreachable(),
    ));

    let base = &started["start_base"];
    assert_eq!(base["fetched"], false, "{started}");
    assert!(base["fetch_error"].is_string(), "{started}");
    assert!(base["cached_ref_age_seconds"].is_u64(), "{started}");
    assert_eq!(base["commit"], cached, "{started}");
}

#[test]
fn reusing_a_session_reports_how_far_its_worktree_drifted() {
    let fixture = Fixture::new();
    let (_, worktree) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_remote_only("tracked.txt", "upstream\n");

    let reused = json(&fixture.run_in(
        &worktree,
        &["start", "--adopt", "--reuse", "--task", "again", "--json"],
        &fixture.remote,
    ));

    let drift = &reused["default_branch"];
    assert_eq!(drift["behind"], 1, "{reused}");
    assert_eq!(drift["would_conflict"], true, "{reused}");
    assert_eq!(
        drift["conflicting_paths"],
        serde_json::json!(["tracked.txt"]),
        "{reused}"
    );
}

#[test]
fn sync_rebases_an_unpublished_clean_branch_onto_the_default_branch() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    let landed = fixture.land_on_remote_only("other.txt", "moved on\n");

    let report = json(&fixture.sync(session));

    assert_eq!(report["outcome"], "synced", "{report}");
    assert_eq!(report["strategy"], "rebase", "{report}");
    assert_eq!(report["behind_before"], 1, "{report}");
    assert_eq!(
        git_output(&worktree, &["rev-parse", "HEAD~1"]),
        landed,
        "the session commit now sits on the fetched tip"
    );
    assert_eq!(
        git_output(&worktree, &["rev-list", "--merges", "--count", "HEAD"]),
        "0",
        "a rebase leaves no merge commit"
    );
}

#[test]
fn sync_merges_into_a_published_branch_without_rewriting_it() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    let published = head(&worktree);
    json(&fixture.run(&["push", "--session", &session.to_string(), "--json"]));
    let landed = fixture.land_on_remote_only("other.txt", "moved on\n");

    let report = json(&fixture.sync(session));

    assert_eq!(report["outcome"], "synced", "{report}");
    assert_eq!(report["strategy"], "merge", "{report}");
    let after = head(&worktree);
    assert_eq!(
        git_output(&worktree, &["rev-parse", "HEAD^1"]),
        published,
        "the published commit is kept as the first parent"
    );
    assert_eq!(git_output(&worktree, &["rev-parse", "HEAD^2"]), landed);
    assert_eq!(report["after"], after, "{report}");
}

#[test]
fn sync_refuses_a_dirty_tree_and_changes_nothing() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_remote_only("other.txt", "moved on\n");
    let before = head(&worktree);
    std::fs::write(worktree.join("tracked.txt"), "uncommitted\n").unwrap();

    let output = fixture.sync(session);

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("modified"),
        "{output:?}"
    );
    assert_eq!(head(&worktree), before);
}

#[test]
fn sync_ignores_ignored_files_when_judging_the_tree_clean() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_remote_only("other.txt", "moved on\n");
    std::fs::create_dir_all(worktree.join("ignored")).unwrap();
    std::fs::write(worktree.join("ignored/build.out"), "x\n").unwrap();

    let report = json(&fixture.sync(session));

    assert_eq!(report["outcome"], "synced", "{report}");
}

#[test]
fn sync_on_a_conflict_changes_nothing_and_lists_the_paths() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    fixture.land_on_remote_only("tracked.txt", "upstream\n");
    let before = head(&worktree);

    let output = fixture.sync(session);

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"], "conflict", "{report}");
    assert_eq!(report["strategy"], "none", "{report}");
    assert_eq!(report["conflicts"], serde_json::json!(["tracked.txt"]));
    assert_eq!(head(&worktree), before, "nothing was changed");
    assert!(
        git_output(&worktree, &["status", "--porcelain"]).is_empty(),
        "no merge or rebase was left behind"
    );
}

#[test]
fn sync_refuses_while_a_rebase_is_in_progress() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_changing("tracked.txt", "session\n");
    // Pause a rebase on a conflict inside the session worktree.
    let landed = fixture.land_on_remote_only("tracked.txt", "upstream\n");
    git(
        &fixture.repo,
        &["fetch", "-q", fixture.remote.to_str().unwrap(), "main"],
    );
    let paused = Command::new("git")
        .args(["rebase", &landed])
        .current_dir(&worktree)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        !paused.status.success(),
        "the rebase must stop on a conflict"
    );

    let output = fixture.sync(session);

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rebase is already in progress"),
        "{output:?}"
    );
}
