//! One lease refusal rule for claims, planned leases, submit and guarded
//! exec: only an explicit lease held by a session that is actively working
//! refuses, and nothing refuses under verify-only. On Mockup every
//! `leases claim` was refused by leases of stale sessions and by implicit
//! leases derived from other sessions' edits.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpError, LeaseKind};

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

fn init_repo(root: &Path, config: Option<&str>) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.py"), "a\n").unwrap();
    std::fs::write(root.join("src/other.py"), "b\n").unwrap();
    if let Some(config) = config {
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(root.join(".aethyme/config.toml"), config).unwrap();
    }
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

const VERIFY_ONLY: &str = "schema = 1\n\n[promote]\nmode = \"verify-only\"\n";

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

/// Make a session read as stale: store activity and the worktree metadata
/// the broker reads for liveness both lie three hours back.
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

#[test]
fn a_verify_only_claim_over_a_working_sessions_explicit_lease_succeeds_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), Some(VERIFY_ONLY));
    let mut broker = Broker::open(tmp.path()).unwrap();
    let owner = broker
        .adopt(&add_worktree(tmp.path(), "owner"), Some("rewrite auth"))
        .unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    let report = broker.claim_lease(claimer.id, "src/", None).unwrap();

    assert!(report.accepted);
    assert!(report.blockers.is_empty(), "{report:?}");
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    let warning = &report.warnings[0];
    assert_eq!(warning.session_id, owner.id);
    assert_eq!(warning.kind, LeaseKind::Explicit);
    assert_eq!(warning.holder_status.as_deref(), Some("active"));
    let reason = warning.reason.as_deref().unwrap_or_default();
    assert!(reason.contains("verify-only"), "{reason}");
    assert!(
        reason.contains(&format!(
            "note send --session {} --to-session {}",
            claimer.id, owner.id
        )),
        "{reason}"
    );

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["accepted"], true);
    assert_eq!(json["warnings"][0]["session_id"], owner.id);
}

#[test]
fn outside_verify_only_a_working_sessions_explicit_lease_still_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let owner = broker
        .adopt(&add_worktree(tmp.path(), "owner"), None)
        .unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    let error = broker.claim_lease(claimer.id, "src/", None).unwrap_err();

    let BrokerOpError::LeaseClaimConflict { blockers, .. } = &error else {
        panic!("expected a refusal, got {error:?}");
    };
    assert_eq!(blockers.len(), 1, "{blockers:?}");
    assert_eq!(blockers[0].session_id, owner.id);
    assert!(
        blockers[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("actively working"),
        "{blockers:?}"
    );
}

#[test]
fn a_stale_sessions_explicit_lease_never_refuses_a_claim() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let quiet = broker
        .adopt(&add_worktree(tmp.path(), "quiet"), None)
        .unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    broker.claim_lease(quiet.id, "src/auth.py", None).unwrap();
    make_stale(&mut broker, tmp.path(), quiet.id, "quiet");

    let report = broker.claim_lease(claimer.id, "src/auth.py", None).unwrap();

    assert!(report.accepted);
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    let warning = &report.warnings[0];
    assert_eq!(warning.session_id, quiet.id);
    assert_eq!(warning.holder_status.as_deref(), Some("stale"));
    assert!(
        warning
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("not live"),
        "{warning:?}"
    );
}

#[test]
fn an_implicit_lease_never_refuses_an_explicit_claim() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_editor = add_worktree(tmp.path(), "editor");
    let editor = broker.adopt(&wt_editor, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    std::fs::write(wt_editor.join("src/auth.py"), "edited elsewhere\n").unwrap();

    let report = broker.claim_lease(claimer.id, "src/", None).unwrap();

    assert!(report.accepted);
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    let warning = &report.warnings[0];
    assert_eq!(warning.session_id, editor.id);
    assert_eq!(warning.kind, LeaseKind::Implicit);
    assert!(
        warning
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("implicit leases never block"),
        "{warning:?}"
    );
}

#[test]
fn leases_implied_by_a_stale_sessions_old_edits_are_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_quiet = add_worktree(tmp.path(), "quiet");
    let quiet = broker.adopt(&wt_quiet, None).unwrap();
    let claimer = broker
        .adopt(&add_worktree(tmp.path(), "claimer"), None)
        .unwrap();
    std::fs::write(wt_quiet.join("src/auth.py"), "old edit\n").unwrap();
    broker.refresh_leases().unwrap();
    make_stale(&mut broker, tmp.path(), quiet.id, "quiet");

    let report = broker.claim_lease(claimer.id, "src/", None).unwrap();

    assert!(report.accepted);
    assert!(report.warnings.is_empty(), "{report:?}");
}

#[test]
fn a_planned_start_is_not_refused_by_a_stale_holder_or_under_verify_only() {
    // auto: a stale holder's explicit lease does not refuse a planned start.
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let quiet = broker
        .adopt(&add_worktree(tmp.path(), "quiet"), None)
        .unwrap();
    broker.claim_lease(quiet.id, "generated/", None).unwrap();
    make_stale(&mut broker, tmp.path(), quiet.id, "quiet");
    broker
        .start_worktree_with_planned_paths("rewrite", &["generated/policy.md".into()], None)
        .expect("a stale holder must not refuse a planned lease");

    // verify-only: even a working holder's explicit lease only informs.
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), Some(VERIFY_ONLY));
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker
        .start_worktree_with_planned_paths("first rewrite", &["generated/".into()], None)
        .unwrap();
    broker
        .start_worktree_with_planned_paths("second rewrite", &["generated/policy.md".into()], None)
        .expect("verify-only must not refuse a planned lease");
}

#[test]
fn guarded_exec_is_not_failed_by_a_stale_sessions_explicit_lease() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path(), None);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let quiet = broker
        .adopt(&add_worktree(tmp.path(), "quiet"), None)
        .unwrap();
    let wt_writer = add_worktree(tmp.path(), "writer");
    let writer = broker.adopt(&wt_writer, None).unwrap();
    broker.claim_lease(quiet.id, "src/auth.py", None).unwrap();
    make_stale(&mut broker, tmp.path(), quiet.id, "quiet");
    broker.claim_lease(writer.id, "src/auth.py", None).unwrap();

    let report = broker
        .guarded_exec(
            writer.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf 'rewritten\\n' > src/auth.py".into(),
            ],
        )
        .unwrap();

    assert!(report.command_success, "{report:?}");
    assert!(
        report.ok,
        "a stale holder must not fail the guard: {report:?}"
    );
}
