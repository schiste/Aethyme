use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{
    Broker, EntryExposureResolutionKind, EntryExposureState, IntegrationReconcileClassification,
    IntegrationReconcileCommitOrigin, IntegrationReconcileEquivalence, IntegrationReconcileOptions,
    MergeStatus,
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn commit(root: &Path, path: &str, content: &str, message: &str) -> String {
    std::fs::write(root.join(path), content).unwrap();
    git(root, &["add", path]);
    git(root, &["commit", "-qm", message]);
    git(root, &["rev-parse", "HEAD"])
}

struct DeployDivergenceFixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    old_integration: String,
    upstream_head: String,
    promoted_commit: String,
    promoted_entry_id: i64,
    functional_upstream: String,
    unrecorded_commit: String,
    pending_head: String,
    pending_entry_id: i64,
}

impl DeployDivergenceFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let deploy = tmp.path().join("deploy");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
        std::fs::write(repo.join("src/service.txt"), "feature=off\n").unwrap();
        std::fs::write(repo.join("docs/CHANGELOG.md"), "Release 0\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "initial"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-qu", "origin", "main"]);

        let mut broker = Broker::open(&repo).unwrap();
        let session = broker.start_worktree("functional promotion").unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        commit(
            &worktree,
            "src/service.txt",
            "feature=on\n",
            "enable feature",
        );
        let promoted = broker.submit(session.id).unwrap();
        assert!(promoted.promoted);
        let promoted_commit = serde_json::from_str::<serde_json::Value>(
            promoted.entry.details_json.as_deref().unwrap(),
        )
        .unwrap()["commit"]
            .as_str()
            .unwrap()
            .to_string();

        git(&repo, &["switch", "aethyme/integration"]);
        let unrecorded_commit = commit(
            &repo,
            "docs/CHANGELOG.md",
            "Release candidate\n",
            "manually resolve release notes",
        );
        let old_integration = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["switch", "main"]);

        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                deploy.to_str().unwrap(),
            ],
        );
        let functional_upstream = commit(
            &deploy,
            "src/service.txt",
            "feature=on\n",
            "deploy equivalent feature",
        );
        commit(
            &deploy,
            "docs/CHANGELOG.md",
            "Release 1\n",
            "write release notes during deploy",
        );
        git(&deploy, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin", "main"]);
        let upstream_head = git(&repo, &["rev-parse", "origin/main"]);

        std::fs::write(
            repo.join(".aethyme/config.toml"),
            "[promote]\nmode = \"manual\"\n",
        )
        .unwrap();
        let pending = broker.start_worktree("pending queue work").unwrap();
        let pending_worktree = PathBuf::from(&pending.worktree_path);
        let pending_head = commit(
            &pending_worktree,
            "src/pending.txt",
            "pending=true\n",
            "pending queue change",
        );
        let pending_outcome = broker.submit(pending.id).unwrap();
        assert_eq!(pending_outcome.entry.status, MergeStatus::Verified);

        Self {
            _tmp: tmp,
            repo,
            old_integration,
            upstream_head,
            promoted_commit,
            promoted_entry_id: promoted.entry.id,
            functional_upstream,
            unrecorded_commit,
            pending_head,
            pending_entry_id: pending_outcome.entry.id,
        }
    }
}

#[test]
fn patch_equivalent_promotion_is_landed_when_local_main_already_equals_upstream() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let remote = tmp.path().join("remote.git");
    let deploy = tmp.path().join("deploy");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "-q", "-b", "main"]);
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
    std::fs::write(repo.join("src/service.txt"), "feature=off\nmode=base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "initial"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-qu", "origin", "main"]);

    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("promote feature").unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(
        &worktree,
        "src/service.txt",
        "feature=on\nmode=base\n",
        "enable feature",
    );
    let promoted = broker.submit(session.id).unwrap();
    assert!(promoted.promoted);
    let promoted_commit =
        serde_json::from_str::<serde_json::Value>(promoted.entry.details_json.as_deref().unwrap())
            .unwrap()["commit"]
            .as_str()
            .unwrap()
            .to_string();

    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            deploy.to_str().unwrap(),
        ],
    );
    let equivalent_upstream = commit(
        &deploy,
        "src/service.txt",
        "feature=on\nmode=base\n",
        "land equivalent feature",
    );
    let upstream_head = commit(
        &deploy,
        "src/service.txt",
        "feature=on\nmode=deployed\n",
        "configure deployed mode",
    );
    git(&deploy, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin", "main"]);
    git(&repo, &["merge", "--ff-only", "origin/main"]);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), upstream_head);

    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");
    let entry = dry_run
        .entries
        .iter()
        .find(|entry| entry.queue_entry_id == promoted.entry.id)
        .unwrap();
    assert_eq!(
        entry.classification,
        IntegrationReconcileClassification::AlreadyLanded
    );
    assert_eq!(
        entry.upstream_landing.as_deref(),
        Some(equivalent_upstream.as_str())
    );
    assert!(entry.replayed_commit.is_none());
    assert_eq!(dry_run.new_integration, upstream_head);

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: dry_run.plan_digest,
        })
        .unwrap();
    assert!(applied.applied);
    assert_eq!(
        git(&repo, &["rev-parse", "aethyme/integration"]),
        upstream_head
    );
    assert_eq!(
        broker
            .store()
            .merge_queue()
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == promoted.entry.id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    assert_ne!(promoted_commit, equivalent_upstream);
}

#[test]
fn reconciliation_plan_classifies_every_relevant_commit_with_full_provenance() {
    let fixture = DeployDivergenceFixture::new();
    let mut broker = Broker::open(&fixture.repo).unwrap();
    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();

    let commits = &report.plan.commits;
    assert_eq!(report.plan.common_base.len(), 40);
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UpstreamOnlyExternalWork
            })
            .count(),
        2
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::RecordedPromotedWork
            })
            .count(),
        1
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            })
            .count(),
        1
    );
    assert_eq!(
        commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::PendingQueueEntry
            })
            .count(),
        1
    );

    let promoted = commits
        .iter()
        .find(|commit| commit.commit == fixture.promoted_commit)
        .unwrap();
    assert_eq!(
        promoted.equivalence,
        IntegrationReconcileEquivalence::PatchEquivalent
    );
    assert_eq!(
        promoted.matching_commits,
        vec![fixture.functional_upstream.clone()]
    );
    let upstream_copy = commits
        .iter()
        .find(|commit| commit.commit == fixture.functional_upstream)
        .unwrap();
    assert_eq!(
        upstream_copy.equivalence,
        IntegrationReconcileEquivalence::PatchEquivalent
    );
    assert_eq!(
        upstream_copy.matching_commits,
        vec![fixture.promoted_commit.clone()]
    );

    let unrecorded = commits
        .iter()
        .find(|commit| commit.commit == fixture.unrecorded_commit)
        .unwrap();
    assert_eq!(
        unrecorded.equivalence,
        IntegrationReconcileEquivalence::None
    );
    assert_eq!(unrecorded.files, vec!["docs/CHANGELOG.md"]);

    let pending = commits
        .iter()
        .find(|commit| commit.commit == fixture.pending_head)
        .unwrap();
    assert_eq!(pending.queue_entry_id, Some(fixture.pending_entry_id));
    assert_eq!(pending.queue_status, Some(MergeStatus::Verified));
    assert_eq!(pending.equivalence, IntegrationReconcileEquivalence::None);
    assert!(commits.iter().all(|commit| commit.commit.len() == 40));
    let json = serde_json::to_value(&report.plan).unwrap();
    assert_eq!(
        json["commits"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|commit| commit["origin"] == "pending_queue_entry")
            .count(),
        1
    );
}

#[test]
fn reviewed_unrecorded_resolutions_are_commit_bound_and_never_blanket_discards() {
    let fixture = DeployDivergenceFixture::new();
    let resolution_path = fixture.repo.join("resolution.json");
    let resolution = |disposition: &str, upstream_commit: Option<&str>| {
        serde_json::json!({
            "schema_version": 2,
            "upstream_ref": "origin/main",
            "upstream_commit": fixture.upstream_head,
            "old_integration": fixture.old_integration,
            "operator": "release-operator@example.test",
            "unrecorded_resolutions": [
                {
                    "integration_commit": fixture.unrecorded_commit,
                    "disposition": disposition,
                    "upstream_commit": upstream_commit,
                    "reason": "reviewed the deploy-written changelog replacement"
                }
            ]
        })
    };

    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&resolution("preserve_and_replay", None)).unwrap(),
    )
    .unwrap();
    let mut broker = Broker::open(&fixture.repo).unwrap();
    let preserved = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap();
    let reviewed = preserved
        .plan
        .commits
        .iter()
        .find(|commit| commit.commit == fixture.unrecorded_commit)
        .unwrap()
        .unrecorded_resolution
        .as_ref()
        .unwrap();
    assert_eq!(reviewed.disposition.as_str(), "preserve_and_replay");
    assert!(reviewed.upstream_commit.is_none());
    assert!(
        preserved
            .warnings
            .iter()
            .any(|warning| warning.contains("validated reviewed dispositions"))
    );
    assert!(!preserved.safe);
    assert_eq!(
        preserved
            .plan
            .commits
            .iter()
            .find(|commit| commit.commit == fixture.unrecorded_commit)
            .unwrap()
            .conflicts,
        vec!["docs/CHANGELOG.md"]
    );

    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&resolution(
            "replaced_by_exact_upstream_sha",
            Some(&fixture.upstream_head),
        ))
        .unwrap(),
    )
    .unwrap();
    let replaced = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap();
    let reviewed = replaced
        .plan
        .commits
        .iter()
        .find(|commit| commit.commit == fixture.unrecorded_commit)
        .unwrap()
        .unrecorded_resolution
        .as_ref()
        .unwrap();
    assert_eq!(
        reviewed.disposition.as_str(),
        "replaced_by_exact_upstream_sha"
    );
    assert_eq!(
        reviewed.upstream_commit.as_deref(),
        Some(fixture.upstream_head.as_str())
    );

    for disposition in ["drop_because_content_empty", "discard_unknown_work"] {
        std::fs::write(
            &resolution_path,
            serde_json::to_vec_pretty(&resolution(disposition, None)).unwrap(),
        )
        .unwrap();
        let error = broker
            .reconcile_integration(IntegrationReconcileOptions {
                upstream: "origin/main".into(),
                apply: false,
                resolution_file: Some(resolution_path.clone()),
                confirm: None,
            })
            .unwrap_err();
        if disposition == "drop_because_content_empty" {
            assert!(error.to_string().contains("is not content-empty"));
        } else {
            assert!(error.to_string().contains("invalid JSON"));
        }
    }
}

#[test]
fn confirmed_reconciliation_rebuilds_from_upstream_and_journals_the_reviewed_digest() {
    let fixture = DeployDivergenceFixture::new();
    let resolution_path = fixture.repo.join("resolution.json");
    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 2,
            "upstream_ref": "origin/main",
            "upstream_commit": fixture.upstream_head,
            "old_integration": fixture.old_integration,
            "operator": "release-operator@example.test",
            "unrecorded_resolutions": [{
                "integration_commit": fixture.unrecorded_commit,
                "disposition": "replaced_by_exact_upstream_sha",
                "upstream_commit": fixture.upstream_head,
                "reason": "the deploy-authored release commit is the reviewed replacement"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut broker = Broker::open(&fixture.repo).unwrap();
    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");
    let digest = dry_run.plan_digest.clone().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        broker
            .store()
            .entry_path_exposure(fixture.promoted_entry_id)
            .unwrap()
            .unwrap()
            .state,
        EntryExposureState::Outstanding,
        "a reviewed dry-run is not publication evidence"
    );

    let missing = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap_err();
    assert!(missing.to_string().contains(&digest));
    let mismatch = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path.clone()),
            confirm: Some("0".repeat(64)),
        })
        .unwrap_err();
    assert!(mismatch.to_string().contains("confirmation mismatch"));
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        fixture.old_integration
    );

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path),
            confirm: Some(digest.clone()),
        })
        .unwrap();
    assert!(applied.safe && applied.applied, "{applied:#?}");
    assert_eq!(applied.new_integration, fixture.upstream_head);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        fixture.upstream_head
    );
    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == fixture.promoted_entry_id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == fixture.pending_entry_id)
            .unwrap()
            .status,
        MergeStatus::Verified
    );
    let database = rusqlite::Connection::open(fixture.repo.join(".aethyme/broker.db")).unwrap();
    let journaled: String = database
        .query_row(
            "SELECT plan_digest FROM integration_reconciliations ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(journaled, digest);
    let exposure = broker
        .store()
        .entry_path_exposure(fixture.promoted_entry_id)
        .unwrap()
        .unwrap();
    assert_eq!(exposure.state, EntryExposureState::Resolved);
    assert_eq!(
        exposure.resolution_kind,
        Some(EntryExposureResolutionKind::ExternalReconciliation)
    );
    assert_eq!(
        exposure.resolution_sha.as_deref(),
        Some(fixture.functional_upstream.as_str())
    );
}

#[test]
fn reviewed_unrecorded_work_is_replayed_in_integration_order() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
    std::fs::write(repo.join("src/service.txt"), "feature=off\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "initial"]);

    let mut broker = Broker::open(repo).unwrap();
    let session = broker.start_worktree("pending functional work").unwrap();
    commit(
        Path::new(&session.worktree_path),
        "src/service.txt",
        "feature=on\n",
        "enable feature",
    );
    assert!(broker.submit(session.id).unwrap().promoted);
    git(repo, &["switch", "aethyme/integration"]);
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    let unrecorded = commit(
        repo,
        "docs/operator-note.md",
        "keep this operator decision\n",
        "record operator decision",
    );
    let old_integration = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["switch", "main"]);

    git(repo, &["switch", "-qc", "external-upstream", "main"]);
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    let upstream = commit(
        repo,
        "docs/release-note.md",
        "release 1\n",
        "deploy release note",
    );
    git(repo, &["update-ref", "refs/remotes/origin/main", &upstream]);
    git(repo, &["switch", "main"]);

    let resolution_path = repo.join("resolution.json");
    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 2,
            "upstream_ref": "origin/main",
            "upstream_commit": upstream,
            "old_integration": old_integration,
            "operator": "release-operator@example.test",
            "unrecorded_resolutions": [{
                "integration_commit": unrecorded,
                "disposition": "preserve_and_replay",
                "reason": "this operator-authored decision is not represented upstream"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");
    let reviewed = dry_run
        .plan
        .commits
        .iter()
        .find(|commit| commit.commit == unrecorded)
        .unwrap();
    assert!(reviewed.replayed_commit.is_some());
    assert_eq!(
        reviewed.execution_evidence.as_deref(),
        Some("reviewed unrecorded delta replays cleanly")
    );

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path),
            confirm: dry_run.plan_digest.clone(),
        })
        .unwrap();
    assert!(applied.safe && applied.applied, "{applied:#?}");
    assert_eq!(
        git(repo, &["show", "aethyme/integration:docs/release-note.md"]),
        "release 1"
    );
    assert_eq!(
        git(repo, &["show", "aethyme/integration:docs/operator-note.md"]),
        "keep this operator decision"
    );
    assert_eq!(
        git(repo, &["show", "aethyme/integration:src/service.txt"]),
        "feature=on"
    );
}

#[test]
fn deploy_written_main_divergence_is_blocked_by_unrecorded_integration_work() {
    let fixture = DeployDivergenceFixture::new();
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "diff",
                "--name-only",
                "origin/main",
                "aethyme/integration",
                "--",
                "src/service.txt",
            ],
        ),
        "",
        "functional work is equivalent across the dual identities"
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "diff",
                "--name-only",
                "origin/main",
                "aethyme/integration",
                "--",
                "docs/CHANGELOG.md",
            ],
        ),
        "docs/CHANGELOG.md",
        "deploy-written release notes are the genuine content divergence"
    );

    let mut broker = Broker::open(&fixture.repo).unwrap();
    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();

    assert!(!report.safe);
    assert!(!report.applied);
    assert!(report.entries.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.contains("unrecorded work")
            || warning.contains("not a contiguous promoted queue layer")
    }));
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        fixture.old_integration
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/main"]),
        fixture.upstream_head
    );
}
