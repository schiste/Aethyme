//! The v0 scenario (issue #27): the deterministic, CI-safe proof of the
//! whole broker story with scripted agents — executable documentation of
//! the flow. Three sessions (two adopted, one spawned) on one repo:
//! an overlap warning, a conflicting submit rejected before any gate ran
//! (naming the blocking session), two clean submits gated on simulated
//! merges and promoted (the second re-simulated after the base moved),
//! and a complete ordered event timeline with cache-hit accounting.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, MergeStatus, SessionStatus};

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

fn commit_edit(worktree: &Path, file: &str, content: &str) {
    std::fs::write(worktree.join(file), content).unwrap();
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", "edit"]);
}

#[test]
fn v0_three_agents_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // ── fixture repo with one cheap gate ─────────────────────────────
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.py"), "auth = 1\n").unwrap();
    std::fs::write(root.join("src/api.py"), "api = 1\n").unwrap();
    std::fs::write(root.join("docs.md"), "docs\n").unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"py\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);

    let mut broker = Broker::open(root).unwrap();

    // ── register three sessions: two adopted, one spawned ────────────
    for name in ["alice", "bob"] {
        let wt = root.join(".aethyme/worktrees").join(name);
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
        sh(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("agent/{name}"),
                wt.to_str().unwrap(),
                "main",
            ],
        );
    }
    let alice_wt = root.join(".aethyme/worktrees/alice");
    let bob_wt = root.join(".aethyme/worktrees/bob");
    let alice = broker.adopt(&alice_wt, Some("refactor auth")).unwrap();
    let bob = broker.adopt(&bob_wt, Some("also touches auth")).unwrap();
    let carol = broker.start_agent("update api", "true").unwrap();
    let carol_wt = std::path::PathBuf::from(&carol.worktree_path);

    // ── work happens ─────────────────────────────────────────────────
    commit_edit(&alice_wt, "src/auth.py", "auth = 100\n");
    commit_edit(&bob_wt, "src/auth.py", "auth = 200\n"); // collision course
    commit_edit(&carol_wt, "src/api.py", "api = 2\n"); // disjoint

    // ── overlap warning fires exactly once for the alice/bob pair ────
    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].session_a, alice.id);
    assert_eq!(overlaps[0].session_b, bob.id);
    assert_eq!(overlaps[0].path, "src/auth.py");

    // ── alice submits: clean, gated, verified; then promoted ─────────
    let alice_out = broker.submit(alice.id).unwrap();
    assert_eq!(alice_out.entry.status, MergeStatus::Verified);
    assert_eq!(alice_out.gate_outcomes.len(), 1);
    assert!(!alice_out.gate_outcomes[0].cached, "first run executes");
    broker.promote(alice_out.entry.id).unwrap();

    // ── carol submits: clean vs the MOVED base, verified, promoted ───
    let carol_out = broker.submit(carol.id).unwrap();
    assert_eq!(carol_out.entry.status, MergeStatus::Verified);
    broker.promote(carol_out.entry.id).unwrap();

    // ── bob submits: conflict, rejected before any gate ran ──────────
    let bob_out = broker.submit(bob.id).unwrap();
    assert_eq!(bob_out.entry.status, MergeStatus::Conflict);
    assert_eq!(bob_out.conflicts, vec!["src/auth.py".to_string()]);
    assert!(bob_out.gate_outcomes.is_empty());
    let note =
        std::fs::read_to_string(bob_wt.join(aethyme_broker::ACTION_REQUIRED_RELPATH)).unwrap();
    assert!(note.contains("src/auth.py"), "instructions name the file");

    // ── bob resolves (takes the integrated state) and resubmits ──────
    sh(&bob_wt, &["fetch", ".", "aethyme/integration"]);
    sh(&bob_wt, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    commit_edit(&bob_wt, "src/auth.py", "auth = 300\n");
    let bob_retry = broker.submit(bob.id).unwrap();
    assert_eq!(bob_retry.entry.status, MergeStatus::Verified);
    broker.promote(bob_retry.entry.id).unwrap();

    // ── the integration branch contains everyone's final work ────────
    let show = |path: &str| {
        let output = Command::new("git")
            .args(["show", &format!("aethyme/integration:{path}")])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(show("src/auth.py"), "auth = 300\n");
    assert_eq!(show("src/api.py"), "api = 2\n");
    // main was never touched (broker never owns main).
    let output = Command::new("git")
        .args(["show", "main:src/auth.py"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "auth = 1\n");

    // ── cleanup: carol's spawned agent exited; her work is promoted ──
    // (her branch head is reachable from integration? No — promotion
    // merges trees, it does not advance her branch; cleanup of a session
    // whose commits are unmerged into MAIN still requires --force. That
    // is correct v0 behavior: main is human territory.)
    let views = broker.agents(0).unwrap();
    assert_eq!(views.len(), 3);
    let carol_view = views.iter().find(|v| v.session.id == carol.id).unwrap();
    assert_eq!(carol_view.derived_status, SessionStatus::Exited);

    // ── the event timeline tells the whole story, in order ───────────
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    // Registration order.
    let first_registered = kinds.iter().position(|k| *k == "session.registered");
    assert_eq!(first_registered, Some(0));
    // One overlap announcement, exactly one.
    assert_eq!(
        kinds.iter().filter(|k| **k == "lease.overlap").count(),
        1,
        "overlap announced once"
    );
    // Three promotions, one conflict.
    assert_eq!(kinds.iter().filter(|k| **k == "merge.promoted").count(), 3);
    assert_eq!(kinds.iter().filter(|k| **k == "merge.conflict").count(), 1);
    // Strictly increasing ids (replayable cursor).
    assert!(events.windows(2).all(|w| w[0].id < w[1].id));

    // ── accounting: gates ran vs cache hits (the CI-economics claim) ──
    let gate_events = kinds.iter().filter(|k| k.starts_with("gate.")).count();
    let passes = kinds.iter().filter(|k| **k == "gate.pass").count();
    assert_eq!(
        gate_events, passes,
        "every gate event in this scenario is a pass"
    );
    assert_eq!(
        passes, 3,
        "three verifications executed the gate (distinct merged trees); \
         conflicted submission cost zero gate runs"
    );

    // ── status renders the whole picture without error ────────────────
    let status = broker.status(0).unwrap();
    assert_eq!(status.agents.len(), 3);
    // Four entries: alice, carol, bob's superseded conflict, bob's retry.
    assert_eq!(status.queue.len(), 4);
    assert_eq!(
        status
            .queue
            .iter()
            .filter(|e| e.status == MergeStatus::Superseded)
            .count(),
        1
    );
    assert!(
        status
            .queue
            .iter()
            .filter(|e| e.status == MergeStatus::Promoted)
            .count()
            == 3
    );
    assert_eq!(status.integration_branch, "aethyme/integration");
}
