//! End-to-end merge queue tests (issues #20-#23): submit → simulate →
//! gates on the merged tree → promote, conflicts rejected pre-gate with
//! the agent-facing instruction drop, and requeue on base move.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    Broker, IntegrationReconcileClassification, IntegrationReconcileOptions, MergeStatus,
    RepairAction, RepairSource, StatusAdviceSeverity,
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
    assert!(!out_a.promoted, "manual mode holds verified entries");
    let out_b = broker.submit(b.id).unwrap();
    assert_eq!(out_b.entry.status, MergeStatus::Verified);

    // Promote A: integration branch advances; B was verified against the
    // OLD base and gets re-simulated automatically.
    broker.promote(out_a.entry.id).unwrap();
    let integration = resolve(tmp.path(), "aethyme/integration");
    assert_ne!(integration, resolve(tmp.path(), "main"));

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
    // Leases exist so the conflict can name the blocking session.
    broker.refresh_leases().unwrap();

    let out_a = broker.submit(a.id).unwrap();
    assert!(out_a.promoted, "A lands immediately (auto default)");

    let out_b = broker.submit(b.id).unwrap();
    assert_eq!(out_b.entry.status, MergeStatus::Conflict);
    assert_eq!(out_b.conflicts, vec!["src/a.py".to_string()]);
    assert!(
        out_b.gate_outcomes.is_empty(),
        "conflict rejected BEFORE any gate ran"
    );
    // Marker file proves the gate never executed for B's submission
    // beyond A's earlier verified run.
    let details = out_b.entry.details_json.as_deref().unwrap();
    assert!(details.contains("\"blocking_sessions\":[1]") || details.contains(&a.id.to_string()));

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
        "no promoted work pending outside main"
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
        })
        .unwrap();
    assert!(!report.safe);
    assert!(!report.applied);
    assert!(report.warnings[0].contains("unrecorded work"));
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
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
        error.contains("does not contain recorded session baseline"),
        "{error}"
    );
    assert_eq!(resolve(&wt_live, "HEAD"), before);
    assert!(!wt_live.join(".git/rebase-merge").exists());
}

#[test]
fn status_distinguishes_stale_local_main_from_configured_upstream() {
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
    }));

    let integration = broker.integration_status(0).unwrap();
    assert_eq!(integration.main_behind_upstream_commits, 1);
    assert!(integration.next_action.summary.contains("reconcile"));
    assert!(
        integration
            .next_action
            .commands
            .iter()
            .any(|command| command.contains("--upstream origin/main --dry-run"))
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
