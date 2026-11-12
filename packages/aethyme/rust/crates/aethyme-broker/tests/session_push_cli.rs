//! `broker push`: a session publishes its own branch while it works.
//!
//! Every case runs against a local bare repository reached through a fake
//! `ssh` behind a GitHub-shaped `origin`, with a fake `gh` on `PATH`, so
//! nothing reaches GitHub.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

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
    branch=$(sed -n '1p' "$state")
    draft=$(sed -n '2p' "$state")
    printf '[{"number":7,"url":"https://github.com/acme/project/pull/7","state":"OPEN","headRefName":"%s","isDraft":%s}]\n' "$branch" "$draft"
  else
    printf '[]\n'
  fi
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  if [ -f "${state}.fail-watch" ]; then
    printf 'synthetic observation failure\n' >&2
    exit 1
  fi
  if [ -f "${state}.slow-watch" ]; then
    exec sleep 10
  fi
  branch=$(sed -n '1p' "$state")
  draft=$(sed -n '2p' "$state")
  sha=$(git rev-parse "refs/heads/$branch") || exit 65
  printf '{"number":7,"url":"https://github.com/acme/project/pull/7","state":"OPEN","baseRefName":"main","headRefName":"%s","headRefOid":"%s","isDraft":%s,"comments":[],"reviews":[],"statusCheckRollup":[]}\n' "$branch" "$sha" "$draft"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  shift 2
  branch=
  draft=false
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --head) branch="$2"; shift 2 ;;
      --draft) draft=true; shift ;;
      *) shift ;;
    esac
  done
  printf '%s\n%s\n' "$branch" "$draft" > "$state"
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

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    storage: common::FixtureRepository,
    repo: PathBuf,
    remote: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp_repo = tempfile::tempdir().unwrap();
        let storage = common::FixtureRepository::from_tempdir(temp_repo);
        let repo = storage.path().to_path_buf();
        let remote = storage.owner_path().join("remote.git");
        let bin = storage.owner_path().join("bin");
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
        let ssh = bin.join("ssh");
        let quoted_ssh = ssh.to_str().unwrap().replace('\'', "'\\''");
        let ssh_command = format!("sh '{quoted_ssh}'");
        git(&repo, &["config", "core.sshCommand", &ssh_command]);
        let fixture = Self {
            storage,
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

    fn set_config_raw(&self, text: &str) {
        std::fs::create_dir_all(self.repo.join(".aethyme")).unwrap();
        std::fs::write(self.repo.join(".aethyme/config.toml"), text).unwrap();
        git(&self.repo, &["add", ".aethyme/config.toml"]);
        git(&self.repo, &["commit", "-qm", "set PR policy"]);
        self.git_env(&["push", "-q", "origin", "main"]);
        self.git_env(&["fetch", "-q", "origin"]);
    }

    fn set_pr_open_as_raw(&self, value: &str) {
        let existing = std::fs::read_to_string(self.repo.join(".aethyme/config.toml")).unwrap();
        self.set_config_raw(&format!(
            "{}\n[broker.pr]\nopen_as = {value}\n",
            existing.trim_end()
        ));
    }

    fn broker(&self) -> common::FixtureBroker {
        common::open_fixture_repository_broker(&self.storage).unwrap()
    }

    /// A live session with one committed change. Returns (id, worktree).
    fn session_with_commit(&self) -> (i64, PathBuf) {
        let session = self.broker().start_worktree("push early", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        self.commit(&worktree, "feature.txt", "one\n", "feat: first step");
        (session.id, worktree)
    }

    fn install_relative_pre_push_hook(&self, checkout: &Path, contents: &str) {
        let hooks = checkout.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        write_executable(&hooks.join("pre-push"), contents);
    }

    fn select_relative_hooks_path(&self) {
        git(&self.repo, &["config", "core.hooksPath", "hooks"]);
    }

    fn enable_hooks_outside_lock(&self) {
        let config = self.repo.join(".aethyme/config.toml");
        let existing = std::fs::read_to_string(&config).unwrap();
        assert!(!existing.contains("[coordination]"));
        std::fs::write(
            &config,
            format!(
                "{}\n[coordination]\nhooks_outside_lock = true\n",
                existing.trim_end()
            ),
        )
        .unwrap();
        git(&self.repo, &["add", ".aethyme/config.toml"]);
        git(
            &self.repo,
            &["commit", "-qm", "enable outside-lock hook check"],
        );
        self.git_env(&["push", "-q", "origin", "main"]);
    }

    fn advanced_push(&self, session: i64, worktree: &Path) -> Output {
        let session_id = session.to_string();
        let branch = self.broker().store().session(session).unwrap().branch;
        let head = git_output(worktree, &["rev-parse", "HEAD"]);
        let refspec = format!("{head}:refs/heads/{branch}");
        self.run(&[
            "advanced",
            "git",
            "--session",
            &session_id,
            "--repo",
            "acme/project",
            "--effect",
            "write",
            "--reason",
            "verify pre-push hook behavior",
            "--",
            "push",
            "origin",
            &refspec,
        ])
    }

    fn commit(&self, worktree: &Path, file: &str, body: &str, message: &str) {
        std::fs::write(worktree.join(file), body).unwrap();
        git(worktree, &["add", file]);
        git(worktree, &["commit", "-qm", message]);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let broker = self.broker();
        let mut command = common::broker_cli(CLI, args);
        command.envs(env.iter().copied());
        command
            .current_dir(&self.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .env(
                "AETHYME_TEST_HOOK_LOG",
                self.storage.owner_path().join("hook-log"),
            )
            .env(
                "AETHYME_FAKE_GH_LOG",
                self.storage.owner_path().join("gh-log"),
            )
            .env(
                "AETHYME_FAKE_PR_STATE",
                self.storage.owner_path().join("pr-state"),
            );
        broker
            .configure_storage_context_for_child(&mut command)
            .unwrap();
        drop(broker);
        command.output().unwrap()
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
        std::fs::read_to_string(self.storage.owner_path().join("gh-log")).unwrap_or_default()
    }

    /// Another clone lands a commit on the remote's default branch. This
    /// repository's `origin/main` moves only when something fetches it, so it
    /// shows whether a push fetched.
    fn land_on_remote_only(&self, name: &str) -> String {
        let other = self.storage.owner_path().join(format!("other-{name}"));
        git(
            self.storage.owner_path(),
            &[
                "clone",
                "-q",
                self.remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        std::fs::write(other.join(format!("{name}.txt")), "landed\n").unwrap();
        git(&other, &["add", "."]);
        git(&other, &["commit", "-qm", "merged elsewhere"]);
        git(&other, &["push", "-q", "origin", "main"]);
        git_output(&other, &["rev-parse", "HEAD"])
    }

    fn tracking_main(&self) -> String {
        git_output(&self.repo, &["rev-parse", "refs/remotes/origin/main"])
    }

    fn default_branch_fetch_record(&self) -> Option<String> {
        self.broker()
            .store()
            .meta_get(DEFAULT_BRANCH_FETCH_KEY)
            .unwrap()
    }

    fn record_default_branch_fetch(&self, value: &str) {
        self.broker()
            .store()
            .meta_set(DEFAULT_BRANCH_FETCH_KEY, value)
            .unwrap();
    }

    /// Journaled fetches of the default branch into `origin/main`.
    fn default_branch_fetches(&self) -> usize {
        self.broker()
            .store()
            .coordinated_operations()
            .unwrap()
            .into_iter()
            .filter(|operation| {
                operation.command_json.contains("\"fetch\"")
                    && operation.command_json.contains("refs/remotes/origin/main")
            })
            .count()
    }

    /// The push plan unknown-outcome recovery reads, from the last journaled
    /// session-branch push.
    fn last_push_plan(&self) -> serde_json::Value {
        let operation = self
            .broker()
            .store()
            .coordinated_operations()
            .unwrap()
            .into_iter()
            .rev()
            .find(|operation| operation.scope.starts_with("session-push:"))
            .expect("a session push is journaled");
        let details: serde_json::Value =
            serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
        details["push_reconciliation"].clone()
    }
}

/// Where the broker records its last default-branch fetch for `origin/main`.
const DEFAULT_BRANCH_FETCH_KEY: &str =
    "default_branch_fetch.refs/remotes/origin/main.started_at_ms";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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
        json_error::message(&refused).contains("delivery.push_session_branches"),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(fixture.remote_branch(&branch), None);

    // Enabling it in a worktree is not authorization: only the default branch is.
    fixture.authorize(false);
    let refused_again = fixture.push(session, &[]);
    assert!(!refused_again.status.success());
    assert!(json_error::message(&refused_again).contains("delivery.push_session_branches"));
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

/// A session push names one exact branch through a configured remote, so it
/// holds only that branch's lane and still updates the branch's tracking ref
/// (which the unpushed-commit count reads). A configured `core.sshCommand`
/// could reroute the transport, so the fixture's other tests keep the
/// repository-wide spelling; here the fake `ssh` on `PATH` reaches the remote.
#[test]
fn a_session_push_holds_only_its_branch_lane_when_the_profile_is_proven() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    // Only the CLI run below puts the fake `ssh` on `PATH`.
    git(&fixture.repo, &["config", "--unset", "core.sshCommand"]);

    let report = json(&fixture.push(session, &[]));
    let head = git_output(&worktree, &["rev-parse", "HEAD"]);
    let branch = report["branch"].as_str().unwrap().to_string();
    assert_eq!(report["pushed_oid"], head);
    assert_eq!(
        fixture.remote_branch(&branch).as_deref(),
        Some(head.as_str())
    );

    let mut broker = fixture.broker();
    let state = broker.session_push_state(session).unwrap();
    assert_eq!(state.unpushed_commits, 0);
    assert_eq!(state.remote_oid.as_deref(), Some(head.as_str()));
    let operation = broker
        .store()
        .coordinated_operations()
        .unwrap()
        .into_iter()
        .rev()
        .find(|operation| operation.scope.starts_with("session-push:refs/heads/"))
        .expect("a session push is journaled");
    let command: Vec<String> = serde_json::from_str(&operation.command_json).unwrap();
    let refspec = format!("{head}:refs/heads/{branch}");
    assert_eq!(
        command,
        [
            "git",
            "push",
            "--no-follow-tags",
            "--porcelain",
            "--",
            "origin",
            refspec.as_str(),
        ]
    );
    let details: serde_json::Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    let lock_key = details["coordination_timing"]["lock_key"].as_str().unwrap();
    assert!(
        lock_key.ends_with(&format!("::git-ref/refs/heads/{branch}")),
        "{lock_key}"
    );
}

/// The strict spelling typed explicitly, in a repository whose configuration
/// cannot prove its footprint (here `core.sshCommand`), runs on the
/// repository-wide lane as it did before narrow lanes existed, instead of
/// being refused.
#[test]
fn an_explicit_strict_push_in_an_unproven_repository_runs_wide() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    let branch = fixture.broker().store().session(session).unwrap().branch;
    let head = git_output(&worktree, &["rev-parse", "HEAD"]);
    let refspec = format!("{head}:refs/heads/{branch}");
    let id = session.to_string();
    let output = fixture.run(&[
        "advanced",
        "git",
        "--session",
        &id,
        "--repo",
        "acme/project",
        "--effect",
        "write",
        "--reason",
        "explicit strict spelling",
        "--",
        "push",
        "--no-follow-tags",
        "--porcelain",
        "--",
        "origin",
        &refspec,
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fixture.remote_branch(&branch).as_deref(),
        Some(head.as_str())
    );
    let operation = fixture
        .broker()
        .store()
        .coordinated_operations()
        .unwrap()
        .into_iter()
        .rev()
        .find(|operation| operation.command_json.contains("--porcelain"))
        .expect("the explicit push is journaled");
    assert_eq!(operation.scope, "repository");
    let details: serde_json::Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        details["coordination_timing"]["lock_key"].as_str(),
        Some(operation.repository.as_str())
    );
}

/// The strict spelling passes `--no-follow-tags`. A repository that follows
/// tags on push keeps the historical spelling, so `broker push` does not
/// silently stop publishing the tags Git would have followed.
#[test]
fn a_repository_that_follows_tags_keeps_the_historical_session_push() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    git(&fixture.repo, &["config", "--unset", "core.sshCommand"]);
    git(&fixture.repo, &["config", "push.followTags", "true"]);

    let report = json(&fixture.push(session, &[]));
    let head = git_output(&worktree, &["rev-parse", "HEAD"]);
    let branch = report["branch"].as_str().unwrap().to_string();
    assert_eq!(report["pushed_oid"], head);
    let operation = fixture
        .broker()
        .store()
        .coordinated_operations()
        .unwrap()
        .into_iter()
        .rev()
        .find(|operation| operation.scope.starts_with("session-push:refs/heads/"))
        .expect("a session push is journaled");
    let command: Vec<String> = serde_json::from_str(&operation.command_json).unwrap();
    let refspec = format!("{head}:refs/heads/{branch}");
    assert_eq!(command, ["git", "push", "origin", refspec.as_str()]);
}

/// An ambient `GIT_INDEX_FILE` could point the commit at another index, so the
/// broker cannot prove the commit is confined to the session branch and keeps
/// the repository-wide lane. The same commit without it narrows.
#[test]
fn a_commit_under_an_ambient_index_override_keeps_the_repository_lane() {
    let fixture = Fixture::new();
    let (session, worktree) = fixture.session_with_commit();
    // The broker's own `git commit` needs an identity of its own.
    git(&fixture.repo, &["config", "user.name", "t"]);
    git(&fixture.repo, &["config", "user.email", "t@t"]);
    let branch = fixture.broker().store().session(session).unwrap().branch;
    let id = session.to_string();
    let index = format!(
        "{}/index",
        git_output(&worktree, &["rev-parse", "--absolute-git-dir"])
    );
    let commit = |file: &str, env: &[(&str, &str)]| {
        std::fs::write(worktree.join(file), "x\n").unwrap();
        git(&worktree, &["add", file]);
        let output = fixture.run_with_env(
            &[
                "advanced",
                "git",
                "--session",
                &id,
                "--reason",
                "index override",
                "--",
                "commit",
                "-qm",
                file,
            ],
            env,
        );
        assert!(output.status.success(), "{}", stderr(&output));
    };
    commit("plain.txt", &[]);
    commit("override.txt", &[("GIT_INDEX_FILE", index.as_str())]);
    let scopes: Vec<String> = fixture
        .broker()
        .store()
        .coordinated_operations()
        .unwrap()
        .into_iter()
        .filter(|operation| operation.command_json.contains("\"commit\""))
        .map(|operation| operation.scope)
        .collect();
    assert_eq!(
        scopes,
        [
            format!("session-commit:refs/heads/{branch}"),
            "repository".to_string()
        ]
    );
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

    let outsider = fixture.storage.owner_path().join("outsider");
    git(
        fixture.storage.owner_path(),
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
    assert!(
        json_error::message(&refused).contains("closed"),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(fixture.remote_branch(&branch), None);
}

/// `--pr` defaults to draft, and later policy changes leave an existing PR unchanged.
#[test]
fn pr_opens_one_draft_and_then_reuses_it_unchanged() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();

    let first = json(&fixture.push(session, &["--pr"]));
    assert_eq!(first["pr"]["number"], 7);
    assert_eq!(first["pr"]["created"], true);
    assert_eq!(first["pr"]["draft"], true);
    let log = fixture.gh_log();
    assert!(log.contains("pr create --draft"), "{log}");
    assert!(log.contains("--base main"), "{log}");
    assert!(log.contains("--title feat: first step"), "{log}");

    fixture.set_pr_open_as_raw("\"ready\"");

    let second = json(&fixture.push(session, &["--pr"]));
    assert_eq!(second["pr"]["created"], false);
    assert_eq!(second["pr"]["draft"], true);
    assert_eq!(fixture.gh_log().matches("pr create").count(), 1);
}

#[test]
fn pr_opens_ready_when_trusted_default_branch_requests_it() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    fixture.set_pr_open_as_raw("\"ready\"");
    let (session, worktree) = fixture.session_with_commit();
    std::fs::write(
        worktree.join(".aethyme/config.toml"),
        "[delivery]\npush_session_branches = true\n[broker.pr]\nopen_as = \"draft\"\n",
    )
    .unwrap();

    let id = session.to_string();
    let output = fixture.run(&["push", "--session", &id, "--pr"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Opened ready pull request #7"), "{stdout}");
    let log = fixture.gh_log();
    assert!(log.contains("pr create --head"), "{log}");
    assert!(!log.contains("pr create --draft"), "{log}");
}

#[test]
fn malformed_pr_policy_refuses_before_pushing_the_session_branch() {
    for config in [
        "broker = false\n[delivery]\npush_session_branches = true\n",
        "[delivery]\npush_session_branches = true\n[broker.pr]\nother = true\n",
        "[delivery]\npush_session_branches = true\n[broker.pr]\nopen_as = true\n",
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = \"true\"\n",
    ] {
        let fixture = Fixture::new();
        fixture.set_config_raw(config);
        let (session, _) = fixture.session_with_commit();
        let branch = fixture.broker().store().session(session).unwrap().branch;

        let refused = fixture.push(session, &["--pr"]);
        assert!(!refused.status.success(), "accepted config: {config}");
        assert_eq!(fixture.remote_branch(&branch), None, "config: {config}");
        assert!(fixture.gh_log().is_empty(), "config: {config}");
    }
}

#[test]
fn invalid_pr_open_as_refuses_before_pushing_the_session_branch() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    fixture.set_pr_open_as_raw("\"soon\"");
    let (session, _) = fixture.session_with_commit();
    let branch = fixture.broker().store().session(session).unwrap().branch;

    let refused = fixture.push(session, &["--pr"]);
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains("open_as must be \"draft\" or \"ready\""),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(fixture.remote_branch(&branch), None);
    assert!(fixture.gh_log().is_empty());
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

#[test]
fn broker_and_advanced_git_push_use_session_hooks_and_caller_path() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    write_executable(
        &fixture.bin.join("hook-only-tool"),
        "#!/bin/sh\nprintf 'session hook ran\\n' >> \"$AETHYME_TEST_HOOK_LOG\"\n",
    );
    fixture.install_relative_pre_push_hook(
        &fixture.repo,
        "#!/bin/sh\nprintf 'main checkout hook selected\\n' >&2\nexit 1\n",
    );
    fixture.install_relative_pre_push_hook(&worktree, "#!/bin/sh\nhook-only-tool\n");
    fixture.select_relative_hooks_path();

    let broker_push = fixture.push(session, &[]);
    assert!(
        broker_push.status.success(),
        "broker push should use the session checkout and caller PATH: {}",
        stderr(&broker_push)
    );
    let hook_log = fixture.storage.owner_path().join("hook-log");
    let log = std::fs::read_to_string(&hook_log).unwrap();
    assert_eq!(log.lines().collect::<Vec<_>>(), ["session hook ran"]);

    fixture.commit(&worktree, "second.txt", "two\n", "feat: second step");
    let advanced = fixture.advanced_push(session, &worktree);
    assert!(
        advanced.status.success(),
        "advanced git push should use the same session hook and caller PATH: stdout={}; stderr={}",
        String::from_utf8_lossy(&advanced.stdout),
        stderr(&advanced)
    );
    let log = std::fs::read_to_string(hook_log).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["session hook ran", "session hook ran"]
    );
}

#[test]
fn both_push_paths_surface_hook_streams_and_missing_path_guidance() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    fixture.install_relative_pre_push_hook(&fixture.repo, "#!/bin/sh\nexit 0\n");
    fixture.install_relative_pre_push_hook(
        &worktree,
        "#!/bin/sh\nprintf 'hook stdout: checks could not start\\n'\nprintf 'env: node: No such file or directory\\n' >&2\nexit 1\n",
    );
    fixture.select_relative_hooks_path();

    let broker_push = fixture.push(session, &[]);
    assert!(!broker_push.status.success());
    let push_error = json_error::message(&broker_push);
    assert!(
        push_error.contains("hook stdout: checks could not start"),
        "{push_error}"
    );
    assert!(
        push_error.contains("env: node: No such file or directory"),
        "{push_error}"
    );
    assert!(
        push_error.contains("Hint: the pre-push hook could not find a required executable"),
        "{push_error}"
    );

    let advanced = fixture.advanced_push(session, &worktree);
    assert!(!advanced.status.success());
    let advanced_output = format!(
        "{}{}",
        String::from_utf8_lossy(&advanced.stdout),
        stderr(&advanced)
    );
    assert!(
        advanced_output.contains("hook stdout: checks could not start"),
        "{advanced_output}"
    );
    assert!(
        advanced_output.contains("env: node: No such file or directory"),
        "{advanced_output}"
    );
    assert!(
        advanced_output.contains("Hint: the pre-push hook could not find a required executable"),
        "{advanced_output}"
    );
    let branch = fixture.broker().store().session(session).unwrap().branch;
    assert_eq!(fixture.remote_branch(&branch), None);
}

#[test]
fn opted_in_pre_push_refusals_show_hook_stderr_and_path_guidance_on_both_pushes() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    fixture.install_relative_pre_push_hook(&fixture.repo, "#!/bin/sh\nexit 0\n");
    fixture.install_relative_pre_push_hook(
        &worktree,
        "#!/bin/sh\nprintf 'env: node: No such file or directory\\n' >&2\nexit 1\n",
    );
    fixture.select_relative_hooks_path();
    fixture.enable_hooks_outside_lock();

    let broker_push = fixture.push(session, &[]);
    assert!(!broker_push.status.success());
    let push_error = json_error::message(&broker_push);
    assert!(
        push_error.contains("env: node: No such file or directory"),
        "{push_error}"
    );
    assert!(
        push_error.contains("Hint: the pre-push hook could not find a required executable"),
        "{push_error}"
    );

    let advanced = fixture.advanced_push(session, &worktree);
    assert!(!advanced.status.success());
    let advanced_error = stderr(&advanced);
    assert!(
        advanced_error.contains("env: node: No such file or directory"),
        "{advanced_error}"
    );
    assert!(
        advanced_error.contains("Hint: the pre-push hook could not find a required executable"),
        "{advanced_error}"
    );
    let branch = fixture.broker().store().session(session).unwrap().branch;
    assert_eq!(fixture.remote_branch(&branch), None);
}

#[path = "common/json_error.rs"]
mod json_error;

#[test]
fn trusted_pr_watch_is_automatic_idempotent_and_preserves_pause() {
    let fixture = Fixture::new();
    fixture.set_config_raw(
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = true\n",
    );
    let (session, worktree) = fixture.session_with_commit();
    // Session edits cannot disable the committed repository default.
    std::fs::write(
        worktree.join(".aethyme/config.toml"),
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = false\n",
    )
    .unwrap();
    let first = json(&fixture.push(session, &["--pr"]));
    let id = first["pr_watch_id"].as_i64().expect("automatic watch");
    let mut broker = fixture.broker();
    let watch = broker.pull_request_watch(id).unwrap();
    assert_eq!(watch.session_id, session);
    assert_eq!(watch.pr_number, 7);
    assert_eq!(watch.head_sha, first["pushed_oid"].as_str().unwrap());
    let digest = watch.cursor_digest.clone();
    broker
        .set_pull_request_watch_status(id, aethyme_broker::PullRequestWatchStatus::Paused, 1_000)
        .unwrap();
    drop(broker);

    let second = json(&fixture.push(session, &["--pr"]));
    assert_eq!(second["pr_watch_id"], id);
    let broker = fixture.broker();
    let watch = broker.pull_request_watch(id).unwrap();
    assert_eq!(watch.status, aethyme_broker::PullRequestWatchStatus::Paused);
    assert_eq!(watch.cursor_digest, digest);
    assert_eq!(broker.pull_request_watches(true).unwrap().len(), 1);
    assert_eq!(
        fixture.gh_log().matches("pr view").count(),
        1,
        "repeated pushes must not inspect the provider or reset the watch"
    );
}

#[test]
fn session_policy_cannot_enable_automatic_pr_watch() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    std::fs::write(
        worktree.join(".aethyme/config.toml"),
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = true\n",
    )
    .unwrap();
    let report = json(&fixture.push(session, &["--pr"]));
    assert!(report.get("pr_watch_id").is_none());
    assert!(report.get("pr_watch_note").is_none());
    assert!(
        fixture
            .broker()
            .pull_request_watches(true)
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.gh_log().contains("pr view"));
}

#[test]
fn watch_observation_failure_preserves_successful_push_and_allows_retry() {
    let fixture = Fixture::new();
    fixture.set_config_raw(
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = true\n",
    );
    let (session, _) = fixture.session_with_commit();
    let failure = fixture.storage.owner_path().join("pr-state.fail-watch");
    std::fs::write(&failure, "").unwrap();
    let first = json(&fixture.push(session, &["--pr"]));
    assert!(
        first["pr_watch_note"]
            .as_str()
            .unwrap()
            .contains("synthetic observation failure")
    );
    assert_eq!(
        fixture.remote_branch(first["branch"].as_str().unwrap()),
        Some(first["pushed_oid"].as_str().unwrap().to_string())
    );
    assert!(
        fixture
            .broker()
            .pull_request_watches(true)
            .unwrap()
            .is_empty()
    );
    std::fs::remove_file(failure).unwrap();
    let second = json(&fixture.push(session, &["--pr"]));
    assert!(second["pr_watch_id"].is_i64());
    assert!(second.get("pr_watch_note").is_none());
}

#[test]
fn automatic_pr_watch_observation_has_a_finite_deadline() {
    let fixture = Fixture::new();
    fixture.set_config_raw(
        "[delivery]\npush_session_branches = true\n[broker.pr]\nwatch_on_start = true\n",
    );
    let (session, _) = fixture.session_with_commit();
    std::fs::write(fixture.storage.owner_path().join("pr-state.slow-watch"), "").unwrap();
    let report = json(&fixture.push(session, &["--pr"]));
    assert!(
        report["pr_watch_note"]
            .as_str()
            .unwrap()
            .contains("three-second deadline")
    );
    assert!(
        fixture
            .broker()
            .pull_request_watches(true)
            .unwrap()
            .is_empty()
    );
}

/// The busiest repository spent ~2.9 s of every push fetching the default
/// branch it had fetched moments before. A push within a minute of a
/// successful fetch compares against the tracking ref that fetch left.
#[test]
fn a_push_reuses_a_default_branch_fetch_from_moments_before() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    let recorded: i64 = fixture
        .default_branch_fetch_record()
        .expect("the push's fetch is recorded")
        .parse()
        .expect("a successful fetch records its time");
    assert!(recorded <= now_ms());
    let cached = fixture.tracking_main();
    fixture.land_on_remote_only("moved");

    // With its own remote read the push would see the move and fetch; the
    // reuse is what remains when that read is unavailable or turned off.
    let id = session.to_string();
    let report = json(&fixture.run_with_env(
        &["push", "--session", &id, "--json"],
        &[("AETHYME_PUSH_FETCH_SKIP_UNMOVED", "off")],
    ));

    assert_eq!(fixture.tracking_main(), cached, "no fetch ran: {report}");
    assert!(
        report.get("default_branch_fetch_skipped_unmoved").is_none(),
        "{report}"
    );
    let age = report["default_branch_fetch_age_seconds"]
        .as_u64()
        .unwrap_or_else(|| panic!("the report says the fetch was reused: {report}"));
    assert!(age <= 60, "{report}");
    assert_eq!(report["default_branch"]["behind"], 0, "{report}");
}

#[test]
fn a_push_fetches_when_the_recorded_fetch_is_old_unreadable_or_in_the_future() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));

    for (name, record) in [
        ("old", Some((now_ms() - 120_000).to_string())),
        ("future", Some((now_ms() + 120_000).to_string())),
        ("unreadable", None),
    ] {
        match record {
            Some(record) => fixture.record_default_branch_fetch(&record),
            None => fixture.record_default_branch_fetch("not a time"),
        }
        let landed = fixture.land_on_remote_only(name);

        let report = json(&fixture.push(session, &[]));

        assert_eq!(fixture.tracking_main(), landed, "{name}: push fetched");
        assert!(
            report.get("default_branch_fetch_age_seconds").is_none(),
            "{name}: {report}"
        );
        assert!(
            report["default_branch"]["behind"].as_u64() > Some(0),
            "{name}: {report}"
        );
    }
}

#[test]
fn a_push_fetches_when_the_last_default_branch_fetch_failed() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    // Sync's fetch fails while the remote has no default branch, and says so
    // in the record.
    let remote_main = git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]);
    git(&fixture.remote, &["update-ref", "-d", "refs/heads/main"]);
    fixture.run(&["sync", "--session", &session.to_string(), "--json"]);
    git(
        &fixture.remote,
        &["update-ref", "refs/heads/main", &remote_main],
    );
    // A failed fetch may be journaled with an unknown outcome; settle it as
    // an operator would, so the push below is not write-blocked by it.
    let mut broker = fixture.broker();
    let unknown: Vec<i64> = broker
        .store()
        .pending_coordinated_operations()
        .unwrap()
        .into_iter()
        .filter(|operation| operation.status == aethyme_broker::OperationStatus::OutcomeUnknown)
        .map(|operation| operation.id)
        .collect();
    for id in unknown {
        broker
            .reconcile_coordinated_operation(id, false, "the remote had no default branch")
            .unwrap();
    }
    drop(broker);
    assert_eq!(
        fixture.default_branch_fetch_record().as_deref(),
        Some("failed")
    );
    let landed = fixture.land_on_remote_only("after-failure");

    let report = json(&fixture.push(session, &[]));

    assert_eq!(fixture.tracking_main(), landed, "push fetched: {report}");
    assert!(
        report.get("default_branch_fetch_age_seconds").is_none(),
        "{report}"
    );
}

/// `--pr` measures the change against the default branch, so it must not
/// measure against a copy another command fetched.
#[test]
fn a_push_that_opens_a_pull_request_always_fetches() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    let landed = fixture.land_on_remote_only("before-pr");

    let report = json(&fixture.push(session, &["--pr"]));

    assert_eq!(report["pr"]["number"], 7, "{report}");
    assert_eq!(
        fixture.tracking_main(),
        landed,
        "push --pr fetched: {report}"
    );
    assert!(
        report.get("default_branch_fetch_age_seconds").is_none(),
        "{report}"
    );
}

#[test]
fn reuse_of_a_recent_default_branch_fetch_can_be_turned_off() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    let landed = fixture.land_on_remote_only("reuse-off");

    let id = session.to_string();
    let report = json(&fixture.run_with_env(
        &["push", "--session", &id, "--json"],
        &[("AETHYME_PUSH_FETCH_REUSE", "off")],
    ));

    assert_eq!(fixture.tracking_main(), landed, "push fetched: {report}");
    assert!(
        report.get("default_branch_fetch_age_seconds").is_none(),
        "{report}"
    );
}

/// Only `push` reuses a recent fetch: `sync` exists to catch up with the
/// default branch as it is now.
#[test]
fn sync_fetches_even_right_after_a_push_fetched() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    let landed = fixture.land_on_remote_only("before-sync");

    fixture.run(&["sync", "--session", &session.to_string(), "--json"]);

    assert_eq!(fixture.tracking_main(), landed, "sync fetched");
}

/// Pushes come minutes apart, so the 60-second reuse almost never fired and
/// almost every push fetched a default branch that had not moved. The push
/// already reads the remote under the lock; that read now carries the
/// default branch too, and a remote default branch exactly where the
/// tracking ref is leaves nothing to fetch.
#[test]
fn a_push_skips_the_default_branch_fetch_when_the_remote_default_has_not_moved() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    fixture.commit(&worktree, "feature.txt", "two\n", "feat: second step");
    // An old record: the reuse alone would fetch.
    fixture.record_default_branch_fetch(&(now_ms() - 600_000).to_string());
    let fetches = fixture.default_branch_fetches();
    let before = now_ms();

    let report = json(&fixture.push(session, &[]));

    assert_eq!(
        report["default_branch_fetch_skipped_unmoved"], true,
        "{report}"
    );
    assert_eq!(
        fixture.default_branch_fetches(),
        fetches,
        "no default-branch fetch was journaled: {report}"
    );
    let age = report["default_branch_fetch_age_seconds"]
        .as_u64()
        .unwrap_or_else(|| panic!("the age counts from the push's remote read: {report}"));
    assert!(age <= 60, "{report}");
    assert_eq!(report["default_branch"]["behind"], 0, "{report}");
    assert!(report.get("default_branch_note").is_none(), "{report}");
    // The proof stands for a fetch started when the read started.
    let recorded: i64 = fixture
        .default_branch_fetch_record()
        .expect("the proof is recorded")
        .parse()
        .expect("as a time");
    assert!(
        (before..=now_ms()).contains(&recorded),
        "recorded the read's start: {recorded}"
    );
}

/// A remote read that shows the default branch moved means only a fetch can
/// bring the comparison up to date, even when a fetch is recent enough to
/// reuse.
#[test]
fn a_push_fetches_when_its_remote_read_shows_the_default_branch_moved() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    json(&fixture.push(session, &[]));
    fixture.record_default_branch_fetch(&now_ms().to_string());
    let landed = fixture.land_on_remote_only("moved-under-read");
    let fetches = fixture.default_branch_fetches();

    let report = json(&fixture.push(session, &[]));

    assert_eq!(fixture.tracking_main(), landed, "push fetched: {report}");
    assert_eq!(fixture.default_branch_fetches(), fetches + 1, "{report}");
    assert!(
        report.get("default_branch_fetch_skipped_unmoved").is_none(),
        "{report}"
    );
    assert!(
        report.get("default_branch_fetch_age_seconds").is_none(),
        "{report}"
    );
    assert_eq!(report["default_branch"]["behind"], 1, "{report}");
}

/// The push reads its push URL; the fetch reads the fetch URL. When they are
/// not the same, what the push read proves nothing about the fetch.
#[test]
fn a_push_fetches_when_the_push_url_differs_from_the_fetch_url() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    git(
        &fixture.repo,
        &[
            "config",
            "remote.origin.pushurl",
            "ssh://git@github.com/acme/project.git",
        ],
    );
    let (session, _) = fixture.session_with_commit();
    let fetches = fixture.default_branch_fetches();

    let report = json(&fixture.push(session, &[]));

    assert_eq!(fixture.default_branch_fetches(), fetches + 1, "{report}");
    assert!(
        report.get("default_branch_fetch_skipped_unmoved").is_none(),
        "{report}"
    );
    assert!(
        report.get("default_branch_fetch_age_seconds").is_none(),
        "{report}"
    );
}

#[test]
fn skipping_an_unmoved_default_branch_fetch_can_be_turned_off() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    let fetches = fixture.default_branch_fetches();

    let id = session.to_string();
    let report = json(&fixture.run_with_env(
        &["push", "--session", &id, "--json"],
        &[("AETHYME_PUSH_FETCH_SKIP_UNMOVED", "off")],
    ));

    assert_eq!(fixture.default_branch_fetches(), fetches + 1, "{report}");
    assert!(
        report.get("default_branch_fetch_skipped_unmoved").is_none(),
        "{report}"
    );
}

/// `--pr` keeps fetching after it opens the pull request, even when nothing
/// moved: its size measurement should see the default branch as it is then.
#[test]
fn a_push_that_opens_a_pull_request_fetches_even_when_nothing_moved() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, _) = fixture.session_with_commit();
    let fetches = fixture.default_branch_fetches();

    let report = json(&fixture.push(session, &["--pr"]));

    assert_eq!(report["pr"]["number"], 7, "{report}");
    assert_eq!(fixture.default_branch_fetches(), fetches + 1, "{report}");
    assert!(
        report.get("default_branch_fetch_skipped_unmoved").is_none(),
        "{report}"
    );
}

/// Unknown-outcome recovery compares the remote with the pre-push value the
/// plan journaled. Reading the default branch in the same `ls-remote` must
/// leave that plan exactly as it was: one destination, the session branch,
/// with the remote's value before this push.
#[test]
fn reading_the_default_branch_leaves_the_recovery_plan_unchanged() {
    let fixture = Fixture::new();
    fixture.authorize(true);
    let (session, worktree) = fixture.session_with_commit();
    let first = json(&fixture.push(session, &[]));
    let branch = first["branch"].as_str().unwrap().to_string();
    let first_oid = first["pushed_oid"].as_str().unwrap().to_string();
    let plan = fixture.last_push_plan();
    assert_eq!(plan["planning"], "planned", "{plan}");
    assert_eq!(
        plan["plan"]["destinations"],
        serde_json::json!([{
            "destination_ref": format!("refs/heads/{branch}"),
            "pre_push_sha": null,
            "proposed_sha": first_oid,
        }]),
        "{plan}"
    );

    fixture.commit(&worktree, "feature.txt", "two\n", "feat: second step");
    let second = json(&fixture.push(session, &[]));
    assert_eq!(
        second["default_branch_fetch_skipped_unmoved"], true,
        "{second}"
    );
    let plan = fixture.last_push_plan();
    assert_eq!(plan["planning"], "planned", "{plan}");
    assert_eq!(
        plan["plan"]["destinations"],
        serde_json::json!([{
            "destination_ref": format!("refs/heads/{branch}"),
            "pre_push_sha": first_oid,
            "proposed_sha": second["pushed_oid"],
        }]),
        "the default branch never enters the recovery plan: {plan}"
    );
    assert!(
        !plan.to_string().contains("refs/heads/main"),
        "the default branch never enters the recovery plan: {plan}"
    );
}
