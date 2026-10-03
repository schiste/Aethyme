//! `broker push`: a session publishes its own branch while it works.
//!
//! Every case runs against a local bare repository reached through a fake
//! `ssh` behind a GitHub-shaped `origin`, with a fake `gh` on `PATH`, so
//! nothing reaches GitHub.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

const FAKE_SSH: &str = r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE" ;;
  *git-receive-pack*) exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE" ;;
esac
exit 64
"#;

/// Records its calls; `pr list` answers from a state file `pr create` writes.
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
state="$AETHYME_FAKE_PR_STATE"
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  if [ -f "$state" ]; then
    branch=$(cat "$state")
    printf '[{"number":7,"url":"https://github.com/acme/project/pull/7","state":"OPEN","headRefName":"%s"}]\n' "$branch"
  else
    printf '[]\n'
  fi
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  shift 2
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--head" ]; then printf '%s' "$2" > "$state"; shift 2; else shift; fi
  done
  printf 'https://github.com/acme/project/pull/7\n'
  exit 0
fi
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
            ".aethyme/*\n!.aethyme/config.toml\n",
        )
        .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", ".gitignore", "tracked.txt"]);
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
        fixture
    }

    /// Git with the fake ssh's remote in the environment.
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

    fn authorize(&self, enabled: bool) {
        std::fs::create_dir_all(self.repo.join(".aethyme")).unwrap();
        std::fs::write(
            self.repo.join(".aethyme/config.toml"),
            format!("[delivery]\npush_session_branches = {enabled}\n"),
        )
        .unwrap();
        git(&self.repo, &["add", ".aethyme/config.toml"]);
        git(&self.repo, &["commit", "-qm", "delivery policy"]);
        self.git_env(&["push", "-q", "origin", "main"]);
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(self.tmp.path().join("host-state/operations.db"))
    }

    /// A live session with one committed change. Returns (id, worktree).
    fn session_with_commit(&self) -> (i64, PathBuf) {
        let session = self.broker().start_worktree("push early", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        self.commit(&worktree, "feature.txt", "one\n", "feat: first step");
        (session.id, worktree)
    }

    fn commit(&self, worktree: &Path, file: &str, body: &str, message: &str) {
        std::fs::write(worktree.join(file), body).unwrap();
        git(worktree, &["add", file]);
        git(worktree, &["commit", "-qm", message]);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
            .current_dir(&self.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .env("AETHYME_FAKE_GH_LOG", self.tmp.path().join("gh-log"))
            .env("AETHYME_FAKE_PR_STATE", self.tmp.path().join("pr-state"))
            .output()
            .unwrap()
    }

    fn push(&self, session: i64, extra: &[&str]) -> Output {
        let id = session.to_string();
        let mut args = vec!["push", "--session", id.as_str(), "--json"];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn remote_branch(&self, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(&self.remote)
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn gh_log(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("gh-log")).unwrap_or_default()
    }
}

/// The first real pushes reported 3,687-5,171 "uncommitted files": every
/// file of one untracked build folder, counted separately. The count exists
/// to show uncommitted work, so it counts as `git status` shows it.
#[test]
fn an_untracked_folder_counts_once_and_ignored_files_not_at_all() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    fixture.commit(&worktree, ".gitignore", "ignored-build/\n", "chore: ignore");
    let build = worktree.join("target/debug");
    std::fs::create_dir_all(&build).unwrap();
    for index in 0..1_000 {
        std::fs::write(build.join(format!("object-{index}.o")), "x").unwrap();
    }
    std::fs::create_dir_all(worktree.join("ignored-build")).unwrap();
    std::fs::write(worktree.join("ignored-build/cache.bin"), "x").unwrap();
    std::fs::write(worktree.join("feature.txt"), "edited\n").unwrap();

    let report = json(&fixture.push(session, &[]));
    assert_eq!(report["uncommitted"]["modified"], 1, "{report}");
    assert_eq!(report["uncommitted"]["untracked_entries"], 1, "{report}");
    assert_eq!(report["uncommitted_files"], 2, "{report}");
    assert_eq!(
        report["uncommitted"]["sample"],
        serde_json::json!(["feature.txt", "target/"]),
        "{report}"
    );

    let id = session.to_string();
    let human = fixture.run(&["push", "--session", id.as_str()]);
    assert!(human.status.success(), "{human:?}");
    let human = String::from_utf8_lossy(&human.stdout);
    let human_line = human
        .lines()
        .find(|line| line.contains("Not pushed"))
        .unwrap_or_else(|| panic!("no uncommitted line in: {human}"));
    assert!(
        human_line.contains("1 modified, 1 untracked (feature.txt, target/)"),
        "{human_line}"
    );
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A repository that has not opted in is refused, nothing is sent, and the
/// refusal names the key that would authorize it.
#[test]
fn push_is_refused_until_the_repository_authorizes_it() {
    let fixture = Fixture::new();
    let (session, _) = fixture.session_with_commit();
    let branch = fixture.broker().store().session(session).unwrap().branch;

    let refused = fixture.push(session, &[]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("delivery.push_session_branches"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(fixture.remote_branch(&branch), None);

    // Enabling it in a worktree is not authorization: only the default branch is.
    fixture.authorize(false);
    assert!(!fixture.push(session, &[]).status.success());
    assert_eq!(fixture.remote_branch(&branch), None);
}

#[test]
fn an_authorized_push_publishes_the_session_branch_and_records_it() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    std::fs::write(worktree.join("scratch.txt"), "not committed\n").unwrap();

    let report = json(&fixture.push(session, &[]));
    let head = git_output(&worktree, &["rev-parse", "HEAD"]);
    let branch = report["branch"].as_str().unwrap().to_string();
    assert!(branch.starts_with("agent/"), "{branch}");
    assert_eq!(report["pushed_oid"], head);
    assert_eq!(report["previous_remote_oid"], serde_json::Value::Null);
    assert_eq!(report["commits_pushed"], 1);
    assert_eq!(report["uncommitted_files"], 1);
    assert_eq!(report["pr"], serde_json::Value::Null);
    assert_eq!(
        fixture.remote_branch(&branch).as_deref(),
        Some(head.as_str())
    );

    let mut broker = fixture.broker();
    let pushed = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "broker.session.pushed")
        .expect("push event");
    assert!(pushed.payload_json.unwrap().contains(&head));
    let state = broker.session_push_state(session).unwrap();
    assert_eq!(state.unpushed_commits, 0);
    assert_eq!(state.remote_oid.as_deref(), Some(head.as_str()));
}

/// Rebasing or amending rewrites the branch; the next push replaces only
/// what this session pushed last.
#[test]
fn a_rewritten_branch_is_replaced_under_a_lease_on_the_last_push() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    let first = json(&fixture.push(session, &[]));
    git(
        &worktree,
        &["commit", "--amend", "-qm", "feat: first step, reworded"],
    );

    let second = json(&fixture.push(session, &[]));
    let head = git_output(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(second["previous_remote_oid"], first["pushed_oid"]);
    assert_eq!(second["pushed_oid"], head);
    let branch = second["branch"].as_str().unwrap();
    assert_eq!(
        fixture.remote_branch(branch).as_deref(),
        Some(head.as_str())
    );
}

/// Someone else moved the remote branch after our last push. Our rewrite must
/// not clobber it.
#[test]
fn the_lease_refuses_when_the_remote_branch_moved_underneath() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    let first = json(&fixture.push(session, &[]));
    let branch = first["branch"].as_str().unwrap().to_string();

    let outsider = fixture.tmp.path().join("outsider");
    git(
        fixture.tmp.path(),
        &[
            "clone",
            "-q",
            fixture.remote.to_str().unwrap(),
            outsider.to_str().unwrap(),
        ],
    );
    git(&outsider, &["checkout", "-q", &branch]);
    fixture.commit(&outsider, "theirs.txt", "theirs\n", "someone else's work");
    git(&outsider, &["push", "-q", "origin", &branch]);
    let theirs = git_output(&outsider, &["rev-parse", "HEAD"]);

    git(&worktree, &["commit", "--amend", "-qm", "feat: rewritten"]);
    let refused = fixture.push(session, &[]);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert_eq!(
        fixture.remote_branch(&branch).as_deref(),
        Some(theirs.as_str())
    );
}

#[test]
fn a_closed_session_cannot_push() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    let branch = fixture.broker().store().session(session).unwrap().branch;
    // Under the push lane an unpushed session only closes on the record.
    let closed = fixture.run(&[
        "finish",
        "close",
        "--session",
        &session.to_string(),
        "--abandon",
        "--reason",
        "testing that a closed session cannot push",
    ]);
    assert!(closed.status.success(), "{}", stderr(&closed));

    let refused = fixture.push(session, &[]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("closed"), "{}", stderr(&refused));
    assert_eq!(fixture.remote_branch(&branch), None);
}

/// `--pr` opens one draft and afterwards reports the existing one.
#[test]
fn pr_opens_one_draft_and_then_reuses_it() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();

    let first = json(&fixture.push(session, &["--pr"]));
    assert_eq!(first["pr"]["number"], 7);
    assert_eq!(first["pr"]["created"], true);
    let log = fixture.gh_log();
    assert!(log.contains("pr create --draft"), "{log}");
    assert!(log.contains("--base main"), "{log}");
    assert!(log.contains("--title feat: first step"), "{log}");

    let second = json(&fixture.push(session, &["--pr"]));
    assert_eq!(second["pr"]["created"], false);
    assert_eq!(fixture.gh_log().matches("pr create").count(), 1);
}

/// A repository with a PR template gets that template, filled from the
/// commit's sections and carrying its contract decision; a CI that skips
/// drafts gets a note saying checks wait for `pr ready`.
#[test]
fn pr_fills_the_repository_template_and_notes_draft_skipping_ci() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    std::fs::create_dir_all(fixture.repo.join(".github/workflows")).unwrap();
    std::fs::write(
        fixture.repo.join(".github/pull_request_template.md"),
        "## Summary\n\n<!-- why -->\n\n## Contract\n\n- [ ] **none** — internal.\n\
         - [ ] **introduce** — new surface.\n\n## Test plan\n\n- [ ] tests\n",
    )
    .unwrap();
    std::fs::write(
        fixture.repo.join(".github/workflows/ci.yml"),
        "on: pull_request\njobs:\n  test:\n    if: github.event.pull_request.draft == false\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", ".github"]);
    git(&fixture.repo, &["commit", "-qm", "template"]);
    fixture.git_env(&["push", "-q", "origin", "main"]);
    let session = fixture
        .broker()
        .start_worktree("tidy the queue", None)
        .unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    fixture.commit(
        &worktree,
        "feature.txt",
        "one\n",
        "fix(queue): drop stale entries\n\nProblem: stale entries pile up.\n\n\
         Decision: drop them on read.\n\nRationale: reads already scan.\n\n\
         Validation: cargo test queue passes.\n\nContract decision: none",
    );

    let id = session.id.to_string();
    let output = fixture.run(&["push", "--session", id.as_str(), "--pr"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}{}", stderr(&output));
    assert!(stdout.contains("Opened draft pull request #7"), "{stdout}");
    assert!(stdout.contains("CI skips draft pull requests"), "{stdout}");
    let log = fixture.gh_log();
    assert!(log.contains("## Summary"), "{log}");
    assert!(log.contains("**Problem:** stale entries pile up."), "{log}");
    assert!(log.contains("- [x] **none** — internal."), "{log}");
    assert!(log.contains("Contract decision: none"), "{log}");
    assert!(log.contains("cargo test queue passes."), "{log}");
}

#[test]
fn a_pull_request_the_push_opens_gets_its_opening_recorded() {
    // PR monitoring is off by default, so a watch poll is not something to
    // rely on for the opening instant: the push that opened the pull request
    // records it, and a later push that only finds it does not move it.
    fn epoch_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();

    let before = epoch_ms();
    json(&fixture.push(session, &["--pr"]));
    let after = epoch_ms();

    let milestones = fixture.broker().store().pull_request_milestones().unwrap();
    let opened = milestones
        .iter()
        .find(|(_, number, _, _)| *number == 7)
        .and_then(|(_, _, opened_at, _)| *opened_at)
        .expect("the opened pull request has a recorded opening");
    assert!(
        (before..=after).contains(&opened),
        "{before} <= {opened} <= {after}"
    );

    json(&fixture.push(session, &["--pr"]));
    let again = fixture.broker().store().pull_request_milestones().unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(
        again[0].2,
        Some(opened),
        "finding the pull request does not re-open it"
    );
}

#[test]
fn push_state_counts_commits_no_remote_holds() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    fixture.commit(&worktree, "second.txt", "two\n", "feat: second step");

    let mut broker = fixture.broker();
    let before = broker.session_push_state(session).unwrap();
    assert_eq!(before.unpushed_commits, 2);
    assert!(before.oldest_unpushed_at_ms.is_some());
    assert_eq!(before.remote_oid, None);

    json(&fixture.push(session, &[]));
    fixture.commit(&worktree, "third.txt", "three\n", "feat: third step");
    let after = broker.session_push_state(session).unwrap();
    assert_eq!(after.unpushed_commits, 1);
    assert_eq!(
        after.head_oid,
        git_output(&worktree, &["rev-parse", "HEAD"])
    );
}
