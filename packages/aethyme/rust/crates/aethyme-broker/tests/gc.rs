use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{
    Broker, BrokerOpenOptions, FinishOptions, GateStatus, GcFileAction, GcRowKind, NewGateResult,
};

mod common;

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

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Self { _root: root, repo }
    }

    fn path(&self) -> &Path {
        &self.repo
    }

    fn worktree_root(&self) -> PathBuf {
        self._root.path().join("worktrees")
    }

    fn open(&self) -> Broker {
        common::open_fixture_broker(self.path(), self._root.path())
            .unwrap()
            .into_broker()
    }
}

/// A closed session whose work landed by pull request, worktree kept.
fn fixture() -> (Fixture, Broker, i64, PathBuf) {
    let tmp = Fixture::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    // The kept checkout is landed, and tests age its close past the grace
    // window. With unattended checkout removal on, the bounded inline sweep
    // of the next writable broker open may remove it before `gc plan` can
    // propose it. These tests exercise reviewed GC, so turn that lane off.
    aethyme_testkit::disable_unattended_checkout_removal(tmp.path());
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nterminal_events_days = 1\ngate_results_days = 1\nterminal_merge_queue_days = 1\ncommand_metrics_days = 1\nclosed_worktrees_days = 1\nstartup_budget_ms = 5\n",
    )
    .unwrap();

    let mut broker = tmp.open();
    let delivered = broker.start_worktree("old delivered work", None).unwrap();
    let worktree = PathBuf::from(&delivered.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    let submitted = broker.submit(delivered.id).unwrap();
    assert!(!submitted.promoted);
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &delivered.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert!(
        broker
            .finish_with_options(
                delivered.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    (tmp, broker, delivered.id, worktree)
}

#[test]
fn gc_fixture_worktree_stays_under_owned_root_and_is_removed_with_fixture() {
    const REPORT_ENV: &str = "AETHYME_TEST_GC_WORKTREE_REPORT";

    if let Some(report_path) = std::env::var_os(REPORT_ENV) {
        let poison_root = PathBuf::from(
            std::env::var_os("AETHYME_WORKTREE_ROOT")
                .expect("the child test process must inherit a poisoned root"),
        );
        let (tmp, broker, _session, worktree) = fixture();
        let expected_root = tmp.worktree_root().canonicalize().unwrap();
        let canonical_worktree = worktree.canonicalize().unwrap();
        assert!(canonical_worktree.starts_with(expected_root));
        assert!(
            !poison_root.exists(),
            "the poisoned root must remain unused"
        );

        drop(broker);
        drop(tmp);
        assert!(
            !worktree.exists(),
            "fixture teardown must remove its worktree"
        );
        assert!(
            !poison_root.exists(),
            "fixture teardown must not touch poison"
        );
        std::fs::write(report_path, canonical_worktree.to_string_lossy().as_bytes()).unwrap();
        return;
    }

    let parent = tempfile::tempdir().unwrap();
    let poison_root = parent.path().join("poisoned-worktrees");
    let report_path = parent.path().join("result.txt");
    let host_state = parent.path().join("private-host-state");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gc_fixture_worktree_stays_under_owned_root_and_is_removed_with_fixture",
            "--nocapture",
        ])
        .env("AETHYME_WORKTREE_ROOT", &poison_root)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .env(REPORT_ENV, &report_path)
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child test failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let removed_path = PathBuf::from(std::fs::read_to_string(report_path).unwrap());
    assert!(!removed_path.exists());
    assert!(!poison_root.exists());
}

#[test]
fn plan_is_exact_deterministic_and_protects_live_or_unresolved_state() {
    let (tmp, mut broker, delivered_id, worktree) = fixture();
    let active = broker.start_worktree("live blocker", None).unwrap();
    let main_root = broker.main_root().to_path_buf();
    let gate_dir = main_root.join(".aethyme/logs/gates");
    std::fs::create_dir_all(&gate_dir).unwrap();
    let gate_log = gate_dir.join("old.log");
    std::fs::write(&gate_log, "old gate output\n").unwrap();
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "old-gate".into(),
            tree_hash: "tree".into(),
            definition_hash: "definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(1),
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: Some(16),
            log_path: Some(gate_log.to_string_lossy().into_owned()),
            session_id: Some(delivered_id),
        })
        .unwrap();
    broker
        .store()
        .append_event("test.old", Some(delivered_id), None)
        .unwrap();
    drop(broker);

    let old = 1_i64;
    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    db.execute("UPDATE events SET ts = ?1", [old]).unwrap();
    db.execute("UPDATE gate_results SET created_at = ?1", [old])
        .unwrap();
    db.execute(
        "UPDATE merge_queue SET created_at = ?1, updated_at = ?1",
        [old],
    )
    .unwrap();
    db.execute(
        "UPDATE sessions SET closed_at = ?1, updated_at = ?1 WHERE id = ?2",
        rusqlite::params![old, delivered_id],
    )
    .unwrap();
    db.execute(
        "UPDATE entry_path_exposures
         SET state = 'resolved', resolved_at = ?1,
             resolution_kind = 'ship_verified', resolution_sha = 'remote',
             resolution_evidence = 'fixture'
         WHERE queue_entry_id IN (SELECT id FROM merge_queue WHERE session_id = ?2)",
        rusqlite::params![old, delivered_id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO merge_queue (
             session_id, head_commit, base_commit, status, created_at, updated_at
         ) VALUES (?1, 'superseded-head', 'superseded-base', 'superseded', 1, 1)",
        [delivered_id],
    )
    .unwrap();
    drop(db);

    let metrics = main_root.join(".aethyme/logs/command-metrics.jsonl");
    std::fs::create_dir_all(metrics.parent().unwrap()).unwrap();
    std::fs::write(
        &metrics,
        "{\"ts\":1,\"command\":\"old\"}\n{\"ts\":9999999999999,\"command\":\"new\"}\nmalformed\n",
    )
    .unwrap();
    std::fs::write(
        main_root.join(".aethyme/broker.toml"),
        "[retention]\nstartup_budget_ms = 5000\n",
    )
    .unwrap();

    let mut broker = tmp.open();
    let first = broker.gc_plan().unwrap();
    let second = broker.gc_plan().unwrap();
    assert_eq!(first.digest, second.digest);
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.kind == GcRowKind::GateResult)
    );
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.kind == GcRowKind::MergeQueue)
    );
    assert!(first.files.iter().any(|file| {
        file.path == ".aethyme/logs/gates/old.log" && file.action == GcFileAction::Delete
    }));
    assert!(first.files.iter().any(|file| {
        file.path == ".aethyme/logs/command-metrics.jsonl"
            && file.action == GcFileAction::Rewrite
            && file.bytes_after < file.bytes_before
    }));
    assert!(
        first
            .worktrees
            .iter()
            .any(|candidate| candidate.session_id == delivered_id)
    );
    assert!(
        first
            .blockers
            .iter()
            .any(|blocker| { blocker.kind == "live_session" && blocker.id == Some(active.id) })
    );
    assert!(
        first
            .blockers
            .iter()
            .any(|blocker| { blocker.kind == "command_metric_line" && blocker.id.is_none() })
    );
    assert!(first.files.iter().all(|file| !file.path.starts_with('/')));
    assert!(worktree.exists(), "planning must not remove the worktree");
    assert!(gate_log.exists(), "planning must not remove gate logs");
    assert!(metrics.exists(), "planning must not rewrite metrics");
}

#[cfg(unix)]
#[test]
fn locked_runtime_file_is_retained_and_resumable() {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, mut broker, _delivered_id, _worktree) = fixture();
    let metrics = tmp.path().join(".aethyme/logs/command-metrics.jsonl");
    std::fs::create_dir_all(metrics.parent().unwrap()).unwrap();
    std::fs::write(
        &metrics,
        "{\"ts\":1,\"command\":\"old\"}\n{\"ts\":9999999999999,\"command\":\"new\"}\n",
    )
    .unwrap();
    let plan = broker.gc_plan().unwrap();
    assert!(
        plan.files
            .iter()
            .any(|file| file.path.ends_with("command-metrics.jsonl"))
    );

    let directory = metrics.parent().unwrap();
    let original_mode = std::fs::metadata(directory).unwrap().permissions().mode();
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = broker.gc_apply(&plan.digest);
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(original_mode)).unwrap();

    let blocked = result.unwrap();
    assert!(!blocked.complete);
    assert!(!blocked.failures.is_empty());
    assert!(metrics.exists());
    assert!(tmp.path().join(".aethyme/gc-journal.json").exists());

    let resumed = broker.gc_apply(&plan.digest).unwrap();
    assert!(resumed.complete);
    assert!(
        !std::fs::read_to_string(metrics)
            .unwrap()
            .contains("\"old\"")
    );
}

#[test]
fn ephemeral_repositories_never_anchor_worktrees_in_durable_host_state() {
    const CHILD_ENV: &str = "AETHYME_TEST_DEFAULT_EPHEMERAL_PLACEMENT";
    const HOME_MARKER: &str = ".aethyme-test-ephemeral-placement-home";
    if std::env::var_os(CHILD_ENV).is_none() {
        let private_home = tempfile::tempdir().unwrap();
        std::fs::write(
            private_home.path().join(HOME_MARKER),
            "owned by placement test\n",
        )
        .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "ephemeral_repositories_never_anchor_worktrees_in_durable_host_state",
                "--nocapture",
            ])
            .env(CHILD_ENV, private_home.path())
            .env("HOME", private_home.path())
            .env_remove("AETHYME_WORKTREE_ROOT")
            .env_remove("AETHYME_HOST_STATE_DIR")
            .env_remove("AETHYME_HOST_CACHE_DIR")
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_GATE_BROKER_DATABASES")
            .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
            .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
            .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
            .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
            .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME");
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "default-placement child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // An inherited sentinel must never bypass environment isolation. Validate
    // the parent's owned home and all placement overrides before opening state.
    for name in [
        "AETHYME_WORKTREE_ROOT",
        "AETHYME_HOST_STATE_DIR",
        "AETHYME_HOST_CACHE_DIR",
        "AETHYME_BROKER_DB",
        "AETHYME_GATE_BROKER_DATABASES",
        "AETHYME_BROKER_STORAGE_CONTEXT_V1",
        "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
        "AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT",
        "AETHYME_BROKER_OPEN_WORKTREE_ROOT",
        "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "unsanitized child environment: {name}"
        );
    }
    let private_home = PathBuf::from(std::env::var_os(CHILD_ENV).unwrap())
        .canonicalize()
        .unwrap();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(
        home, private_home,
        "placement child must use its parent's owned HOME"
    );
    assert!(home.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert_eq!(
        std::fs::read_to_string(home.join(HOME_MARKER)).unwrap(),
        "owned by placement test\n"
    );
    // This tests the production default. The shared fixture intentionally
    // supplies a library override and therefore cannot exercise that default.
    let tmp = Fixture::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "README.md"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let options = BrokerOpenOptions::default()
        .with_host_operation_database(tmp._root.path().join("host-operations.db"));
    let mut broker = Broker::open_with_options(tmp.path(), options).unwrap();
    let session = broker
        .start_worktree("default ephemeral placement", None)
        .unwrap();
    let repo = tmp.path().canonicalize().unwrap();
    let worktree = PathBuf::from(session.worktree_path).canonicalize().unwrap();
    assert!(
        worktree.starts_with(&repo),
        "a temp-dir repository must keep its worktrees repo-local so they die with it, got {}",
        worktree.display()
    );
    drop(broker);
    drop(tmp);
    assert!(
        !worktree.exists(),
        "default worktree must die with its fixture"
    );
}

#[test]
fn a_resumed_candidate_that_stopped_qualifying_is_retained_without_stranding_the_rest() {
    let (tmp, broker, _delivered_id, worktree) = fixture();
    drop(broker);
    // Reclaim build caches immediately and leave the autonomous sweep off, so
    // the authorized plan is the only thing that touches them.
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n",
    )
    .unwrap();
    // The shared fixture is represented and therefore intentionally bypasses
    // the age gate now. Keep this scenario age-held by making the retained
    // checkout dirty; the test is about resuming independently reclaimable
    // caches under a worktree that whole-worktree cleanup cannot remove.
    std::fs::write(worktree.join("pending.txt"), "pending\n").unwrap();
    std::fs::write(tmp.path().join(".git/info/exclude"), "target/\n").unwrap();
    let caches = ["one/target", "two/target"].map(|relative| {
        let dir = worktree.join(relative);
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        std::fs::write(dir.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(dir.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();
        dir
    });

    let mut broker = tmp.open();
    let plan = broker.gc_plan().unwrap();
    assert_eq!(
        plan.artifacts.len(),
        2,
        "expected two build caches: {:?}",
        plan.artifacts
    );
    let first = worktree.join(&plan.artifacts[0].relative_dir);
    let second = worktree.join(&plan.artifacts[1].relative_dir);

    // Pause immediately: what follows can only be reached by resuming a
    // journal, because a fresh plan simply would not name a candidate that no
    // longer qualifies.
    assert!(
        !broker
            .gc_apply_bounded(&plan.digest, Some(0))
            .unwrap()
            .complete
    );
    assert!(tmp.path().join(".aethyme/gc-journal.json").exists());

    // Take the witness off the candidate the resume reaches first. This is the
    // state an interrupted removal used to leave behind, and the state a stray
    // `.DS_Store` produced once the final `rmdir` failed partway through.
    std::fs::remove_file(first.join("CACHEDIR.TAG")).unwrap();

    let resumed = broker.gc_apply(&plan.digest).unwrap();
    assert!(
        resumed.complete,
        "one disqualified candidate must not leave the run unfinished: {resumed:?}"
    );
    assert!(
        resumed
            .failures
            .iter()
            .any(|failure| failure.contains(&plan.artifacts[0].relative_dir)),
        "the candidate left in place must be reported: {:?}",
        resumed.failures
    );
    assert!(
        first.exists(),
        "a directory that no longer proves it is a build cache must be left alone"
    );
    assert!(
        !second.exists(),
        "the other candidate must still be reclaimed"
    );
    assert!(caches.iter().any(|cache| cache.exists()));

    // The journal is gone, so planning works again. While it survived, a
    // disqualified candidate pinned every later plan and no command released it.
    assert!(!tmp.path().join(".aethyme/gc-journal.json").exists());
    let replanned = broker.gc_plan().unwrap();
    assert_ne!(replanned.digest, plan.digest);
    assert!(
        broker.gc_apply(&replanned.digest).is_ok(),
        "a fresh plan must be runnable"
    );
}

#[test]
fn empty_build_output_still_reports_inode_usage() {
    let (tmp, mut broker, _delivered_id, worktree) = fixture();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join(".git/info/exclude"), "/node_modules/\n").unwrap();
    std::fs::create_dir_all(worktree.join("node_modules/empty-package")).unwrap();

    let plan = broker.gc_plan().unwrap();
    let candidate = plan
        .artifacts
        .iter()
        .find(|artifact| artifact.relative_dir == "node_modules")
        .expect("the ignored, non-empty node_modules tree is a build-output candidate");
    assert_eq!(candidate.estimated_bytes, 0);
    assert_eq!(candidate.estimated_inodes, Some(2));
    assert!(
        plan.estimated_build_output_reclaimable_inodes.unwrap() >= 2,
        "the build-output total must include directory-only caches: {plan:#?}"
    );
}
