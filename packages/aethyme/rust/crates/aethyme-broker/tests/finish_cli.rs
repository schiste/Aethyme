#[path = "common/isolation.rs"]
mod isolation;

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{GateStatus, NewGateResult, events};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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

fn run(repo: &Path, args: &[&str]) -> Output {
    isolation::command(CLI, repo, args).output().unwrap()
}

fn promoted_fixture() -> (isolation::FixtureRepo, i64, std::path::PathBuf, String) {
    let tmp = isolation::FixtureRepo::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = isolation::open(tmp.path());
    let session = broker.start_worktree("finish CLI fixture", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker
        .store()
        .claim_lease(session.id, "src/", None)
        .unwrap();
    assert!(broker.submit(session.id).unwrap().promoted);
    let tree = git(&worktree, &["rev-parse", "HEAD^{tree}"]);
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "finish-cli-gate".into(),
            tree_hash: tree,
            definition_hash: "test-definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(4),
            wait_duration_ms: Some(1),
            first_output_ms: Some(2),
            output_bytes: Some(3),
            log_path: Some("/redacted/gate.log".into()),
            session_id: Some(session.id),
        })
        .unwrap();
    let session_id = session.id;
    let branch = session.branch;
    (tmp, session_id, worktree, branch)
}

#[test]
fn finish_cli_json_is_structured_and_persists_a_redacted_handoff() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session, "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "cleaned");
    assert_eq!(report["delivery"]["submitted"], true);
    assert_eq!(report["delivery"]["promoted"], true);
    assert_eq!(report["delivery"]["published"], false);
    assert_eq!(report["pending_work"]["present"], false);
    let src_lease = report["leases_held"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lease| lease["path"] == "src/")
        .unwrap();
    // A closed session no longer holds anything: the snapshot is kept as
    // handoff history, but it must not claim active, unreleased leases (#141).
    assert_eq!(src_lease["state"], "released");
    assert!(
        src_lease["released_at"].is_i64(),
        "a released lease must carry its release time: {src_lease}"
    );
    assert_eq!(report["last_gate"]["gate"], "finish-cli-gate");
    assert_eq!(report["last_gate"]["cache_source"], "executed");
    assert!(report["last_gate"]["recorded_at"].is_i64());
    assert!(report["last_gate"]["tree_hash"].is_string());
    assert_eq!(report["cleanup_safe"], true);
    assert_eq!(report["cleanup"]["requested"], true);
    assert_eq!(report["cleanup"]["attempted"], true);
    assert_eq!(report["cleanup"]["completed"], true);
    assert_eq!(report["cleanup"]["worktree_removed"], true);
    assert_eq!(report["cleanup"]["branch_removed"], true);
    assert!(!worktree.exists());
    assert!(
        !Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        report["recommended_next_action"]
            .as_str()
            .unwrap()
            .starts_with("aethyme broker advanced ship plan --entry ")
    );

    let mut broker = isolation::open(tmp.path());
    let event = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    let payload = event.payload_json.unwrap();
    assert!(!payload.contains(tmp.path().to_str().unwrap()));
    assert!(!payload.contains("redacted/gate.log"));
}

#[cfg(unix)]
#[test]
fn finish_preserves_discovery_errors_before_closing_the_session() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = isolation::FixtureRepo::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let worktree_root = tmp
        .path()
        .parent()
        .expect("fixture repo has an owning root")
        .join("worktrees with spaces");
    let mut broker = isolation::open(tmp.path()).with_worktree_root(&worktree_root);
    let session = broker.start_worktree("slow finish check", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    assert!(worktree.starts_with(&worktree_root));
    assert!(worktree.to_string_lossy().contains(' '));
    let session_id = session.id;

    let original_path = std::env::var_os("PATH").unwrap();
    let real_git = std::env::split_paths(&original_path)
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("the test host has git on PATH");
    let bin = tmp
        .path()
        .parent()
        .expect("fixture repo has an owning root")
        .join("slow-git-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$PWD\" = \"$AETHYME_TEST_SLOW_WORKTREE\" ] && [ \"$1\" = rev-parse ] && [ \"$2\" = --show-toplevel ]; then exec /bin/sleep 5; fi\nexec \"$AETHYME_TEST_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&original_path));
    let path = std::env::join_paths(paths).unwrap();
    let session_arg = session_id.to_string();
    let mut command = Command::new(CLI);
    command
        .args(["finish", "--session", &session_arg, "--timeout", "1"])
        .current_dir(tmp.path())
        .env("PATH", path)
        .env("AETHYME_TEST_REAL_GIT", real_git)
        .env("AETHYME_TEST_SLOW_WORKTREE", &worktree)
        .env(
            "AETHYME_CHAU7_MCP_BRIDGE",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/no-chau7-bridge"),
        );
    for variable in [
        "AETHYME_BROKER_DB",
        "AETHYME_GATE_BROKER_DATABASES",
        "AETHYME_HOST_STATE_DIR",
        "AETHYME_HOST_CACHE_DIR",
        "AETHYME_WORKTREE_ROOT",
        "AETHYME_BROKER_STORAGE_CONTEXT_V1",
        "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
        "AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT",
        "AETHYME_BROKER_OPEN_WORKTREE_ROOT",
        "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
    ] {
        command.env_remove(variable);
    }
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    drop(broker);
    let output = command.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("git rev-parse --show-toplevel did not finish within 1s"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("is not inside a git repository"),
        "a discovery timeout must not be reported as a missing repository: {stderr}"
    );
    // The timeout leaves the session active and points to a bounded retry.
    assert!(
        stderr.contains(&format!(
            "session {session_id} is still active and nothing was closed"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "aethyme broker finish --session {session_id} --timeout 2"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("broker status --json"), "{stderr}");

    // An unrelated Git failure from the same discovery command is not proof
    // that the checkout is absent either. Keep the child process and failure
    // finite so this checks diagnostic classification without adding a second
    // timeout path.
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$PWD\" = \"$AETHYME_TEST_SLOW_WORKTREE\" ] && [ \"$1\" = rev-parse ] && [ \"$2\" = --show-toplevel ]; then printf 'fatal: repository metadata is unreadable\\n' >&2; exit 128; fi\nexec \"$AETHYME_TEST_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let output = command.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: repository metadata is unreadable"),
        "the unrelated Git failure should be preserved: {stderr}"
    );
    assert!(
        !stderr.contains("is not inside a git repository"),
        "an unrelated Git failure must not be reported as a missing repository: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "session {session_id} is still active and nothing was closed"
        )),
        "{stderr}"
    );

    let store = aethyme_broker::BrokerStore::open_in_repo(tmp.path()).unwrap();
    assert_eq!(store.session(session_id).unwrap().status.as_str(), "active");
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn finish_timeout_does_not_kill_worktree_removal_after_the_session_closes() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = isolation::FixtureRepo::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let mut broker = isolation::open(tmp.path());
    let session = broker
        .start_worktree("slow worktree removal", None)
        .unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    let session_id = session.id;
    drop(broker);

    let original_path = std::env::var_os("PATH").unwrap();
    let real_git = std::env::split_paths(&original_path)
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("the test host has git on PATH");
    let bin = tmp.path().join("slow-remove-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("git");
    // Removal outlasts the 2-second finish budget; every check stays fast.
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = remove ]; then /bin/sleep 4; fi\nexec \"$AETHYME_TEST_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&original_path));
    let path = std::env::join_paths(paths).unwrap();
    let session_arg = session_id.to_string();
    let output = isolation::command(CLI, tmp.path(), &[])
        .args([
            "finish",
            "--session",
            &session_arg,
            "--timeout",
            "2",
            "--json",
        ])
        .current_dir(tmp.path())
        .env("PATH", path)
        .env("AETHYME_TEST_REAL_GIT", real_git)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["cleanup"]["completed"], true, "{stdout}");
    assert_eq!(report["cleanup"]["worktree_removed"], true, "{stdout}");
    assert!(!worktree.exists());
}

#[test]
fn finish_cli_text_summarizes_the_structured_handoff() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("delivery: submitted=yes, promoted=yes, published=no"),
        "{text}"
    );
    assert!(
        text.contains("pending work: no (0 dirty paths, 0 unsubmitted commits)"),
        "{text}"
    );
    assert!(text.contains("leases at close:"), "{text}");
    assert!(
        text.contains("explicit released src/"),
        "a closed session must not list active leases: {text}"
    );
    assert!(
        !text.contains("released never"),
        "a released lease must carry its release time: {text}"
    );
    assert!(
        text.contains("last gate: finish-cli-gate pass on tree"),
        "{text}"
    );
    assert!(text.contains("(executed)"), "{text}");
    assert!(text.contains("cleanup safe: yes"), "{text}");
    assert!(text.contains("physical cleanup: requested=true"), "{text}");
    assert!(text.contains("attempted=true, completed=true"), "{text}");
    assert!(text.contains("(removed)"), "{text}");
    assert!(!worktree.exists());
    assert!(
        text.contains("recommended next: aethyme broker advanced ship plan --entry"),
        "{text}"
    );
}

#[test]
fn dirty_finish_guidance_is_stash_free() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    std::fs::write(worktree.join("dirty.txt"), "keep me\n").unwrap();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session, "--json"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rendered = report.to_string();
    assert!(rendered.contains("managed pre-commit lane"), "{rendered}");
    assert!(!rendered.contains("stash"), "{rendered}");
}

#[test]
fn finish_cli_keep_worktree_closes_state_without_reclaiming_artifacts() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    let output = run(
        tmp.path(),
        &[
            "finish",
            "--session",
            &session_id.to_string(),
            "--keep-worktree",
            "--json",
        ],
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "closed");
    assert_eq!(report["cleanup"]["kept"], true);
    assert_eq!(report["cleanup"]["attempted"], false);
    assert!(worktree.exists());
    assert!(
        Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn finish_cli_reports_partial_cleanup_and_resumes_idempotently() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    git(
        tmp.path(),
        &["worktree", "lock", worktree.to_str().unwrap()],
    );

    let first = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(first.status.success());
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["status"], "closed");
    assert_eq!(first["cleanup"]["attempted"], true);
    assert_eq!(first["cleanup"]["completed"], false);
    assert!(first["cleanup"]["failure"].is_string());
    assert_eq!(
        first["cleanup"]["recovery_action"],
        format!("aethyme broker finish cleanup {session_id}")
    );
    assert!(worktree.exists());

    git(
        tmp.path(),
        &["worktree", "unlock", worktree.to_str().unwrap()],
    );
    let resumed = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(resumed.status.success());
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["status"], "cleaned");
    assert_eq!(resumed["cleanup"]["completed"], true);
    assert!(!worktree.exists());
    assert!(
        !Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn finish_cli_can_remove_its_current_worktree_after_the_command_starts() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    let output = isolation::command(
        CLI,
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    )
    .current_dir(&worktree)
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "cleaned");
    assert_eq!(report["cleanup"]["worktree_removed"], true);
    assert!(!worktree.exists());
}

#[test]
fn finish_cli_recovers_a_branch_only_interruption_after_branch_deletion_fails() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    git(
        tmp.path(),
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    let branch_lock = tmp
        .path()
        .join(".git/refs/heads")
        .join(format!("{branch}.lock"));
    std::fs::create_dir_all(branch_lock.parent().unwrap()).unwrap();
    std::fs::write(&branch_lock, "held by test\n").unwrap();

    let first = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(first.status.success());
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["status"], "closed");
    assert_eq!(first["pending_work"]["worktree_missing"], true);
    assert_eq!(first["cleanup"]["attempted"], true);
    assert_eq!(first["cleanup"]["completed"], false);
    assert!(first["cleanup"]["failure"].is_string());
    assert!(branch_lock.exists());

    std::fs::remove_file(&branch_lock).unwrap();
    let resumed = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(resumed.status.success());
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["status"], "cleaned");
    assert_eq!(resumed["cleanup"]["completed"], true);
    assert!(
        !Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}
