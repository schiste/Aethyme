//! #354: a repository-scoped cleanup audit that judges retained work by
//! content against a named snapshot of the authoritative target.
//!
//! Every fixture is a clone of a bare `origin`, so the target has a real
//! upstream that can move independently of the local default branch -- the
//! shape that made a stale local `main` a silent wrong baseline.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{AuditDisposition, Broker, CheckoutState, CleanupAudit, LandingEvidence};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", message]);
    git_out(dir, &["rev-parse", "HEAD"])
}

fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// A bare origin, a publisher clone standing in for the forge, and the
/// repository under audit. Returns (tmp, publisher, repo).
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let publisher = tmp.path().join("publisher");
    let repo = tmp.path().join("repo");
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "publisher"],
    );
    std::fs::write(publisher.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    std::fs::write(
        publisher.join("shared.txt"),
        "one\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    git(&publisher, &["add", "-A"]);
    git(&publisher, &["commit", "-qm", "init"]);
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "repo"],
    );
    (tmp, publisher, repo)
}

/// Land `session_worktree`'s net content on origin as one new commit, the way
/// a forge squash merge does: new SHA, no ancestry to the session.
fn squash_publish(publisher: &Path, session_worktree: &Path, files: &[&str]) {
    git(publisher, &["pull", "-q", "--ff-only"]);
    for file in files {
        std::fs::copy(session_worktree.join(file), publisher.join(file)).unwrap();
        git(publisher, &["add", file]);
    }
    git(publisher, &["commit", "-qm", "squashed delivery"]);
    git(publisher, &["push", "-q", "origin", "main"]);
}

fn item_for(audit: &CleanupAudit, session_id: i64) -> &aethyme_broker::AuditItem {
    audit
        .items
        .iter()
        .find(|item| {
            matches!(item.owner, aethyme_broker::AuditOwner::Session { session_id: id, .. } if id == session_id)
        })
        .unwrap_or_else(|| panic!("no item for session {session_id}: {audit:#?}"))
}

#[test]
fn squash_landing_is_in_target_against_upstream_while_local_main_is_stale() {
    let (_tmp, publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("squashed work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "a\n", "feature part one");
    commit(&worktree, "feature.txt", "a\nb\n", "feature part two");
    broker.close(session.id).unwrap();

    squash_publish(&publisher, &worktree, &["feature.txt"]);
    // A later unrelated change on the target must not hide the landing.
    commit(&publisher, "later.txt", "later\n", "unrelated later change");
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);

    let audit = broker.cleanup_audit().unwrap();
    assert_eq!(audit.target.proof_ref, "refs/remotes/origin/main");
    assert_eq!(
        audit.target.proof_commit,
        git_out(&repo, &["rev-parse", "origin/main"])
    );
    assert_eq!(audit.target.local_behind_upstream, 2);
    assert!(
        audit
            .warnings
            .iter()
            .any(|warning| warning.contains("behind refs/remotes/origin/main")),
        "a stale local main must be called out: {:?}",
        audit.warnings
    );

    let item = item_for(&audit, session.id);
    assert_eq!(item.disposition, AuditDisposition::InTarget, "{item:#?}");
    assert!(matches!(
        item.committed,
        Some(aethyme_broker::CommittedWork::InTarget {
            evidence: LandingEvidence::Content,
            landed_by: Some(_),
        })
    ));
    // The cleanup plan asks the same landing question (#408), so content
    // proof on the upstream makes the worktree removable without a separate
    // recorded representation.
    assert!(item.removable, "{item:#?}");
    assert!(audit.apply_command.is_some());
    assert_eq!(audit.summary.by_disposition["in_target"], 1);
}

#[test]
fn rebased_delivery_is_recognised_by_patch_identity() {
    let (_tmp, publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("rebased work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    let change = commit(
        &worktree,
        "shared.txt",
        "one\ntwo\nthree\nfour\nFIVE\n",
        "edit the last line",
    );
    broker.close(session.id).unwrap();

    // The target edited another line of the same file first, so no target
    // commit ever holds the session's exact blob: only the patch matches.
    commit(
        &publisher,
        "shared.txt",
        "ONE\ntwo\nthree\nfour\nfive\n",
        "edit the first line",
    );
    git(
        &publisher,
        &["fetch", "-q", repo.to_str().unwrap(), &session.branch],
    );
    git(&publisher, &["cherry-pick", &change]);
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);

    let audit = broker.cleanup_audit().unwrap();
    let item = item_for(&audit, session.id);
    assert_eq!(item.disposition, AuditDisposition::InTarget, "{item:#?}");
    assert!(matches!(
        item.committed,
        Some(aethyme_broker::CommittedWork::InTarget {
            evidence: LandingEvidence::PatchEquivalent,
            ..
        })
    ));
}

/// #408: the session commit was rebased onto a newer main and merged
/// through a pull request's merge commit, so it reached main only via a
/// second parent and with different blobs wherever the base had moved. The
/// representation scan, the audit and the cleanup plan must give one verdict.
#[test]
fn rebased_then_merged_work_gets_one_verdict_from_scan_audit_and_plan() {
    let (_tmp, publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("rebased then merged", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    let change = commit(
        &worktree,
        "shared.txt",
        "one\ntwo\nthree\nfour\nFIVE\n",
        "edit the last line",
    );
    broker.close(session.id).unwrap();

    // Main moves the same file outside the edit's context, then the rebased
    // copy lands on a branch that is merged with a merge commit.
    std::fs::write(
        publisher.join("shared.txt"),
        "one\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    commit(&publisher, "top.txt", "top\n", "an unrelated file first");
    std::fs::write(
        publisher.join("shared.txt"),
        "zero\none\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    git(&publisher, &["commit", "-qam", "prepend a line"]);
    git(&publisher, &["checkout", "-q", "-b", "pr"]);
    git(
        &publisher,
        &["fetch", "-q", repo.to_str().unwrap(), &session.branch],
    );
    git(&publisher, &["cherry-pick", &change]);
    let copy = git_out(&publisher, &["rev-parse", "HEAD"]);
    git(&publisher, &["checkout", "-q", "main"]);
    git(
        &publisher,
        &[
            "merge",
            "-q",
            "--no-ff",
            "pr",
            "-m",
            "Merge pull request #1",
        ],
    );
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    // Local main follows the upstream, as after a pull.
    git(&repo, &["merge", "-q", "--ff-only", "origin/main"]);

    let scan = broker.scan_session_representation(session.id).unwrap();
    let landing = scan.search.landing().expect("the scan finds the landing");
    assert_eq!(landing.evidence, LandingEvidence::PatchEquivalent);
    assert_eq!(landing.commit, copy);

    let audit = broker.cleanup_audit().unwrap();
    let item = item_for(&audit, session.id);
    assert_eq!(item.disposition, AuditDisposition::InTarget, "{item:#?}");
    assert!(matches!(
        item.committed,
        Some(aethyme_broker::CommittedWork::InTarget {
            evidence: LandingEvidence::PatchEquivalent,
            ..
        })
    ));
    assert!(item.removable, "{item:#?}");

    let plan = broker.cleanup_plan().unwrap();
    let planned = plan
        .worktrees
        .iter()
        .find(|planned| planned.session_id == session.id)
        .unwrap();
    assert!(planned.eligible(), "{planned:#?}");
    assert_eq!(
        planned
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.represented_by_commit.as_deref()),
        Some(copy.as_str())
    );
}

#[test]
fn unique_dirty_integration_only_and_live_are_distinct_dispositions() {
    let (_tmp, _publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();

    let unique = broker.start_worktree("unique work", None).unwrap();
    let unique_path = PathBuf::from(&unique.worktree_path);
    commit(&unique_path, "unique.txt", "mine\n", "unique commit");
    broker.close(unique.id).unwrap();

    let dirty = broker.start_worktree("dirty work", None).unwrap();
    let dirty_path = PathBuf::from(&dirty.worktree_path);
    std::fs::write(dirty_path.join("shared.txt"), "edited\n").unwrap();
    std::fs::write(dirty_path.join("scratch-1.txt"), "x\n").unwrap();
    std::fs::write(dirty_path.join("scratch-2.txt"), "y\n").unwrap();
    broker.close(dirty.id).unwrap();

    let promoted = broker.start_worktree("promoted work", None).unwrap();
    let promoted_path = PathBuf::from(&promoted.worktree_path);
    commit(&promoted_path, "promoted.txt", "done\n", "promoted commit");
    assert!(broker.submit(promoted.id).unwrap().promoted);
    broker.close(promoted.id).unwrap();

    // Pushed to its own remote branch (an open pull request's head): not in
    // the target, but not lost with the checkout either.
    let pushed = broker.start_worktree("pushed work", None).unwrap();
    let pushed_path = PathBuf::from(&pushed.worktree_path);
    commit(&pushed_path, "pushed.txt", "review me\n", "pushed commit");
    git(&pushed_path, &["push", "-q", "origin", &pushed.branch]);
    broker.close(pushed.id).unwrap();

    let live = broker.start_worktree("live work", None).unwrap();

    let audit = broker.cleanup_audit().unwrap();

    let item = item_for(&audit, unique.id);
    assert_eq!(item.disposition, AuditDisposition::WorktreeOnly);
    assert!(matches!(
        item.committed,
        Some(aethyme_broker::CommittedWork::WorktreeOnly { unique_commits: 1 })
    ));
    assert!(!item.removable);
    assert!(item.blocker.is_some());
    assert!(item.next_action.contains("git log --oneline"), "{item:#?}");

    let item = item_for(&audit, dirty.id);
    assert_eq!(item.disposition, AuditDisposition::Dirty);
    assert_eq!((item.tracked_changes, item.untracked_files), (1, 2));
    assert!(!item.removable);

    // Promoted to integration, never published upstream.
    let item = item_for(&audit, promoted.id);
    assert_eq!(
        item.disposition,
        AuditDisposition::IntegrationOnly,
        "{item:#?}"
    );
    assert!(matches!(
        &item.committed,
        Some(aethyme_broker::CommittedWork::IntegrationOnly { refs })
            if refs.iter().any(|name| name.contains("aethyme/integration"))
    ));

    let item = item_for(&audit, pushed.id);
    assert_eq!(
        item.disposition,
        AuditDisposition::RemoteBranchOnly,
        "{item:#?}"
    );
    assert!(matches!(
        &item.committed,
        Some(aethyme_broker::CommittedWork::RemoteBranchOnly { refs })
            if refs.iter().any(|name| name.ends_with(&pushed.branch))
    ));

    let item = item_for(&audit, live.id);
    assert_eq!(item.disposition, AuditDisposition::Live);
    assert!(item.next_action.contains("finish --session"));

    for (name, count) in [
        ("worktree_only", 1),
        ("dirty", 1),
        ("integration_only", 1),
        ("remote_branch_only", 1),
        ("live", 1),
    ] {
        assert_eq!(audit.summary.by_disposition[name], count, "{name}");
    }
    // Every retained item says why and what to do.
    for item in audit.items.iter().filter(|item| !item.removable) {
        assert!(item.blocker.is_some(), "{item:#?}");
        assert!(!item.next_action.is_empty());
    }
}

#[test]
fn missing_directories_metadata_and_unowned_roots_are_distinct_states() {
    let (tmp, _publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();

    // Session directory deleted behind Git's back: the branch still holds
    // its work and Git still lists the registration as prunable.
    let gone = broker.start_worktree("gone directory", None).unwrap();
    let gone_path = PathBuf::from(&gone.worktree_path);
    commit(
        &gone_path,
        "gone.txt",
        "kept on branch\n",
        "work on a vanished checkout",
    );
    broker.close(gone.id).unwrap();
    std::fs::remove_dir_all(&gone_path).unwrap();

    // Session directory present, Git registration removed.
    let unregistered = broker.start_worktree("unregistered", None).unwrap();
    let unregistered_path = PathBuf::from(&unregistered.worktree_path);
    broker.close(unregistered.id).unwrap();
    let admin = git_out(&unregistered_path, &["rev-parse", "--git-dir"]);
    std::fs::remove_dir_all(&admin).unwrap();

    // A registration no session owns, whose directory is gone.
    let stray = tmp.path().join("stray-checkout");
    git(
        &repo,
        &["worktree", "add", "-q", "--detach", stray.to_str().unwrap()],
    );
    std::fs::remove_dir_all(&stray).unwrap();

    // A directory under the broker root nobody claims.
    let root = gone_path.parent().unwrap();
    let orphan = root.join("orphan-dir");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("payload.bin"), vec![b'x'; 2048]).unwrap();

    let audit = broker.cleanup_audit().unwrap();

    let item = item_for(&audit, gone.id);
    assert_eq!(item.checkout, CheckoutState::MissingDirectory);
    assert_eq!(
        item.disposition,
        AuditDisposition::WorktreeOnly,
        "{item:#?}"
    );
    assert!(item.evidence.iter().any(|line| line.contains("prunable")));

    let item = item_for(&audit, unregistered.id);
    assert_eq!(
        item.checkout,
        CheckoutState::MissingGitMetadata,
        "{item:#?}"
    );
    assert!(
        item.evidence
            .iter()
            .any(|line| line.contains("uncommitted state cannot be read")),
        "{item:#?}"
    );

    let prunable = audit
        .items
        .iter()
        .find(|item| item.checkout == CheckoutState::PrunableRegistration)
        .unwrap_or_else(|| panic!("no prunable registration: {audit:#?}"));
    assert_eq!(
        prunable.disposition,
        AuditDisposition::MissingCheckoutMetadata
    );
    assert!(
        prunable
            .next_action
            .contains("git worktree prune --dry-run")
    );

    let unowned = audit
        .items
        .iter()
        .find(|item| item.checkout == CheckoutState::UnownedRoot)
        .unwrap_or_else(|| panic!("no unowned root: {audit:#?}"));
    assert_eq!(unowned.bytes, Some(2048));
    assert_eq!(unowned.disposition, AuditDisposition::UnknownProvenance);
    assert!(!unowned.removable);
    assert!(orphan.exists(), "the audit must never remove anything");
}

#[test]
fn target_advancement_after_review_invalidates_the_apply_digest() {
    let (_tmp, publisher, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("represented work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "done.txt", "done\n", "done");
    assert!(broker.submit(session.id).unwrap().promoted);
    broker.close(session.id).unwrap();
    drop(broker);

    let reviewed = cli(&repo, &["finish", "cleanup", "audit", "--json"]);
    assert!(
        reviewed.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&reviewed.stdout).unwrap();
    let item = reviewed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["owner"]["session_id"].as_i64() == Some(session.id))
        .unwrap();
    assert_eq!(item["removable"], true, "{item}");
    let digest = reviewed["cleanup_plan_digest"].as_str().unwrap().to_owned();
    assert!(
        reviewed["apply_command"]
            .as_str()
            .unwrap()
            .ends_with(&digest)
    );
    assert!(reviewed["target"]["proof_commit"].is_string());

    // The target moves after review.
    commit(&publisher, "other.txt", "other\n", "someone else lands");
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);

    let stale = cli(
        &repo,
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(!stale.status.success(), "a stale digest must not apply");
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("no longer matches current state"),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(worktree.exists());

    // Re-reviewing yields a fresh digest that does apply.
    let fresh = cli(&repo, &["finish", "cleanup", "audit", "--json"]);
    let fresh: serde_json::Value = serde_json::from_slice(&fresh.stdout).unwrap();
    let fresh_digest = fresh["cleanup_plan_digest"].as_str().unwrap().to_owned();
    assert_ne!(fresh_digest, digest);
    let apply = cli(
        &repo,
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &fresh_digest,
            "--json",
        ],
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(!worktree.exists());
}

#[test]
fn audit_is_scoped_to_the_named_repository_and_renders_a_summary() {
    let (_tmp, _publisher, repo) = fixture();
    let other = tempfile::tempdir().unwrap();
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("scoped", None).unwrap();
    broker.close(session.id).unwrap();
    drop(broker);

    // Run from an unrelated directory, naming the repository explicitly.
    let output = cli(
        other.path(),
        &[
            "finish",
            "cleanup",
            "audit",
            "--repo",
            repo.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Cleanup audit:"), "{text}");
    assert!(
        text.contains("target: refs/remotes/origin/main at "),
        "{text}"
    );
    assert!(text.contains("in_target 1"), "{text}");
    assert!(text.contains(&format!("session {}", session.id)), "{text}");

    let refused = cli(&repo, &["finish", "cleanup", "audit", "12"]);
    assert!(!refused.status.success());
}
