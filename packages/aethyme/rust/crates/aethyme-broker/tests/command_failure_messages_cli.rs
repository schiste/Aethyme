//! A failed broker command keeps the error it printed, redacted and capped,
//! so the failure can be explained after its terminal is gone, and `doctor`
//! lists the last day's failures with that message.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    NewSession, OperationProvider, OperationReconciliationState, OperationStatus, SessionOrigin,
};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn clear_inherited_git_environment(command: &mut Command) {
    let inherited = std::env::vars_os()
        .filter_map(|(key, _)| key.to_string_lossy().starts_with("GIT_").then_some(key))
        .collect::<Vec<_>>();
    for key in inherited {
        command.env_remove(key);
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.args(args).current_dir(repo);
    clear_inherited_git_environment(&mut command);
    let output = command
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
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn test_broker(repo: &Path) -> common::FixtureBroker {
    common::open_fixture_repository_path_broker(repo).unwrap()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    run_with_env(repo, args, &[])
}

fn command_with_env(repo: &Path, args: &[&str], environment: &[(&str, &str)]) -> Command {
    let broker = test_broker(repo);
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(repo);
    clear_inherited_git_environment(&mut command);
    for (key, value) in environment {
        command.env(key, value);
    }
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    drop(broker);
    command
}

fn run_with_env(repo: &Path, args: &[&str], environment: &[(&str, &str)]) -> Output {
    command_with_env(repo, args, environment).output().unwrap()
}

/// Payloads of every `broker.command.failed` event, oldest first.
fn failed_payloads(repo: &Path) -> Vec<serde_json::Value> {
    let mut broker = test_broker(repo);
    broker
        .store()
        .events_after_filtered(0, 1000, Some("broker.command.failed"))
        .unwrap()
        .into_iter()
        .map(|event| serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap())
        .collect()
}

fn fixture() -> common::FixtureRepository {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    drop(test_broker(fixture.path()));
    fixture
}

fn last_failure_message(repo: &Path) -> String {
    let payloads = failed_payloads(repo);
    let last = payloads.last().expect("a broker.command.failed event");
    last["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message recorded: {last}"))
        .to_string()
}

#[test]
fn a_failed_command_records_the_error_it_printed() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &["start", "--task", "t", "--base", "no-such-ref"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-ref"), "{stderr}");

    let message = last_failure_message(tmp.path());
    assert!(
        message.contains("cannot select a safe base") && message.contains("no-such-ref"),
        "{message}"
    );
}

#[test]
fn credentials_in_the_error_are_never_stored() {
    let tmp = fixture();
    run(
        tmp.path(),
        &["start", "--task", "t", "--base", "ghp_SECRET0123456789"],
    );
    let message = last_failure_message(tmp.path());
    assert!(!message.contains("SECRET0123456789"), "{message}");
    assert!(message.ends_with("[redacted]"), "{message}");

    run(
        tmp.path(),
        &[
            "start",
            "--task",
            "t",
            "--base",
            "https://u:PW-SECRET@example.invalid/x",
        ],
    );
    let message = last_failure_message(tmp.path());
    assert!(!message.contains("PW-SECRET"), "{message}");
    assert!(
        message.contains("https://[redacted]@example.invalid/x"),
        "{message}"
    );
}

#[test]
fn a_long_error_is_capped() {
    let tmp = fixture();
    let long = "r".repeat(2000);
    run(tmp.path(), &["start", "--task", "t", "--base", &long]);
    let message = last_failure_message(tmp.path());
    assert_eq!(message.chars().count(), 501, "{message}");
    assert!(message.ends_with('…'));
}

#[test]
fn a_coordinated_failure_records_the_providers_reason() {
    let tmp = fixture();
    let mut broker = test_broker(tmp.path());
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let output = run(
        tmp.path(),
        &[
            "advanced",
            "git",
            "--session",
            &session_id,
            "--reason",
            "test",
            "--",
            "checkout",
            "does-not-exist",
        ],
    );
    assert!(!output.status.success());
    let message = last_failure_message(tmp.path());
    assert!(
        message.contains("coordinated git operation") && message.contains("does-not-exist"),
        "the provider's own reason must be kept, not only the operation id: {message}"
    );
}

#[test]
fn doctor_lists_recent_failures_with_their_message() {
    let tmp = fixture();
    run(
        tmp.path(),
        &["start", "--task", "t", "--base", "no-such-ref"],
    );

    let json = run(tmp.path(), &["status", "doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor --json: {error}: {}",
            String::from_utf8_lossy(&json.stderr)
        )
    });
    let failures = report["recent_command_failures"]
        .as_array()
        .unwrap_or_else(|| panic!("no recent_command_failures: {report}"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0]["command_surface"], "broker.start");
    assert!(
        failures[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("no-such-ref")),
        "{failures:?}"
    );

    let text = String::from_utf8_lossy(&run(tmp.path(), &["status", "doctor"]).stdout).into_owned();
    assert!(
        text.contains("recent command failures: 1") && text.contains("no-such-ref"),
        "{text}"
    );
}

#[test]
fn doctor_omits_the_section_when_nothing_failed() {
    let tmp = fixture();
    let json = run(tmp.path(), &["status", "doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        report.get("recent_command_failures").is_none(),
        "an empty list is omitted: {report}"
    );
}

/// #706: an explicitly bounded GitHub read that stalls in the provider emits
/// the normal JSON error and a failed, non-reconcilable operation record.
#[cfg(all(unix, debug_assertions))]
#[test]
fn a_bounded_github_read_timeout_is_json_journaled_and_reaps_the_provider() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::time::{Duration, Instant};

    let tmp = fixture();
    let fake_bin = tmp.path().join(".git/fake-gh/bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_gh = fake_bin.join("gh");
    std::fs::write(
        &fake_gh,
        r#"#!/bin/sh
printf '%s\n' started > "$AETHYME_TEST_GH_STARTED"
printf '%s\n' "$0" > "$AETHYME_TEST_GH_EXECUTABLE"
printf '%s\n' "$$" > "$AETHYME_TEST_GH_PID"
if [ "${GIT_SSH_COMMAND+x}" = x ]; then
    printf '%s\n' present > "$AETHYME_TEST_GH_GIT_SSH_COMMAND"
else
    printf '%s\n' absent > "$AETHYME_TEST_GH_GIT_SSH_COMMAND"
fi
exec /bin/sleep 20
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    // This mirrors the production debug-test resolver's acceptance checks.
    // Prove the fixture executable will be selected before launching the CLI;
    // the test must never fall through to an installed, live gh.
    let canonical_fake_gh = fake_gh.canonicalize().unwrap();
    let metadata = std::fs::metadata(&canonical_fake_gh).unwrap();
    let mode = metadata.permissions().mode();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    assert!(metadata.is_file());
    assert!(metadata.uid() == 0 || metadata.uid() == uid);
    assert_ne!(mode & 0o111, 0, "fake gh must be executable");
    assert_eq!(mode & 0o022, 0, "fake gh must not be group/world writable");

    let search_path = std::env::join_paths([&fake_bin]).unwrap();
    let first_candidate = std::env::split_paths(&search_path)
        .next()
        .expect("fixture PATH has one directory")
        .join("gh")
        .canonicalize()
        .unwrap();
    assert_eq!(
        first_candidate, canonical_fake_gh,
        "debug gh resolution must choose the prevalidated fixture before trusted system directories"
    );
    let search_path = search_path.to_str().expect("fixture PATH is UTF-8");

    let mut broker = test_broker(tmp.path());
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let mut command = command_with_env(
        tmp.path(),
        &[
            "advanced",
            "gh",
            "--session",
            &session_id,
            "--repo",
            "Owner/Repo",
            "--queue-timeout",
            "2",
            "--json",
            "--",
            "api",
            "repos/Owner/Repo/issues",
        ],
        &[
            ("PATH", search_path),
            ("GIT_SSH_COMMAND", "test-only-should-not-reach-gh"),
            ("AETHYME_TEST_GH_STARTED", ".git/fake-gh/started"),
            ("AETHYME_TEST_GH_EXECUTABLE", ".git/fake-gh/executable"),
            ("AETHYME_TEST_GH_PID", ".git/fake-gh/pid"),
            (
                "AETHYME_TEST_GH_GIT_SSH_COMMAND",
                ".git/fake-gh/git-ssh-command",
            ),
        ],
    );
    let started = Instant::now();
    let output = command.output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the bounded provider call returned too late: {:?}",
        started.elapsed()
    );
    assert_eq!(
        output.status.code(),
        Some(i32::from(aethyme_broker::exit_status::ENVIRONMENT)),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let marker_root = tmp.path().join(".git/fake-gh");
    assert_eq!(
        std::fs::read_to_string(marker_root.join("started")).unwrap(),
        "started\n",
        "the fake provider must start before the deadline"
    );
    assert_eq!(
        std::fs::read_to_string(marker_root.join("executable"))
            .unwrap()
            .trim(),
        canonical_fake_gh.to_string_lossy(),
        "the bounded child must be the fixture fake, never a live gh binary"
    );
    let provider_pid: libc::pid_t = std::fs::read_to_string(marker_root.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The fake script execs sleep, preserving its PID, so absence proves the
    // direct provider child was killed and reaped before the command returned.
    // SAFETY: signal 0 only checks the process identified by the fixture-owned PID.
    let process_exists = unsafe { libc::kill(provider_pid, 0) };
    assert_eq!(
        process_exists, -1,
        "timed-out provider PID {provider_pid} remains"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH),
        "the provider PID must be absent, not merely inaccessible"
    );

    assert_eq!(
        std::fs::read_to_string(marker_root.join("git-ssh-command")).unwrap(),
        "absent\n",
        "coordinated gh must not inherit executable Git configuration"
    );

    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["schema"], "aethyme.broker-error/v1");
    assert_eq!(
        error["error"]["code"],
        aethyme_broker::exit_status::ENVIRONMENT
    );
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("read-only operation"), "{message}");
    assert!(message.contains("github.com/owner/repo"), "{message}");
    assert!(
        message.contains("waiting for \x60gh\x60 to respond"),
        "{message}"
    );
    assert!(message.contains("2s budget"), "{message}");
    assert!(message.contains("it changed nothing"), "{message}");
    assert!(message.contains("--queue-timeout"), "{message}");

    let mut broker = test_broker(tmp.path());
    let operations = broker.store().coordinated_operations().unwrap();
    assert_eq!(operations.len(), 1, "exactly one GH operation is journaled");
    let operation = &operations[0];
    assert_eq!(operation.provider, OperationProvider::Github);
    assert_eq!(operation.status, OperationStatus::Failed);
    let details: serde_json::Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["failure_class"], "coordinated_operation_timeout");
    assert_eq!(details["stage"], "executing coordinated operation");
    assert_eq!(details["remote_outcome"], "not_applicable");
    assert!(details["preparation_ms"].is_u64(), "{details}");
    assert!(details["provider_wait_ms"].as_u64().is_some(), "{details}");

    let shown = broker.show_coordinated_operation(operation.id).unwrap();
    assert_eq!(shown.operation.status, OperationStatus::Failed);
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::NotRequired
    );
    assert!(!shown.reconciliation.required);
    assert!(!shown.reconciliation.write_blocked);
    assert!(!shown.reconciliation.automatic_retry_allowed);
}

/// #555: a read nobody bounded still ends inside its default budget, with the
/// phase that spent it and the environment exit code, instead of outliving the
/// caller's own timeout.
#[cfg(unix)]
#[test]
fn an_unbounded_read_ends_at_its_default_budget_with_a_diagnostic() {
    let tmp = fixture();
    let hook = tmp.path().join(".git/slow-fsmonitor");
    std::fs::write(&hook, "#!/bin/sh\nsleep 6\n").unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    git(
        tmp.path(),
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    let mut broker = test_broker(tmp.path());
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let started = std::time::Instant::now();
    // No --queue-timeout and no --no-wait: the caller asked for no bound.
    let output = run_with_env(
        tmp.path(),
        &["advanced", "git", "--session", &session_id, "--", "status"],
        &[("AETHYME_BROKER_READ_BUDGET_SECS", "1")],
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the read outlived its budget: {:?}",
        started.elapsed()
    );
    assert_eq!(
        output.status.code(),
        Some(i32::from(aethyme_broker::exit_status::ENVIRONMENT))
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("waiting for `git` to respond"), "{stderr}");
    assert!(stderr.contains("1s budget"), "{stderr}");
}
