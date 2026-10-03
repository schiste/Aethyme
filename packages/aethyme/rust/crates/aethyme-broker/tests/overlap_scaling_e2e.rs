//! Overlap classification cost grows with sessions, not session pairs, and
//! leaves stale sessions out.
//!
//! Measured on one repository with 77 sessions (66 stale) and 224
//! overlapping pairs: a lease refresh read every session's state once per
//! pair it was in, about 450 Git-heavy reads per `status`, and rescanned the
//! diff of every stale session. On a loaded host `status` took 81 minutes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use aethyme_broker::{BUDGET_EXHAUSTED_REASON, Broker, overlap_state_reads};

const HOUR_MS: i64 = 60 * 60 * 1000;

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

fn numbered(edit: Option<(usize, &str)>) -> String {
    (1..=40)
        .map(|line| match edit {
            Some((at, text)) if at == line => format!("{text}\n"),
            _ => format!("line {line}\n"),
        })
        .collect()
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/shared.py"), numbered(None)).unwrap();
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

/// Make a session read as stale: store activity and the worktree Git
/// metadata the broker reads for liveness both lie three hours back.
fn make_stale(broker: &mut Broker, root: &Path, session_id: i64, name: &str) {
    broker
        .store()
        .touch_session_activity(session_id, now_ms() - 3 * HOUR_MS)
        .unwrap();
    let when = std::time::SystemTime::now() - Duration::from_millis((3 * HOUR_MS) as u64);
    for file in ["index", "HEAD"] {
        let path = root.join(".git/worktrees").join(name).join(file);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

/// `count` sessions, each editing its own line of one shared file, so every
/// pair overlaps on that file and merges cleanly.
fn sessions_sharing_one_file(root: &Path, broker: &mut Broker, count: usize) -> Vec<(i64, String)> {
    (0..count)
        .map(|index| {
            let name = format!("s{index}");
            let worktree = add_worktree(root, &name);
            let session = broker.adopt(&worktree, Some(&name)).unwrap();
            std::fs::write(
                worktree.join("src/shared.py"),
                numbered(Some((index + 1, &format!("edit by {name}")))),
            )
            .unwrap();
            (session.id, name)
        })
        .collect()
}

// The state-read counter is process-wide, so these tests run as one test to
// keep their counts from interleaving.
#[test]
fn classification_scales_with_sessions_skips_stale_ones_and_honours_its_budget() {
    reads_each_session_once_however_many_pairs_it_is_in();
    stale_sessions_are_neither_rescanned_nor_paired();
    an_exhausted_budget_leaves_pairs_unclassified_and_says_why();
}

fn reads_each_session_once_however_many_pairs_it_is_in() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let sessions = 6;
    sessions_sharing_one_file(tmp.path(), &mut broker, sessions);

    let before = overlap_state_reads();
    let overlaps = broker.refresh_leases().unwrap();
    let reads = overlap_state_reads() - before;
    let pairs = broker.overlap_pairs_snapshot(&overlaps);

    assert_eq!(pairs.len(), sessions * (sessions - 1) / 2, "{pairs:?}");
    assert!(pairs.iter().all(|pair| pair.classified), "{pairs:?}");
    assert_eq!(
        reads,
        sessions,
        "one state read per session, not two per pair ({} pairs)",
        pairs.len()
    );
}

fn stale_sessions_are_neither_rescanned_nor_paired() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let all = sessions_sharing_one_file(tmp.path(), &mut broker, 5);
    // Record every session's lease while all are live, then let two go
    // quiet for three hours.
    broker.refresh_leases().unwrap();
    let stale: Vec<i64> = all[..2].iter().map(|(id, _)| *id).collect();
    for (id, name) in &all[..2] {
        make_stale(&mut broker, tmp.path(), *id, name);
    }

    let before = overlap_state_reads();
    let overlaps = broker.refresh_leases().unwrap();
    let reads = overlap_state_reads() - before;

    assert!(
        overlaps
            .iter()
            .all(|overlap| !stale.contains(&overlap.session_a)
                && !stale.contains(&overlap.session_b)),
        "a stale session is not paired: {overlaps:?}"
    );
    assert_eq!(overlaps.len(), 3, "the three working sessions pair up");
    assert_eq!(reads, 3, "only the working sessions' state is read");
    // Their recorded leases are kept as data.
    let leases = broker.store().active_leases().unwrap();
    for id in &stale {
        assert!(
            leases.iter().any(|lease| lease.session_id == *id),
            "stale session {id} keeps its lease"
        );
    }
    let snapshot = broker.lease_overlaps_snapshot().unwrap();
    assert_eq!(snapshot, overlaps, "the snapshot pairs the same sessions");
}

fn an_exhausted_budget_leaves_pairs_unclassified_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    sessions_sharing_one_file(tmp.path(), &mut broker, 3);
    let overlaps = broker.lease_overlaps_snapshot().unwrap();
    let overlaps = if overlaps.is_empty() {
        // Leases are recorded by a refresh; classify that set again below.
        broker.refresh_leases().unwrap();
        broker.lease_overlaps_snapshot().unwrap()
    } else {
        overlaps
    };
    // Forget the verdicts the refresh above produced.
    broker
        .store()
        .meta_set("lease.overlap.pairs.v1", "{}")
        .unwrap();

    let before = overlap_state_reads();
    let pairs = broker
        .classify_and_announce_overlaps_within(&overlaps, Duration::ZERO)
        .unwrap();
    assert_eq!(overlap_state_reads(), before, "no Git work past the budget");
    assert_eq!(pairs.len(), 3, "{pairs:?}");
    for pair in &pairs {
        assert!(!pair.classified, "{pair:?}");
        assert_eq!(pair.reason, BUDGET_EXHAUSTED_REASON, "{pair:?}");
    }

    // The next pass with time to spare classifies them.
    let pairs = broker
        .classify_and_announce_overlaps_within(&overlaps, Duration::from_secs(60))
        .unwrap();
    assert!(pairs.iter().all(|pair| pair.classified), "{pairs:?}");
}

/// `status` and a lease refresh must not rewrite a stale session's index.
/// The broker reads that file's mtime as evidence the agent is working, so a
/// `git status` that refreshed it made merely looking at a stale session
/// revive it for the next two hours, and every later pass paired it again.
#[test]
fn looking_at_a_stale_session_does_not_revive_it() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let all = sessions_sharing_one_file(tmp.path(), &mut broker, 2);
    broker.refresh_leases().unwrap();
    let (stale_id, stale_name) = &all[0];
    make_stale(&mut broker, tmp.path(), *stale_id, stale_name);
    // A file edited after the index was written is what makes Git want to
    // refresh the index's stat data.
    let worktree = tmp.path().join(".aethyme/worktrees").join(stale_name);
    std::fs::write(
        worktree.join("src/shared.py"),
        numbered(Some((30, "edited again"))),
    )
    .unwrap();
    let index = tmp
        .path()
        .join(".git/worktrees")
        .join(stale_name)
        .join("index");
    let before = std::fs::metadata(&index).unwrap().modified().unwrap();

    for _ in 0..2 {
        broker.status(now_ms()).unwrap();
        broker.refresh_leases().unwrap();
    }

    let after = std::fs::metadata(&index).unwrap().modified().unwrap();
    assert_eq!(before, after, "the stale session's index was rewritten");
    let agents = broker.agents(now_ms()).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.session.id == *stale_id)
        .unwrap();
    assert_eq!(
        agent.derived_status,
        aethyme_broker::SessionStatus::Stale,
        "looking at the session revived it"
    );
}

/// Leaving stale sessions out of a refresh must not leave a session that
/// comes back after a long pause without leases for its own new work: its
/// submit audit rescans it even though it still reads as stale.
#[test]
fn a_session_back_from_a_long_pause_still_owns_its_new_work() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let name = "returning";
    let worktree = add_worktree(tmp.path(), name);
    let session = broker.adopt(&worktree, Some(name)).unwrap();
    broker.refresh_leases().unwrap();
    make_stale(&mut broker, tmp.path(), session.id, name);

    // New work committed while the broker last saw the session three hours
    // ago; committing moves its HEAD, so age the metadata again after.
    std::fs::write(worktree.join("src/new.py"), "new = 1\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "new work"]);
    make_stale(&mut broker, tmp.path(), session.id, name);

    let audit = broker.audit_submit_ownership(session.id).unwrap();
    assert!(
        audit.missing_lease_paths.is_empty(),
        "its own new path is leased: {audit:?}"
    );
}
