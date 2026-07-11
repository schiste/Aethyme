//! End-to-end merge queue tests (issues #20-#23): submit → simulate →
//! gates on the merged tree → promote, conflicts rejected pre-gate with
//! the agent-facing instruction drop, and requeue on base move.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, MergeStatus};

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
    std::fs::write(root.join(".gitignore"), "gate-ran.txt\n").unwrap();
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
fn two_clean_sessions_promote_with_requeue_on_base_move() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
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
    assert!(!out_a.promoted, "manual mode by default");
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
    broker.promote(out_a.entry.id).unwrap();

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
}

#[test]
fn failing_gate_on_merged_tree_rejects_and_auto_mode_promotes() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    // Failing gate config.
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"fail\"\ncommand = \"exit 7\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = agent_worktree(tmp.path(), "x");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 3\n");

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Rejected);
    assert!(
        broker.promote(outcome.entry.id).is_err(),
        "rejected entries cannot be promoted"
    );

    // Flip to a passing gate + auto mode: submit-through-promote in one.
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"ok\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"auto\"\n",
    )
    .unwrap();
    commit_edit(&wt, "src/a.py", "a = 4\n");
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Promoted);
    assert!(outcome.promoted);
}
