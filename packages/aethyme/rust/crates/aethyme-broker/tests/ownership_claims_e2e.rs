//! Named ownership claims: who is driving a release, and whether they still
//! are. On 2026-10-02/03 a second session took over v0.8.15 while the first was
//! rate-limited, and the only way to see it was the coordinated-operation
//! journal. A claim refuses only while its holder is working; a quiet holder's
//! claim is taken over and the report names it.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{
    Broker, BrokerOpError, NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance,
    OperationProvider, SessionStatus,
};

const HOUR_MS: i64 = 60 * 60 * 1_000;

fn sh(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "a\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn add_worktree(root: &Path, name: &str) -> PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Store activity and the worktree metadata liveness reads, three hours back.
fn make_stale(broker: &mut Broker, root: &Path, session_id: i64, name: &str) {
    broker
        .store()
        .touch_session_activity(session_id, now_ms() - 3 * HOUR_MS)
        .unwrap();
    let when =
        std::time::SystemTime::now() - std::time::Duration::from_millis((3 * HOUR_MS) as u64);
    for file in ["index", "HEAD"] {
        std::fs::File::open(root.join(".git/worktrees").join(name).join(file))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

struct Two {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    broker: Broker,
    first: i64,
    second: i64,
}

fn two_sessions() -> Two {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    init_repo(&root);
    let mut broker = Broker::open(&root).unwrap();
    let first = broker
        .adopt(&add_worktree(&root, "first"), Some("cut the release"))
        .unwrap()
        .id;
    let second = broker
        .adopt(&add_worktree(&root, "second"), None)
        .unwrap()
        .id;
    Two {
        _tmp: tmp,
        root,
        broker,
        first,
        second,
    }
}

#[test]
fn a_claim_held_by_a_working_session_is_refused_with_its_holder() {
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();

    let error = t
        .broker
        .claim_ownership(t.second, "release v1", "cut v1 too")
        .unwrap_err();

    let BrokerOpError::OwnershipClaimHeld { holder, .. } = &error else {
        panic!("expected a refusal, got {error:?}");
    };
    assert_eq!(holder.claim.session_id, t.first);
    assert!(holder.working);
    let message = error.to_string();
    assert!(
        message.contains(&format!(
            "note send --session {} --to-session {}",
            t.second, t.first
        )),
        "{message}"
    );
}

#[test]
fn a_quiet_holders_claim_is_taken_over_and_the_report_names_it() {
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();
    make_stale(&mut t.broker, &t.root, t.first, "first");

    let report = t
        .broker
        .claim_ownership(t.second, "release v1", "finish v1")
        .unwrap();

    let replaced = report
        .replaced
        .expect("the takeover names the claim it replaced");
    assert_eq!(replaced.claim.session_id, t.first);
    assert_eq!(replaced.holder_status, SessionStatus::Stale);
    assert!(!replaced.working);
    let claims = t.broker.ownership_claims().unwrap();
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].claim.session_id, t.second);
    assert_eq!(claims[0].claim.taken_over_from, Some(t.first));
}

#[test]
fn a_recent_coordinated_operation_keeps_a_quiet_session_working() {
    // A release driver's last act is a merge or a tag through the
    // coordinator, which hook-driven activity does not always see.
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();
    make_stale(&mut t.broker, &t.root, t.first, "first");
    let repository = format!("local:{}", t.root.display());
    t.broker
        .store()
        .create_coordinated_operation(&NewCoordinatedOperation {
            session_id: t.first,
            provider: OperationProvider::Git,
            repository,
            scope: "repository".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("tag v1".into()),
            command_json: r#"["git","tag","v1"]"#.into(),
            pid: std::process::id() as i64,
            host_operation_id: None,
            identity_provenance: OperationIdentityProvenance::LocalRepository,
        })
        .unwrap();

    let error = t
        .broker
        .claim_ownership(t.second, "release v1", "take over")
        .unwrap_err();

    let BrokerOpError::OwnershipClaimHeld { holder, .. } = &error else {
        panic!("expected a refusal, got {error:?}");
    };
    assert_eq!(holder.holder_status, SessionStatus::Stale);
    assert_eq!(holder.last_operation_reason.as_deref(), Some("tag v1"));
}

#[test]
fn a_finished_holders_claim_stops_counting() {
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();
    t.broker
        .store()
        .set_session_status(t.first, SessionStatus::Closed, None)
        .unwrap();

    assert!(t.broker.ownership_claims().unwrap().is_empty());
    let report = t
        .broker
        .claim_ownership(t.second, "release v1", "cut v1")
        .unwrap();
    assert!(report.replaced.is_none());
}

#[test]
fn status_names_the_holder_in_json_and_advice() {
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();

    let status = t.broker.status_current(now_ms()).unwrap();
    assert_eq!(status.ownership_claims.len(), 1);
    assert_eq!(status.ownership_claims[0].claim.name, "release v1");
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "ownership.claimed")
        .expect("the claim is in advice");
    assert_eq!(advice.session_id, Some(t.first));
    assert!(advice.summary.contains("release v1"), "{}", advice.summary);

    let brief = t.broker.status_brief(now_ms()).unwrap();
    assert!(
        brief
            .advice
            .iter()
            .any(|item| item.id == "ownership.claimed"),
        "status --summary shows it too"
    );
}

#[test]
fn reclaiming_updates_the_purpose_and_release_ends_the_claim() {
    let mut t = two_sessions();
    t.broker
        .claim_ownership(t.first, "release v1", "cut v1")
        .unwrap();
    let again = t
        .broker
        .claim_ownership(t.first, " release v1 ", "cut and tag v1")
        .unwrap();
    assert!(again.already_held);
    assert_eq!(
        t.broker.ownership_claims().unwrap()[0].claim.purpose,
        "cut and tag v1"
    );

    t.broker.release_ownership(t.first, "release v1").unwrap();
    assert!(t.broker.ownership_claims().unwrap().is_empty());
    assert!(t.broker.release_ownership(t.first, "release v1").is_err());
}
