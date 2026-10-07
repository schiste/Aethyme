//! `integration reconcile` may drop unrecorded integration work without replay
//! only when it is proven preserved elsewhere: on a branch the remote reports
//! right now, or on the head of an open pull request (#464). Every case drives
//! a fake `gh` on `PATH`, so nothing reaches GitHub.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

const FAKE_GH: &str = r#"#!/bin/sh
# `gh pr view <n> --json state,headRefName,headRefOid`, answered from the env.
if [ "$1" = pr ] && [ "$2" = view ]; then
  printf '%s\n' "$AETHYME_FAKE_GH_PR"
  exit 0
fi
echo "unexpected gh call: $*" >&2
exit 2
"#;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(root: &Path, path: &str, content: &str, message: &str) -> String {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, content).unwrap();
    git(root, &["add", path]);
    git(root, &["commit", "-qm", message]);
    git(root, &["rev-parse", "HEAD"])
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    bin: PathBuf,
    pull_request: String,
    upstream_head: String,
    old_integration: String,
}

impl Fixture {
    /// A repository whose integration holds unrecorded commits and whose
    /// upstream moved on, so reconcile must decide what happens to them.
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let state = tmp.path().join("state");
        let bin = tmp.path().join("bin");
        for dir in [&repo, &remote, &state, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let gh = bin.join("gh");
        std::fs::write(&gh, FAKE_GH).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
        commit(&repo, "README.md", "base\n", "initial");
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-qu", "origin", "main"]);

        let fixture = Self {
            _tmp: tmp,
            repo,
            state,
            bin,
            pull_request: String::new(),
            upstream_head: String::new(),
            old_integration: String::new(),
        };
        let status = fixture.run(&["status", "--json"]);
        assert!(
            status.status.success(),
            "status: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        fixture
    }

    /// Commit on integration directly, which leaves the commit unrecorded.
    fn unrecorded_commit(&mut self, path: &str, content: &str) -> String {
        git(&self.repo, &["switch", "-q", "aethyme/integration"]);
        let sha = commit(&self.repo, path, content, &format!("operator edit {path}"));
        git(&self.repo, &["switch", "-q", "main"]);
        self.old_integration = sha.clone();
        sha
    }

    /// Move upstream on with an unrelated commit and fetch it.
    fn advance_upstream(&mut self) {
        let clone = self.repo.parent().unwrap().join("upstream-clone");
        git(
            self.repo.parent().unwrap(),
            &[
                "clone",
                "-q",
                self.repo
                    .parent()
                    .unwrap()
                    .join("remote.git")
                    .to_str()
                    .unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        commit(&clone, "upstream.txt", "upstream\n", "upstream moves on");
        git(&clone, &["push", "-q", "origin", "main"]);
        git(&self.repo, &["fetch", "-q", "origin"]);
        self.upstream_head = git(&self.repo, &["rev-parse", "origin/main"]);
    }

    fn run(&self, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_FAKE_GH_PR", &self.pull_request)
            .output()
            .unwrap()
    }

    fn write_resolution(&self, unrecorded: &[serde_json::Value]) -> PathBuf {
        let path = self.repo.parent().unwrap().join("resolution.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 2,
                "upstream_ref": "origin/main",
                "upstream_commit": self.upstream_head,
                "old_integration": self.old_integration,
                "operator": "operator@example.invalid",
                "unrecorded_resolutions": unrecorded,
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }

    fn reconcile(&self, resolution: &Path, extra: &[&str]) -> Output {
        let mut args = vec![
            "advanced",
            "integration",
            "reconcile",
            "--upstream",
            "origin/main",
            "--resolution-file",
            resolution.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "reconcile failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn audit<'a>(report: &'a serde_json::Value, commit: &str) -> &'a serde_json::Value {
    report["plan"]["commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["commit"] == commit)
        .unwrap_or_else(|| panic!("{commit} missing from plan: {report:#}"))
}

#[test]
fn work_preserved_on_a_pushed_branch_is_dropped_without_replay() {
    let mut fixture = Fixture::new();
    let kept = fixture.unrecorded_commit("docs/keep.md", "keep\n");
    git(
        &fixture.repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("{kept}:refs/heads/agent/keep"),
        ],
    );
    fixture.advance_upstream();
    let resolution = fixture.write_resolution(&[serde_json::json!({
        "integration_commit": kept,
        "disposition": "tracked_elsewhere",
        "tracked_branch": "agent/keep",
        "reason": "the session branch holds this work",
    })]);

    let dry_run = json(&fixture.reconcile(&resolution, &["--dry-run"]));
    assert_eq!(dry_run["safe"], true, "{dry_run:#}");
    let entry = audit(&dry_run, &kept);
    let tracked = &entry["unrecorded_resolution"]["tracked_elsewhere"];
    assert_eq!(tracked["branch"], "agent/keep");
    assert_eq!(tracked["remote"], "origin");
    assert_eq!(tracked["remote_commit"], kept.as_str());
    assert_eq!(tracked["containment"], "ancestor");
    assert!(entry["replayed_commit"].is_null(), "{entry:#}");
    assert_eq!(dry_run["new_integration"], fixture.upstream_head.as_str());

    let digest = dry_run["plan_digest"].as_str().unwrap().to_string();
    let applied = json(&fixture.reconcile(&resolution, &["--apply", "--confirm", &digest]));
    assert_eq!(applied["applied"], true, "{applied:#}");
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        fixture.upstream_head
    );
}

#[test]
fn work_preserved_on_an_open_pull_request_matches_by_patch() {
    let mut fixture = Fixture::new();
    let kept = fixture.unrecorded_commit("docs/keep.md", "keep\n");
    fixture.advance_upstream();
    // The pull request carries the same change on top of current upstream:
    // a different SHA, the same patch.
    git(
        &fixture.repo,
        &["switch", "-qc", "pr-branch", "origin/main"],
    );
    git(&fixture.repo, &["cherry-pick", &kept]);
    let pr_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_ne!(pr_head, kept);
    git(
        &fixture.repo,
        &["push", "-q", "origin", "HEAD:refs/heads/agent/pr-branch"],
    );
    git(&fixture.repo, &["switch", "-q", "main"]);
    git(&fixture.repo, &["branch", "-qD", "pr-branch"]);
    fixture.pull_request = serde_json::json!({
        "state": "OPEN",
        "headRefName": "agent/pr-branch",
        "headRefOid": pr_head,
    })
    .to_string();
    let resolution = fixture.write_resolution(&[serde_json::json!({
        "integration_commit": kept,
        "disposition": "tracked_elsewhere",
        "pull_request": 41,
        "reason": "the open pull request carries this change",
    })]);

    let dry_run = json(&fixture.reconcile(&resolution, &["--dry-run"]));
    assert_eq!(dry_run["safe"], true, "{dry_run:#}");
    let tracked = &audit(&dry_run, &kept)["unrecorded_resolution"]["tracked_elsewhere"];
    assert_eq!(tracked["pull_request"], 41);
    assert_eq!(tracked["branch"], "agent/pr-branch");
    assert_eq!(tracked["remote_commit"], pr_head.as_str());
    assert_eq!(tracked["containment"], "patch_id");
    assert_eq!(tracked["matching_commit"], pr_head.as_str());

    // A closed pull request preserves nothing the broker can rely on.
    fixture.pull_request = serde_json::json!({
        "state": "CLOSED",
        "headRefName": "agent/pr-branch",
        "headRefOid": pr_head,
    })
    .to_string();
    let closed = fixture.reconcile(&resolution, &["--dry-run"]);
    assert!(!closed.status.success());
    assert!(
        String::from_utf8_lossy(&closed.stderr).contains("pull request #41 is not open"),
        "{}",
        String::from_utf8_lossy(&closed.stderr)
    );
}

#[test]
fn a_commit_preserved_nowhere_refuses_tracked_elsewhere() {
    let mut fixture = Fixture::new();
    let kept = fixture.unrecorded_commit("docs/keep.md", "keep\n");
    git(
        &fixture.repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("{kept}:refs/heads/agent/keep"),
        ],
    );
    let lost = fixture.unrecorded_commit("docs/lost.md", "only here\n");
    fixture.advance_upstream();
    let resolution = fixture.write_resolution(&[
        serde_json::json!({
            "integration_commit": kept,
            "disposition": "tracked_elsewhere",
            "tracked_branch": "agent/keep",
            "reason": "the session branch holds this work",
        }),
        serde_json::json!({
            "integration_commit": lost,
            "disposition": "tracked_elsewhere",
            "tracked_branch": "agent/keep",
            "reason": "claimed, but never pushed",
        }),
    ]);

    let refused = fixture.reconcile(&resolution, &["--dry-run"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(&format!(
            "unrecorded integration commit {lost} cannot use tracked_elsewhere"
        )) && stderr.contains("neither it nor an identical patch is on origin/agent/keep"),
        "{stderr}"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        lost,
        "a refused disposition moves nothing"
    );

    // A branch the remote does not have is not proven pushed either.
    let unpushed = fixture.write_resolution(&[serde_json::json!({
        "integration_commit": lost,
        "disposition": "tracked_elsewhere",
        "tracked_branch": "agent/never-pushed",
        "reason": "claimed, but never pushed",
    })]);
    let refused = fixture.reconcile(&unpushed, &["--dry-run"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("it is not proven pushed"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
