#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn run(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(cwd)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

#[test]
fn acquire_wait_is_structured_and_grant_out_is_private() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let first_request = temp.path().join("first.json");
    let second_request = temp.path().join("second.json");
    let grant_path = temp.path().join("private-grant.json");
    let request = |id: &str, limit: u32| {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": id,
            "repository": "owner/repo",
            "worktree_fingerprint": id,
            "run_id": id,
            "ttl_seconds": 60,
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "host-work",
                "units": 1,
                "limit": limit
            }]
        }))
        .unwrap()
    };
    std::fs::write(&first_request, request("first", 4)).unwrap();
    std::fs::write(&second_request, request("second", 2)).unwrap();

    let acquired = run(
        temp.path(),
        &state,
        &[
            "resources",
            "acquire",
            first_request.to_str().unwrap(),
            "--grant-out",
            grant_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    assert!(!String::from_utf8_lossy(&acquired.stdout).contains("ownership_token"));
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
    assert!(stored["ownership_token"].as_str().unwrap().len() >= 64);
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&grant_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let blocked = run(
        temp.path(),
        &state,
        &[
            "resources",
            "acquire",
            second_request.to_str().unwrap(),
            "--wait",
            "1ms",
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(75));
    let failure: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(failure["code"], "capacity_policy_mismatch");
    assert_eq!(failure["retryable"], false);
    assert_eq!(failure["conflicts"][0]["code"], "capacity_policy_mismatch");
}

#[test]
fn supervised_run_preserves_child_status_and_quarantines_failed_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("run.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "supervised-run",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque",
            "run_id": "existing-runner",
            "ttl_seconds": 15,
            "resources": [{
                "key": "docker_project",
                "kind": "namespace",
                "prefix": "supervised"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run(
        temp.path(),
        &state,
        &[
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--cleanup-command",
            "test -n \"$AETHYME_RESOURCE_DOCKER_PROJECT\" && exit 9",
            "--json",
            "--",
            "sh",
            "-c",
            "test -n \"$AETHYME_RESOURCE_DOCKER_PROJECT\" && test -z \"$AETHYME_RESOURCE_OWNERSHIP_TOKEN\" && exit 7",
        ],
    );
    assert_eq!(output.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"child_exit_code\":7"), "{stderr}");
    assert!(stderr.contains("\"cleanup_exit_code\":9"), "{stderr}");
    assert!(
        stderr.contains("\"final_lease_state\":\"quarantined\""),
        "{stderr}"
    );
    assert!(!stderr.contains("ownership_token"), "{stderr}");

    let inventory = run(temp.path(), &state, &["resources", "list", "--json"]);
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(inventory[0]["state"], "quarantined");
}

#[cfg(unix)]
#[test]
fn supervised_run_forwards_termination_to_the_child_group_and_releases() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("signal.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "signal-run",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque",
            "run_id": "signal",
            "ttl_seconds": 15,
            "resources": [{
                "key": "runner",
                "kind": "exclusive_key",
                "name": "signal-runner"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let child = Command::new(CLI)
        .args([
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--json",
            "--",
            "sh",
            "-c",
            "trap 'exit 42' TERM; while :; do sleep 1; done",
        ])
        .current_dir(temp.path())
        .env("AETHYME_HOST_STATE_DIR", &state)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let listed = run(temp.path(), &state, &["resources", "list", "--json"]);
        if serde_json::from_slice::<serde_json::Value>(&listed.stdout)
            .ok()
            .and_then(|value| value.as_array().map(Vec::len))
            == Some(1)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "supervised lease was not acquired"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("forwarding signal"), "{stderr}");
    assert!(
        stderr.contains("\"final_lease_state\":\"released\""),
        "{stderr}"
    );
}

#[test]
fn host_resource_cli_plans_acquires_lists_and_releases_without_token_leaks() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "cli-run-1",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque-worktree-digest",
            "run_id": "prepush-1",
            "ttl_seconds": 60,
            "resources": [{
                "key": "docker_project",
                "kind": "namespace",
                "prefix": "project"
            }, {
                "key": "database",
                "kind": "exclusive_key",
                "name": "test-db"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let planned = run(
        temp.path(),
        &state,
        &[
            "resources",
            "plan",
            request_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert!(
        !state.exists(),
        "read-only planning initialized durable state"
    );
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["available"], true);
    assert_eq!(plan["advisory"], true);

    let acquired = run(
        temp.path(),
        &state,
        &[
            "resources",
            "acquire",
            request_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let grant: serde_json::Value = serde_json::from_slice(&acquired.stdout).unwrap();
    assert!(grant["ownership_token"].as_str().unwrap().len() >= 64);
    assert_eq!(grant["lease"]["allocations"].as_array().unwrap().len(), 2);
    let grant_path = temp.path().join("grant.json");
    std::fs::write(&grant_path, &acquired.stdout).unwrap();

    let listed = run(temp.path(), &state, &["resources", "list", "--json"]);
    assert!(listed.status.success());
    let inventory: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(inventory[0]["request_id"], "cli-run-1");
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("ownership_token"));

    let released = run(
        temp.path(),
        &state,
        &[
            "resources",
            "release",
            grant_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    let released_grant: serde_json::Value = serde_json::from_slice(&released.stdout).unwrap();
    assert_eq!(released_grant["lease"]["state"], "released");

    let empty = run(temp.path(), &state, &["resources", "list", "--json"]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&empty.stdout).unwrap(),
        serde_json::json!([])
    );
}
