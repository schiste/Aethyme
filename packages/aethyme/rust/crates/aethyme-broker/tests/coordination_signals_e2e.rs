//! Coordination signals that should inform rather than stall agents:
//! duplicate sessions on one pull request, a worktree stuck mid-merge, lease
//! ignore rules read from the committed default branch, and a verify-only
//! submit that warns about a conflicting lease instead of refusing.
//!
//! The evidence is one repository's broker on 2026-10-01: pull request #1122
//! had three live repair sessions at once, one overlap could not be
//! classified because a worktree was mid-merge, and generated files every
//! session regenerates conflicted in nearly every pair.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, DuplicateWorkReason, SessionStatus};

const HOUR_MS: i64 = 60 * 60 * 1000;

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn sh(cwd: &Path, args: &[&str]) {
    let output = git(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn numbered(edit: Option<(usize, &str)>) -> String {
    (1..=20)
        .map(|line| match edit {
            Some((at, text)) if at == line => format!("{text}\n"),
            _ => format!("line {line}\n"),
        })
        .collect()
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    for name in ["auth.py", "other.py"] {
        std::fs::write(root.join("src").join(name), numbered(None)).unwrap();
    }
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

fn edit_and_commit(worktree: &Path, file: &str, edit: (usize, &str)) {
    std::fs::write(worktree.join(file), numbered(Some(edit))).unwrap();
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", edit.1]);
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
fn sessions_whose_tasks_name_the_same_pr_are_reported_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let first = broker
        .adopt(
            &add_worktree(tmp.path(), "first"),
            Some("Resolve merge conflicts and refresh PR #1122 against current main"),
        )
        .unwrap();
    let second = broker
        .adopt(
            &add_worktree(tmp.path(), "second"),
            Some("Repair and merge PR1122 first in authorized order"),
        )
        .unwrap();
    let unrelated = broker
        .adopt(
            &add_worktree(tmp.path(), "third"),
            Some("Fix PR 1181 diagnostics"),
        )
        .unwrap();

    for (me, other) in [(&first, &second), (&second, &first)] {
        let found = broker.duplicate_work_for(me);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].session_id, other.id);
        assert_eq!(found[0].reason, DuplicateWorkReason::TaskMentionsPr);
        assert_eq!(found[0].pull_request, Some(1122));
    }
    assert!(broker.duplicate_work_for(&unrelated).is_empty());

    let status = broker.status(now_ms()).unwrap();
    let rows: Vec<_> = status
        .advice
        .iter()
        .filter(|row| row.id == "session.duplicate-work")
        .collect();
    assert_eq!(rows.len(), 1, "one row per pair: {:?}", status.advice);
    assert_eq!(rows[0].severity.as_str(), "warning");
    assert!(rows[0].summary.contains(&format!("session {}", first.id)));
    assert!(rows[0].summary.contains(&format!("session {}", second.id)));
    assert!(
        rows[0]
            .commands
            .iter()
            .any(|command| command.contains("note send")),
        "{rows:?}"
    );
}

#[test]
fn a_stale_duplicate_is_reported_but_only_as_info() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let quiet = broker
        .adopt(&add_worktree(tmp.path(), "quiet"), Some("refresh PR #1149"))
        .unwrap();
    let live = broker
        .adopt(&add_worktree(tmp.path(), "live"), Some("finish PR #1149"))
        .unwrap();
    make_stale(&mut broker, tmp.path(), quiet.id, "quiet");

    let found = broker.duplicate_work_for(&live);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].status, SessionStatus::Stale);

    let status = broker.status(now_ms()).unwrap();
    let row = status
        .advice
        .iter()
        .find(|row| row.id == "session.duplicate-work")
        .expect("duplicate-work advice");
    assert_eq!(row.severity.as_str(), "info", "{row:?}");
}

#[test]
fn a_worktree_mid_merge_is_reported_instead_of_failing_classification() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_stuck = add_worktree(tmp.path(), "stuck");
    let wt_other = add_worktree(tmp.path(), "other");
    let stuck = broker.adopt(&wt_stuck, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    edit_and_commit(&wt_stuck, "src/auth.py", (10, "stuck rewrote line ten"));
    edit_and_commit(&wt_other, "src/auth.py", (10, "other rewrote line ten"));
    // Merging the other branch conflicts and leaves the merge half done.
    let merge = git(&wt_stuck, &["merge", "--no-edit", "agent/other"]);
    assert!(!merge.status.success(), "the fixture merge must conflict");

    let status = broker.status(now_ms()).unwrap();
    let pair = status
        .overlap_pairs
        .iter()
        .find(|pair| {
            [pair.session_a, pair.session_b] == [stuck.id.min(other.id), stuck.id.max(other.id)]
        })
        .expect("the two sessions overlap on src/auth.py");
    assert!(!pair.classified, "{pair:?}");
    assert_eq!(
        pair.reason,
        format!(
            "session {} is mid-merge with unresolved conflicts",
            stuck.id
        )
    );
    let row = status
        .advice
        .iter()
        .find(|row| row.id == "session.mid-merge")
        .expect("mid-merge advice");
    assert_eq!(row.session_id, Some(stuck.id));
    assert!(
        row.commands
            .iter()
            .any(|command| command.ends_with("merge --abort")),
        "{row:?}"
    );
}

#[test]
fn lease_ignore_rules_come_from_the_committed_default_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let origin = tmp.path().join("origin.git");
    std::fs::create_dir_all(&root).unwrap();
    init_repo(&root);
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "schema = 1\n\n[leases]\nignore = [\"src/auth.py\"]\n",
    )
    .unwrap();
    sh(&root, &["add", "-f", ".aethyme/config.toml"]);
    sh(&root, &["commit", "-qm", "ignore the generated file"]);
    sh(
        tmp.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    sh(
        &root,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    sh(&root, &["push", "-q", "-u", "origin", "main"]);
    sh(&root, &["remote", "set-head", "origin", "main"]);
    // The main checkout's file is stale: it has not pulled the rule.
    std::fs::write(root.join(".aethyme/config.toml"), "schema = 1\n").unwrap();

    let mut broker = Broker::open(&root).unwrap();
    let wt_a = add_worktree(&root, "a");
    let wt_b = add_worktree(&root, "b");
    broker.adopt(&wt_a, None).unwrap();
    broker.adopt(&wt_b, None).unwrap();
    edit_and_commit(&wt_a, "src/auth.py", (3, "a"));
    edit_and_commit(&wt_b, "src/auth.py", (17, "b"));

    let overlaps = broker.refresh_leases().unwrap();
    assert!(
        overlaps.iter().all(|overlap| overlap.path != "src/auth.py"),
        "the committed rule must apply although the checkout file lacks it: {overlaps:?}"
    );
}

#[test]
fn a_verify_only_submit_warns_about_a_conflicting_lease_instead_of_refusing() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "schema = 1\n\n[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_owner = add_worktree(tmp.path(), "owner");
    let wt_other = add_worktree(tmp.path(), "other");
    let owner = broker.adopt(&wt_owner, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();
    std::fs::write(
        wt_owner.join("src/auth.py"),
        numbered(Some((10, "owner rewrote line ten"))),
    )
    .unwrap();
    edit_and_commit(&wt_other, "src/auth.py", (10, "other rewrote line ten"));

    let audit = broker.audit_submit_ownership(other.id).unwrap();
    assert!(audit.ok, "verify-only must not refuse: {audit:?}");
    assert!(audit.conflicting_leases.is_empty(), "{audit:?}");
    assert_eq!(audit.warned_leases.len(), 1, "{audit:?}");
    let warning = &audit.warned_leases[0];
    assert_eq!(warning.session_id, owner.id);
    assert_eq!(warning.severity.as_deref(), Some("high"));
    let reason = warning.reason.as_deref().unwrap_or_default();
    assert!(reason.contains("verify-only"), "{reason}");
    assert!(reason.contains("note send"), "{reason}");
}
