//! Committed work only this machine holds: reported by `status` and `doctor`,
//! and, under the push lane, a reason `finish` and `close` refuse.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

/// A repository with a bare `origin` whose default branch is fetched and
/// tracked, so the broker has an upstream to compare against. A non-empty
/// `config` is committed as `.aethyme/config.toml` before the push.
fn fixture(config: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    if !config.is_empty() {
        // `broker push` trusts only the policy committed on the fetched
        // default branch, and finish reads it the same way.
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/config.toml"), config).unwrap();
        git(&repo, &["add", "-f", ".aethyme/config.toml"]);
        git(&repo, &["commit", "-qm", "enable the push lane"]);
    }
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);
    Fixture { _tmp: tmp, repo }
}

/// Start a session and commit one file in its worktree.
fn session_with_commit(repo: &Path, file: &str) -> (i64, PathBuf, String) {
    let mut broker = Broker::open(repo).unwrap();
    let session = broker.start_worktree("unpushed fixture", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join(file), format!("{file}\n")).unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", file]);
    (session.id, worktree, session.branch)
}

fn status(repo: &Path) -> serde_json::Value {
    json(&run(repo, &["status", "--refresh", "--json"]))
}

fn advice<'a>(status: &'a serde_json::Value, id: &str) -> Vec<&'a serde_json::Value> {
    status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["id"] == id)
        .collect()
}

#[test]
fn status_reports_unpushed_session_commits_with_the_push_command() {
    let fixture = fixture("");
    let (session, _, branch) = session_with_commit(&fixture.repo, "work.txt");

    let status = status(&fixture.repo);
    let sessions = status["unpushed_work"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{status:#}");
    assert_eq!(sessions[0]["session_id"], session);
    assert_eq!(sessions[0]["branch"], branch.as_str());
    assert_eq!(sessions[0]["unpushed_commits"], 1);
    assert!(sessions[0]["oldest_unpushed_at_ms"].is_i64());
    // No push lane: the count is context, never an alarm.
    assert_eq!(sessions[0]["severity"], "info");
    assert_eq!(status["unpushed_work"]["push_session_branches"], false);

    let rows = advice(&status, "session.unpushed-commits");
    assert_eq!(rows.len(), 1, "{status:#}");
    let commands = rows[0]["commands"].as_array().unwrap();
    assert_eq!(
        commands[0],
        format!("aethyme broker push --session {session}")
    );
    assert!(
        commands[1]
            .as_str()
            .unwrap()
            .contains("push_session_branches"),
        "without the opt-in the advice must name it: {commands:?}"
    );
}

#[test]
fn a_session_branch_on_the_remote_is_not_reported() {
    let fixture = fixture("");
    let (_, worktree, branch) = session_with_commit(&fixture.repo, "work.txt");
    git(&worktree, &["push", "-q", "origin", &branch]);

    let status = status(&fixture.repo);
    assert!(status.get("unpushed_work").is_none(), "{status:#}");
    assert!(advice(&status, "session.unpushed-commits").is_empty());
}

/// A pull request merged by squash or rebase lands the same patch under a
/// different SHA; that work is delivered, not at risk (#408).
#[test]
fn a_commit_whose_patch_is_already_upstream_is_not_counted() {
    let fixture = fixture("");
    let (_, worktree, _) = session_with_commit(&fixture.repo, "work.txt");
    let commit = git(&worktree, &["rev-parse", "HEAD"]);
    let repo = &fixture.repo;
    git(repo, &["checkout", "-q", "-b", "land", "origin/main"]);
    std::fs::write(repo.join("other.txt"), "moves main first\n").unwrap();
    git(repo, &["add", "other.txt"]);
    git(repo, &["commit", "-qm", "unrelated"]);
    git(repo, &["cherry-pick", &commit]);
    git(repo, &["push", "-q", "origin", "land:main"]);
    git(repo, &["checkout", "-q", "main"]);
    git(repo, &["branch", "-q", "-D", "land"]);
    git(repo, &["fetch", "-q", "origin"]);

    let status = status(repo);
    assert!(
        status["unpushed_work"]["sessions"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{status:#}"
    );
}

#[test]
fn integration_ahead_of_upstream_is_reported_with_its_age() {
    let fixture = fixture("");
    let (session, _, _) = session_with_commit(&fixture.repo, "work.txt");
    let mut broker = Broker::open(&fixture.repo).unwrap();
    assert!(broker.submit(session).unwrap().promoted);

    let status = status(&fixture.repo);
    let integration = &status["unpushed_work"]["integration"];
    assert_eq!(integration["unpublished_commits"], 1, "{status:#}");
    assert_eq!(integration["on_no_remote"], 1, "{status:#}");
    assert_eq!(integration["upstream_ref"], "refs/remotes/origin/main");
    assert!(integration["oldest_unpublished_at_ms"].is_i64());
    let rows = advice(&status, "integration.unpublished-work");
    assert_eq!(rows.len(), 1, "{status:#}");
    assert!(
        rows[0]["summary"].as_str().unwrap().contains("verify-only"),
        "{}",
        rows[0]["summary"]
    );

    let doctor = json(&run(&fixture.repo, &["status", "doctor", "--json"]));
    assert_eq!(
        doctor["unpushed_work"]["integration"]["unpublished_commits"], 1,
        "{doctor:#}"
    );
}

/// A session built on integration inherits its unpublished promotions. They
/// are integration's backlog, reported once there -- not a stranded commit in
/// every worktree that started after them.
#[test]
fn a_new_session_does_not_inherit_unpublished_integration_work() {
    let fixture = fixture("");
    let (first, _, _) = session_with_commit(&fixture.repo, "work.txt");
    let mut broker = Broker::open(&fixture.repo).unwrap();
    assert!(broker.submit(first).unwrap().promoted);
    let second = broker.start_worktree("built on integration", None).unwrap();

    let status = status(&fixture.repo);
    let sessions = status["unpushed_work"]["sessions"].as_array().unwrap();
    assert!(
        sessions
            .iter()
            .all(|session| session["session_id"] != second.id),
        "{status:#}"
    );
    assert_eq!(
        status["unpushed_work"]["integration"]["unpublished_commits"], 1,
        "{status:#}"
    );
}

const PUSH_LANE: &str = "[delivery]\npush_session_branches = true\n";

/// Promoted work is accepted locally but exists on no remote; under the push
/// lane that is exactly the pile-up finish must not close over.
fn promoted_unpushed_session(fixture: &Fixture) -> (i64, PathBuf, String) {
    let (session, worktree, branch) = session_with_commit(&fixture.repo, "work.txt");
    let mut broker = Broker::open(&fixture.repo).unwrap();
    assert!(broker.submit(session).unwrap().promoted);
    (session, worktree, branch)
}

#[test]
fn finish_refuses_unpushed_work_under_the_push_lane() {
    let fixture = fixture(PUSH_LANE);
    let (session, worktree, _) = promoted_unpushed_session(&fixture);
    let id = session.to_string();

    let report = json(&run(&fixture.repo, &["finish", "--session", &id, "--json"]));
    assert_eq!(report["status"], "blocked", "{report:#}");
    assert_eq!(report["closed"], false);
    assert_eq!(report["unpushed_commits"], 1);
    let commands = report["next_commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .any(|command| command == &format!("aethyme broker push --session {session}")),
        "{commands:?}"
    );
    assert!(worktree.exists());

    let close = run(&fixture.repo, &["finish", "close", "--session", &id]);
    assert!(!close.status.success());
    assert!(
        String::from_utf8_lossy(&close.stderr).contains("aethyme broker push --session"),
        "{}",
        String::from_utf8_lossy(&close.stderr)
    );
}

#[test]
fn finish_closes_pushed_work_under_the_push_lane() {
    let fixture = fixture(PUSH_LANE);
    let (session, worktree, branch) = promoted_unpushed_session(&fixture);
    git(&worktree, &["push", "-q", "origin", &branch]);

    let report = json(&run(
        &fixture.repo,
        &["finish", "--session", &session.to_string(), "--json"],
    ));
    assert_ne!(report["status"], "blocked", "{report:#}");
    assert_eq!(report["closed"], true);
    assert_eq!(report["unpushed_commits"], 0);
}

/// Repositories without the push lane keep today's finish behaviour.
#[test]
fn finish_ignores_unpushed_work_without_the_push_lane() {
    let fixture = fixture("");
    let (session, _, _) = promoted_unpushed_session(&fixture);

    let report = json(&run(
        &fixture.repo,
        &["finish", "--session", &session.to_string(), "--json"],
    ));
    assert_eq!(report["closed"], true, "{report:#}");
    assert!(report.get("unpushed_commits").is_none(), "{report:#}");
}

#[test]
fn abandoning_unpushed_work_needs_a_reason_and_is_recorded() {
    let fixture = fixture(PUSH_LANE);
    let (session, _, branch) = promoted_unpushed_session(&fixture);
    let id = session.to_string();

    let bare = run(&fixture.repo, &["finish", "--session", &id, "--abandon"]);
    assert!(!bare.status.success());
    assert!(
        String::from_utf8_lossy(&bare.stderr).contains("--abandon requires --reason"),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );
    let stray = run(
        &fixture.repo,
        &["finish", "close", "--session", &id, "--reason", "x"],
    );
    assert!(!stray.status.success());
    assert!(
        String::from_utf8_lossy(&stray.stderr).contains("only meaningful with --abandon"),
        "{}",
        String::from_utf8_lossy(&stray.stderr)
    );

    let report = json(&run(
        &fixture.repo,
        &[
            "finish",
            "--session",
            &id,
            "--abandon",
            "--reason",
            "superseded by session 99",
            "--json",
        ],
    ));
    assert_eq!(report["closed"], true, "{report:#}");

    let mut broker = Broker::open(&fixture.repo).unwrap();
    let events = broker
        .store()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "broker.session.abandoned_unpushed")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1, "{events:?}");
    let payload: serde_json::Value =
        serde_json::from_str(events[0].payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(payload["session_id"], session);
    assert_eq!(payload["branch"], branch.as_str());
    assert_eq!(payload["unpushed_commits"], 1);
    assert_eq!(payload["reason"], "superseded by session 99");
}

#[test]
fn routine_status_marks_unpushed_work_unknown_until_an_explicit_audit() {
    let fixture = fixture("");
    let (session, _, _) = session_with_commit(&fixture.repo, "pending.txt");
    let routine = json(&run(&fixture.repo, &["status", "--json"]));
    assert!(
        routine["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "unpushed_commits")
    );
    let audited = status(&fixture.repo);
    assert_eq!(
        audited["unpushed_work"]["sessions"][0]["session_id"],
        session
    );
    assert_eq!(
        audited["unpushed_work"]["sessions"][0]["unpushed_commits"],
        1
    );
}
