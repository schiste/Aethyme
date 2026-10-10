//! The candidate boundary's legacy adapter (#663) against `submit` itself.
//!
//! Each test builds the candidate first, proves the build changed nothing a
//! submit would change, then runs the real submit on the same state and
//! compares: the tree, the baseline and the outcome class must agree.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::collaboration_archive::{CommitOid, snapshot_of_commit};
use aethyme_broker::composition::{
    CompositionMode, CompositionOutcome, Producer, Refusal, Unsupported,
};
use aethyme_broker::{Broker, BrokerOpError, MergeStatus};

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

fn fixture(promote_mode: Option<&str>) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Aethyme Test"]);
    git(
        root,
        &["config", "user.email", "aethyme-test@example.invalid"],
    );
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    std::fs::write(root.join("shared.txt"), "one\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    if let Some(mode) = promote_mode {
        std::fs::write(
            root.join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let broker = Broker::open(root).unwrap();
    (tmp, broker)
}

fn start(broker: &mut Broker, task: &str) -> (i64, PathBuf) {
    let session = broker.start_worktree(task, None).unwrap();
    (session.id, PathBuf::from(session.worktree_path))
}

fn commit(worktree: &Path, file: &str, content: &str) -> String {
    std::fs::write(worktree.join(file), content).unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-qm", &format!("edit {file}")]);
    git(worktree, &["rev-parse", "HEAD"])
}

/// Everything a submit writes: refs, worktrees, checkouts, the queue and the
/// event log.
fn world(root: &Path, broker: &mut Broker, sessions: &[&Path]) -> String {
    let mut out = String::new();
    out.push_str(&git(
        root,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    ));
    out.push_str(&git(root, &["rev-parse", "HEAD"]));
    out.push_str(&git(root, &["worktree", "list", "--porcelain"]));
    out.push_str(&git(root, &["status", "--porcelain", "--ignored"]));
    for session in sessions {
        out.push_str(&git(session, &["status", "--porcelain", "--ignored"]));
    }
    out.push_str(&format!("{:?}", broker.store().merge_queue().unwrap()));
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    out.push_str(&format!("events={}", events.len()));
    out
}

#[test]
fn a_clean_merge_builds_the_tree_submit_gates_and_promotes() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    let (b, b_path) = start(&mut broker, "b");
    let a_head = commit(&a_path, "a.txt", "from a\n");
    commit(&b_path, "b.txt", "from b\n");
    assert!(broker.submit(b).unwrap().promoted);

    let before = world(root, &mut broker, &[&a_path, &b_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path, &b_path]), before);

    let candidate = outcome.candidate().expect("a candidate").clone();
    assert_eq!(outcome.code(), "candidate");
    assert_eq!(candidate.producer, Producer::LegacyGitReplay);
    assert_eq!(candidate.producer.profile(), "legacy-git-replay/v0");
    assert_eq!(candidate.mode, CompositionMode::Text);
    assert_eq!(candidate.baseline_source, "integration");
    assert_eq!(candidate.inputs.len(), 1);
    assert_eq!(candidate.inputs[0].result.as_str(), a_head);
    assert_eq!(
        git(
            root,
            &[
                "rev-parse",
                &format!("{}^{{tree}}", candidate.commit.as_str())
            ]
        ),
        candidate.tree
    );
    assert_eq!(
        git(
            root,
            &["rev-parse", &format!("{}^", candidate.commit.as_str())]
        ),
        candidate.baseline.as_str()
    );

    let submitted = broker.submit(a).unwrap();
    assert!(submitted.promoted, "{submitted:?}");
    let verified = submitted.verified_against.expect("a verification base");
    assert_eq!(verified.commit, candidate.baseline.as_str());
    assert_eq!(verified.source, candidate.baseline_source);
    assert_eq!(
        submitted.entry.merged_tree.as_deref(),
        Some(candidate.tree.as_str())
    );
    let promoted = git(root, &["rev-parse", "aethyme/integration"]);
    let promoted = CommitOid::parse(&promoted).unwrap();
    assert_eq!(
        snapshot_of_commit(root, &promoted).unwrap().id(),
        candidate.subject
    );
}

#[test]
fn a_conflict_names_the_path_and_input_submit_rejects_and_no_source() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    let (b, b_path) = start(&mut broker, "b");
    let a_head = commit(&a_path, "shared.txt", "from a\n");
    commit(&b_path, "shared.txt", "from b\n");
    assert!(broker.submit(b).unwrap().promoted);
    let integration = git(root, &["rev-parse", "aethyme/integration"]);

    let before = world(root, &mut broker, &[&a_path, &b_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path, &b_path]), before);

    let CompositionOutcome::Conflict {
        baseline,
        conflicts,
    } = &outcome
    else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert_eq!(outcome.code(), "conflict");
    assert!(outcome.candidate().is_none());
    assert_eq!(baseline.as_str(), integration);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "shared.txt");
    assert_eq!(conflicts[0].input.as_str(), a_head);

    let submitted = broker.submit(a).unwrap();
    assert_eq!(submitted.entry.status, MergeStatus::Conflict);
    assert_eq!(submitted.conflicts, vec!["shared.txt".to_string()]);
    assert_eq!(submitted.conflict_details[0].originating_commit, a_head);
}

#[test]
fn work_already_on_the_baseline_is_no_change_as_submit_reports() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    git(&a_path, &["commit", "-q", "--allow-empty", "-m", "nothing"]);

    let before = world(root, &mut broker, &[&a_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path]), before);
    assert_eq!(outcome.code(), "no_change", "{outcome:?}");
    assert!(outcome.candidate().is_none());

    let submitted = broker.submit(a).unwrap();
    assert!(submitted.no_changes, "{submitted:?}");
    assert!(!submitted.promoted);
}

#[test]
fn a_merge_commit_is_unsupported_where_submit_refuses_it() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    git(&a_path, &["checkout", "-q", "-b", "side"]);
    commit(&a_path, "side.txt", "side\n");
    git(&a_path, &["checkout", "-q", "-"]);
    commit(&a_path, "a.txt", "a\n");
    git(
        &a_path,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    let before = world(root, &mut broker, &[&a_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path]), before);
    let CompositionOutcome::Unsupported { reason, .. } = &outcome else {
        panic!("expected unsupported, got {outcome:?}");
    };
    assert_eq!(*reason, Unsupported::MergeCommit);
    assert_eq!(outcome.code(), "merge_commit");

    match broker.submit(a) {
        Err(BrokerOpError::UnsupportedSubmissionCommit { .. }) => {}
        other => panic!("submit should refuse the merge commit: {other:?}"),
    }
}

/// Under `verify-only` the baseline is the fetched default branch, not the
/// integration branch, and neither the adapter nor submit moves anything.
#[test]
fn verify_only_builds_against_the_fetched_default_branch_like_submit() {
    let (tmp, mut broker) = fixture(Some("verify-only"));
    let root = tmp.path();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    let remote_path = remote.path().to_str().unwrap();
    git(root, &["remote", "add", "origin", remote_path]);
    git(root, &["push", "-q", "-u", "origin", "main"]);
    let (a, a_path) = start(&mut broker, "a");
    commit(&a_path, "a.txt", "a\n");

    // The default branch moves on the provider; the main checkout only
    // fetches it.
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["clone", "-q", remote_path, "."]);
    commit(other.path(), "upstream.txt", "merged elsewhere\n");
    git(other.path(), &["push", "-q", "origin", "main"]);
    git(root, &["fetch", "-q", "origin"]);
    let upstream = git(root, &["rev-parse", "origin/main"]);
    assert_ne!(upstream, git(root, &["rev-parse", "HEAD"]));

    let before = world(root, &mut broker, &[&a_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path]), before);
    let candidate = outcome.candidate().expect("a candidate").clone();
    assert_eq!(candidate.baseline_source, "upstream");
    assert_eq!(candidate.baseline.as_str(), upstream);

    let submitted = broker.submit(a).unwrap();
    assert!(!submitted.promoted);
    let verified = submitted.verified_against.expect("a verification base");
    assert_eq!(verified.source, "upstream");
    assert_eq!(verified.commit, upstream);
    assert_eq!(
        submitted.entry.merged_tree.as_deref(),
        Some(candidate.tree.as_str())
    );
}

/// A session whose base is behind a main checkout that moved on: submit
/// fast-forwards integration first, and the adapter computes the same base
/// without moving it.
#[test]
fn the_follows_main_refresh_is_computed_not_performed() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    commit(&a_path, "a.txt", "a\n");
    // Integration exists at init; main then moves.
    let (b, b_path) = start(&mut broker, "b");
    commit(&b_path, "b.txt", "b\n");
    assert!(broker.submit(b).unwrap().promoted);
    git(root, &["merge", "-q", "--ff-only", "aethyme/integration"]);
    commit(root, "main.txt", "main\n");
    let main_head = git(root, &["rev-parse", "HEAD"]);
    let integration = git(root, &["rev-parse", "aethyme/integration"]);
    assert_ne!(main_head, integration);

    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(
        git(root, &["rev-parse", "aethyme/integration"]),
        integration
    );
    let candidate = outcome.candidate().expect("a candidate").clone();
    assert_eq!(candidate.baseline.as_str(), main_head);

    let submitted = broker.submit(a).unwrap();
    assert_eq!(
        submitted
            .verified_against
            .expect("a verification base")
            .commit,
        main_head
    );
    assert_eq!(
        submitted.entry.merged_tree.as_deref(),
        Some(candidate.tree.as_str())
    );
}

/// Submit carries a gitlink Git can merge, but a #652 snapshot cannot name
/// one: the adapter says so instead of inventing a subject.
#[test]
fn a_candidate_a_snapshot_cannot_name_is_unsupported_not_unnamed() {
    let (tmp, mut broker) = fixture(None);
    let root = tmp.path();
    let (a, a_path) = start(&mut broker, "a");
    let oid = git(&a_path, &["rev-parse", "HEAD"]);
    git(
        &a_path,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},vendor/sub"),
        ],
    );
    git(&a_path, &["commit", "-qm", "add a gitlink"]);

    let before = world(root, &mut broker, &[&a_path]);
    let outcome = broker.legacy_candidate(a).unwrap();
    assert_eq!(world(root, &mut broker, &[&a_path]), before);
    let CompositionOutcome::Unsupported { reason, detail } = &outcome else {
        panic!("expected unsupported, got {outcome:?}");
    };
    assert_eq!(*reason, Unsupported::SnapshotEntry);
    assert!(detail.contains("vendor/sub"), "{detail}");
    assert!(outcome.candidate().is_none());
}

#[test]
fn refusal_codes_are_stable() {
    let codes = [
        (Refusal::UnknownBase, "unknown_base"),
        (Refusal::DependencyCycle, "dependency_cycle"),
        (Refusal::MissingInput, "missing_input"),
        (Refusal::CompetingRevisions, "competing_revisions"),
        (Refusal::BudgetExhausted, "budget_exhausted"),
        (Refusal::InseparableSelection, "inseparable_selection"),
        (Refusal::UnsafePlan, "unsafe_plan"),
        (Refusal::UntrustedPolicy, "untrusted_policy"),
    ];
    for (reason, code) in codes {
        let outcome = CompositionOutcome::Refused {
            reason,
            detail: String::new(),
        };
        assert_eq!(outcome.code(), code);
        assert!(outcome.candidate().is_none());
    }
    assert_eq!(Unsupported::SnapshotEntry.code(), "snapshot_entry");
}
