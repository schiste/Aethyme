#[path = "common/isolation.rs"]
mod isolation;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{FinishOptions, GateStatus, MergeStatus, NewGateResult, events};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    let git_keys = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect::<Vec<_>>();
    for key in git_keys {
        command.env_remove(key);
    }
    command.args(args).current_dir(repo);
    command
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    let reference = format!("refs/heads/{branch}");
    git_command(repo, &["show-ref", "--verify", "--quiet", &reference])
        .status()
        .unwrap()
        .success()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = git_command(repo, args)
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

#[cfg(unix)]
fn run_with_transient_git_removal_failure(
    repo: &Path,
    worktree: &Path,
    args: &[&str],
    artifacts: &Path,
) -> (Output, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(artifacts).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("the test runner PATH contains Git");
    let shim_dir = artifacts.join("git-shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        r##"#!/bin/sh
case "$*" in
  *"worktree remove"*)
    : > "$AETHYME_TEST_GIT_REMOVE_MARKER"
    echo 'injected transient worktree removal failure' >&2
    exit 73
    ;;
esac
if [ -e "$AETHYME_TEST_GIT_REMOVE_MARKER" ]; then
  case "$*" in
    *"rev-parse --show-toplevel"*)
      if [ "$(pwd -P)" = "$AETHYME_TEST_GIT_WORKTREE_ROOT" ]; then
        : > "$AETHYME_TEST_GIT_DISCOVERY_MARKER"
        echo 'injected transient worktree discovery failure' >&2
        exit 74
      fi
      ;;
  esac
fi
exec "$AETHYME_TEST_REAL_GIT" "$@"
"##,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();

    let prior_path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(shim_dir).chain(std::env::split_paths(&prior_path)))
            .unwrap();
    let removal_marker = artifacts.join("git-removal-attempted");
    let discovery_marker = artifacts.join("git-discovery-attempted");
    let mut command = isolation::command(CLI, repo, args);
    let output = command
        .env("PATH", path)
        .env("AETHYME_TEST_REAL_GIT", real_git)
        .env("AETHYME_TEST_GIT_REMOVE_MARKER", &removal_marker)
        .env("AETHYME_TEST_GIT_DISCOVERY_MARKER", &discovery_marker)
        .env(
            "AETHYME_TEST_GIT_WORKTREE_ROOT",
            worktree.canonicalize().expect("fixture worktree exists"),
        )
        .output()
        .unwrap();
    (output, removal_marker, discovery_marker)
}

fn promoted_fixture() -> (isolation::FixtureRepo, i64, std::path::PathBuf, String) {
    promoted_fixture_with_worktree_root(None)
}

fn promoted_fixture_with_worktree_root(
    worktree_root: Option<&Path>,
) -> (isolation::FixtureRepo, i64, std::path::PathBuf, String) {
    promoted_fixture_with_worktree_root_and_gitignore(worktree_root, "/.aethyme/\n")
}

fn promoted_fixture_with_gitignore(
    gitignore: &str,
) -> (isolation::FixtureRepo, i64, std::path::PathBuf, String) {
    promoted_fixture_with_worktree_root_and_gitignore(None, gitignore)
}

fn promoted_fixture_with_worktree_root_and_gitignore(
    worktree_root: Option<&Path>,
    gitignore: &str,
) -> (isolation::FixtureRepo, i64, std::path::PathBuf, String) {
    let tmp = isolation::FixtureRepo::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), gitignore).unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = match worktree_root {
        Some(worktree_root) => isolation::open_with_worktree_root(tmp.path(), worktree_root),
        None => isolation::open(tmp.path()),
    };
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

fn interrupt_finish_cleanup(tmp: &isolation::FixtureRepo, session_id: i64, worktree: &Path) {
    git(
        tmp.path(),
        &["worktree", "lock", worktree.to_str().unwrap()],
    );
    let first = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["status"], "closed");
    assert_eq!(first["cleanup"]["completed"], false);
    git(
        tmp.path(),
        &["worktree", "unlock", worktree.to_str().unwrap()],
    );
}

#[test]
fn finish_does_not_decode_unrelated_merge_queue_history() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    let mut broker = isolation::open(tmp.path());
    let expected_entry = broker
        .store()
        .latest_merge_queue_for_session(session_id)
        .unwrap()
        .unwrap();
    let unrelated = broker
        .start_worktree("unrelated merge history", None)
        .unwrap();

    let database = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    database
        .execute(
            "INSERT INTO merge_queue
                (session_id, head_commit, base_commit, status, created_at, updated_at)
             VALUES (?1, 'malformed-unrelated-history', 'base', 'submitted',
                     'not-an-integer', 'not-an-integer')",
            [unrelated.id],
        )
        .unwrap();
    drop(database);

    assert!(
        broker.store().merge_queue().is_err(),
        "the fixture must fail if finish materializes the full queue history"
    );

    let report = broker
        .finish_with_options(
            session_id,
            FinishOptions {
                keep_worktree: true,
            },
        )
        .unwrap();
    assert_eq!(report.status, aethyme_broker::FinishStatus::Closed);
    assert!(report.closed, "{report:#?}");
    assert_eq!(report.latest_queue_entry_id, Some(expected_entry.id));
    assert_eq!(report.latest_queue_status, Some(MergeStatus::Promoted));
    assert!(report.delivery.promoted, "{report:#?}");
    assert!(report.cleanup.kept, "{report:#?}");
    assert!(
        worktree.exists(),
        "keep-worktree should preserve the fixture"
    );
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
    assert!(!branch_exists(tmp.path(), &branch));
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

#[test]
fn finish_cli_succeeds_for_a_clean_worktree_root_with_spaces() {
    let worktree_parent = tempfile::Builder::new()
        .prefix("aethyme finish worktree parent ")
        .tempdir()
        .unwrap();
    let worktree_root_path = worktree_parent
        .path()
        .canonicalize()
        .unwrap()
        .join("worktrees with spaces");
    assert!(
        !worktree_root_path.exists(),
        "the selected worktree root must be absent before broker open: {}",
        worktree_root_path.display()
    );
    let (tmp, session_id, worktree, _) =
        promoted_fixture_with_worktree_root(Some(&worktree_root_path));
    assert!(
        worktree.to_string_lossy().contains(' '),
        "fixture path must contain spaces: {}",
        worktree.display()
    );
    assert!(worktree.starts_with(&worktree_root_path));
    assert!(worktree_root_path.is_dir());

    let broker = isolation::open_with_worktree_root(tmp.path(), &worktree_root_path);
    let session_arg = session_id.to_string();
    let mut command = Command::new(CLI);
    command
        .args(["finish", "--session", &session_arg, "--json"])
        .current_dir(tmp.path())
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["status"], "cleaned", "{stdout}");
    assert_eq!(report["cleanup"]["completed"], true, "{stdout}");
    assert_eq!(report["cleanup"]["worktree_removed"], true, "{stdout}");
    assert!(!worktree.exists());
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
        .canonicalize()
        .unwrap()
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

fn finish_guidance_text(report: &serde_json::Value) -> String {
    let rendered = format!(
        "{}\n{}\n{}\n{}\n{}",
        report["error"]["message"],
        report["summary"],
        report["recommended_next_action"],
        report["warnings"],
        report["next_commands"],
    );
    match report["worktree_path"]
        .as_str()
        .filter(|path| !path.is_empty())
    {
        Some(worktree_path) => rendered.replace(worktree_path, "<worktree>"),
        None => rendered,
    }
}

#[test]
fn dirty_finish_guidance_is_stash_free() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    std::fs::write(worktree.join("dirty.txt"), "keep me\n").unwrap();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session, "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["code"], 3);
    assert_eq!(report["error"]["message"], "session is not ready to finish");
    let guidance = finish_guidance_text(&report);
    assert!(guidance.contains("managed pre-commit lane"), "{guidance}");
    assert!(!guidance.contains("stash"), "{guidance}");

    let path_with_token = "/tmp/dirty_finish_guidance_is_stash_free/worktrees/finish-cli-fixture";
    let path_only_report = serde_json::json!({
        "worktree_path": path_with_token,
        "error": {"message": "session is not ready to finish"},
        "summary": "session is not finished yet",
        "recommended_next_action": "inspect the worktree",
        "warnings": [],
        "next_commands": [format!("git -C {path_with_token} status --short")]
    });
    let path_control_guidance = finish_guidance_text(&path_only_report);
    assert!(
        path_control_guidance.contains("<worktree>"),
        "{path_control_guidance}"
    );
    assert!(
        path_control_guidance.contains("git -C <worktree> status --short"),
        "{path_control_guidance}"
    );
    assert!(
        !path_control_guidance.contains("stash"),
        "{path_control_guidance}"
    );

    let stash_instruction_report = serde_json::json!({
        "worktree_path": path_with_token,
        "error": {"message": "session is not ready to finish"},
        "summary": "session is not finished yet",
        "recommended_next_action": "inspect the worktree",
        "warnings": [],
        "next_commands": [format!("git -C {path_with_token} stash")]
    });
    let stash_instruction = finish_guidance_text(&stash_instruction_report);
    assert!(
        stash_instruction.contains("git -C <worktree> stash"),
        "{stash_instruction}"
    );
    assert!(stash_instruction.contains("stash"), "{stash_instruction}");
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
    assert!(branch_exists(tmp.path(), &branch));
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
    // A real interrupted `git worktree remove` can leave only deleted tracked
    // files. The durable finish intent binds this retry to the same checkout.
    std::fs::remove_file(worktree.join("README.md")).unwrap();
    let resumed = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(resumed.status.success());
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["cleaned"], session_id);
    assert!(!worktree.exists());
    assert!(!branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_refuses_partial_deletions_without_a_proof_bearing_intent() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    {
        let mut broker = isolation::open(tmp.path());
        broker
            .store()
            .begin_finish_cleanup(
                session_id,
                r#"{"worktree_present":true,"branch_present":true}"#,
            )
            .unwrap();
    }
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_untracked_files() {
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
    git(
        tmp.path(),
        &["worktree", "unlock", worktree.to_str().unwrap()],
    );
    std::fs::write(worktree.join("untracked.txt"), "keep me\n").unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert_eq!(
        std::fs::read_to_string(worktree.join("untracked.txt")).unwrap(),
        "keep me\n"
    );
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_replacement_checkout_at_the_same_path() {
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
    git(
        tmp.path(),
        &["worktree", "unlock", worktree.to_str().unwrap()],
    );

    git(
        tmp.path(),
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    git(
        tmp.path(),
        &["worktree", "add", "--", worktree.to_str().unwrap(), &branch],
    );
    let replacement_sentinel = worktree.join("replacement-owned.txt");
    std::fs::write(&replacement_sentinel, "preserve replacement checkout\n").unwrap();
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert_eq!(
        std::fs::read_to_string(&replacement_sentinel).unwrap(),
        "preserve replacement checkout\n"
    );
    assert!(!worktree.join("README.md").exists());
    assert!(
        git(tmp.path(), &["worktree", "list", "--porcelain"]).contains(worktree.to_str().unwrap()),
        "Git must still list the replacement checkout after recovery refuses it"
    );
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_resumes_with_managed_runtime_and_witnessed_build_artifacts() {
    let mut gitignore = aethyme_broker::broker_runtime_ignore_patterns()
        .collect::<Vec<_>>()
        .join("\n");
    gitignore.push_str("\ntarget/\n");
    let (tmp, session_id, worktree, branch) = promoted_fixture_with_gitignore(&gitignore);
    let managed_status = worktree.join(".aethyme/generated/experience-status.md");
    std::fs::create_dir_all(managed_status.parent().unwrap()).unwrap();
    std::fs::write(&managed_status, "generated status\n").unwrap();
    let broker_log = worktree.join(".aethyme/logs/broker.log");
    std::fs::create_dir_all(broker_log.parent().unwrap()).unwrap();
    std::fs::write(&broker_log, "broker log\n").unwrap();
    let target = worktree.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();

    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let resumed = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(
        resumed.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&resumed.stdout),
        String::from_utf8_lossy(&resumed.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(report["cleaned"], session_id);
    assert!(!worktree.exists());
    assert!(!managed_status.exists());
    assert!(!broker_log.exists());
    assert!(!target.exists());
    assert!(!branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_same_named_ignored_runtime_file() {
    // The slash-free ignore rule makes a regular file at this name ignored.
    // The runtime catalog permits only the directory form `.aethyme/logs/`.
    let (tmp, session_id, worktree, branch) = promoted_fixture_with_gitignore("/.aethyme/logs\n");
    let log_file = worktree.join(".aethyme/logs");
    std::fs::create_dir_all(log_file.parent().unwrap()).unwrap();
    std::fs::write(&log_file, "preserve this file\n").unwrap();
    assert!(git(&worktree, &["check-ignore", "--quiet", ".aethyme/logs"]).is_empty());
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    assert_eq!(
        std::fs::read_to_string(&log_file).unwrap(),
        "preserve this file\n"
    );
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_arbitrary_ignored_files() {
    let (tmp, session_id, worktree, branch) = promoted_fixture_with_gitignore("/.env\n");
    let secret = worktree.join(".env");
    std::fs::write(&secret, "KEEP=me\n").unwrap();
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    assert_eq!(std::fs::read_to_string(secret).unwrap(), "KEEP=me\n");
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_staged_deletion() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::remove_file(worktree.join("README.md")).unwrap();
    git(&worktree, &["add", "-u", "README.md"]);

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    assert_eq!(
        git(&worktree, &["diff", "--cached", "--name-status"]),
        "D\tREADME.md"
    );
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_mixed_modification() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::remove_file(worktree.join("README.md")).unwrap();
    std::fs::write(worktree.join("done.txt"), "local edit\n").unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    assert_eq!(
        std::fs::read_to_string(worktree.join("done.txt")).unwrap(),
        "local edit\n"
    );
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_moved_unrepresented_head() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    std::fs::write(worktree.join("late.txt"), "committed after close\n").unwrap();
    git(&worktree, &["add", "late.txt"]);
    git(&worktree, &["commit", "-qm", "unrepresented after close"]);
    std::fs::remove_file(worktree.join("README.md")).unwrap();
    let moved_head = git(&worktree, &["rev-parse", "HEAD"]);

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), moved_head);
    assert!(worktree.join("late.txt").exists());
    assert!(!worktree.join("README.md").exists());
    assert!(branch_exists(tmp.path(), &branch));
}

#[test]
fn finish_cleanup_proof_does_not_authorize_a_live_replacement_session() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    interrupt_finish_cleanup(&tmp, session_id, &worktree);
    let live = {
        let mut broker = isolation::open(tmp.path());
        broker
            .adopt_with(
                &worktree,
                Some("new owner of interrupted checkout"),
                aethyme_broker::AdoptMode::ReplaceStale,
                None,
            )
            .unwrap()
            .session
    };
    std::fs::remove_file(worktree.join("README.md")).unwrap();

    let output = run(
        tmp.path(),
        &["finish", "cleanup", &session_id.to_string(), "--json"],
    );
    assert!(!output.status.success());
    assert!(worktree.exists());
    assert!(!worktree.join("README.md").exists());
    let mut broker = isolation::open(tmp.path());
    assert!(!broker.store().session(live.id).unwrap().status.is_closed());
    assert!(branch_exists(tmp.path(), &branch));
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
    assert!(!branch_exists(tmp.path(), &branch));
}

#[path = "common/json_error.rs"]
mod json_error;

#[cfg(unix)]
#[test]
fn invalid_finish_cleanup_proof_keeps_checkout_after_transient_git_errors() {
    let (tmp, session_id, worktree, branch) = promoted_fixture();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[cleanup]\nauto_remove = false\n",
    )
    .unwrap();

    let session = session_id.to_string();
    let finish = run(
        tmp.path(),
        &["finish", "--session", &session, "--keep-worktree", "--json"],
    );
    assert!(
        finish.status.success(),
        "{}",
        String::from_utf8_lossy(&finish.stderr)
    );
    assert!(worktree.exists());
    {
        let mut broker = isolation::open(tmp.path());
        // This legacy intent has no immutable proof. The complete checkout is
        // still eligible through ordinary cleanup, which must retain it when
        // Git cannot establish that registered removal succeeded.
        broker
            .store()
            .begin_finish_cleanup(
                session_id,
                r#"{"worktree_present":true,"branch_present":true}"#,
            )
            .unwrap();
    }

    let artifacts = tmp.path().join("transient-git-failure");
    let (output, removal_marker, discovery_marker) = run_with_transient_git_removal_failure(
        tmp.path(),
        &worktree,
        &["finish", "cleanup", &session, "--json"],
        &artifacts,
    );
    assert!(
        !output.status.success(),
        "transient Git failure must be reported, not converted to success"
    );
    assert!(
        removal_marker.exists(),
        "the registered removal was attempted"
    );
    assert!(
        !discovery_marker.exists(),
        "ordinary cleanup must not use an unrelated discovery error as orphan proof"
    );
    assert!(worktree.join("README.md").is_file());
    assert!(worktree.exists());
    assert!(
        branch_exists(tmp.path(), &branch),
        "a failed cleanup must preserve the branch"
    );
}

#[test]
fn cleaned_ledger_does_not_claim_a_recreated_directory_was_removed() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    let first = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["cleanup"]["worktree_removed"], true);
    assert!(!worktree.exists());
    std::fs::create_dir_all(&worktree).unwrap();
    let sentinel = worktree.join("unique-local-work.txt");
    std::fs::write(&sentinel, b"preserve me").unwrap();
    let output = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "already_closed");
    assert_eq!(report["closed"], true);
    assert_eq!(report["cleanup"]["completed"], false);
    assert_eq!(report["cleanup"]["worktree_removed"], false);
    assert_eq!(report["cleanup"]["attempted"], false);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve me");
    assert!(
        report["next_commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command.as_str()
                == Some(&format!("aethyme broker finish cleanup {session_id}")))
    );
}

#[cfg(unix)]
#[test]
fn cleaned_ledger_does_not_treat_a_dangling_symlink_as_absent() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    let first = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!worktree.exists());
    let missing_target = tmp.path().join("missing-symlink-target");
    std::os::unix::fs::symlink(&missing_target, &worktree).unwrap();
    let output = run(
        tmp.path(),
        &["finish", "--session", &session_id.to_string(), "--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "already_closed");
    assert_eq!(report["cleanup"]["completed"], false);
    assert_eq!(report["cleanup"]["worktree_removed"], false);
    assert_eq!(std::fs::read_link(&worktree).unwrap(), missing_target);
}

#[test]
fn cleaned_ledger_reports_verified_absence_on_repeat_finish() {
    let (tmp, session_id, worktree, _) = promoted_fixture();
    for _ in 0..2 {
        let output = run(
            tmp.path(),
            &["finish", "--session", &session_id.to_string(), "--json"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["cleanup"]["completed"], true);
        assert_eq!(report["cleanup"]["worktree_removed"], true);
        assert!(!worktree.exists());
    }
}
