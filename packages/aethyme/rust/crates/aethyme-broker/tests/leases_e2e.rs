//! End-to-end lease tests (issues #11, #12, #14): two real worktrees,
//! diff-derived leases, overlap warnings, ignore rules, and
//! new-overlap-only event emission.

use std::path::Path;
use std::process::Command;

use aethyme_broker::Broker;

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

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.py"), "a\n").unwrap();
    std::fs::write(root.join("src/other.py"), "b\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
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

#[test]
fn overlapping_edits_warn_once_and_lockfiles_never_warn() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    let session_a = broker.adopt(&wt_a, Some("task a")).unwrap();
    let session_b = broker.adopt(&wt_b, Some("task b")).unwrap();

    // Both touch only a lockfile → ignore rules keep this quiet (#14).
    std::fs::write(wt_a.join("pnpm-lock.yaml"), "x\n").unwrap();
    std::fs::write(wt_b.join("pnpm-lock.yaml"), "y\n").unwrap();
    assert!(broker.refresh_leases().unwrap().is_empty());

    // Disjoint edits → leases exist, no overlap.
    std::fs::write(wt_a.join("src/auth.py"), "edited by a\n").unwrap();
    std::fs::write(wt_b.join("src/other.py"), "edited by b\n").unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.is_empty());
    assert_eq!(broker.store().active_leases().unwrap().len(), 2);

    // B edits the same file as A → overlap detected, event emitted.
    std::fs::write(wt_b.join("src/auth.py"), "edited by b too\n").unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].session_a, session_a.id.min(session_b.id));
    assert_eq!(overlaps[0].path, "src/auth.py");

    // Refresh again: overlap still reported, but the event fired ONCE.
    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let overlap_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "lease.overlap")
        .collect();
    assert_eq!(
        overlap_events.len(),
        1,
        "known overlaps are not re-announced"
    );
    assert!(
        overlap_events[0]
            .payload_json
            .as_deref()
            .unwrap()
            .contains("src/auth.py")
    );

    // A reverts its edit → overlap clears on the next refresh.
    std::fs::write(wt_a.join("src/auth.py"), "a\n").unwrap();
    sh(&wt_a, &["checkout", "--", "src/auth.py"]);
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.is_empty());
}

#[test]
fn explicit_directory_claim_overlaps_other_sessions_files() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    let session_a = broker.adopt(&wt_a, None).unwrap();
    let _session_b = broker.adopt(&wt_b, None).unwrap();

    // A claims the whole src/ directory explicitly; B edits a file in it.
    broker
        .store()
        .claim_lease(session_a.id, "src/", None)
        .unwrap();
    std::fs::write(wt_b.join("src/other.py"), "edit\n").unwrap();

    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].path, "src/other.py");

    // Config-driven extra ignore rule silences a path (#14).
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[leases]\nignore = [\"src/\"]\n",
    )
    .unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert!(
        overlaps.is_empty(),
        "config-ignored prefix keeps implicit leases (and thus overlaps) out"
    );
}

// ── #41: leases derive from merge-base, self-healing after rebases ─────

#[test]
fn rebase_onto_integration_does_not_inflate_leases() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Session 1 promotes work on file_one — integration now contains it.
    let wt1 = add_worktree(tmp.path(), "one");
    let s1 = broker.adopt(&wt1, Some("one")).unwrap();
    std::fs::write(wt1.join("file_one.txt"), "one\n").unwrap();
    sh(&wt1, &["add", "-A"]);
    sh(&wt1, &["commit", "-qm", "one"]);
    assert!(broker.submit(s1.id).unwrap().promoted);

    // Session 2 rebases onto the integration branch (as the
    // action-required flow instructs), bringing session 1's promoted
    // commit into its history, then does its own work on file_two.
    let wt2 = add_worktree(tmp.path(), "two");
    let s2 = broker.adopt(&wt2, Some("two")).unwrap();
    sh(&wt2, &["fetch", ".", "aethyme/integration"]);
    sh(&wt2, &["rebase", "FETCH_HEAD"]);
    std::fs::write(wt2.join("file_two.txt"), "two\n").unwrap();
    sh(&wt2, &["add", "-A"]);
    sh(&wt2, &["commit", "-qm", "two"]);

    broker.refresh_leases().unwrap();
    let leases = broker.store().active_leases().unwrap();
    let s2_paths: Vec<&str> = leases
        .iter()
        .filter(|l| l.session_id == s2.id)
        .map(|l| l.path.as_str())
        .collect();
    assert!(
        s2_paths.contains(&"file_two.txt"),
        "own work must be leased: {s2_paths:?}"
    );
    assert!(
        !s2_paths.contains(&"file_one.txt"),
        "promoted work brought in by the rebase must NOT appear as a \
         phantom lease (#41): {s2_paths:?}"
    );
}
