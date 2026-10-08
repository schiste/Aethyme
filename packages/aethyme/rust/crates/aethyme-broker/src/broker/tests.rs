use super::{
    DoctorRepairStatus, DoctorReport, RepairCommandOutput, VersionRepairReport,
    WORKTREE_CARGO_CONFIG, active_version_matches, execute_version_repair_steps,
    local_cli_repair_step_specs, slugify, write_worktree_build_defaults,
};
use crate::types::{MergeQueueEntry, Session, SessionOrigin, SessionStatus};
use crate::version::{BinaryBuild, VersionDriftReport, VersionDriftStatus};
use std::path::{Path, PathBuf};

#[test]
fn routine_inventory_zero_budget_is_incomplete_and_cannot_authorize_cleanup() {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-qm", "init"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let mut broker = super::Broker::open(repo.path()).unwrap();
    let session = broker.start_worktree("retained", None).unwrap();
    broker
        .store()
        .set_session_status(session.id, SessionStatus::Closed, None)
        .unwrap();
    let (plan, deferred) = broker
        .cleanup_plan_observed_with_budget(std::time::Duration::ZERO)
        .unwrap();
    assert_eq!(deferred, 1);
    assert!(plan.digest.is_empty());
    assert!(plan.worktrees.is_empty());
    assert_eq!(plan.eligible_worktree_count, 0);
    assert!(std::path::Path::new(&session.worktree_path).exists());
}

#[test]
fn a_degraded_status_snapshot_keeps_git_refs_unknown_after_its_deadline() {
    let (repo, _) = landing_fixture();
    let broker = super::Broker::open(repo.path()).unwrap();
    let _expired = crate::git::limit_git_until(
        std::time::Instant::now() - std::time::Duration::from_millis(1),
    );
    let started = std::time::Instant::now();

    let report = broker
        .status_current_snapshot(0)
        .expect("expired Git inspection returns a partial snapshot");

    assert!(
        report
            .deferred_checks
            .iter()
            .any(|check| check == "git_refs"),
        "{report:#?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "snapshot outlived the expired inspection deadline: {:?}",
        started.elapsed()
    );
}

/// The status deadline bounds reads. A mutation that starts inside it --
/// the verify-only integration refresh status runs -- must not have its
/// Git writes cut off midway, so it runs outside that deadline.
#[test]
fn an_integration_refresh_is_not_cut_off_by_the_status_deadline() {
    let (repo, git) = landing_fixture();
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-qm", "ignore broker state"]);
    let local_main = git(&["rev-parse", "HEAD"]);
    git(&["update-ref", "refs/heads/aethyme/integration", &base]);
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::write(
        repo.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    let remote = tempfile::tempdir().unwrap();
    let remote_path = remote.path().to_str().unwrap();
    git(&["init", "-q", "--bare", "-b", "main", remote_path]);
    git(&["remote", "add", "origin", remote_path]);
    git(&["push", "-q", "origin", "main"]);
    // Upstream moves past local main, which the merge of a pull request
    // on the provider leaves behind.
    git(&["commit", "--allow-empty", "-qm", "merged upstream"]);
    let upstream = git(&["rev-parse", "HEAD"]);
    git(&["push", "-q", "origin", "main"]);
    git(&["reset", "-q", "--hard", &local_main]);
    let mut broker = super::Broker::open(repo.path()).unwrap();

    let _expired = crate::git::limit_git_until(
        std::time::Instant::now() - std::time::Duration::from_millis(1),
    );
    let report = broker
        .auto_cleanup_landed_integration("origin/main")
        .expect("the refresh runs its Git commands to completion");

    assert_eq!(
        report.state,
        crate::AutomaticIntegrationCleanupState::Cleaned,
        "{report:#?}"
    );
    assert_eq!(git(&["rev-parse", "aethyme/integration"]), upstream);
    assert!(
        crate::git::active_git_deadline().is_some(),
        "the caller's deadline is restored afterwards"
    );
}

/// A temporary repository with a fixed identity, so `git commit` works
/// on a runner with no global configuration.
fn landing_fixture() -> (tempfile::TempDir, impl Fn(&[&str]) -> String) {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().to_path_buf();
    let git = move |args: &[&str]| -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
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
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "--allow-empty", "-qm", "base"]);
    (repo, git)
}

#[test]
fn a_merged_head_is_proved_by_ancestry_before_any_candidate_walk() {
    let (repo, git) = landing_fixture();
    let base = git(&["rev-parse", "HEAD"]);
    // A target that does not contain the head but gained commits since
    // the fork: a candidate walk over it has something to examine.
    git(&["switch", "-q", "-c", "diverged", &base]);
    for n in 0..3 {
        std::fs::write(repo.path().join(format!("other{n}.txt")), "x\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", &format!("other {n}")]);
    }
    let diverged = git(&["rev-parse", "HEAD"]);
    git(&["switch", "-q", "-c", "feature", &base]);
    std::fs::write(repo.path().join("feature.txt"), "work\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "feature"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["switch", "-q", "main"]);
    git(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
    let merged = git(&["rev-parse", "HEAD"]);

    let broker = super::Broker::open(repo.path()).unwrap();
    // An expired deadline turns any candidate walk into an error, so
    // success here proves none ran -- with the non-containing target
    // deliberately listed first.
    broker.landing_deadline.set(Some(std::time::Instant::now()));
    let landed = broker
        .landing_on_delivery_targets(&head, &[diverged, merged.clone()])
        .unwrap()
        .expect("a merged head is landed");
    assert_eq!(landed.0, merged);
    assert_eq!(landed.1, crate::LandingEvidence::Ancestry);
    assert_eq!(landed.2, None);
}

#[test]
fn a_squash_landing_still_reaches_the_deep_proof() {
    let (repo, git) = landing_fixture();
    let base = git(&["rev-parse", "HEAD"]);
    git(&["switch", "-q", "-c", "feature", &base]);
    std::fs::write(repo.path().join("feature.txt"), "work\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "feature"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["switch", "-q", "main"]);
    git(&["merge", "-q", "--squash", "feature"]);
    git(&["commit", "-qm", "squash feature"]);
    let squashed = git(&["rev-parse", "HEAD"]);

    let broker = super::Broker::open(repo.path()).unwrap();
    let landed = broker
        .landing_on_delivery_targets(&head, std::slice::from_ref(&squashed))
        .unwrap()
        .expect("a squashed head is landed");
    assert_eq!(landed.1, crate::LandingEvidence::Content);
    assert_eq!(landed.2.as_deref(), Some(squashed.as_str()));
}

#[test]
fn worktree_build_defaults_are_written_once_and_never_over_a_choice() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join(".cargo/config.toml");

    write_worktree_build_defaults(tmp.path()).unwrap();
    let written = std::fs::read_to_string(&config).unwrap();
    assert_eq!(written, WORKTREE_CARGO_CONFIG);
    assert!(written.contains("incremental = false"));
    assert!(written.contains("debug = \"line-tables-only\""));

    // Emptying the file is how an operator turns this off. Rewriting it on
    // the next session start would make that impossible to express.
    std::fs::write(&config, "").unwrap();
    write_worktree_build_defaults(tmp.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "");

    // Nothing temporary is left behind for a later scan to puzzle over.
    let leftovers = std::fs::read_dir(tmp.path().join(".cargo"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name() != "config.toml")
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn a_worktree_directory_is_never_mistaken_for_broker_infrastructure() {
    // `is_worktree_root_infrastructure` skips dotted entries on the
    // grounds that `slugify` cannot produce one. If that ever stops being
    // true, the sweep silently stops reporting a whole class of drift.
    for task in [
        ".hidden",
        "...",
        "-.-",
        "   .leading space",
        "\u{2022} bullet",
        "",
    ] {
        let slug = super::slugify(task);
        assert!(
            !std::path::Path::new(&slug)
                .file_name()
                .map(super::is_worktree_root_infrastructure)
                .unwrap_or(false),
            "slugify({task:?}) produced {slug:?}, which the sweep would skip"
        );
    }
}

#[test]
fn slugify_is_safe_for_branches_and_paths() {
    assert_eq!(slugify("Fix auth bug!"), "fix-auth-bug");
    assert_eq!(slugify("  weird///name  "), "weird-name");
    assert_eq!(slugify("émojis 🎉 stripped"), "mojis-stripped");
    assert_eq!(slugify(""), "task");
    assert!(slugify(&"x".repeat(100)).len() <= 40);
}

#[test]
fn integration_based_start_requires_an_explicit_pull_request_target() {
    assert!(super::Broker::reject_integration_based_review_task(None).is_ok());

    match super::Broker::reject_integration_based_review_task(Some(42)) {
        Err(super::BrokerOpError::ReviewRequiresPullRequestHead { pull_request }) => {
            assert_eq!(pull_request, 42)
        }
        other => panic!("expected a PR-head refusal, got {other:?}"),
    }
}

#[test]
fn doctor_healthy_accepts_successful_explicit_version_repair() {
    let report = doctor_report(
        VersionDriftStatus::BehindIntegration,
        Some(repair_report(DoctorRepairStatus::Pass)),
    );

    assert!(report.healthy());
}

#[test]
fn doctor_healthy_rejects_failed_explicit_version_repair() {
    let report = doctor_report(
        VersionDriftStatus::BehindIntegration,
        Some(repair_report(DoctorRepairStatus::Fail)),
    );

    assert!(!report.healthy());
}

#[test]
fn version_repair_targets_and_verifies_both_required_binaries() {
    let source = std::path::Path::new("/tmp/aethyme-release-source");
    let install_bin = std::path::Path::new("/tmp/cargo-root/bin");

    let specs = local_cli_repair_step_specs(Some(source), Some(install_bin));

    assert_eq!(specs.len(), 6);
    assert_eq!(
        specs
            .iter()
            .map(|spec| (spec.component, spec.action))
            .collect::<Vec<_>>(),
        vec![
            ("router", "install"),
            ("router", "verify"),
            ("router", "verify-active"),
            ("engine", "install"),
            ("engine", "verify"),
            ("engine", "verify-active"),
        ]
    );
    assert!(specs[0].command.join(" ").contains("aethyme-cli"));
    assert_eq!(
        specs[1].command,
        vec!["/tmp/cargo-root/bin/aethyme", "--version"]
    );
    assert_eq!(specs[2].command, vec!["aethyme", "--version"]);
    assert!(specs[3].command.join(" ").contains("aethyme-engine"));
    assert_eq!(
        specs[4].command,
        vec!["/tmp/cargo-root/bin/aethyme-engine-cli", "--version"]
    );
    assert_eq!(specs[5].command, vec!["aethyme-engine-cli", "--version"]);
}

#[test]
fn version_repair_requires_every_install_and_verification_step() {
    let specs = local_cli_repair_step_specs(None, None);

    for failed_index in 0..specs.len() {
        let mut observed = 0;
        let steps = execute_version_repair_steps(&specs, |_| {
            let current = observed;
            observed += 1;
            Ok(RepairCommandOutput {
                success: current != failed_index,
                exit_code: Some(if current == failed_index { 7 } else { 0 }),
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        assert_eq!(observed, 6, "all outcomes must remain observable");
        assert!(!steps.iter().all(|step| step.success));
        assert_eq!(steps.iter().filter(|step| !step.success).count(), 1);
        assert_eq!(steps[failed_index].exit_code, Some(7));
    }
}

#[test]
fn active_version_verification_rejects_a_shadowing_path_binary() {
    let command = vec!["aethyme".into(), "--version".into()];
    assert!(!active_version_matches(
        &command,
        "aethyme 0.2.2 (v0.2.2-2-gf8a2e3a)",
        "ec334dfadcc1b692a02507306b3d9cb9052472a3",
    ));
    assert!(active_version_matches(
        &command,
        "aethyme 0.2.2 (v0.2.2-24-gec334df)",
        "ec334dfadcc1b692a02507306b3d9cb9052472a3",
    ));
}

#[test]
fn integration_movement_advice_points_to_wait_stable() {
    let agent = super::AgentView {
        session: session(56),
        activity_at: 0,
        derived_status: SessionStatus::Active,
        pid_alive: None,
    };

    let advice =
        super::integration_movement_advice("aethyme/integration", "abcdef1234567890", &[agent]);

    assert_eq!(advice.id, "integration.may-move");
    assert_eq!(advice.severity, super::StatusAdviceSeverity::Notice);
    assert!(
        advice
            .commands
            .contains(&"aethyme broker advanced integration wait-stable --seconds 30".into())
    );
}

#[test]
fn promoted_clean_finish_advice_points_to_finish() {
    let agent = super::AgentView {
        session: session(69),
        activity_at: 0,
        derived_status: SessionStatus::Idle,
        pid_alive: None,
    };
    let entry = queue_entry(104, 69, crate::MergeStatus::Promoted);

    let advice = super::promoted_clean_finish_advice(&agent, &entry);

    assert_eq!(advice.id, "session.promoted-clean-finish");
    assert_eq!(advice.severity, super::StatusAdviceSeverity::Notice);
    assert_eq!(advice.queue_entry_id, Some(104));
    assert_eq!(
        advice.commands,
        vec!["aethyme broker finish --session 69".to_string()]
    );
    assert!(advice.summary.contains("session 69 is promoted and clean"));
}

fn current_summary_integration() -> super::SummaryIntegration {
    super::SummaryIntegration {
        branch: "aethyme/integration".into(),
        head: "a".repeat(40),
        baseline_ref: "refs/heads/main".into(),
        baseline_head: "a".repeat(40),
        relation: super::StatusIntegrationRelation::CurrentWithMain,
        ahead_baseline_commits: 0,
        main_head: "a".repeat(40),
        main_is_ancestor: true,
        ahead_main_commits: 0,
        promotes: true,
    }
}

/// The #374 field report: local main was fast-forwarded to integration
/// and not pushed. The baseline count is right -- publishing would still
/// add those commits -- but the summary must name what it counts against
/// and say the local checkout already matches, or it reads as a stale
/// contradiction of `integration status`.
#[test]
fn summary_names_its_baseline_after_an_unpushed_local_fast_forward() {
    let integration = super::SummaryIntegration {
        baseline_ref: "refs/remotes/origin/main".into(),
        baseline_head: "b".repeat(40),
        relation: super::StatusIntegrationRelation::AheadOfMain,
        ahead_baseline_commits: 12,
        ..current_summary_integration()
    };
    let summary = super::status_summary(
        &[],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &integration,
    );
    assert!(
            summary.message.contains(
                "aethyme/integration ahead of origin/main by 12 commits (local checkout matches integration)"
            ),
            "{}",
            summary.message
        );
    assert_eq!(summary.integration_ahead_main_commits, 12);
    assert_eq!(summary.integration_ahead_local_main_commits, 0);
    assert_eq!(summary.main_head, summary.integration_head);
    assert_eq!(summary.baseline_ref, "refs/remotes/origin/main");
}

#[test]
fn summary_reports_a_local_checkout_behind_integration() {
    let integration = super::SummaryIntegration {
        baseline_ref: "refs/remotes/origin/main".into(),
        baseline_head: "b".repeat(40),
        relation: super::StatusIntegrationRelation::AheadOfMain,
        ahead_baseline_commits: 5,
        main_head: "c".repeat(40),
        ahead_main_commits: 3,
        ..current_summary_integration()
    };
    let summary = super::status_summary(
        &[],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &integration,
    );
    assert!(
        summary.message.contains(
            "ahead of origin/main by 5 commits (local checkout 3 commits behind integration)"
        ),
        "{}",
        summary.message
    );
}

#[test]
fn affected_gate_timings_name_each_phase_over_budget() {
    let timings = super::AffectedGatePhaseTimings {
        graph_read: super::GATES_AFFECTED_PHASE_BUDGET_MS + 1,
        manifest: super::GATES_AFFECTED_PHASE_BUDGET_MS,
        selection: super::GATES_AFFECTED_PHASE_BUDGET_MS + 5,
        lock_wait: super::GATES_AFFECTED_PHASE_BUDGET_MS + 1,
    };

    assert_eq!(
        timings.over_budget_phases(),
        vec!["graph_read", "selection", "lock_wait"]
    );
}

/// Without a default branch the baseline is the checkout itself, which
/// must not be called "main".
#[test]
fn a_head_fallback_baseline_is_named_checkout() {
    let integration = super::SummaryIntegration {
        baseline_ref: "HEAD".into(),
        ..current_summary_integration()
    };
    let summary = super::status_summary(
        &[],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &integration,
    );
    assert!(
        summary
            .message
            .contains("aethyme/integration current with checkout;"),
        "{}",
        summary.message
    );
}

#[test]
fn status_summary_explains_single_active_session_risk() {
    let agent = super::AgentView {
        session: session(70),
        activity_at: 0,
        derived_status: SessionStatus::Active,
        pid_alive: None,
    };

    let summary = super::status_summary(
        &[agent],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &current_summary_integration(),
    );

    assert_eq!(summary.live_sessions, 1);
    assert_eq!(summary.active_sessions, 1);
    assert!(summary.may_move_integration);
    assert_eq!(
        summary.message,
        "1 active session; no overlaps; aethyme/integration current with main; active session may promote new integration work"
    );
    assert_eq!(
        summary.commands,
        vec!["aethyme broker advanced integration wait-stable --seconds 30"]
    );
}

/// Verify-only: live sessions submit, but nothing they do moves
/// integration, so neither the flag nor the commands may say it can.
#[test]
fn verify_only_summary_does_not_say_sessions_move_integration() {
    let agent = super::AgentView {
        session: session(71),
        activity_at: 0,
        derived_status: SessionStatus::Active,
        pid_alive: None,
    };
    let integration = super::SummaryIntegration {
        relation: super::StatusIntegrationRelation::AheadOfMain,
        ahead_baseline_commits: 3,
        promotes: false,
        ..current_summary_integration()
    };

    let summary = super::status_summary(
        &[agent],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &integration,
    );

    assert!(!summary.may_move_integration);
    assert!(
        summary
            .message
            .ends_with("; verify-only: submit does not move integration"),
        "{}",
        summary.message
    );
    assert!(summary.commands.is_empty(), "{:?}", summary.commands);
}

#[test]
fn status_summary_explains_no_active_submitters() {
    let summary = super::status_summary(
        &[],
        0,
        super::OverlapPairCounts::default(),
        0,
        0,
        &current_summary_integration(),
    );

    assert_eq!(summary.live_sessions, 0);
    assert_eq!(summary.active_sessions, 0);
    assert!(!summary.may_move_integration);
    assert_eq!(
        summary.message,
        "no live sessions; no overlaps; aethyme/integration current with main; no active submitters"
    );
    assert!(summary.commands.is_empty());
}

#[test]
fn cleanup_retention_severity_scales_by_count_bytes_and_policy_age() {
    assert_eq!(
        super::cleanup_retention_severity(1, 1024, 1, 7, 1_073_741_824),
        super::StatusAdviceSeverity::Notice
    );
    assert_eq!(
        super::cleanup_retention_severity(5, 1024, 1, 7, 1_073_741_824),
        super::StatusAdviceSeverity::Warning
    );
    assert_eq!(
        super::cleanup_retention_severity(1, 1024, 1, 7, 1024),
        super::StatusAdviceSeverity::Warning
    );
    assert_eq!(
        super::cleanup_retention_severity(1, 1024, 7, 7, 0),
        super::StatusAdviceSeverity::Warning
    );
    assert_eq!(
        super::cleanup_retention_severity(1, u64::MAX, 1, 7, 0),
        super::StatusAdviceSeverity::Notice
    );

    let mut retention = super::CleanupRetention {
        eligibility_checked: true,
        inventory_complete: true,
        inventory_deferred_sessions: 0,
        gate_headroom_deferred: false,
        closed_worktrees: Default::default(),
        reconciliation: crate::WorktreeReconciliation {
            schema_version: crate::WORKTREE_RECONCILIATION_SCHEMA_VERSION,
            complete: true,
            scanned_root_count: 0,
            directory_count: 0,
            claimed_count: 0,
            unclaimed_count: 0,
            unclaimed_bytes: 0,
            sized: false,
            unclaimed: Vec::new(),
        },
        broker_owned_worktree_count: 1,
        retained_session_branch_count: 1,
        eligible_worktree_count: 0,
        estimated_retained_bytes: 2048,
        estimated_reclaimable_bytes: 0,
        estimated_blocked_bytes: 2048,
        retained_bytes_budget: 1024,
        over_retained_bytes_budget: true,
        retained_bytes_deficit: 1024,
        clears_retained_bytes_budget: false,
        budget_verdict: crate::BudgetVerdict::Over,
        unmeasured_worktree_count: 0,
        sizes_measured_at_ms: None,
        oldest_closed_age_days: 1,
        closed_worktrees_policy_days: 7,
        // The cases below are about the per-repository budget; the volume
        // is reported by its own advisory, pinned by
        // `gate_headroom_advice_fires_exactly_below_the_gate_threshold`.
        host_available_bytes: None,
        host_volume_probe: None,
        host_inode_volume_probe: None,
        inodes_free: None,
        worktree_inodes: 0,
        worktree_inode_unmeasured: 0,
        severity: super::StatusAdviceSeverity::Warning,
        retention_config: Default::default(),
    };
    let unmeetable = super::cleanup_retention_warning(&retention).unwrap();
    assert!(unmeetable.contains("aethyme broker gc plan"));
    // The deficit, not the total, is what an operator has to act on.
    assert!(unmeetable.contains("1024 bytes over"));
    assert!(unmeetable.contains("cannot be met by cleanup alone"));

    // Same overage, but reclaimable work covers it: a backlog, not a wall.
    retention.estimated_reclaimable_bytes = 2048;
    retention.clears_retained_bytes_budget = true;
    let backlog = super::cleanup_retention_warning(&retention).unwrap();
    assert!(backlog.contains("would clear it"));
    assert!(!backlog.contains("cannot be met"));

    // A floor may report the breach and must not promise the cure: the
    // bytes nobody walked all count against the deficit and none of them
    // are known to be reclaimable.
    retention.unmeasured_worktree_count = 2;
    let floor = super::cleanup_retention_warning(&retention).unwrap();
    assert!(floor.contains("at least 1024 bytes over"));
    assert!(floor.contains("2 retained worktrees have never been sized"));
    assert!(!floor.contains("would clear it"));
    retention.unmeasured_worktree_count = 0;

    // Within budget there is nothing to warn about at all.
    retention.over_retained_bytes_budget = false;
    assert!(super::cleanup_retention_warning(&retention).is_none());
}

/// Gates refuse below the headroom and start at it, so the advisory must
/// fire on exactly the same side of the same number -- one byte either way
/// would have status and gates describing two different disks.
#[test]
fn gate_headroom_probes_capture_bytes_and_inodes_in_one_stat_read_each() {
    let probes = vec![PathBuf::from("/bytes"), PathBuf::from("/inodes")];
    let reads = std::cell::Cell::new(0);
    let read = |path: &Path| {
        reads.set(reads.get() + 1);
        if path == Path::new("/bytes") {
            Some(crate::disk_headroom::DiskHeadroom {
                bytes: 1,
                inodes: Some(10),
            })
        } else {
            Some(crate::disk_headroom::DiskHeadroom {
                bytes: 10,
                inodes: Some(1),
            })
        }
    };
    let headroom = super::lowest_headroom_with(&probes, read);
    assert_eq!(
        headroom.bytes,
        Some(super::HeadroomReading {
            path: PathBuf::from("/bytes"),
            available: 1,
        })
    );
    assert_eq!(
        headroom.inodes,
        Some(super::HeadroomReading {
            path: PathBuf::from("/inodes"),
            available: 1,
        })
    );
    assert_eq!(reads.get(), probes.len());
}

#[test]
fn gate_headroom_advice_fires_exactly_below_the_gate_threshold() {
    let required = crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES;
    let probe = std::path::Path::new("/host/state/worktrees/repo");

    let inode_threshold = crate::disk_headroom::MIN_GATE_HEADROOM_INODES;
    let ample_inodes = Some(inode_threshold * 2);
    let starved = super::gate_headroom_advice(
        Some(required - 1),
        Some(probe),
        ample_inodes,
        Some(probe),
        required,
        inode_threshold,
    )
    .expect("one byte under the gate threshold blocks every gate");
    assert_eq!(starved.id, "host.gate-headroom");
    assert_eq!(starved.severity, super::StatusAdviceSeverity::Blocked);
    assert!(
        starved.summary.contains("/host/state/worktrees/repo"),
        "the advisory must name the volume it measured: {}",
        starved.summary
    );
    assert_eq!(
        starved.commands,
        vec![
            "aethyme broker gc reclaim plan",
            "aethyme broker gc plan",
            "aethyme broker gc storage plan"
        ]
    );

    let at_threshold = super::gate_headroom_advice(
        Some(required),
        Some(probe),
        ample_inodes,
        Some(probe),
        required,
        inode_threshold,
    )
    .expect("exactly the requirement still warns: one build from refusing");
    assert_eq!(
        at_threshold.id, "host.disk-low",
        "exactly the requirement lets a gate start, so it must not report a blocked gate"
    );
    assert_eq!(at_threshold.severity, super::StatusAdviceSeverity::Warning);
    assert_eq!(at_threshold.commands, starved.commands);
    let warn_below = required * super::DISK_LOW_WARNING_MULTIPLE;
    assert_eq!(
        super::gate_headroom_advice(
            Some(warn_below - 1),
            None,
            ample_inodes,
            None,
            required,
            inode_threshold
        )
        .map(|advice| advice.id),
        Some("host.disk-low")
    );
    assert!(
        super::gate_headroom_advice(
            Some(warn_below),
            None,
            ample_inodes,
            None,
            required,
            inode_threshold
        )
        .is_none()
    );
    // Unknown fails open, for the same reason `refusal` does.
    let inode_starved = super::gate_headroom_advice(
        Some(required * 2),
        None,
        Some(inode_threshold - 1),
        Some(probe),
        required,
        inode_threshold,
    )
    .expect("low free inodes blocks every gate");
    assert_eq!(inode_starved.id, "host.gate-headroom");
    assert!(
        inode_starved
            .summary
            .contains("inodes on /host/state/worktrees/repo")
    );
    assert!(
        super::gate_headroom_advice(
            None,
            Some(probe),
            None,
            Some(probe),
            required,
            inode_threshold
        )
        .is_none()
    );
}

/// A session gate runs under the worktree root and a verification slot in
/// host state; either refusing stops work, so the lower reading decides --
/// and a probe that cannot be read must not mask one that can.
#[test]
fn gate_headroom_takes_the_lowest_readable_volume() {
    let roomy = std::path::PathBuf::from("/roomy");
    let starved = std::path::PathBuf::from("/starved");
    let unreadable = std::path::PathBuf::from("/unreadable");
    let read = |path: &std::path::Path| match path.to_str() {
        Some("/roomy") => Some(crate::disk_headroom::DiskHeadroom {
            bytes: 500,
            inodes: Some(500),
        }),
        Some("/starved") => Some(crate::disk_headroom::DiskHeadroom {
            bytes: 3,
            inodes: Some(3),
        }),
        _ => None,
    };
    assert_eq!(
        super::lowest_headroom_with(&[roomy.clone(), starved.clone()], read).bytes,
        Some(super::HeadroomReading {
            path: starved.clone(),
            available: 3,
        })
    );
    assert_eq!(
        super::lowest_headroom_with(&[unreadable.clone(), roomy.clone()], read).bytes,
        Some(super::HeadroomReading {
            path: roomy,
            available: 500,
        })
    );
    assert_eq!(super::lowest_headroom_with(&[unreadable], read).bytes, None);
    assert_eq!(super::lowest_headroom_with(&[], read).bytes, None);
}

/// A worktree root does not exist before a repository's first session;
/// reading the missing path would be unknown and never escalate.
#[test]
fn headroom_is_read_at_the_nearest_existing_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("worktrees/not-yet-created");
    assert!(!missing.exists());
    assert!(crate::disk_headroom::available_bytes(&missing).is_none());
    assert!(
        crate::disk_headroom::available_headroom_at_or_above(&missing).is_some(),
        "a path that does not exist yet is read on the volume it will be created on"
    );
}

fn doctor_report(
    status: VersionDriftStatus,
    version_repair: Option<VersionRepairReport>,
) -> DoctorReport {
    DoctorReport {
        integrity: "ok".into(),
        version: VersionDriftReport {
            binary: BinaryBuild {
                version: "0.1.1".into(),
                describe: Some("v0.1.1".into()),
                commit: Some("aaaaaaaaaaaa".into()),
                path: Some("/tmp/aethyme".into()),
            },
            repo_is_aethyme_source: true,
            integration_branch: "aethyme/integration".into(),
            integration_head: Some("bbbbbbbbbbbb".into()),
            integration_describe: Some("v0.1.1-1-gbbbbbbbbbbbb".into()),
            release_tag: Some("v0.1.1".into()),
            status,
            message: "test report".into(),
        },
        version_repair,
        missing_worktrees: Vec::new(),
        orphaned_pidfiles: Vec::new(),
        purged_stale_leases: 0,
        retention: crate::GcHealth {
            deferred_checks: Vec::new(),
            closed_worktrees: Default::default(),
            unclaimed_worktree_count: 0,
            unclaimed_worktree_bytes: 0,
            policy: crate::RetentionPolicy::default(),
            retention_config_warnings: Vec::new(),
            pending_recovery_digest: None,
            candidate_rows: 0,
            candidate_files: 0,
            candidate_worktrees: 0,
            candidate_artifacts: 0,
            candidate_orphans: 0,
            estimated_reclaimable_bytes: 0,
            estimated_reclaimable_inodes: None,
            estimated_retained_bytes: 0,
            estimated_retained_inodes: None,
            estimated_blocked_bytes: 0,
            estimated_blocked_inodes: None,
            over_retained_bytes_budget: false,
            retained_bytes_deficit: 0,
            clears_retained_bytes_budget: true,
            reclaim_order: crate::ReclaimOrder::OldestFirst,
            budget_verdict: crate::BudgetVerdict::Within,
            unmeasured_directory_count: 0,
            artifact_worktrees_not_scanned: 0,
            sizes_measured_at_ms: None,
            blockers: 0,
        },
        integration_movement: None,
        unpushed_work: Default::default(),
        recent_command_failures: Vec::new(),
        hooks_path: None,
        leftover_integration_work: None,
        phase_timings_ms: Default::default(),
        deferred_checks: Vec::new(),
        budget_cut: Vec::new(),
    }
}

fn repair_report(status: DoctorRepairStatus) -> VersionRepairReport {
    VersionRepairReport {
        status,
        attempted: true,
        command: vec![
            "cargo".into(),
            "install".into(),
            "--path".into(),
            "/tmp/source".into(),
            "--force".into(),
            "--locked".into(),
        ],
        install_source: Some("/tmp/source".into()),
        integration_head: Some("bbbbbbbbbbbb".into()),
        exit_code: Some(if status == DoctorRepairStatus::Pass {
            0
        } else {
            1
        }),
        duration_ms: 1,
        message: "test repair".into(),
        stdout_tail: Vec::new(),
        stderr_tail: Vec::new(),
        commands: Vec::new(),
        steps: Vec::new(),
    }
}

fn session(id: i64) -> Session {
    Session {
        id,
        worktree_path: format!("/tmp/session-{id}"),
        branch: format!("agent/session-{id}"),
        origin: SessionOrigin::Adopted,
        status: SessionStatus::Active,
        cleanup_state: crate::SessionCleanupState::Open,
        closed_at: None,
        cleanup_completed_at: None,
        task: Some("test".into()),
        diff_base: Some("HEAD".into()),
        adoption_base: Some("HEAD".into()),
        adopted_head: Some("HEAD".into()),
        accepted_session_head: None,
        accepted_integration_commit: None,
        accepted_integration_tree: None,
        accepted_queue_entry_id: None,
        accepted_at: None,
        repository_contract: None,
        pid: None,
        command: None,
        log_path: None,
        exit_code: None,
        agent_identity: None,
        repository_name: None,
        tab_name: None,
        ai_provider: None,
        short_name: None,
        created_at: 0,
        updated_at: 0,
        last_activity_at: 0,
    }
}

fn queue_entry(id: i64, session_id: i64, status: crate::MergeStatus) -> MergeQueueEntry {
    MergeQueueEntry {
        id,
        session_id,
        head_commit: "abcdef1234567890".into(),
        base_commit: "0123456789abcdef".into(),
        status,
        merged_tree: Some("tree".into()),
        details_json: None,
        created_at: 0,
        updated_at: 0,
    }
}
