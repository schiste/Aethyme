//! #1238 outcome hooks: one report drives owners, the issue and the notes.

use std::path::Path;
use std::process::Command;

use super::*;
use crate::outcome_issues::OutcomeRule;
use crate::types::{NewSession, SessionOrigin, SessionStatus};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git fixture command");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn land(repo: &Path, number: i64, branch: &str, path: &str) {
    git(repo, &["checkout", "-q", "-b", branch]);
    let file = repo.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, branch).unwrap();
    git(repo, &["add", path]);
    git(repo, &["commit", "-qm", &format!("work on {branch}")]);
    git(repo, &["checkout", "-q", "main"]);
    git(
        repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            branch,
            "-m",
            &format!("Merge pull request #{number} from o/{branch}"),
        ],
    );
}

/// Records every issue request and answers with a fixed action.
struct FakeSink {
    requests: Vec<OutcomeIssueRequest>,
    answer: OutcomeIssueAction,
}

impl OutcomeIssueSink for FakeSink {
    fn upsert(
        &mut self,
        _broker: &mut Broker,
        request: &OutcomeIssueRequest,
    ) -> Result<OutcomeIssueResult, String> {
        self.requests.push(request.clone());
        let refused = self.answer == OutcomeIssueAction::Refused;
        Ok(OutcomeIssueResult {
            schema: crate::OUTCOME_ISSUE_SCHEMA,
            repository: request.repository.clone(),
            fingerprint: request.fingerprint.clone(),
            action: self.answer,
            issue_number: (!refused).then_some(77),
            issue_url: (!refused).then(|| "https://github.com/o/n/issues/77".to_string()),
            code: refused.then_some("automation_issues_denied"),
            message: "ok".into(),
            operation_ids: Vec::new(),
        })
    }
}

fn sink(answer: OutcomeIssueAction) -> FakeSink {
    FakeSink {
        requests: Vec::new(),
        answer,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    broker: Broker,
    base: String,
    live: i64,
    ended: i64,
}

/// PR #11 by a live session and PR #12 by an ended one both touch
/// migrations; PR #13 touches only source.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "initial"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    land(&repo, 11, "agent/a", "db/migrations/001.sql");
    land(&repo, 12, "agent/b", "db/migrations/002.sql");
    land(&repo, 13, "agent/c", "src/lib.rs");
    let host_state = temp.path().join("host-state");
    let options = crate::BrokerOpenOptions::default()
        .with_host_state_dir(&host_state)
        .with_host_cache_dir(temp.path().join("host-cache"))
        .with_worktree_root(temp.path().join("worktrees"))
        .with_collaboration_state(host_state.clone())
        .with_host_operation_database(host_state.join("host-operations.db"));
    let mut broker = Broker::open_with_options(&repo, options).unwrap();
    let mut register = |branch: &str| {
        broker
            .store()
            .register_session(&NewSession {
                worktree_path: format!("/nonexistent/{branch}"),
                branch: branch.into(),
                origin: SessionOrigin::Adopted,
                task: None,
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap()
            .id
    };
    let live = register("agent/a");
    let ended = register("agent/b");
    let _source_only = register("agent/c");
    broker
        .store()
        .set_session_status(ended, SessionStatus::Closed, None)
        .unwrap();
    Fixture {
        _temp: temp,
        broker,
        base,
        live,
        ended,
    }
}

fn rule() -> OutcomeRule {
    OutcomeRule {
        source: "prebuild:db-template".into(),
        issue: true,
        labels: vec!["migrations".into()],
        notify_owners: true,
        paths: vec!["db/migrations/**".into()],
    }
}

fn failure(base: &str) -> OutcomeReportRequest {
    OutcomeReportRequest {
        source: "prebuild:db-template".into(),
        failed: true,
        key: Some("abc123".into()),
        commits: Some(format!("{base}..main")),
        title: Some("Migration 002 failed on main".into()),
        body: Some("psql: relation already exists".into()),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// #1238 acceptance: a failing report opens one issue that names every
/// owner of the migration commits (the ended one by branch and PR), sends
/// the live owner a note linking it, and leaves the other sessions alone.
#[test]
fn a_failure_files_the_issue_and_tells_the_live_owner() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Created);
    let report = report_outcome(
        &mut f.broker,
        &[rule()],
        Some("o/n"),
        &failure(&f.base),
        &mut issues,
        now(),
    )
    .unwrap();
    assert_eq!(report.fingerprint, "outcome:prebuild:db-template:abc123");
    assert!(!report.incomplete(), "{:?}", report.actions);
    let request = &issues.requests[0];
    assert!(!request.resolve);
    assert_eq!(request.labels, vec!["migrations".to_string()]);
    let body = request.body.as_deref().unwrap();
    assert!(body.starts_with("psql: relation already exists"), "{body}");
    assert!(
        body.contains(&format!(
            "- session {} (`agent/a`, #11): live, notified",
            f.live
        )),
        "{body}"
    );
    assert!(
        body.contains(&format!("- session {} (`agent/b`, #12): ended", f.ended)),
        "{body}"
    );
    assert!(!body.contains("agent/c"), "{body}");

    let kinds: Vec<(&str, &str)> = report
        .actions
        .iter()
        .map(|action| (action.kind, action.status))
        .collect();
    assert_eq!(
        kinds,
        vec![("owners", "done"), ("issue", "done"), ("note", "done")]
    );
    let notes = f.broker.store().unread_automation_notes(f.live).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].sender, "automation:prebuild.db-template");
    assert_eq!(
        notes[0].message,
        "Migration 002 failed on main — tracked in https://github.com/o/n/issues/77"
    );
    assert!(
        f.broker
            .store()
            .unread_automation_notes(f.ended)
            .unwrap()
            .is_empty()
    );
    let events = f
        .broker
        .store()
        .events_after_filtered(0, 10, Some("automation.outcome.reported"))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(!events[0].payload_json.as_deref().unwrap().contains("psql"));
}

/// A repeat while the note is unread does not notify twice; the issue step
/// still runs, and the upsert decides whether to comment.
#[test]
fn a_repeated_failure_does_not_notify_twice() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Commented);
    for _ in 0..2 {
        report_outcome(
            &mut f.broker,
            &[rule()],
            Some("o/n"),
            &failure(&f.base),
            &mut issues,
            now(),
        )
        .unwrap();
    }
    assert_eq!(issues.requests.len(), 2);
    assert_eq!(
        f.broker
            .store()
            .unread_automation_notes(f.live)
            .unwrap()
            .len(),
        1
    );
}

/// With the policy denying issues the owner is still told, the note says no
/// issue was filed, and the report is incomplete so the caller can see it.
#[test]
fn a_refused_issue_never_hides_the_failure() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Refused);
    let report = report_outcome(
        &mut f.broker,
        &[rule()],
        Some("o/n"),
        &failure(&f.base),
        &mut issues,
        now(),
    )
    .unwrap();
    assert!(report.incomplete());
    let issue = report
        .actions
        .iter()
        .find(|action| action.kind == "issue")
        .unwrap();
    assert_eq!(
        (issue.status, issue.code.as_deref()),
        ("refused", Some("automation_issues_denied"))
    );
    let notes = f.broker.store().unread_automation_notes(f.live).unwrap();
    assert_eq!(
        notes[0].message,
        "Migration 002 failed on main — no issue filed (automation_issues_denied)"
    );
}

/// A recovery resolves the outcome's issue and notifies no one.
#[test]
fn a_recovery_resolves_the_issue() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Resolved);
    let report = report_outcome(
        &mut f.broker,
        &[rule()],
        Some("o/n"),
        &OutcomeReportRequest {
            source: "prebuild:db-template".into(),
            failed: false,
            key: Some("abc123".into()),
            commits: None,
            title: None,
            body: None,
        },
        &mut issues,
        now(),
    )
    .unwrap();
    assert_eq!(report.state, "recovered");
    assert!(issues.requests[0].resolve);
    assert_eq!(
        issues.requests[0].fingerprint,
        "outcome:prebuild:db-template:abc123"
    );
    assert_eq!(report.actions.len(), 1);
    assert!(
        f.broker
            .store()
            .unread_automation_notes(f.live)
            .unwrap()
            .is_empty()
    );
}

/// Without a GitHub origin the issue steps are skipped, not failed, and the
/// owners are still told.
#[test]
fn without_a_github_origin_only_the_issue_is_skipped() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Created);
    let report = report_outcome(
        &mut f.broker,
        &[rule()],
        None,
        &failure(&f.base),
        &mut issues,
        now(),
    )
    .unwrap();
    assert!(issues.requests.is_empty());
    assert!(!report.incomplete());
    let issue = report.actions.iter().find(|a| a.kind == "issue").unwrap();
    assert_eq!(
        (issue.status, issue.code.as_deref()),
        ("skipped", Some("no_github_origin"))
    );
    assert_eq!(
        f.broker.store().unread_automation_notes(f.live).unwrap()[0].message,
        "Migration 002 failed on main — no issue filed"
    );
}

#[test]
fn reports_without_a_rule_or_a_title_are_refused() {
    let mut f = fixture();
    let mut issues = sink(OutcomeIssueAction::Created);
    let mut request = failure(&f.base);
    request.source = "gate:unit".into();
    let error = report_outcome(
        &mut f.broker,
        &[rule()],
        Some("o/n"),
        &request,
        &mut issues,
        now(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "no_outcome_rule");
    let mut request = failure(&f.base);
    request.title = Some("  ".into());
    let error = report_outcome(
        &mut f.broker,
        &[rule()],
        Some("o/n"),
        &request,
        &mut issues,
        now(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "invalid_request");
    let mut request = failure(&f.base);
    request.key = Some("has space".into());
    assert!(
        report_outcome(
            &mut f.broker,
            &[rule()],
            Some("o/n"),
            &request,
            &mut issues,
            now()
        )
        .is_err()
    );
    assert!(issues.requests.is_empty());
}
