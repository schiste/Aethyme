//! End-to-end merge queue tests (issues #20-#23): submit → simulate →
//! gates on the merged tree → promote, conflicts rejected pre-gate with
//! the agent-facing instruction drop, and requeue on base move.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    AdoptMode, Broker, EntryExposureState, FinishStatus, IntegrationDeliveryState,
    IntegrationReconcileClassification, IntegrationReconcileOptions, MergeStatus, NewSession,
    RepairAction, RepairSource, SessionOrigin, StatusAdviceSeverity, SubmissionCommitOwnership,
    SubmissionGateVerificationStatus, SubmissionIntegrationState,
};

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
    // Broker operations spawn their own Git processes, so configure the
    // disposable repository itself instead of relying on the test runner's
    // global identity or on environment variables attached to `sh` above.
    sh(root, &["config", "user.name", "Aethyme Test"]);
    sh(
        root,
        &["config", "user.email", "aethyme-test@example.invalid"],
    );
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.py"), "a = 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "b = 1\n").unwrap();
    std::fs::write(
        root.join(".gitignore"),
        "gate-ran.txt\n.aethyme/broker-action-required.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"marker\"\ncommand = \"echo ran >> gate-ran.txt\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn agent_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    agent_worktree_at(root, name, "main")
}

fn agent_worktree_at(root: &Path, name: &str, base: &str) -> std::path::PathBuf {
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
            base,
        ],
    );
    path
}

fn file_at(root: &Path, rev: &str, path: &str) -> String {
    let output = Command::new("git")
        .args(["show", &format!("{rev}:{path}")])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "git show {rev}:{path} failed");
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn commit_edit(worktree: &Path, file: &str, content: &str) {
    std::fs::write(worktree.join(file), content).unwrap();
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", "edit"]);
}

fn resolve(root: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn promoted_merge_commit(entry: &aethyme_broker::MergeQueueEntry) -> String {
    serde_json::from_str::<serde_json::Value>(entry.details_json.as_deref().unwrap())
        .unwrap()["commit"]
        .as_str()
        .unwrap()
        .to_string()
}

fn unmerged_paths(worktree: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(worktree)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git diff for unmerged paths failed"
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Build the dual-identity history from #56/#57:
///
/// - main and integration contain the same `a = 2` patch under different SHAs;
/// - integration then advances both `a.py` and `b.py`;
/// - the victim starts from main and authors only the competing `b.py` edit.
///
/// A real rebase drops main's duplicate `a.py` commit before finding the
/// genuine `b.py` conflict. The legacy whole-head merge simulation instead
/// reports both paths and attributes `a.py` to whichever live lease happens
/// to own it.
fn dual_identity_conflict_fixture(root: &Path) -> (Broker, std::path::PathBuf, i64, String) {
    init_repo(root);
    let mut broker = Broker::open(root).unwrap();

    let duplicate_on_integration = agent_worktree(root, "integration-duplicate");
    let duplicate_session = broker
        .adopt(&duplicate_on_integration, Some("integration copy"))
        .unwrap();
    commit_edit(&duplicate_on_integration, "src/a.py", "a = 2\n");
    assert!(broker.submit(duplicate_session.id).unwrap().promoted);
    broker.close(duplicate_session.id).unwrap();
    broker
        .store()
        .release_lease(duplicate_session.id, "src/a.py")
        .unwrap();
    let integration_with_duplicate = resolve(root, "aethyme/integration");

    std::fs::write(root.join("src/a.py"), "a = 2\n").unwrap();
    sh(root, &["add", "src/a.py"]);
    sh(root, &["commit", "-qm", "same patch under a main identity"]);
    let main_duplicate = resolve(root, "main");
    assert_ne!(main_duplicate, resolve(&duplicate_on_integration, "HEAD"));

    let integration_followup =
        agent_worktree_at(root, "integration-followup", &integration_with_duplicate);
    let followup_session = broker
        .adopt(&integration_followup, Some("advance integration"))
        .unwrap();
    commit_edit(&integration_followup, "src/a.py", "a = 3\n");
    commit_edit(&integration_followup, "src/b.py", "b = 3\n");
    assert!(broker.submit(followup_session.id).unwrap().promoted);
    broker.close(followup_session.id).unwrap();
    for path in ["src/a.py", "src/b.py"] {
        broker
            .store()
            .release_lease(followup_session.id, path)
            .unwrap();
    }

    let victim = agent_worktree_at(root, "victim", &main_duplicate);
    let victim_session = broker.adopt(&victim, Some("edit only b")).unwrap();
    commit_edit(&victim, "src/b.py", "b = 2\n");

    (broker, victim, victim_session.id, main_duplicate)
}

#[test]
fn two_clean_sessions_promote_with_requeue_on_base_move_manual_mode() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    // Manual mode opt-in: this test exercises the explicit promote path
    // and requeue-on-base-move of already-verified entries.
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"manual\"\n",
    )
    .unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = agent_worktree(tmp.path(), "a");
    let wt_b = agent_worktree(tmp.path(), "b");
    let a = broker.adopt(&wt_a, Some("edit a")).unwrap();
    let b = broker.adopt(&wt_b, Some("edit b")).unwrap();
    commit_edit(&wt_a, "src/a.py", "a = 2\n");
    commit_edit(&wt_b, "src/b.py", "b = 2\n");

    // Both submit: clean simulations, gates pass on merged trees.
    let out_a = broker.submit(a.id).unwrap();
    assert_eq!(out_a.entry.status, MergeStatus::Verified);
    assert_eq!(out_a.gate_outcomes.len(), 1);
    assert_eq!(
        out_a.gate_outcomes[0].tree_hash,
        out_a.entry.merged_tree.as_deref().unwrap()
    );
    assert!(!out_a.promoted, "manual mode holds verified entries");
    let out_b = broker.submit(b.id).unwrap();
    assert_eq!(out_b.entry.status, MergeStatus::Verified);
    assert_eq!(
        broker.store().session(a.id).unwrap().accepted_session_head,
        None,
        "verification alone must not accept A's session HEAD"
    );
    assert_eq!(
        broker.store().session(b.id).unwrap().accepted_session_head,
        None,
        "verification alone must not accept B's session HEAD"
    );

    // Promote A: integration branch advances; B was verified against the
    // OLD base and gets re-simulated automatically.
    broker.promote(out_a.entry.id).unwrap();
    let integration = resolve(tmp.path(), "aethyme/integration");
    assert_ne!(integration, resolve(tmp.path(), "main"));
    let accepted_a = broker.store().session(a.id).unwrap();
    assert_eq!(
        accepted_a.accepted_session_head.as_deref(),
        Some(out_a.entry.head_commit.as_str())
    );
    assert_eq!(
        accepted_a.accepted_integration_commit.as_deref(),
        Some(integration.as_str())
    );
    assert_eq!(
        accepted_a.accepted_integration_tree,
        out_a.entry.merged_tree
    );
    assert_eq!(accepted_a.accepted_queue_entry_id, Some(out_a.entry.id));
    assert!(accepted_a.accepted_at.is_some());

    let queue = broker.store().merge_queue().unwrap();
    let entry_b = queue.iter().find(|e| e.id == out_b.entry.id).unwrap();
    assert_eq!(
        entry_b.status,
        MergeStatus::Verified,
        "B re-simulated against the moved base: {entry_b:?}"
    );

    // Promote B: integration now contains BOTH edits.
    broker.promote(out_b.entry.id).unwrap();
    let show = |path: &str| {
        let output = Command::new("git")
            .args(["show", &format!("aethyme/integration:{path}")])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(show("src/a.py"), "a = 2\n");
    assert_eq!(show("src/b.py"), "b = 2\n");

    // Timeline: submitted → verified → promoted events all present.
    let kinds: Vec<String> = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    for expected in [
        "merge.integration_branch_created",
        "merge.submitted",
        "merge.simulating",
        "merge.verified",
        "merge.promoted",
    ] {
        assert!(kinds.iter().any(|k| k == expected), "missing {expected}");
    }
}

#[test]
fn promotion_persists_one_non_blocking_advisory_per_affected_live_session() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"manual\"\n",
    )
    .unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();

    let affected_worktree = agent_worktree(tmp.path(), "affected");
    let affected = broker
        .adopt(&affected_worktree, Some("continue editing a"))
        .unwrap();
    let unrelated_worktree = agent_worktree(tmp.path(), "unrelated");
    let unrelated = broker
        .adopt(&unrelated_worktree, Some("edit documentation"))
        .unwrap();
    broker
        .claim_lease(unrelated.id, "docs/", None)
        .expect("unrelated lease");
    let promoting_worktree = agent_worktree(tmp.path(), "promoting");
    let promoting = broker
        .adopt(&promoting_worktree, Some("promote a"))
        .unwrap();
    commit_edit(&promoting_worktree, "src/b.py", "b = promoted\n");
    commit_edit(&promoting_worktree, "src/a.py", "a = promoted\n");
    let outcome = broker.submit(promoting.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    assert!(!outcome.promoted);

    // The intersection appears after verification but before manual
    // promotion. The submitter's implicit lease is no longer needed once its
    // exact merged tree is verified, so release it before the other session
    // establishes the explicit and implicit overlap under test.
    broker
        .store()
        .release_lease(promoting.id, "src/a.py")
        .unwrap();
    broker
        .store()
        .release_lease(promoting.id, "src/b.py")
        .unwrap();
    broker
        .store()
        .claim_lease(affected.id, "src/", None)
        .expect("directory lease");
    std::fs::write(affected_worktree.join("src/a.py"), "local dirty work\n").unwrap();
    std::fs::write(
        affected_worktree.join("src/b.py"),
        "other local dirty work\n",
    )
    .unwrap();
    broker.refresh_leases().unwrap();

    // Force projection failure after the advisory row is stored. Promotion
    // must remain successful and the database must retain the notification.
    let projection = tmp.path().join(aethyme_broker::BROKER_ADVISORY_RELPATH);
    std::fs::create_dir_all(&projection).unwrap();
    broker.promote(outcome.entry.id).unwrap();

    let integration_sha = resolve(tmp.path(), "aethyme/integration");
    assert_eq!(integration_sha.len(), 40);
    let advisories = broker.advisories(false).unwrap();
    assert_eq!(advisories.len(), 1);
    let advisory = &advisories[0];
    assert_eq!(advisory.session_id, Some(affected.id));
    assert_eq!(advisory.queue_entry_id, Some(outcome.entry.id));
    assert_eq!(
        advisory.integration_sha.as_deref(),
        Some(integration_sha.as_str())
    );
    assert_eq!(advisory.paths, vec!["src/a.py", "src/b.py"]);
    assert_eq!(
        advisory.identity,
        format!(
            "promotion_lease_intersection:{integration_sha}:{}",
            affected.id
        )
    );
    assert!(
        advisory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "lease_overlap"
                && evidence.summary.contains("explicit lease"))
    );
    assert!(
        advisory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "lease_overlap"
                && evidence.summary.contains("implicit lease"))
    );
    assert!(advisory.evidence.iter().any(|evidence| {
        evidence.kind == "safe_next_action" && evidence.summary == "aethyme broker status --json"
    }));
    assert!(
        advisories
            .iter()
            .all(|advisory| advisory.session_id != Some(unrelated.id)
                && advisory.session_id != Some(promoting.id)),
        "unrelated and promoting sessions are not affected sessions"
    );

    let status = broker.status_snapshot(0).unwrap();
    assert_eq!(status.outstanding_advisories, advisories);
    assert_eq!(status.outstanding_entry_exposures.len(), 1);
    assert_eq!(
        status.outstanding_entry_exposures[0].queue_entry_id,
        outcome.entry.id
    );

    let exposure = broker
        .store()
        .entry_path_exposure(outcome.entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(exposure.paths, vec!["src/a.py", "src/b.py"]);
    assert_eq!(exposure.state, EntryExposureState::Outstanding);
    broker.close(promoting.id).unwrap();
    broker.close(affected.id).unwrap();
    assert_eq!(
        broker
            .store()
            .entry_path_exposure(outcome.entry.id)
            .unwrap()
            .unwrap()
            .state,
        EntryExposureState::Outstanding,
        "closing the originating and affected sessions must not erase entry publication exposure"
    );
    assert_eq!(broker.advisories(false).unwrap(), advisories);

    std::fs::remove_dir(&projection).unwrap();
    broker.refresh_advisory_projection().unwrap();
    let markdown = std::fs::read_to_string(&projection).unwrap();
    assert!(markdown.contains(&integration_sha));
    assert!(markdown.contains("safe_next_action"));

    let acknowledged = broker.acknowledge_advisory(advisory.id).unwrap();
    assert_eq!(
        acknowledged.resolution_state,
        aethyme_broker::AdvisoryResolutionState::Acknowledged
    );
    assert!(broker.advisories(false).unwrap().is_empty());
    assert_eq!(
        broker
            .store()
            .entry_path_exposure(outcome.entry.id)
            .unwrap()
            .unwrap()
            .state,
        EntryExposureState::Outstanding,
        "acknowledgement changes delivery, not publication exposure"
    );

    // A later session starts from the promoted integration tip, rewrites an
    // exposed path, and can still submit. The old exposure persists because
    // neither rebase-equivalent adoption nor advisory acknowledgement is
    // publication evidence.
    let later_worktree = agent_worktree_at(tmp.path(), "later", "aethyme/integration");
    let later = broker
        .adopt(
            &later_worktree,
            Some("rewrite an exposed path after rebasing"),
        )
        .unwrap();
    commit_edit(&later_worktree, "src/a.py", "a = rewritten later\n");
    let later_outcome = broker.submit(later.id).unwrap();
    assert_eq!(later_outcome.entry.status, MergeStatus::Verified);
    assert!(!later_outcome.promoted);
    assert_eq!(
        broker
            .store()
            .entry_path_exposure(outcome.entry.id)
            .unwrap()
            .unwrap()
            .state,
        EntryExposureState::Outstanding
    );
}

#[test]
fn verification_commit_preserves_only_the_owned_contract_decision() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "contract-decision");
    let session = broker
        .adopt(&worktree, Some("document an agent protocol"))
        .unwrap();
    std::fs::write(worktree.join("src/a.py"), "a = protocol\n").unwrap();
    sh(&worktree, &["add", "src/a.py"]);
    sh(
        &worktree,
        &[
            "commit",
            "-qm",
            "docs: document protocol",
            "-m",
            "Private implementation rationale.\n\nContract decision: introduce",
        ],
    );

    let outcome = broker.submit(session.id).unwrap();
    assert!(outcome.promoted);
    let promoted = promoted_merge_commit(&outcome.entry);
    let message = Command::new("git")
        .args(["show", "-s", "--format=%B", &promoted])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(message.status.success());
    let message = String::from_utf8_lossy(&message.stdout);
    assert!(message.contains("Contract decision: introduce"));
    assert!(!message.contains("Private implementation rationale"));
}

#[test]
fn repeated_submission_uses_the_last_accepted_session_head_as_its_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "repeat");
    let session = broker.adopt(&worktree, Some("two contributions")).unwrap();
    let adopted_head = session.adopted_head.clone();

    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let first_head = resolve(&worktree, "HEAD");
    let first = broker.submit(session.id).unwrap();
    assert!(first.promoted);
    assert_eq!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .accepted_session_head,
        Some(first_head.clone())
    );

    let other_worktree =
        agent_worktree_at(tmp.path(), "between-contributions", "aethyme/integration");
    let other = broker
        .adopt(&other_worktree, Some("advance integration independently"))
        .unwrap();
    commit_edit(&other_worktree, "src/c.py", "c = 2\n");
    assert!(broker.submit(other.id).unwrap().promoted);
    broker.close(other.id).unwrap();
    broker.store().release_lease(other.id, "src/c.py").unwrap();
    let integration_before_follow_up = resolve(tmp.path(), "aethyme/integration");

    commit_edit(&worktree, "src/b.py", "b = 2\n");
    let second_head = resolve(&worktree, "HEAD");
    let second = broker.submit(session.id).unwrap();
    assert!(second.promoted);
    assert_eq!(
        second.submission_plan.recorded_baseline.as_deref(),
        Some(first_head.as_str())
    );
    assert_eq!(
        second.submission_plan.integration_head, integration_before_follow_up,
        "integration is the replay target, not the ownership boundary"
    );
    assert_eq!(second.submission_plan.commits.len(), 1);
    assert_eq!(
        second.submission_plan.commits[0].ownership,
        SubmissionCommitOwnership::SessionOwned
    );
    assert_eq!(second.submission_plan.commits[0].commit, second_head);

    let persisted = broker.store().session(session.id).unwrap();
    assert_eq!(persisted.adopted_head, adopted_head);
    assert_eq!(persisted.accepted_session_head, Some(second_head));
    assert_eq!(persisted.accepted_queue_entry_id, Some(second.entry.id));
}

#[test]
fn follow_up_refuses_to_replace_a_rewritten_accepted_checkpoint_with_integration() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "rewritten-follow-up");
    let session = broker.adopt(&worktree, Some("two contributions")).unwrap();

    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let accepted_head = resolve(&worktree, "HEAD");
    assert!(broker.submit(session.id).unwrap().promoted);

    commit_edit(&worktree, "src/b.py", "b = 2\n");
    sh(&worktree, &["rebase", "aethyme/integration"]);
    let rewritten_head = resolve(&worktree, "HEAD");
    assert_ne!(rewritten_head, accepted_head);
    assert!(
        !Command::new("git")
            .args([
                "merge-base",
                "--is-ancestor",
                &accepted_head,
                &rewritten_head
            ])
            .current_dir(&worktree)
            .status()
            .unwrap()
            .success()
    );

    let error = broker.submit(session.id).unwrap_err();

    let message = error.to_string();
    assert!(message.contains(&format!(
        "follow-up ownership must remain {accepted_head}..{rewritten_head}"
    )));
    assert!(message.contains("cannot replace it as the ownership boundary"));
    let persisted = broker.store().session(session.id).unwrap();
    assert_eq!(persisted.accepted_session_head, Some(accepted_head));
    assert!(
        broker
            .store()
            .merge_queue()
            .unwrap()
            .iter()
            .all(|entry| entry.head_commit != rewritten_head),
        "the refused rewritten head must not appear as submitted work"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/b.py"),
        "b = 1\n",
        "an unsafe follow-up plan must not mutate integration"
    );
}

#[test]
fn reviewed_checkpoint_recovery_preserves_head_and_reanchors_without_hiding_followup_work() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "checkpoint-recovery");
    let session = broker
        .adopt(&worktree, Some("recover rewritten checkpoint"))
        .unwrap();

    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let accepted_head = resolve(&worktree, "HEAD");
    assert!(broker.submit(session.id).unwrap().promoted);
    let integration = resolve(tmp.path(), "aethyme/integration");

    sh(&worktree, &["reset", "--hard", "aethyme/integration"]);
    commit_edit(&worktree, "src/b.py", "b = 2\n");
    let followup_head = resolve(&worktree, "HEAD");
    assert!(
        !Command::new("git")
            .args([
                "merge-base",
                "--is-ancestor",
                &accepted_head,
                &followup_head
            ])
            .current_dir(&worktree)
            .status()
            .unwrap()
            .success()
    );

    std::fs::write(worktree.join("src/dirty.py"), "dirty = True\n").unwrap();
    let dirty_plan = broker.plan_session_checkpoint_recovery(session.id).unwrap();
    assert!(!dirty_plan.safe);
    assert!(
        dirty_plan
            .refusals
            .iter()
            .any(|reason| reason.contains("worktree is dirty"))
    );
    std::fs::remove_file(worktree.join("src/dirty.py")).unwrap();

    let plan = broker.plan_session_checkpoint_recovery(session.id).unwrap();
    assert!(plan.safe, "{:?}", plan.refusals);
    assert_eq!(plan.old_checkpoint, Some(accepted_head.clone()));
    assert_eq!(plan.proposed_checkpoint, Some(integration.clone()));
    assert_eq!(
        plan.integration_relation,
        Some(aethyme_broker::AdoptIntegrationRelation::Ahead)
    );
    assert_eq!(plan.pending_commits, vec![followup_head.clone()]);
    assert_eq!(plan.digest.len(), 64);
    let cli = Command::new(env!("CARGO_BIN_EXE_broker-cli-shim"))
        .args([
            "checkpoint",
            "plan",
            "--session",
            &session.id.to_string(),
            "--json",
        ])
        .current_dir(&worktree)
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_plan: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli_plan["digest"], plan.digest);
    assert_eq!(cli_plan["pending_commits"][0], followup_head);
    assert_eq!(cli_plan["integration_relation"], "ahead");

    let mismatch = broker
        .apply_session_checkpoint_recovery(session.id, &"0".repeat(64))
        .unwrap_err();
    assert!(mismatch.to_string().contains("confirmation mismatch"));
    assert_eq!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .accepted_session_head,
        Some(accepted_head)
    );

    let applied = broker
        .apply_session_checkpoint_recovery(session.id, &plan.digest)
        .unwrap();
    assert!(applied.applied);
    assert_eq!(applied.accepted_session_head, integration);
    assert_eq!(
        resolve(tmp.path(), &applied.preservation_ref),
        followup_head
    );
    let recovered = broker.store().session(session.id).unwrap();
    assert_eq!(recovered.accepted_session_head, plan.proposed_checkpoint);
    assert_eq!(recovered.diff_base, plan.proposed_checkpoint);
    assert_eq!(
        broker
            .store()
            .events_after_filtered(0, 10, Some("session.checkpoint_reanchored"))
            .unwrap()
            .len(),
        1
    );

    let followup = broker.submit(session.id).unwrap();
    assert!(followup.promoted);
    let pending = followup
        .submission_plan
        .commits
        .iter()
        .filter(|commit| {
            commit.ownership == SubmissionCommitOwnership::SessionOwned
                && commit.integration_state == SubmissionIntegrationState::Pending
        })
        .map(|commit| commit.commit.clone())
        .collect::<Vec<_>>();
    assert_eq!(pending, vec![followup_head]);
}

#[test]
fn checkpoint_recovery_refuses_divergence_and_sessions_without_acceptance_proof() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let fresh_worktree = agent_worktree(tmp.path(), "no-checkpoint");
    let fresh = broker.adopt(&fresh_worktree, None).unwrap();
    let missing = broker.plan_session_checkpoint_recovery(fresh.id).unwrap();
    assert!(!missing.safe);
    assert!(
        missing
            .refusals
            .iter()
            .any(|reason| reason.contains("no accepted checkpoint"))
    );
    broker.close(fresh.id).unwrap();

    let worktree = agent_worktree(tmp.path(), "diverged-checkpoint");
    let session = broker.adopt(&worktree, None).unwrap();
    commit_edit(&worktree, "src/a.py", "a = 2\n");
    assert!(broker.submit(session.id).unwrap().promoted);
    sh(&worktree, &["reset", "--hard", "main"]);
    commit_edit(&worktree, "src/b.py", "b = 3\n");

    let plan = broker.plan_session_checkpoint_recovery(session.id).unwrap();
    assert!(!plan.safe);
    assert_eq!(
        plan.integration_relation,
        Some(aethyme_broker::AdoptIntegrationRelation::Diverged)
    );
    assert!(
        plan.refusals
            .iter()
            .any(|reason| reason.contains("recovery requires integration to be an ancestor"))
    );
    let error = broker
        .apply_session_checkpoint_recovery(session.id, &plan.digest)
        .unwrap_err();
    assert!(error.to_string().contains("not safe"));
}

#[test]
fn conflicting_submission_rejected_pre_gate_with_instruction_drop() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = agent_worktree(tmp.path(), "a");
    let wt_b = agent_worktree(tmp.path(), "b");
    let a = broker.adopt(&wt_a, None).unwrap();
    let b = broker.adopt(&wt_b, None).unwrap();

    // Both change the same line of the same file.
    commit_edit(&wt_a, "src/a.py", "a = 111\n");
    commit_edit(&wt_b, "src/a.py", "a = 222\n");
    // Refresh leases before promotion. Once A promotes, its implicit lease
    // may correctly disappear because its work is now integrated; the Git
    // conflict remains real even when no live lease owner remains to name.
    broker.refresh_leases().unwrap();

    let out_a = broker.submit(a.id).unwrap();
    assert!(out_a.promoted, "A lands immediately (auto default)");

    let out_b = broker.submit(b.id).unwrap();
    assert_eq!(out_b.entry.status, MergeStatus::Conflict);
    assert_eq!(
        broker.store().session(b.id).unwrap().accepted_session_head,
        None,
        "a conflicted submission must not advance the checkpoint"
    );
    assert_eq!(out_b.conflicts, vec!["src/a.py".to_string()]);
    assert!(
        out_b.gate_outcomes.is_empty(),
        "conflict rejected BEFORE any gate ran"
    );
    // Marker file proves the gate never executed for B's submission
    // beyond A's earlier verified run.
    let details = out_b.entry.details_json.as_deref().unwrap();
    let details_json: serde_json::Value = serde_json::from_str(details).unwrap();
    assert!(
        details_json["blocking_sessions"].is_array(),
        "conflict details must retain the blocking-session evidence field: {details}"
    );

    // The agent-facing instruction drop exists in B's worktree and names
    // the conflicting file and the blocking session (#21 decision).
    let note = std::fs::read_to_string(wt_b.join(aethyme_broker::ACTION_REQUIRED_RELPATH)).unwrap();
    assert!(note.contains("src/a.py"));
    assert!(note.contains(&format!("session {}", b.id)) || note.contains("Blocking session"));
    assert!(note.contains("rebase"));

    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.latest-submit-conflict")
        .expect("latest conflict should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.session_id, Some(b.id));
    assert_eq!(advice.queue_entry_id, Some(out_b.entry.id));
    assert!(advice.summary.contains("conflicted"));
    assert!(
        advice.evidence.iter().any(|line| line.contains("src/a.py")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command.contains("broker-action-required.md")),
        "{advice:?}"
    );

    let err = broker.repair(b.id).unwrap_err().to_string();
    assert!(
        err.contains("repair paused during rebase"),
        "repair should apply the documented rebase path and stop for true content conflicts: {err}"
    );
}

#[test]
fn normalized_replay_drops_dual_identity_noise_and_preserves_real_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut broker, victim, victim_id, victim_baseline) =
        dual_identity_conflict_fixture(tmp.path());

    let blocker_a_worktree = agent_worktree_at(tmp.path(), "blocker-a", &victim_baseline);
    let blocker_a = broker
        .adopt(&blocker_a_worktree, Some("unrelated lease owner A"))
        .unwrap();
    commit_edit(&blocker_a_worktree, "src/a.py", "a = 4\n");
    broker.refresh_leases().unwrap();

    let first = broker.submit(victim_id).unwrap();
    assert_eq!(first.entry.status, MergeStatus::Conflict);
    assert!(first.submission_plan.safe);
    assert_eq!(first.submission_plan.session_head.len(), 40);
    assert_eq!(first.submission_plan.integration_head.len(), 40);
    assert_eq!(
        first.submission_plan.recorded_baseline.as_deref(),
        Some(victim_baseline.as_str())
    );
    assert_eq!(first.submission_plan.commits.len(), 2);
    let inherited = &first.submission_plan.commits[0];
    assert_eq!(
        inherited.ownership,
        SubmissionCommitOwnership::InheritedFromRecordedBaseline
    );
    assert_eq!(
        inherited.integration_state,
        SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity
    );
    assert_eq!(inherited.commit, victim_baseline);
    assert_eq!(inherited.matching_integration_commits.len(), 1);
    assert_eq!(inherited.matching_integration_commits[0].len(), 40);
    let owned = &first.submission_plan.commits[1];
    assert_eq!(owned.ownership, SubmissionCommitOwnership::SessionOwned);
    assert_eq!(owned.integration_state, SubmissionIntegrationState::Pending);
    assert_eq!(owned.commit.len(), 40);
    let plan_json = serde_json::to_value(&first.submission_plan).unwrap();
    assert_eq!(
        plan_json["commits"][0]["ownership"],
        "inherited_from_recorded_baseline"
    );
    assert_eq!(
        plan_json["commits"][0]["integration_state"],
        "already_integrated_by_stable_patch_identity"
    );
    assert_eq!(plan_json["commits"][1]["ownership"], "session_owned");
    assert_eq!(plan_json["commits"][1]["integration_state"], "pending");
    assert_eq!(
        first.conflicts,
        vec!["src/b.py".to_string()],
        "normalized replay must report only the session-owned conflict"
    );
    assert_eq!(first.conflict_details.len(), 1);
    let conflict = &first.conflict_details[0];
    assert_eq!(conflict.path, "src/b.py");
    assert_eq!(conflict.originating_commit, owned.commit);
    assert_eq!(conflict.ownership, SubmissionCommitOwnership::SessionOwned);
    assert_eq!(conflict.integration_side_commits.len(), 1);
    assert_eq!(conflict.integration_side_commits[0].len(), 40);
    assert!(conflict.remediation.contains(&owned.commit));
    let expected_submit = format!("aethyme broker submit --session {victim_id}");
    assert_eq!(
        conflict.commands.last().map(String::as_str),
        Some(expected_submit.as_str())
    );
    let first_details: serde_json::Value =
        serde_json::from_str(first.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(first_details["blocking_sessions"], serde_json::json!([]));
    assert_eq!(
        first_details["conflict_details"][0]["originating_commit"],
        owned.commit
    );
    assert_eq!(
        first_details["conflict_details"][0]["ownership"],
        "session_owned"
    );

    let note =
        std::fs::read_to_string(victim.join(aethyme_broker::ACTION_REQUIRED_RELPATH)).unwrap();
    assert!(note.contains(&owned.commit));
    assert!(note.contains(&conflict.integration_side_commits[0]));

    broker.close(blocker_a.id).unwrap();
    broker
        .store()
        .release_lease(blocker_a.id, "src/a.py")
        .unwrap();
    let blocker_b_worktree = agent_worktree_at(tmp.path(), "blocker-b", &victim_baseline);
    let _blocker_b = broker
        .adopt(&blocker_b_worktree, Some("unrelated lease owner B"))
        .unwrap();
    commit_edit(&blocker_b_worktree, "src/a.py", "a = 5\n");
    broker.refresh_leases().unwrap();

    let second = broker.simulate_and_gate(first.entry.id).unwrap();
    assert_eq!(second.conflicts, first.conflicts);
    assert_eq!(second.conflict_details, first.conflict_details);
    let second_details: serde_json::Value =
        serde_json::from_str(second.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(second_details["blocking_sessions"], serde_json::json!([]));

    let blocker_real_worktree = agent_worktree_at(tmp.path(), "blocker-real", &victim_baseline);
    let blocker_real = broker
        .adopt(
            &blocker_real_worktree,
            Some("actual conflicting lease owner"),
        )
        .unwrap();
    commit_edit(&blocker_real_worktree, "src/b.py", "b = 5\n");
    broker.refresh_leases().unwrap();

    let third = broker.simulate_and_gate(first.entry.id).unwrap();
    assert_eq!(third.conflicts, vec!["src/b.py".to_string()]);
    assert_eq!(third.conflict_details, first.conflict_details);
    let third_details: serde_json::Value =
        serde_json::from_str(third.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        third_details["blocking_sessions"],
        serde_json::json!([blocker_real.id])
    );

    let rebase = Command::new("git")
        .args(["rebase", "aethyme/integration"])
        .current_dir(&victim)
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    assert!(
        !rebase.status.success(),
        "the owned b.py edit should conflict"
    );
    assert_eq!(
        unmerged_paths(&victim),
        vec!["src/b.py".to_string()],
        "rebase drops the patch-equivalent a.py commit and exposes only the real conflict"
    );
    sh(&victim, &["rebase", "--abort"]);
}

#[test]
fn normalized_replay_handles_rebased_rename_binary_empty_and_stable_order() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let integration_worktree = agent_worktree(tmp.path(), "integration-base");
    let integration_session = broker.adopt(&integration_worktree, None).unwrap();
    commit_edit(&integration_worktree, "src/a.py", "a = 2\n");
    assert!(broker.submit(integration_session.id).unwrap().promoted);
    broker.close(integration_session.id).unwrap();
    broker
        .store()
        .release_lease(integration_session.id, "src/a.py")
        .unwrap();

    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "duplicate main patch"]);
    let old_baseline = resolve(tmp.path(), "main");

    let rebased = agent_worktree_at(tmp.path(), "rebased", &old_baseline);
    let rebased_session = broker.adopt(&rebased, None).unwrap();
    commit_edit(&rebased, "src/b.py", "b = 2\n");
    sh(&rebased, &["rebase", "aethyme/integration"]);
    let rebased_outcome = broker.submit(rebased_session.id).unwrap();
    assert!(rebased_outcome.promoted);
    assert!(rebased_outcome.submission_plan.safe);
    assert_eq!(rebased_outcome.submission_plan.commits.len(), 1);
    assert!(
        rebased_outcome
            .submission_plan
            .warnings
            .iter()
            .any(|warning| warning.contains("unambiguous rebased range"))
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/a.py"),
        "a = 2\n"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/b.py"),
        "b = 2\n"
    );

    let shapes = agent_worktree_at(tmp.path(), "file-shapes", "aethyme/integration");
    let shapes_session = broker.adopt(&shapes, None).unwrap();
    let shapes_baseline = resolve(&shapes, "HEAD");
    sh(&shapes, &["mv", "src/a.py", "src/renamed.py"]);
    sh(&shapes, &["commit", "-qm", "rename source"]);
    std::fs::write(shapes.join("src/blob.bin"), [0_u8, 1, 255, 4]).unwrap();
    sh(&shapes, &["add", "src/blob.bin"]);
    sh(&shapes, &["commit", "-qm", "add binary"]);
    sh(&shapes, &["commit", "--allow-empty", "-qm", "empty marker"]);
    let expected_order = {
        let output = Command::new("git")
            .args(["rev-list", "--reverse", &format!("{shapes_baseline}..HEAD")])
            .current_dir(&shapes)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let shapes_outcome = broker.submit(shapes_session.id).unwrap();
    assert!(shapes_outcome.promoted);
    assert_eq!(
        shapes_outcome
            .submission_plan
            .commits
            .iter()
            .map(|commit| commit.commit.clone())
            .collect::<Vec<_>>(),
        expected_order
    );
    assert_eq!(
        std::fs::read({
            let checkout = agent_worktree_at(tmp.path(), "inspect-shapes", "aethyme/integration");
            checkout.join("src/blob.bin")
        })
        .unwrap(),
        vec![0_u8, 1, 255, 4]
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/renamed.py"),
        "a = 2\n"
    );
}

#[test]
fn repeated_reuse_after_rebase_preserves_owned_work_and_finish_truth() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "reuse-after-rebase");

    let first = broker
        .adopt(&worktree, Some("land the product change"))
        .unwrap();
    commit_edit(&worktree, "src/a.py", "a = 2\n");
    assert!(broker.submit(first.id).unwrap().promoted);
    assert_eq!(
        broker.finish(first.id).unwrap().status,
        FinishStatus::Closed
    );

    std::fs::create_dir_all(worktree.join("docs")).unwrap();
    std::fs::write(
        worktree.join("docs/RELEASE.md"),
        "release notes for a = 2\n",
    )
    .unwrap();
    sh(&worktree, &["add", "docs/RELEASE.md"]);
    sh(&worktree, &["commit", "-qm", "document the product change"]);
    let pre_rebase_head = resolve(&worktree, "HEAD");

    // Reusing a closed worktree creates a new identity at its current HEAD.
    let created = broker
        .adopt_with(&worktree, Some("land the release notes"), AdoptMode::Reuse)
        .unwrap();
    assert_eq!(
        created.session.diff_base.as_deref(),
        Some(pre_rebase_head.as_str())
    );

    // The rebase drops the patch-equivalent product commit and rewrites the
    // release-note commit. A second active reuse must preserve the original
    // ownership boundary instead of replacing it with the rewritten HEAD.
    sh(&worktree, &["rebase", "aethyme/integration"]);
    let rebased_head = resolve(&worktree, "HEAD");
    assert_ne!(rebased_head, pre_rebase_head);
    let reused = broker
        .adopt_with(&worktree, Some("land the release notes"), AdoptMode::Reuse)
        .unwrap();
    assert_eq!(reused.session.id, created.session.id);
    assert_eq!(
        reused.session.diff_base.as_deref(),
        Some(pre_rebase_head.as_str())
    );
    assert_eq!(
        reused
            .integration_drift
            .as_ref()
            .expect("reuse drift")
            .safe_next_action,
        format!("aethyme broker submit --session {}", created.session.id)
    );

    let integration_before = resolve(tmp.path(), "aethyme/integration");
    let outcome = broker.submit(created.session.id).unwrap();
    assert!(outcome.promoted);
    assert!(!outcome.no_changes);
    let promotion = promoted_merge_commit(&outcome.entry);
    assert_ne!(
        resolve(tmp.path(), &format!("{integration_before}^{{tree}}")),
        resolve(tmp.path(), &format!("{promotion}^{{tree}}")),
        "a successful promotion must not reuse its parent tree"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "docs/RELEASE.md"),
        "release notes for a = 2\n"
    );

    // Once the checkout is exactly at integration, finish must not resurrect
    // commits from the rewritten baseline as phantom pending work.
    sh(&worktree, &["reset", "--hard", "aethyme/integration"]);
    let finished = broker.finish(created.session.id).unwrap();
    assert_eq!(finished.status, FinishStatus::Closed);
    assert_eq!(finished.unsubmitted_commits, 0);
}

#[test]
fn submission_with_unchanged_result_is_a_truthful_noop() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "nothing-to-submit");
    let session = broker.adopt(&worktree, Some("inspect only")).unwrap();
    let submitted_head = resolve(&worktree, "HEAD");
    let integration_before = broker.integration_head().unwrap().1;
    let integration_tree = resolve(tmp.path(), "aethyme/integration^{tree}");

    let outcome = broker.submit(session.id).unwrap();

    assert!(outcome.no_changes);
    assert!(!outcome.promoted);
    assert_eq!(outcome.entry.status, MergeStatus::Superseded);
    assert!(outcome.gate_outcomes.is_empty());
    assert_eq!(
        outcome.gate_verification.status,
        SubmissionGateVerificationStatus::NotRun
    );
    assert_eq!(broker.integration_head().unwrap().1, integration_before);
    let details: serde_json::Value =
        serde_json::from_str(outcome.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["reason"], "submission produces no content change");
    let accepted = broker.store().session(session.id).unwrap();
    assert_eq!(accepted.accepted_session_head, Some(submitted_head));
    assert_eq!(
        accepted.accepted_integration_commit,
        Some(integration_before)
    );
    assert_eq!(accepted.accepted_integration_tree, Some(integration_tree));
    assert_eq!(accepted.accepted_queue_entry_id, Some(outcome.entry.id));
    assert!(accepted.accepted_at.is_some());
}

#[test]
fn patch_equivalent_supersession_checkpoints_the_submitted_head() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let duplicate = agent_worktree(tmp.path(), "duplicate-patch");
    let publisher = agent_worktree(tmp.path(), "patch-publisher");
    let duplicate_session = broker
        .adopt(&duplicate, Some("submit duplicate patch"))
        .unwrap();
    let publisher_session = broker.adopt(&publisher, Some("publish patch")).unwrap();

    std::fs::write(publisher.join("src/a.py"), "a = 2\n").unwrap();
    sh(&publisher, &["add", "src/a.py"]);
    sh(&publisher, &["commit", "-qm", "publisher identity"]);
    assert!(broker.submit(publisher_session.id).unwrap().promoted);
    broker.close(publisher_session.id).unwrap();

    std::fs::write(duplicate.join("src/a.py"), "a = 2\n").unwrap();
    sh(&duplicate, &["add", "src/a.py"]);
    sh(&duplicate, &["commit", "-qm", "duplicate identity"]);
    let duplicate_head = resolve(&duplicate, "HEAD");
    let integration_commit = resolve(tmp.path(), "aethyme/integration");
    let integration_tree = resolve(tmp.path(), "aethyme/integration^{tree}");

    let outcome = broker.submit(duplicate_session.id).unwrap();

    assert!(outcome.no_changes);
    assert!(!outcome.promoted);
    assert_eq!(outcome.entry.status, MergeStatus::Superseded);
    assert_eq!(
        outcome.submission_plan.commits[0].integration_state,
        SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity
    );
    let accepted = broker.store().session(duplicate_session.id).unwrap();
    assert_eq!(accepted.accepted_session_head, Some(duplicate_head));
    assert_eq!(
        accepted.accepted_integration_commit,
        Some(integration_commit)
    );
    assert_eq!(accepted.accepted_integration_tree, Some(integration_tree));
    assert_eq!(accepted.accepted_queue_entry_id, Some(outcome.entry.id));
    assert!(accepted.accepted_at.is_some());
}

#[test]
fn normalized_replay_refuses_missing_baseline_and_owned_merge_commits() {
    let missing_tmp = tempfile::tempdir().unwrap();
    init_repo(missing_tmp.path());
    let mut missing_broker = Broker::open(missing_tmp.path()).unwrap();
    let missing_worktree = agent_worktree(missing_tmp.path(), "missing-baseline");
    let missing_session = missing_broker
        .store()
        .register_session(&NewSession {
            worktree_path: missing_worktree.to_string_lossy().into_owned(),
            branch: "agent/missing-baseline".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap();
    commit_edit(&missing_worktree, "src/a.py", "a = 9\n");
    missing_broker
        .claim_lease(missing_session.id, "src/a.py", None)
        .unwrap();
    let missing_error = missing_broker.submit(missing_session.id).unwrap_err();
    assert!(
        missing_error.to_string().contains("no recorded baseline"),
        "{missing_error}"
    );
    assert_eq!(
        missing_broker
            .store()
            .session(missing_session.id)
            .unwrap()
            .accepted_session_head,
        None,
        "ambiguous ownership must not advance the checkpoint"
    );
    assert!(missing_broker.store().merge_queue().unwrap().is_empty());
    assert!(missing_broker.submit(missing_session.id).is_err());
    assert!(
        missing_broker.store().merge_queue().unwrap().is_empty(),
        "repeated planning failures must not accumulate queue entries"
    );
    assert_eq!(
        missing_broker
            .store()
            .events_after_filtered(0, 10, Some("merge.submission_planning_failed"))
            .unwrap()
            .len(),
        2,
        "each refusal remains auditable without a live queue row"
    );

    let merge_tmp = tempfile::tempdir().unwrap();
    init_repo(merge_tmp.path());
    let mut merge_broker = Broker::open(merge_tmp.path()).unwrap();
    let merge_worktree = agent_worktree(merge_tmp.path(), "merge-commit");
    let merge_session = merge_broker.adopt(&merge_worktree, None).unwrap();
    let baseline = resolve(&merge_worktree, "HEAD");
    sh(&merge_worktree, &["branch", "side", &baseline]);
    commit_edit(&merge_worktree, "src/a.py", "a = 7\n");
    sh(&merge_worktree, &["checkout", "side"]);
    commit_edit(&merge_worktree, "src/b.py", "b = 7\n");
    sh(&merge_worktree, &["checkout", "agent/merge-commit"]);
    sh(
        &merge_worktree,
        &["merge", "--no-ff", "side", "-m", "owned merge"],
    );
    let merge_head = resolve(&merge_worktree, "HEAD");
    let merge_error = merge_broker.submit(merge_session.id).unwrap_err();
    let remediation = merge_error.to_string();
    let recovery_branch = format!(
        "aethyme/recovery/session-{}-{}",
        merge_session.id,
        &merge_head[..12]
    );
    assert!(remediation.contains("has 2 parents"), "{remediation}");
    assert!(
        remediation.contains(&format!("accepted checkpoint: {baseline}")),
        "{remediation}"
    );
    assert!(
        remediation.contains(&format!("owned commit {merge_head}")),
        "{remediation}"
    );
    let preserve = remediation
        .find(&format!("git branch {recovery_branch} {merge_head}"))
        .unwrap();
    let reset = remediation
        .find(&format!("git reset --soft {baseline}"))
        .unwrap();
    assert!(preserve < reset, "recovery must preserve HEAD before reset");
    assert!(
        remediation.contains(&format!(
            "aethyme broker submit --session {}",
            merge_session.id
        )),
        "{remediation}"
    );
    assert!(merge_broker.store().merge_queue().unwrap().is_empty());
    assert!(merge_broker.submit(merge_session.id).is_err());
    assert!(merge_broker.store().merge_queue().unwrap().is_empty());
}

#[test]
fn failing_gate_on_merged_tree_rejects_and_auto_mode_promotes() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = agent_worktree(tmp.path(), "x");
    let session = broker.adopt(&wt, None).unwrap();
    // Failing gate config is committed in the session worktree so the
    // simulated merged tree, not the broker's main checkout, defines the
    // verification policy.
    std::fs::write(
        wt.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"fail\"\ncommand = \"exit 7\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    commit_edit(&wt, "src/a.py", "a = 3\n");

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Rejected);
    assert_eq!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .accepted_session_head,
        None,
        "a rejected submission must not advance the checkpoint"
    );
    assert!(
        broker.promote(outcome.entry.id).is_err(),
        "rejected entries cannot be promoted"
    );
    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.latest-submit-rejected")
        .expect("latest rejected submit should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.session_id, Some(session.id));
    assert_eq!(advice.queue_entry_id, Some(outcome.entry.id));
    assert!(advice.summary.contains("fail"));
    assert!(
        advice
            .evidence
            .iter()
            .any(|line| line.contains("gate fail status fail")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command == &format!("aethyme broker submit --session {}", session.id)),
        "{advice:?}"
    );

    // Flip to a passing gate: with the DEFAULT config (no config.toml),
    // verified promotes immediately — auto is the default (2026-07-13).
    std::fs::write(
        wt.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"ok\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    commit_edit(&wt, "src/a.py", "a = 4\n");
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Promoted);
    assert!(outcome.promoted, "auto-promote is the default");
}

#[test]
fn repo_without_gates_is_a_pure_conflict_manager() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt = agent_worktree(tmp.path(), "solo");
    let session = broker.adopt(&wt, None).unwrap();
    // Remove the gate config in the submitted tree: conflict-only mode is
    // committed repository state, not an untracked local checkout setting.
    std::fs::remove_file(wt.join(".aethyme/gates.toml")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 9\n");

    // Clean merge, zero gates configured -> promoted with no verification,
    // and the outcome makes that visible (empty gate_outcomes).
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Promoted);
    assert!(outcome.gate_outcomes.is_empty());
    assert_eq!(
        outcome.gate_verification.status,
        SubmissionGateVerificationStatus::NoConfiguration
    );

    // Conflicts are still caught: a second session editing the same line.
    let wt_b = agent_worktree(tmp.path(), "rival");
    let rival = broker.adopt(&wt_b, None).unwrap();
    commit_edit(&wt_b, "src/a.py", "a = 10\n");
    let out = broker.submit(rival.id).unwrap();
    assert_eq!(out.entry.status, MergeStatus::Conflict);
}

#[test]
fn integration_follows_main_but_never_clobbers_unmerged_promotions() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Branch created at main HEAD; main then advances → integration
    // fast-forwards on the next touch (issue #40), with an event.
    let (_branch, base) = broker.integration_head().unwrap();
    assert_eq!(base, resolve(tmp.path(), "main"));
    std::fs::write(tmp.path().join("src/new.py"), "n\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "main advances"]);
    let (_branch, refreshed) = broker.integration_head().unwrap();
    assert_eq!(refreshed, resolve(tmp.path(), "main"), "fast-forwarded");
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "merge.integration_refreshed")
    );

    // A promotion puts a merge commit on integration that main lacks:
    // advancing main afterwards must NOT clobber it.
    let wt = agent_worktree(tmp.path(), "keeper");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 40\n");
    let out = broker.submit(session.id).unwrap();
    assert!(out.promoted);
    let promoted_tip = resolve(tmp.path(), "aethyme/integration");

    std::fs::write(tmp.path().join("src/other.py"), "o\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "main advances again"]);
    let (_branch, still) = broker.integration_head().unwrap();
    assert_eq!(
        still, promoted_tip,
        "unmerged promotions are never clobbered by a refresh"
    );
}

// ── #42: main-checkout sessions get real (advisory) verification ────────

#[test]
fn main_checkout_submission_selects_gates_from_pre_refresh_base() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // First submission from the main checkout creates the integration
    // branch at the session's own head — no earlier verified state exists,
    // so this one is legitimately vacuous.
    let session = broker.adopt(tmp.path(), Some("main-root work")).unwrap();
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "first main change"]);
    let first = broker.submit(session.id).unwrap();
    assert!(first.promoted);

    // Second submission: integration now lags main by exactly this new
    // commit. Pre-#42 the follows-main refresh made base == head and the
    // entry verified with gates:[] — now the gate must actually run.
    std::fs::write(tmp.path().join("src/b.py"), "b = 2\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "second main change"]);
    let second = broker.submit(session.id).unwrap();
    assert!(second.promoted);
    assert!(
        !second.gate_outcomes.is_empty(),
        "main-checkout submission must select gates from the pre-refresh \
         integration head, not verify vacuously (#42)"
    );
    assert_eq!(second.gate_outcomes[0].gate, "marker");
}

// ── #41: action-required drop is cleared once the session promotes ─────

#[test]
fn action_required_is_cleared_after_successful_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Session 1 promotes a change to src/a.py.
    let wt1 = agent_worktree(tmp.path(), "one");
    let s1 = broker.adopt(&wt1, Some("one")).unwrap();
    std::fs::write(wt1.join("src/a.py"), "a = 111\n").unwrap();
    sh(&wt1, &["add", "-A"]);
    sh(&wt1, &["commit", "-qm", "one edits a"]);
    assert!(broker.submit(s1.id).unwrap().promoted);

    // Session 2 conflicts on the same file → action-required appears.
    let wt2 = agent_worktree(tmp.path(), "two");
    let s2 = broker.adopt(&wt2, Some("two")).unwrap();
    std::fs::write(wt2.join("src/a.py"), "a = 222\n").unwrap();
    sh(&wt2, &["add", "-A"]);
    sh(&wt2, &["commit", "-qm", "two edits a"]);
    let conflicted = broker.submit(s2.id).unwrap();
    assert!(!conflicted.conflicts.is_empty());
    let drop_path = wt2.join(aethyme_broker::ACTION_REQUIRED_RELPATH);
    assert!(drop_path.exists(), "conflict must write the action file");

    // Resolve exactly as the instructions say, resubmit → promoted, and
    // the stale action file is gone (agent A4's report: it survived with
    // outdated blocking info).
    sh(&wt2, &["fetch", ".", &conflicted.entry.base_commit]);
    let rebase = Command::new("git")
        .args(["rebase", &conflicted.entry.base_commit])
        .current_dir(&wt2)
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    if !rebase.status.success() {
        // conflict: take ours and continue
        std::fs::write(wt2.join("src/a.py"), "a = 333\n").unwrap();
        sh(&wt2, &["add", "-A"]);
        let cont = Command::new("git")
            .args(["rebase", "--continue"])
            .current_dir(&wt2)
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap();
        assert!(cont.status.success(), "rebase --continue failed");
    }
    let resubmit = broker.submit(s2.id).unwrap();
    assert!(resubmit.promoted, "resubmission after rebase must promote");
    assert!(
        !drop_path.exists(),
        "stale action-required drop must be cleared on promotion (#41)"
    );
}

#[test]
fn status_reports_promoted_unmerged_work_as_separate_conflict_surface() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_promoted = agent_worktree(tmp.path(), "promoted");
    let promoted = broker.adopt(&wt_promoted, Some("promoted")).unwrap();
    commit_edit(&wt_promoted, "src/a.py", "a = 20\n");
    assert!(broker.submit(promoted.id).unwrap().promoted);
    broker.close(promoted.id).unwrap();

    let wt_live = agent_worktree(tmp.path(), "live");
    let live = broker.adopt(&wt_live, Some("live")).unwrap();
    commit_edit(&wt_live, "src/a.py", "a = 30\n");

    let status = broker.status(0).unwrap();
    assert!(
        status.overlaps.is_empty(),
        "closed-session leases stay purged; the promoted branch owns this surface"
    );
    assert_eq!(status.promoted_conflicts.len(), 1);
    let conflict = &status.promoted_conflicts[0];
    assert_eq!(conflict.session_id, live.id);
    assert_eq!(conflict.path, "src/a.py");
    assert_eq!(conflict.session_path, "src/a.py");
    assert_eq!(conflict.promoted_path, "src/a.py");

    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.promoted-conflict")
        .expect("promoted conflict should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.session_id, Some(live.id));
    assert!(advice.summary.contains("rebase onto aethyme/integration"));
    assert!(
        advice.evidence.iter().any(|line| line.contains("src/a.py")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command == &format!("aethyme broker repair --session {}", live.id)),
        "{advice:?}"
    );
}

#[test]
fn integration_status_reports_pending_layer_entries_files_and_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_promoted = agent_worktree(tmp.path(), "pending-promoted");
    let promoted = broker.adopt(&wt_promoted, Some("promoted layer")).unwrap();
    commit_edit(&wt_promoted, "src/a.py", "a = 20\n");
    let promoted_out = broker.submit(promoted.id).unwrap();
    assert!(promoted_out.promoted);
    broker.close(promoted.id).unwrap();

    let wt_live = agent_worktree(tmp.path(), "pending-live");
    let live = broker.adopt(&wt_live, Some("live overlap")).unwrap();
    commit_edit(&wt_live, "src/a.py", "a = 30\n");

    let report = broker.integration_status(0).unwrap();
    assert_eq!(report.branch, "aethyme/integration");
    assert_eq!(report.head, resolve(tmp.path(), "aethyme/integration"));
    assert_eq!(report.main_head, resolve(tmp.path(), "main"));
    assert!(report.main_is_ancestor);
    assert!(report.commits_ahead_main > 0);
    assert_eq!(report.changed_files, vec!["src/a.py".to_string()]);

    assert_eq!(report.promoted_entries.len(), 1);
    let entry = &report.promoted_entries[0];
    assert_eq!(entry.queue_entry_id, promoted_out.entry.id);
    assert_eq!(entry.session_id, promoted.id);
    assert_eq!(entry.task.as_deref(), Some("promoted layer"));
    assert_eq!(entry.files, vec!["src/a.py".to_string()]);

    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].session_id, live.id);
    assert_eq!(report.conflicts[0].path, "src/a.py");
    assert!(
        report
            .next_action
            .summary
            .contains("pending integration layer")
    );
    assert!(
        report
            .next_action
            .commands
            .iter()
            .any(|command| command == &format!("aethyme broker repair --session {}", live.id)),
        "{report:?}"
    );
    assert_eq!(report.next_action.state, IntegrationDeliveryState::Blocked);

    sh(tmp.path(), &["merge", "--ff-only", "aethyme/integration"]);
    let report = broker.integration_status(0).unwrap();
    assert!(report.changed_files.is_empty());
    assert!(report.promoted_entries.is_empty());
    assert!(
        report.conflicts.is_empty(),
        "once integration is merged to main, this focused command should not \
         label old-main drift as promoted-but-unmerged work"
    );
    assert_eq!(
        report.next_action.summary,
        "local main is synchronized with aethyme/integration"
    );
    assert_eq!(
        report.next_action.state,
        IntegrationDeliveryState::LocallySynchronized
    );
}

#[test]
fn repair_rebases_promoted_conflict_and_reports_affected_gates() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::write(tmp.path().join("src/a.py"), "a = 1\nmiddle = 1\nb = 1\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "make separate lines"]);
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_promoted = agent_worktree(tmp.path(), "promoted-repair");
    let promoted = broker.adopt(&wt_promoted, Some("promoted")).unwrap();
    commit_edit(&wt_promoted, "src/a.py", "a = 20\nmiddle = 1\nb = 1\n");
    assert!(broker.submit(promoted.id).unwrap().promoted);
    broker.close(promoted.id).unwrap();

    let wt_live = agent_worktree(tmp.path(), "live-repair");
    let live = broker.adopt(&wt_live, Some("live")).unwrap();
    commit_edit(&wt_live, "src/a.py", "a = 1\nmiddle = 1\nb = 30\n");

    let before = broker.status(0).unwrap();
    assert_eq!(before.promoted_conflicts.len(), 1);

    let report = broker.repair(live.id).unwrap();
    assert_eq!(report.source, RepairSource::PromotedConflict);
    assert_eq!(report.action, RepairAction::Rebased);
    assert!(report.leases_refreshed);
    assert_eq!(
        report.next_command,
        format!("aethyme broker submit --session {}", live.id)
    );
    assert!(
        report
            .affected_gates
            .iter()
            .any(|gate| gate.gate == "marker"),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(wt_live.join("src/a.py")).unwrap(),
        "a = 20\nmiddle = 1\nb = 30\n"
    );

    let after = broker.status(0).unwrap();
    assert!(
        after.promoted_conflicts.is_empty(),
        "repair should refresh leases against the rebased worktree"
    );
}

#[test]
fn session_161_rebase_submit_promote_continue_and_submit_again() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "session-161");
    let session = broker
        .adopt(&worktree, Some("session 161 sequence"))
        .unwrap();
    let adopted = resolve(&worktree, "HEAD");

    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let concurrent = agent_worktree(tmp.path(), "session-161-concurrent");
    let concurrent_session = broker.adopt(&concurrent, None).unwrap();
    commit_edit(&concurrent, "src/b.py", "b = 2\n");
    assert!(broker.submit(concurrent_session.id).unwrap().promoted);
    broker.close(concurrent_session.id).unwrap();

    sh(
        &worktree,
        &["rebase", "--onto", "aethyme/integration", &adopted, "HEAD"],
    );
    commit_edit(&worktree, "src/b.py", "b = 3\n");
    let first_accepted_head = resolve(&worktree, "HEAD");
    let first = broker.submit(session.id).unwrap();
    assert!(first.promoted);
    assert_eq!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .accepted_session_head,
        Some(first_accepted_head.clone())
    );

    std::fs::write(worktree.join("src/follow_up.py"), "pending = true\n").unwrap();
    sh(&worktree, &["add", "src/follow_up.py"]);
    sh(&worktree, &["commit", "-qm", "follow-up after promotion"]);
    let follow_up = resolve(&worktree, "HEAD");
    let second = broker.submit(session.id).unwrap();

    assert!(second.promoted);
    assert_eq!(
        second.submission_plan.recorded_baseline.as_deref(),
        Some(first_accepted_head.as_str())
    );
    assert_eq!(second.submission_plan.commits.len(), 1);
    assert_eq!(second.submission_plan.commits[0].commit, follow_up);
    assert_eq!(
        second.submission_plan.commits[0].ownership,
        SubmissionCommitOwnership::SessionOwned
    );
}

#[test]
fn repair_replays_only_submission_plan_pending_commits_after_a_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::write(
        tmp.path().join("src/a.py"),
        "top = 1\nmiddle = 1\nbottom = 1\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "separate repair lines"]);
    let mut broker = Broker::open(tmp.path()).unwrap();

    let worktree = agent_worktree(tmp.path(), "checkpoint-repair");
    let session = broker.adopt(&worktree, Some("repair follow-up")).unwrap();
    commit_edit(&worktree, "src/b.py", "b = 2\n");
    assert!(broker.submit(session.id).unwrap().promoted);
    let accepted = broker
        .store()
        .session(session.id)
        .unwrap()
        .accepted_session_head
        .unwrap();

    commit_edit(&worktree, "src/a.py", "top = 1\nmiddle = 1\nbottom = 2\n");
    let pending_before_repair = resolve(&worktree, "HEAD");

    let concurrent = agent_worktree_at(
        tmp.path(),
        "checkpoint-repair-concurrent",
        "aethyme/integration",
    );
    let concurrent_session = broker.adopt(&concurrent, None).unwrap();
    commit_edit(&concurrent, "src/a.py", "top = 2\nmiddle = 1\nbottom = 1\n");
    assert!(broker.submit(concurrent_session.id).unwrap().promoted);
    broker.close(concurrent_session.id).unwrap();
    let repair_base = resolve(tmp.path(), "aethyme/integration");

    broker.refresh_leases().unwrap();

    let report = broker.repair(session.id).unwrap();
    assert_eq!(report.action, RepairAction::Rebased);
    assert_eq!(report.pending_commits, vec![pending_before_repair]);
    assert!(report.submission_plan.safe);
    assert_eq!(report.base.as_deref(), Some(repair_base.as_str()));

    let repaired = broker.store().session(session.id).unwrap();
    assert_eq!(
        repaired.accepted_session_head.as_deref(),
        Some(accepted.as_str())
    );
    assert_eq!(repaired.diff_base.as_deref(), Some(repair_base.as_str()));
    assert_ne!(
        repaired.diff_base.as_deref(),
        Some(resolve(&worktree, "HEAD").as_str()),
        "repair must never record the pending commit as its new baseline"
    );

    let reused = broker
        .adopt_with(&worktree, Some("continue repaired work"), AdoptMode::Reuse)
        .unwrap();
    assert_eq!(
        reused.integration_drift.unwrap().safe_next_action,
        format!("aethyme broker submit --session {}", session.id)
    );
    let before_submit = broker.finish(session.id).unwrap();
    assert_eq!(before_submit.status, FinishStatus::Blocked);
    assert_eq!(before_submit.unsubmitted_commits, 1);

    let submitted = broker.submit(session.id).unwrap();
    assert!(submitted.promoted);
    assert_eq!(submitted.submission_plan.commits.len(), 1);
    assert_eq!(
        std::fs::read_to_string(worktree.join("src/a.py")).unwrap(),
        "top = 2\nmiddle = 1\nbottom = 2\n"
    );
    sh(&worktree, &["reset", "--hard", "aethyme/integration"]);
    assert_eq!(
        broker.finish(session.id).unwrap().status,
        FinishStatus::Closed
    );
}

#[test]
fn unsafe_repair_guidance_preserves_pending_commits_before_any_reset() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "unsafe-repair");
    let session = broker
        .adopt(&worktree, Some("preserve pending work"))
        .unwrap();
    commit_edit(&worktree, "src/a.py", "a = 2\n");
    assert!(broker.submit(session.id).unwrap().promoted);

    let concurrent = agent_worktree_at(
        tmp.path(),
        "unsafe-repair-concurrent",
        "aethyme/integration",
    );
    let concurrent_session = broker.adopt(&concurrent, None).unwrap();
    commit_edit(&concurrent, "src/a.py", "a = 3\n");
    assert!(broker.submit(concurrent_session.id).unwrap().promoted);
    broker.close(concurrent_session.id).unwrap();

    commit_edit(&worktree, "src/a.py", "a = 4\n");
    let conflicted = broker.submit(session.id).unwrap();
    assert_eq!(conflicted.entry.status, MergeStatus::Conflict);

    sh(&worktree, &["reset", "--hard", "aethyme/integration"]);
    commit_edit(&worktree, "src/b.py", "b = 4\n");
    let pending = resolve(&worktree, "HEAD");
    let error = broker.repair(session.id).unwrap_err().to_string();

    assert!(error.contains(&pending), "{error}");
    assert!(!error.contains("adopt/reuse"), "{error}");
    let preserve = error.find("git branch aethyme/preserve-session-").unwrap();
    let reset = error.find("git reset --hard").unwrap();
    assert!(preserve < reset, "preservation must precede reset: {error}");
    assert!(error.contains("git cherry-pick"), "{error}");
}

#[test]
fn broker_open_checkpoints_a_promotion_interrupted_after_the_ref_move() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"manual\"\n",
    )
    .unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "promotion-checkpoint-recovery");
    let session = broker
        .adopt(&worktree, Some("recover promotion checkpoint"))
        .unwrap();
    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let submitted_head = resolve(&worktree, "HEAD");
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let details: serde_json::Value =
        serde_json::from_str(outcome.entry.details_json.as_deref().unwrap()).unwrap();
    let merge_commit = details["merge_commit"].as_str().unwrap();

    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", merge_commit],
    );
    drop(broker);

    let mut recovered = Broker::open(tmp.path()).unwrap();
    let persisted = recovered.store().session(session.id).unwrap();
    assert_eq!(persisted.accepted_session_head, Some(submitted_head));
    assert_eq!(
        persisted.accepted_integration_commit.as_deref(),
        Some(merge_commit)
    );
    assert_eq!(
        recovered
            .store()
            .merge_queue()
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == outcome.entry.id)
            .unwrap()
            .status,
        MergeStatus::Promoted
    );
    let recovered_exposure = recovered
        .store()
        .entry_path_exposure(outcome.entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered_exposure.promotion_sha, merge_commit);
    assert_eq!(recovered_exposure.paths, vec!["src/a.py"]);

    commit_edit(&worktree, "src/b.py", "b = 2\n");
    let follow_up = recovered.submit(session.id).unwrap();
    assert_eq!(follow_up.entry.status, MergeStatus::Verified);
    assert_eq!(follow_up.submission_plan.commits.len(), 1);
}

#[test]
fn broker_open_backfills_exposure_for_a_pre_v17_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let worktree = agent_worktree(tmp.path(), "legacy-promotion-exposure");
    let session = broker
        .adopt(&worktree, Some("legacy promoted entry"))
        .unwrap();
    commit_edit(&worktree, "src/a.py", "a = legacy promotion\n");
    let outcome = broker.submit(session.id).unwrap();
    assert!(outcome.promoted);
    let promotion_sha = resolve(tmp.path(), "aethyme/integration");
    drop(broker);

    let database = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    database
        .execute(
            "DELETE FROM entry_path_exposures WHERE queue_entry_id = ?1",
            [outcome.entry.id],
        )
        .unwrap();
    drop(database);

    let mut reopened = Broker::open(tmp.path()).unwrap();
    let exposure = reopened
        .store()
        .entry_path_exposure(outcome.entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(exposure.promotion_sha, promotion_sha);
    assert_eq!(exposure.paths, vec!["src/a.py"]);
    assert_eq!(exposure.state, EntryExposureState::Outstanding);
}

#[test]
fn reconcile_recognizes_squash_preserves_followups_and_replays_pending_work() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = agent_worktree(tmp.path(), "reconcile-a");
    let wt_b = agent_worktree(tmp.path(), "reconcile-b");
    let wt_pending = agent_worktree(tmp.path(), "reconcile-pending");
    let session_a = broker.adopt(&wt_a, Some("land a externally")).unwrap();
    let session_b = broker.adopt(&wt_b, Some("land b externally")).unwrap();
    let pending = broker
        .adopt(&wt_pending, Some("keep pending work"))
        .unwrap();
    commit_edit(&wt_a, "src/a.py", "a = 2\n");
    commit_edit(&wt_b, "src/b.py", "b = 2\n");
    commit_edit(&wt_pending, "src/c.py", "c = 2\n");
    let promoted_a = broker.submit(session_a.id).unwrap();
    let promoted_b = broker.submit(session_b.id).unwrap();
    let promoted_pending = broker.submit(pending.id).unwrap();
    assert!(promoted_a.promoted && promoted_b.promoted && promoted_pending.promoted);

    // One upstream squash represents two queue entries, followed by a fix
    // touching the same file. Local main deliberately remains at the old
    // base, exactly like a fetch without a local checkout update.
    sh(tmp.path(), &["switch", "-qc", "external-upstream", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    std::fs::write(tmp.path().join("src/b.py"), "b = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py", "src/b.py"]);
    sh(
        tmp.path(),
        &["commit", "-qm", "squash externally landed work"],
    );
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\nfollowup = 1\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "follow-up production fix"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let old_integration = resolve(tmp.path(), "aethyme/integration");
    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");
    assert!(!dry_run.applied);
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
    assert_eq!(
        dry_run
            .entries
            .iter()
            .filter(|entry| {
                entry.classification == IntegrationReconcileClassification::AlreadyLanded
            })
            .count(),
        2,
        "the two local promotions should match one upstream squash: {dry_run:?}"
    );
    assert_eq!(
        dry_run
            .entries
            .iter()
            .filter(|entry| {
                entry.classification == IntegrationReconcileClassification::StillPending
            })
            .count(),
        1,
        "unshipped promoted work must remain pending: {dry_run:?}"
    );

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: dry_run.plan_digest.clone(),
        })
        .unwrap();
    assert!(applied.safe && applied.applied);
    assert!(
        Command::new("git")
            .args([
                "merge-base",
                "--is-ancestor",
                "origin/main",
                "aethyme/integration",
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/a.py"),
        "a = 2\nfollowup = 1\n",
        "upstream follow-up fixes must survive reconciliation"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/c.py"),
        "c = 2\n",
        "genuinely pending promoted work must be replayed"
    );
    for landed_entry in [promoted_a.entry.id, promoted_b.entry.id] {
        assert_eq!(
            broker
                .store()
                .entry_path_exposure(landed_entry)
                .unwrap()
                .unwrap()
                .state,
            EntryExposureState::Resolved
        );
    }
    let replayed_pending = applied
        .entries
        .iter()
        .find(|entry| entry.queue_entry_id == promoted_pending.entry.id)
        .and_then(|entry| entry.replayed_commit.as_deref())
        .unwrap();
    let pending_exposure = broker
        .store()
        .entry_path_exposure(promoted_pending.entry.id)
        .unwrap()
        .unwrap();
    assert_eq!(pending_exposure.state, EntryExposureState::Outstanding);
    assert_eq!(pending_exposure.promotion_sha, replayed_pending);
    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == promoted_a.entry.id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == promoted_b.entry.id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == promoted_pending.entry.id)
            .unwrap()
            .status,
        MergeStatus::Promoted
    );

    // A fresh session adopted from production submits only its own delta;
    // the already-landed upstream commits are below its recorded baseline.
    let wt_new = agent_worktree_at(tmp.path(), "after-reconcile", "origin/main");
    let fresh = broker.adopt(&wt_new, Some("new delta only")).unwrap();
    commit_edit(&wt_new, "src/d.py", "d = 1\n");
    let fresh_out = broker.submit(fresh.id).unwrap();
    assert!(
        fresh_out.promoted,
        "new work should submit after reconciliation"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/d.py"),
        "d = 1\n"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/a.py"),
        "a = 2\nfollowup = 1\n"
    );
}

#[test]
fn reconcile_blocks_ambiguous_patch_equivalence_without_mutating_state() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = agent_worktree(tmp.path(), "ambiguous-promotion");
    let session = broker.adopt(&wt, Some("ambiguous upstream")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let promoted = broker.submit(session.id).unwrap();
    let old_integration = resolve(tmp.path(), "aethyme/integration");

    sh(tmp.path(), &["switch", "-qc", "ambiguous-upstream", "main"]);
    for (content, message) in [
        ("a = 2\n", "apply equivalent patch once"),
        ("a = 1\n", "revert equivalent patch"),
        ("a = 2\n", "apply equivalent patch twice"),
    ] {
        std::fs::write(tmp.path().join("src/a.py"), content).unwrap();
        sh(tmp.path(), &["add", "src/a.py"]);
        sh(tmp.path(), &["commit", "-qm", message]);
    }
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(!report.safe, "{report:#?}");
    assert!(!report.applied);
    assert_eq!(
        report.entries[0].classification,
        IntegrationReconcileClassification::Ambiguous
    );
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == promoted.entry.id)
            .unwrap()
            .status,
        MergeStatus::Promoted
    );
}

#[test]
fn reconcile_accepts_commit_bound_operator_supersession_with_durable_audit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt_a = agent_worktree(tmp.path(), "operator-resolution-a");
    let wt_b = agent_worktree(tmp.path(), "operator-resolution-b");
    let session_a = broker.adopt(&wt_a, Some("superseded a")).unwrap();
    let session_b = broker.adopt(&wt_b, Some("superseded b")).unwrap();
    commit_edit(&wt_a, "src/a.py", "a = 2\n");
    commit_edit(&wt_b, "src/b.py", "b = 2\n");
    let promoted_a = broker.submit(session_a.id).unwrap();
    let promoted_b = broker.submit(session_b.id).unwrap();
    let old_integration = resolve(tmp.path(), "aethyme/integration");
    let promoted_a_commit = promoted_merge_commit(&promoted_a.entry);
    let promoted_b_commit = promoted_merge_commit(&promoted_b.entry);

    // Both features landed externally and were subsequently improved, so
    // neither original patch nor its final path content matches upstream.
    sh(tmp.path(), &["switch", "-qc", "operator-upstream", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 3\n").unwrap();
    std::fs::write(tmp.path().join("src/b.py"), "b = 3\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py", "src/b.py"]);
    sh(
        tmp.path(),
        &["commit", "-qm", "land and improve both features"],
    );
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let upstream = resolve(tmp.path(), "origin/main");
    sh(tmp.path(), &["switch", "main"]);

    let blocked = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(!blocked.safe, "{blocked:#?}");
    assert_eq!(
        blocked.entries[0].classification,
        IntegrationReconcileClassification::GenuinelyConflicting
    );
    assert_eq!(
        blocked.entries[1].classification,
        IntegrationReconcileClassification::Ambiguous
    );

    let resolution_path = tmp.path().join("reconciliation.json");
    let resolution_document = |upstream_commit: &str| {
        serde_json::json!({
            "schema_version": 1,
            "upstream_ref": "origin/main",
            "upstream_commit": upstream_commit,
            "old_integration": old_integration,
            "operator": "release-operator@example.test",
            "resolutions": [
                {
                    "queue_entry_id": promoted_a.entry.id,
                    "old_merge_commit": promoted_a_commit,
                    "classification": "superseded_upstream",
                    "reason": "landed externally and improved in the production series"
                },
                {
                    "queue_entry_id": promoted_b.entry.id,
                    "old_merge_commit": promoted_b_commit,
                    "classification": "superseded_upstream",
                    "reason": "landed externally and improved in the production series"
                }
            ]
        })
    };

    // A stale or mistyped upstream binding fails before planning or mutation.
    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&resolution_document(&"0".repeat(40))).unwrap(),
    )
    .unwrap();
    let stale_error = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path.clone()),
            confirm: None,
        })
        .unwrap_err();
    assert!(stale_error.to_string().contains("upstream_commit"));
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
    assert!(
        broker
            .store()
            .merge_queue()
            .unwrap()
            .iter()
            .filter(|entry| [promoted_a.entry.id, promoted_b.entry.id].contains(&entry.id))
            .all(|entry| entry.status == MergeStatus::Promoted)
    );

    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&resolution_document(&upstream)).unwrap(),
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
    assert!(dry_run.safe && !dry_run.applied, "{dry_run:#?}");
    assert_eq!(dry_run.new_integration, upstream);
    assert!(dry_run.entries.iter().all(|entry| {
        entry.classification == IntegrationReconcileClassification::SupersededUpstream
            && entry.operator_resolution.is_some()
    }));
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path),
            confirm: dry_run.plan_digest.clone(),
        })
        .unwrap();
    assert!(applied.safe && applied.applied, "{applied:#?}");
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), upstream);
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/a.py"),
        "a = 3\n"
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/b.py"),
        "b = 3\n"
    );

    let queue = broker.store().merge_queue().unwrap();
    assert!(
        queue
            .iter()
            .filter(|entry| [promoted_a.entry.id, promoted_b.entry.id].contains(&entry.id))
            .all(|entry| entry.status == MergeStatus::ExternallyLanded)
    );
    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    let details: String = db
        .query_row(
            "SELECT details_json FROM integration_reconciliation_entries
             WHERE queue_entry_id = ?1 ORDER BY id DESC LIMIT 1",
            [promoted_a.entry.id],
            |row| row.get(0),
        )
        .unwrap();
    let details: serde_json::Value = serde_json::from_str(&details).unwrap();
    assert_eq!(
        details["operator_resolution"]["operator"],
        "release-operator@example.test"
    );
    assert_eq!(details["operator_resolution"]["upstream_commit"], upstream);
}

#[test]
fn broker_open_finishes_reconciliation_interrupted_after_ref_move() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = agent_worktree(tmp.path(), "crash-recovery-promotion");
    let session = broker.adopt(&wt, Some("crash recovery proof")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let promoted = broker.submit(session.id).unwrap();

    sh(
        tmp.path(),
        &["switch", "-qc", "crash-recovery-upstream", "main"],
    );
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "external squash"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(report.safe);
    let entry = &report.entries[0];
    let details = serde_json::json!({
        "branch": report.branch,
        "commit": entry.old_merge_commit,
        "externally_landed": true,
        "classification": "already_landed",
        "upstream_ref": report.upstream_ref,
        "upstream_landing": entry.upstream_landing,
    })
    .to_string();
    let db_path = tmp.path().join(".aethyme/broker.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO integration_reconciliation_intent
            (id, branch, upstream_ref, local_main_commit, old_integration,
             upstream_commit, new_integration, created_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, 1)",
        rusqlite::params![
            report.branch,
            report.upstream_ref,
            report.local_main,
            report.old_integration,
            report.upstream_head,
            report.new_integration,
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO integration_reconciliation_intent_entries
            (queue_entry_id, status, details_json, classification,
             old_merge_commit, upstream_landing)
         VALUES (?1, 'externally_landed', ?2, 'already_landed', ?3, ?4)",
        rusqlite::params![
            entry.queue_entry_id,
            details,
            entry.old_merge_commit,
            entry.upstream_landing,
        ],
    )
    .unwrap();
    drop(db);
    sh(
        tmp.path(),
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            &report.new_integration,
            &report.old_integration,
        ],
    );
    drop(broker); // Simulate process death before phase-two SQLite commit.

    let mut recovered = Broker::open(tmp.path()).unwrap();
    let queue = recovered.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|queue_entry| queue_entry.id == promoted.entry.id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    let db = rusqlite::Connection::open(db_path).unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM integration_reconciliation_intent",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "recovery must consume the durable intent");
}

#[test]
fn reconcile_refuses_to_discard_unrecorded_integration_commits() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.integration_head().unwrap();
    sh(tmp.path(), &["switch", "aethyme/integration"]);
    std::fs::write(tmp.path().join("src/unrecorded.py"), "keep = 1\n").unwrap();
    sh(tmp.path(), &["add", "src/unrecorded.py"]);
    sh(
        tmp.path(),
        &["commit", "-qm", "unrecorded integration work"],
    );
    let old_integration = resolve(tmp.path(), "aethyme/integration");
    sh(tmp.path(), &["switch", "main"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "main"],
    );

    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(!report.safe);
    assert!(!report.applied);
    assert!(report.warnings[0].contains("unrecorded work"));
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
}

#[test]
fn promotion_does_not_resurrect_conflicts_from_cleaned_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let stale_a_wt = agent_worktree(tmp.path(), "cleaned-stale-a");
    let stale_b_wt = agent_worktree(tmp.path(), "cleaned-stale-b");
    let blocker_wt = agent_worktree(tmp.path(), "cleaned-blocker");
    let stale_a = broker.adopt(&stale_a_wt, Some("old a")).unwrap();
    let stale_b = broker.adopt(&stale_b_wt, Some("old b")).unwrap();
    let blocker = broker
        .adopt(&blocker_wt, Some("temporary blocker"))
        .unwrap();
    commit_edit(&stale_a_wt, "src/a.py", "a = 2\n");
    commit_edit(&stale_b_wt, "src/b.py", "b = 2\n");
    commit_edit(&blocker_wt, "src/a.py", "a = 99\n");
    commit_edit(&blocker_wt, "src/b.py", "b = 99\n");

    let blocker_entry = broker.submit(blocker.id).unwrap();
    assert!(blocker_entry.promoted);
    let old_a = broker.submit(stale_a.id).unwrap();
    let old_b = broker.submit(stale_b.id).unwrap();
    assert_eq!(old_a.entry.status, MergeStatus::Conflict);
    assert_eq!(old_b.entry.status, MergeStatus::Conflict);
    broker.close(stale_a.id).unwrap();
    broker.close(stale_b.id).unwrap();

    // Both old submitted heads later land upstream. Model an already
    // completed reconciliation by moving only the integration ref; local
    // main deliberately remains at the original checkout commit.
    sh(tmp.path(), &["switch", "-qc", "external-cleaned", "main"]);
    sh(
        tmp.path(),
        &["merge", "--no-edit", "--no-ff", "agent/cleaned-stale-a"],
    );
    sh(
        tmp.path(),
        &["merge", "--no-edit", "--no-ff", "agent/cleaned-stale-b"],
    );
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);
    broker
        .store()
        .set_merge_status(
            blocker_entry.entry.id,
            MergeStatus::ExternallyLanded,
            None,
            None,
        )
        .unwrap();

    let fresh_wt = agent_worktree_at(tmp.path(), "after-cleaned", "origin/main");
    let fresh = broker.adopt(&fresh_wt, Some("fresh work")).unwrap();
    commit_edit(&fresh_wt, "src/c.py", "c = 2\n");
    assert!(broker.submit(fresh.id).unwrap().promoted);

    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == old_a.entry.id)
            .unwrap()
            .status,
        MergeStatus::Superseded
    );
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == old_b.entry.id)
            .unwrap()
            .status,
        MergeStatus::Superseded
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/c.py"),
        "c = 2\n"
    );
}

#[test]
fn reconcile_uses_upstream_boundary_and_recovers_repromoted_empty_entries() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let stale_a_wt = agent_worktree(tmp.path(), "active-stale-a");
    let stale_b_wt = agent_worktree(tmp.path(), "active-stale-b");
    let blocker_wt = agent_worktree(tmp.path(), "active-blocker");
    let stale_a = broker.adopt(&stale_a_wt, Some("old a")).unwrap();
    let stale_b = broker.adopt(&stale_b_wt, Some("old b")).unwrap();
    let blocker = broker
        .adopt(&blocker_wt, Some("temporary blocker"))
        .unwrap();
    commit_edit(&stale_a_wt, "src/a.py", "a = 2\n");
    commit_edit(&stale_b_wt, "src/b.py", "b = 2\n");
    commit_edit(&blocker_wt, "src/a.py", "a = 99\n");
    commit_edit(&blocker_wt, "src/b.py", "b = 99\n");

    let blocker_entry = broker.submit(blocker.id).unwrap();
    let old_a = broker.submit(stale_a.id).unwrap();
    let old_b = broker.submit(stale_b.id).unwrap();
    assert_eq!(old_a.entry.status, MergeStatus::Conflict);
    assert_eq!(old_b.entry.status, MergeStatus::Conflict);

    sh(tmp.path(), &["switch", "-qc", "external-active", "main"]);
    sh(
        tmp.path(),
        &["merge", "--no-edit", "--no-ff", "agent/active-stale-a"],
    );
    sh(
        tmp.path(),
        &["merge", "--no-edit", "--no-ff", "agent/active-stale-b"],
    );
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["config", "remote.origin.url", "."]);
    sh(
        tmp.path(),
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    sh(tmp.path(), &["config", "branch.main.remote", "origin"]);
    sh(
        tmp.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    let upstream = resolve(tmp.path(), "origin/main");
    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);
    broker
        .store()
        .set_merge_status(
            blocker_entry.entry.id,
            MergeStatus::ExternallyLanded,
            None,
            None,
        )
        .unwrap();

    let fresh_wt = agent_worktree_at(tmp.path(), "reconcile-fresh", "origin/main");
    let fresh = broker.adopt(&fresh_wt, Some("fresh delta")).unwrap();
    commit_edit(&fresh_wt, "src/c.py", "c = 2\n");
    let fresh_out = broker.submit(fresh.id).unwrap();
    assert!(fresh_out.promoted);

    // The active old conflicts are revalidated once each and become
    // content-empty promotions because their heads are already upstream.
    // Add one more empty bookkeeping commit to reproduce the v0.1.3 state
    // where a recursive stale snapshot promoted the same queue item twice.
    let integration = resolve(tmp.path(), "aethyme/integration");
    let tree = resolve(tmp.path(), "aethyme/integration^{tree}");
    let output = Command::new("git")
        .args([
            "commit-tree",
            &tree,
            "-p",
            &integration,
            "-m",
            "duplicate bookkeeping merge",
        ])
        .current_dir(tmp.path())
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success());
    let duplicate = String::from_utf8_lossy(&output.stdout).trim().to_string();
    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", &duplicate],
    );
    let resolution_path = tmp.path().join("empty-bookkeeping-resolution.json");
    std::fs::write(
        &resolution_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 2,
            "upstream_ref": "origin/main",
            "upstream_commit": upstream,
            "old_integration": duplicate,
            "operator": "release-operator@example.test",
            "unrecorded_resolutions": [{
                "integration_commit": duplicate,
                "disposition": "drop_because_content_empty",
                "reason": "reviewed duplicate bookkeeping commit with an unchanged tree"
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
    assert!(
        dry_run
            .warnings
            .iter()
            .any(|warning| warning.contains("validated reviewed dispositions"))
    );
    assert_eq!(
        dry_run.entries[0].queue_entry_id, fresh_out.entry.id,
        "entries must follow integration topology, not historical queue IDs"
    );
    assert_eq!(
        dry_run.entries[0].classification,
        IntegrationReconcileClassification::StillPending
    );
    assert!(dry_run.entries[1..].iter().all(|entry| {
        entry.classification == IntegrationReconcileClassification::AlreadyLanded
            && entry.evidence.contains("submitted session head")
    }));

    let applied = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: Some(resolution_path),
            confirm: dry_run.plan_digest.clone(),
        })
        .unwrap();
    assert!(applied.safe && applied.applied, "{applied:#?}");
    assert!(
        Command::new("git")
            .args([
                "merge-base",
                "--is-ancestor",
                &upstream,
                "aethyme/integration"
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        file_at(tmp.path(), "aethyme/integration", "src/c.py"),
        "c = 2\n"
    );

    let status = broker.status(0).unwrap();
    assert!(
        status.main_behind_upstream_commits > 0,
        "expected stale local main: main={}, upstream={:?}, integration={}",
        status.main_head,
        status.upstream_head,
        status.integration_head
    );
    let upstream_advice = status
        .advice
        .iter()
        .find(|advice| advice.id == "integration.upstream-main-ahead")
        .unwrap();
    assert_eq!(upstream_advice.severity, StatusAdviceSeverity::Notice);
    assert!(upstream_advice.commands.is_empty());
    let integration_status = broker.integration_status(0).unwrap();
    assert!(
        !integration_status
            .next_action
            .summary
            .contains("reconcile before")
    );
}

#[test]
fn failed_reconciliation_rolls_back_ref_and_database() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = agent_worktree(tmp.path(), "rollback-promotion");
    let session = broker.adopt(&wt, Some("rollback proof")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let promoted = broker.submit(session.id).unwrap();
    let old_integration = resolve(tmp.path(), "aethyme/integration");

    sh(tmp.path(), &["switch", "-qc", "rollback-upstream", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "external squash"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");

    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_reconcile
         BEFORE INSERT ON integration_reconciliations
         BEGIN SELECT RAISE(FAIL, 'injected reconciliation failure'); END;",
    )
    .unwrap();
    drop(db);

    let error = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: dry_run.plan_digest,
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected reconciliation failure")
    );
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == promoted.entry.id)
            .unwrap()
            .status,
        MergeStatus::Promoted,
        "failed apply must leave the broker row unchanged"
    );
}

#[test]
fn repair_refuses_when_session_baseline_is_newer_than_integration() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_promoted = agent_worktree(tmp.path(), "older-integration");
    let promoted = broker.adopt(&wt_promoted, Some("older layer")).unwrap();
    commit_edit(&wt_promoted, "src/a.py", "a = 2\n");
    assert!(broker.submit(promoted.id).unwrap().promoted);

    sh(tmp.path(), &["switch", "-qc", "newer-upstream", "main"]);
    std::fs::write(tmp.path().join("src/b.py"), "b = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/b.py"]);
    sh(tmp.path(), &["commit", "-qm", "upstream baseline"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let wt_live = agent_worktree_at(tmp.path(), "newer-session", "origin/main");
    let live = broker.adopt(&wt_live, Some("new production work")).unwrap();
    commit_edit(&wt_live, "src/a.py", "a = 3\n");
    let before = resolve(&wt_live, "HEAD");
    let error = broker.repair(live.id).unwrap_err().to_string();
    assert!(
        error.contains("does not contain replay boundary"),
        "{error}"
    );
    assert!(error.contains("Preservation-first recovery"), "{error}");
    assert!(!error.contains("adopt/reuse"), "{error}");
    assert_eq!(resolve(&wt_live, "HEAD"), before);
    assert!(!wt_live.join(".git/rebase-merge").exists());
}

#[test]
fn status_warns_on_the_first_external_main_movement() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    sh(tmp.path(), &["switch", "-qc", "status-upstream", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "upstream advances"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);
    sh(tmp.path(), &["config", "remote.origin.url", "."]);
    sh(
        tmp.path(),
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    sh(tmp.path(), &["config", "branch.main.remote", "origin"]);
    sh(
        tmp.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    let status = broker.status(0).unwrap();
    let expected_upstream = resolve(tmp.path(), "origin/main");
    assert_eq!(status.main_head, resolve(tmp.path(), "main"));
    assert_eq!(status.upstream_ref.as_deref(), Some("origin/main"));
    assert_eq!(
        status.upstream_head.as_deref(),
        Some(expected_upstream.as_str())
    );
    assert_eq!(status.main_behind_upstream_commits, 1);
    assert!(status.advice.iter().any(|advice| {
        advice.id == "integration.upstream-main-ahead"
            && advice.severity == StatusAdviceSeverity::Blocked
            && advice.summary.contains("external main movement detected")
            && advice.commands
                == vec!["aethyme broker integration reconcile --upstream origin/main --dry-run"]
    }));

    let integration = broker.integration_status(0).unwrap();
    assert_eq!(integration.main_behind_upstream_commits, 1);
    assert!(
        integration
            .next_action
            .summary
            .contains("external main movement detected")
    );
    assert!(
        integration
            .next_action
            .commands
            .iter()
            .any(|command| command.contains("--upstream origin/main --dry-run"))
    );
}

#[test]
fn status_reports_both_local_and_upstream_only_counts_after_divergence() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    sh(tmp.path(), &["switch", "-qc", "upstream-history", "main"]);
    for index in 1..=2 {
        std::fs::write(
            tmp.path().join(format!("upstream-{index}.txt")),
            format!("upstream {index}\n"),
        )
        .unwrap();
        sh(tmp.path(), &["add", "."]);
        sh(tmp.path(), &["commit", "-qm", &format!("upstream {index}")]);
    }
    let upstream = resolve(tmp.path(), "HEAD");

    sh(tmp.path(), &["switch", "main"]);
    std::fs::write(tmp.path().join("local-only.txt"), "local\n").unwrap();
    sh(tmp.path(), &["add", "local-only.txt"]);
    sh(tmp.path(), &["commit", "-qm", "local only"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", &upstream],
    );
    sh(tmp.path(), &["config", "remote.origin.url", "."]);
    sh(
        tmp.path(),
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    sh(tmp.path(), &["config", "branch.main.remote", "origin"]);
    sh(
        tmp.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    let status = broker.status(0).unwrap();
    assert_eq!(status.main_ahead_upstream_commits, 1);
    assert_eq!(status.main_behind_upstream_commits, 2);
    assert!(status.advice.iter().any(|advice| {
        advice.id == "integration.upstream-main-ahead"
            && advice.severity == StatusAdviceSeverity::Blocked
    }));

    let integration = broker.integration_status(0).unwrap();
    assert_eq!(integration.main_ahead_upstream_commits, 1);
    assert_eq!(integration.main_behind_upstream_commits, 2);
    assert_eq!(
        integration.next_action.state,
        IntegrationDeliveryState::Blocked
    );
}

#[test]
fn status_warns_when_local_and_upstream_main_move_together_beyond_integration() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    sh(tmp.path(), &["switch", "-qc", "promoted", "main"]);
    std::fs::write(tmp.path().join("src/b.py"), "b = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/b.py"]);
    sh(tmp.path(), &["commit", "-qm", "promoted integration work"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", "HEAD"],
    );
    sh(tmp.path(), &["switch", "-qc", "external-main", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(
        tmp.path(),
        &["commit", "-qm", "main advances outside broker"],
    );
    sh(tmp.path(), &["update-ref", "refs/heads/main", "HEAD"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);
    sh(tmp.path(), &["config", "remote.origin.url", "."]);
    sh(
        tmp.path(),
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    sh(tmp.path(), &["config", "branch.main.remote", "origin"]);
    sh(
        tmp.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );

    let status = broker.status(0).unwrap();
    assert_eq!(status.main_head, resolve(tmp.path(), "origin/main"));
    assert_eq!(status.upstream_ref.as_deref(), Some("origin/main"));
    assert_eq!(status.upstream_head, Some(status.main_head.clone()));
    assert_ne!(status.integration_head, status.main_head);
    assert_eq!(status.main_behind_upstream_commits, 0);
    assert!(
        status.advice.iter().any(|advice| {
            advice.id == "integration.upstream-main-ahead"
                && advice.severity == StatusAdviceSeverity::Blocked
                && advice
                    .summary
                    .contains("integration does not contain origin/main")
                && advice.commands
                    == vec!["aethyme broker integration reconcile --upstream origin/main --dry-run"]
        }),
        "{:?}",
        status.advice
    );

    let integration = broker.integration_status(0).unwrap();
    assert_eq!(integration.main_behind_upstream_commits, 0);
    assert_eq!(
        integration.next_action.state,
        IntegrationDeliveryState::Blocked
    );
    assert!(
        integration
            .next_action
            .summary
            .contains("integration does not contain origin/main")
    );
}

#[test]
fn status_advice_warns_about_dirty_worktree_wip() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt = agent_worktree(tmp.path(), "dirty");
    let session = broker.adopt(&wt, Some("dirty wip")).unwrap();
    std::fs::write(wt.join("src/a.py"), "a = 99\n").unwrap();

    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.dirty-worktree")
        .expect("dirty worktree should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Warning);
    assert_eq!(advice.session_id, Some(session.id));
    assert!(advice.summary.contains("uncommitted change"));
    assert!(
        advice.evidence.iter().any(|line| line.contains("src/a.py")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command.contains("status --short")),
        "{advice:?}"
    );
}
