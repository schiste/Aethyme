//! Lease overlaps grouped by session pair, ranked by Git's merge condition,
//! announced once per pair, and blocking submit only on a real conflict with
//! a session that is actively working.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, OverlapSeverity};

const HOUR_MS: i64 = 60 * 60 * 1000;

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

/// Twenty numbered lines: far enough apart that edits to line 1 and line 20
/// are disjoint hunks Git merges cleanly.
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
    for name in ["auth.py", "other.py", "third.py"] {
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

fn write(worktree: &Path, file: &str, edit: (usize, &str)) {
    std::fs::write(worktree.join(file), numbered(Some(edit))).unwrap();
}

fn commit(worktree: &Path, message: &str) {
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", message]);
}

fn overlap_events(broker: &mut Broker) -> Vec<serde_json::Value> {
    broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "lease.overlap")
        .map(|event| serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap())
        .collect()
}

/// Make a session read as stale: its store activity and the worktree Git
/// metadata the broker reads for liveness both lie three hours back.
fn make_stale(broker: &mut Broker, root: &Path, session_id: i64, name: &str) {
    let three_hours_ago = aethyme_now_ms() - 3 * HOUR_MS;
    broker
        .store()
        .touch_session_activity(session_id, three_hours_ago)
        .unwrap();
    let when =
        std::time::SystemTime::now() - std::time::Duration::from_millis((3 * HOUR_MS) as u64);
    for file in ["index", "HEAD"] {
        let path = root.join(".git/worktrees").join(name).join(file);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

fn aethyme_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// One event per pair, however many paths it shares and however often it is
/// refreshed; a new event only when the pair's severity changes.
#[test]
fn a_pair_is_announced_once_and_again_only_when_its_severity_changes() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    broker.adopt(&wt_a, Some("a")).unwrap();
    broker.adopt(&wt_b, Some("b")).unwrap();

    // Three shared files, every edit in a disjoint hunk.
    for file in ["src/auth.py", "src/other.py", "src/third.py"] {
        write(&wt_a, file, (1, "a first"));
        write(&wt_b, file, (20, "b last"));
    }
    for _ in 0..5 {
        assert_eq!(
            broker.refresh_leases().unwrap().len(),
            3,
            "three paths overlap"
        );
    }
    let events = overlap_events(&mut broker);
    assert_eq!(
        events.len(),
        1,
        "one event for the pair, not per path or refresh: {events:?}"
    );
    assert_eq!(events[0]["severity"], "low");
    assert_eq!(events[0]["paths_count"], 3);

    // B now rewrites A's line: a real conflict on one path.
    write(&wt_b, "src/auth.py", (1, "b first"));
    broker.refresh_leases().unwrap();
    broker.refresh_leases().unwrap();
    let events = overlap_events(&mut broker);
    assert_eq!(
        events.len(),
        2,
        "severity change announces once: {events:?}"
    );
    assert_eq!(events[1]["severity"], "high");
    assert_eq!(
        events[1]["conflicting_paths"],
        serde_json::json!(["src/auth.py"])
    );
}

#[test]
fn disjoint_hunks_in_one_file_are_low_and_do_not_block_submit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_owner = add_worktree(tmp.path(), "owner");
    let wt_other = add_worktree(tmp.path(), "other");
    let owner = broker.adopt(&wt_owner, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    write(&wt_owner, "src/auth.py", (1, "owner edit"));
    write(&wt_other, "src/auth.py", (20, "other edit"));
    commit(&wt_other, "other");

    let pairs = {
        let overlaps = broker.refresh_leases().unwrap();
        broker.overlap_pairs_snapshot(&overlaps)
    };
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].severity, OverlapSeverity::Low, "{pairs:?}");
    assert!(pairs[0].classified, "{pairs:?}");

    let audit = broker.audit_submit_ownership(other.id).unwrap();
    assert!(audit.ok, "{audit:?}");
    assert!(audit.conflicting_leases.is_empty(), "{audit:?}");
    assert_eq!(audit.warned_leases.len(), 1, "{audit:?}");
    assert_eq!(audit.warned_leases[0].severity.as_deref(), Some("low"));
}

/// The owner's edit is uncommitted: classification must see it anyway.
#[test]
fn overlapping_hunks_with_an_active_holder_block_submit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_owner = add_worktree(tmp.path(), "owner");
    let wt_other = add_worktree(tmp.path(), "other");
    let owner = broker.adopt(&wt_owner, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    write(&wt_owner, "src/auth.py", (10, "owner rewrote line ten"));
    write(&wt_other, "src/auth.py", (10, "other rewrote line ten"));
    commit(&wt_other, "other");

    let audit = broker.audit_submit_ownership(other.id).unwrap();
    assert!(!audit.ok, "{audit:?}");
    assert_eq!(audit.conflicting_leases.len(), 1, "{audit:?}");
    assert_eq!(audit.conflicting_leases[0].session_id, owner.id);
    assert_eq!(
        audit.conflicting_leases[0].severity.as_deref(),
        Some("high")
    );
    assert!(
        audit.conflicting_leases[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("actively working")),
        "{audit:?}"
    );
}

/// The same conflict, but the holder has gone quiet: warn, never block. Also
/// proves classification does not touch the holder's index, which would
/// make it look active again.
#[test]
fn a_conflict_with_a_stale_holder_only_warns() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_owner = add_worktree(tmp.path(), "owner");
    let wt_other = add_worktree(tmp.path(), "other");
    let owner = broker.adopt(&wt_owner, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    broker.claim_lease(owner.id, "src/auth.py", None).unwrap();

    write(&wt_owner, "src/auth.py", (10, "owner rewrote line ten"));
    write(&wt_other, "src/auth.py", (10, "other rewrote line ten"));
    commit(&wt_other, "other");
    make_stale(&mut broker, tmp.path(), owner.id, "owner");

    let audit = broker.audit_submit_ownership(other.id).unwrap();
    assert!(audit.ok, "{audit:?}");
    assert_eq!(audit.warned_leases.len(), 1, "{audit:?}");
    assert_eq!(audit.warned_leases[0].severity.as_deref(), Some("high"));
    assert!(
        audit.warned_leases[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not actively working")),
        "{audit:?}"
    );
}

#[test]
fn a_directory_claim_over_a_file_its_holder_never_edited_is_low() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_owner = add_worktree(tmp.path(), "owner");
    let wt_other = add_worktree(tmp.path(), "other");
    let owner = broker.adopt(&wt_owner, None).unwrap();
    let other = broker.adopt(&wt_other, None).unwrap();
    broker.store().claim_lease(owner.id, "src/", None).unwrap();

    write(&wt_owner, "src/auth.py", (10, "owner edit"));
    write(&wt_other, "src/other.py", (10, "other edit"));
    commit(&wt_other, "other");

    let overlaps = broker.refresh_leases().unwrap();
    let pairs = broker.overlap_pairs_snapshot(&overlaps);
    assert_eq!(pairs.len(), 1, "{pairs:?}");
    assert_eq!(pairs[0].severity, OverlapSeverity::Low, "{pairs:?}");
    let audit = broker.audit_submit_ownership(other.id).unwrap();
    assert!(audit.ok, "{audit:?}");
}
