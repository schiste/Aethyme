#[cfg(unix)]
use std::io::BufRead as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{Duration, Instant};

use aethyme_broker::{
    HostLeaseState, HostResourceCoordinator, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

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
fn reap_reports_unproven_live_and_unverifiable_holders_without_releasing_allocations() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let database = state.join("host-resources.db");
    let mut coordinator = HostResourceCoordinator::open(&database).unwrap();
    let request = |request_id: &str, holder_pid| HostResourceRequest {
        schema_version: 1,
        request_id: request_id.into(),
        repository: "owner/repo".into(),
        worktree_fingerprint: request_id.into(),
        run_id: format!("run-{request_id}"),
        ttl_seconds: 60,
        holder_pid: Some(holder_pid),
        resources: vec![
            HostResourceRequirement {
                key: "workers".into(),
                resource: HostResourceKind::Capacity {
                    pool: "diagnostics".into(),
                    units: 1,
                    limit: 4,
                },
            },
            HostResourceRequirement {
                key: "database".into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: format!("db-{request_id}"),
                },
            },
        ],
    };

    let live = coordinator
        .acquire(&request("live", std::process::id()))
        .unwrap();
    let unverifiable = coordinator.acquire(&request("unverifiable", u32::MAX)).unwrap();
    for grant in [&live, &unverifiable] {
        coordinator
            .quarantine(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
    }
    let before = coordinator.list(false).unwrap();
    let live_lease_id = live.lease.lease_id.clone();
    let unverifiable_lease_id = unverifiable.lease.lease_id.clone();
    drop(coordinator);

    let reaped = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        reaped.status.success(),
        "{}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&reaped.stdout).unwrap();
    assert_eq!(report["dead_holders_seen"], 0);
    assert_eq!(report["reclaimed_capacity_units"], 0);
    let not_reaped = report["not_reaped"].as_array().unwrap();
    assert_eq!(not_reaped.len(), 2);
    for lease_id in [&live_lease_id, &unverifiable_lease_id] {
        assert!(not_reaped.iter().any(|entry| {
            entry["lease_id"].as_str() == Some(lease_id.as_str())
                && entry["reason"] == "not_proven_gone"
        }));
    }

    let after = HostResourceCoordinator::open(&database)
        .unwrap()
        .list(false)
        .unwrap();
    assert_eq!(after.len(), before.len());
    for prior in before {
        let retained = after
            .iter()
            .find(|lease| lease.lease_id == prior.lease_id)
            .unwrap();
        assert_eq!(retained.state, HostLeaseState::Quarantined);
        assert_eq!(retained.allocations, prior.allocations);
    }

    let human = run(temp.path(), &state, &["gc", "reap"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains(&live_lease_id));
    assert!(human.contains(&unverifiable_lease_id));
    assert!(human.contains("not_proven_gone"));
}

#[cfg(unix)]
struct ProcessGroupSurvivor {
    started: PathBuf,
    release: PathBuf,
    finished: PathBuf,
}

#[cfg(unix)]
impl ProcessGroupSurvivor {
    fn new(directory: &Path, name: &str) -> Self {
        Self {
            started: directory.join(format!("{name}-started")),
            release: directory.join(format!("{name}-release")),
            finished: directory.join(format!("{name}-finished")),
        }
    }

    fn shell_fragment(&self) -> String {
        let started = self.started.file_name().unwrap().to_str().unwrap();
        let release = self.release.file_name().unwrap().to_str().unwrap();
        let finished = self.finished.file_name().unwrap().to_str().unwrap();
        // The iteration cap gives a best-effort fallback; scheduler delays can
        // stretch its nominal 60 seconds.
        format!(
            ": > '{started}'; ( i=0; while [ ! -e '{release}' ] && [ \"$i\" -lt 600 ]; do sleep 0.1; i=$((i + 1)); done; : > '{finished}' ) </dev/null >/dev/null 2>&1 &"
        )
    }

    fn wait_for(&self, marker: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        marker.exists()
    }

    fn wait_started(&self) -> bool {
        self.wait_for(&self.started)
    }

    fn stop_and_wait(&self) -> bool {
        if std::fs::write(&self.release, b"release").is_err() {
            return false;
        }
        self.wait_for(&self.finished)
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupSurvivor {
    fn drop(&mut self) {
        if std::fs::write(&self.release, b"release").is_ok() && self.started.exists() {
            let _ = self.wait_for(&self.finished);
        }
    }
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
            "advanced",
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
            "advanced",
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
fn explain_joins_capacity_contention_to_the_live_holder_without_a_token() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let first_request = temp.path().join("first.json");
    let second_request = temp.path().join("second.json");
    let request = |id: &str| {
        serde_json::json!({
            "schema_version": 1,
            "request_id": id,
            "repository": "owner/repo",
            "worktree_fingerprint": id,
            "run_id": id,
            "ttl_seconds": 60,
            "holder_pid": std::process::id(),
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "host-work",
                "units": 1,
                "limit": 1
            }]
        })
    };
    std::fs::write(
        &first_request,
        serde_json::to_vec_pretty(&request("first")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &second_request,
        serde_json::to_vec_pretty(&request("second")).unwrap(),
    )
    .unwrap();

    let acquired = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            first_request.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );

    let explained = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "explain",
            second_request.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        explained.status.success(),
        "{}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&explained.stdout).unwrap();
    assert_eq!(value["request_id"], "second");
    assert!(value["request_digest"].as_str().is_some());
    assert_eq!(value["available"], false);
    assert_eq!(value["blockers"][0]["conflict"]["resource_key"], "workers");
    assert_eq!(value["blockers"][0]["leases"][0]["run_id"], "first");
    assert_eq!(
        value["blockers"][0]["holders"][0]["holder_pid"],
        std::process::id()
    );
    assert_eq!(value["blockers"][0]["holders"][0]["process_alive"], true);
    assert_eq!(value["wait"]["waitable"], true);
    assert_eq!(value["wait"]["reason"], "resource_contention");
    assert!(!String::from_utf8_lossy(&explained.stdout).contains("ownership_token"));
}

#[cfg(unix)]
#[test]
fn reap_reclaims_dead_capacity_and_explain_names_the_quarantined_holder() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let first_request = temp.path().join("first.json");
    let second_request = temp.path().join("second.json");
    let mut child = Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let request = |id: &str, holder_pid: u32| {
        serde_json::json!({
            "schema_version": 1,
            "request_id": id,
            "repository": "owner/repo",
            "worktree_fingerprint": id,
            "run_id": id,
            "ttl_seconds": 60,
            "holder_pid": holder_pid,
            "resources": [{
                "key": "prepush_slot",
                "kind": "capacity",
                "pool": "prepush",
                "units": 1,
                "limit": 3
            }, {
                "key": "worktree",
                "kind": "exclusive_key",
                "name": "shared-worktree"
            }]
        })
    };
    std::fs::write(
        &first_request,
        serde_json::to_vec_pretty(&request("first", dead_pid)).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &second_request,
        serde_json::to_vec_pretty(&request("second", std::process::id())).unwrap(),
    )
    .unwrap();

    let acquired = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            first_request.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let grant: serde_json::Value = serde_json::from_slice(&acquired.stdout).unwrap();
    let lease_id = grant["lease"]["lease_id"].as_str().unwrap();
    let generation = grant["lease"]["generation"].as_u64().unwrap();

    let reaped = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        reaped.status.success(),
        "{}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&reaped.stdout).unwrap();
    assert_eq!(report["dead_holders_seen"], 1);
    assert_eq!(report["reclaimed_capacity_units"], 1);
    assert_eq!(report["retained_quarantined_leases"], 1);
    assert_eq!(report["leases"][0]["lease_id"], lease_id);
    assert_eq!(report["leases"][0]["state"], "quarantined");

    let repeated = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let repeated_report: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated_report["dead_holders_seen"], 0);
    assert_eq!(repeated_report["reclaimed_capacity_units"], 0);
    assert!(repeated_report["leases"].as_array().unwrap().is_empty());

    let explained = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "explain",
            second_request.to_str().unwrap(),
        ],
    );
    assert!(
        explained.status.success(),
        "{}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let rendered = String::from_utf8_lossy(&explained.stdout);
    assert!(rendered.contains("blocked"), "{rendered}");
    assert!(rendered.contains(lease_id), "{rendered}");
    assert!(
        rendered.contains(&format!("holder PID {dead_pid}")),
        "{rendered}"
    );
    assert!(rendered.contains("process is gone"), "{rendered}");
    assert!(
        rendered.contains(&format!("--confirm {generation}")),
        "{rendered}"
    );
    assert!(rendered.contains("waitable: false"), "{rendered}");
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
            "advanced",
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

    let inventory = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(inventory[0]["state"], "quarantined");
}

#[cfg(unix)]
#[test]
fn supervised_capacity_hold_blocks_after_dead_holder_and_group_survivor() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("run.json");
    let cleanup_marker = temp.path().join("cleanup-marker");
    let survivor = ProcessGroupSurvivor::new(temp.path(), "survivor");
    let mut holder = Command::new("sleep").arg("30").spawn().unwrap();
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "surviving-group-run",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque",
            "run_id": "surviving-group",
            "ttl_seconds": 15,
            "holder_pid": holder.id(),
            "resources": [{
                "key": "runner",
                "kind": "exclusive_key",
                "name": "surviving-group-runner"
            }, {
                "key": "workers",
                "kind": "capacity",
                "pool": "supervised-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let child_command = format!("{} exit 23", survivor.shell_fragment());
    let output = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--cleanup-command",
            ": > cleanup-marker",
            "--json",
            "--",
            "sh",
            "-c",
            child_command.as_str(),
        ],
    );
    assert_eq!(output.status.code(), Some(23));
    assert!(
        survivor.wait_started(),
        "process-group survivor did not start"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"child_exit_code\":23"), "{stderr}");
    assert!(
        stderr.contains("\"final_lease_state\":\"quarantined\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("skipping exact cleanup command"),
        "{stderr}"
    );
    assert!(!cleanup_marker.exists());
    holder.kill().unwrap();
    holder.wait().unwrap();
    let reaped = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        reaped.status.success(),
        "{}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    let reaped: serde_json::Value = serde_json::from_slice(&reaped.stdout).unwrap();
    assert_eq!(reaped["reclaimed_capacity_units"], 0);

    let inventory = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(inventory[0]["state"], "quarantined");

    let contender_path = temp.path().join("contender.json");
    std::fs::write(
        &contender_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "surviving-group-contender",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque-contender",
            "run_id": "surviving-group-contender",
            "ttl_seconds": 15,
            "resources": [{
                "key": "runner",
                "kind": "exclusive_key",
                "name": "surviving-group-runner"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let blocked = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            contender_path.to_str().unwrap(),
            "--wait",
            "1ms",
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(75));

    let capacity_contender_path = temp.path().join("capacity-contender.json");
    std::fs::write(
        &capacity_contender_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "surviving-group-capacity-contender",
            "repository": "owner/repo",
            "worktree_fingerprint": "capacity-contender",
            "run_id": "capacity-contender",
            "ttl_seconds": 15,
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "supervised-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let capacity_blocked = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            capacity_contender_path.to_str().unwrap(),
            "--wait",
            "1ms",
            "--json",
        ],
    );
    assert_eq!(capacity_blocked.status.code(), Some(75));
    let failure: serde_json::Value = serde_json::from_slice(&capacity_blocked.stdout).unwrap();
    assert_eq!(failure["conflicts"][0]["resource_key"], "workers");
    assert!(
        survivor.stop_and_wait(),
        "process-group survivor did not acknowledge release"
    );
}

#[cfg(unix)]
#[test]
fn supervised_cleanup_capacity_hold_blocks_after_dead_holder() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("run.json");
    let cleanup_started = temp.path().join("cleanup-started");
    let cleanup_survivor = ProcessGroupSurvivor::new(temp.path(), "cleanup-survivor");
    let mut holder = Command::new("sleep").arg("30").spawn().unwrap();
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "surviving-cleanup-group-run",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque",
            "run_id": "surviving-cleanup-group",
            "ttl_seconds": 15,
            "holder_pid": holder.id(),
            "resources": [{
                "key": "runner",
                "kind": "exclusive_key",
                "name": "surviving-cleanup-runner"
            }, {
                "key": "workers",
                "kind": "capacity",
                "pool": "cleanup-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let cleanup_command = format!(": > cleanup-started; {}", cleanup_survivor.shell_fragment());
    let output = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--cleanup-command",
            cleanup_command.as_str(),
            "--json",
            "--",
            "sh",
            "-c",
            "exit 31",
        ],
    );
    assert_eq!(output.status.code(), Some(31));
    assert!(
        cleanup_survivor.wait_started(),
        "cleanup process-group survivor did not start"
    );
    assert!(cleanup_started.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"child_exit_code\":31"), "{stderr}");
    assert!(stderr.contains("\"cleanup_exit_code\":0"), "{stderr}");
    assert!(
        stderr.contains("\"final_lease_state\":\"quarantined\""),
        "{stderr}"
    );
    assert!(stderr.contains("cleanup process group"), "{stderr}");
    holder.kill().unwrap();
    holder.wait().unwrap();
    let reaped = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        reaped.status.success(),
        "{}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    let reaped: serde_json::Value = serde_json::from_slice(&reaped.stdout).unwrap();
    assert_eq!(reaped["reclaimed_capacity_units"], 0);

    let inventory = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(inventory[0]["state"], "quarantined");

    let contender_path = temp.path().join("contender.json");
    std::fs::write(
        &contender_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "surviving-cleanup-contender",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque-contender",
            "run_id": "surviving-cleanup-contender",
            "ttl_seconds": 15,
            "resources": [{
                "key": "runner",
                "kind": "exclusive_key",
                "name": "surviving-cleanup-runner"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let blocked = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            contender_path.to_str().unwrap(),
            "--wait",
            "1ms",
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(75));

    let capacity_contender_path = temp.path().join("capacity-contender.json");
    std::fs::write(
        &capacity_contender_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "cleanup-capacity-contender",
            "repository": "owner/repo",
            "worktree_fingerprint": "cleanup-capacity-contender",
            "run_id": "cleanup-capacity-contender",
            "ttl_seconds": 15,
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "cleanup-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let capacity_blocked = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            capacity_contender_path.to_str().unwrap(),
            "--wait",
            "1ms",
            "--json",
        ],
    );
    assert_eq!(capacity_blocked.status.code(), Some(75));
    let failure: serde_json::Value = serde_json::from_slice(&capacity_blocked.stdout).unwrap();
    assert_eq!(failure["conflicts"][0]["resource_key"], "workers");
    assert!(
        cleanup_survivor.stop_and_wait(),
        "cleanup process-group survivor did not acknowledge release"
    );
}

#[cfg(unix)]
#[test]
fn supervised_capacity_hold_is_cleared_after_both_groups_are_absent() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request_path = temp.path().join("run.json");
    let cleanup_marker = temp.path().join("clean-group-cleanup-marker");
    let mut holder = Command::new("sleep").arg("30").spawn().unwrap();
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "clean-group-run",
            "repository": "owner/repo",
            "worktree_fingerprint": "opaque",
            "run_id": "clean-group",
            "ttl_seconds": 15,
            "holder_pid": holder.id(),
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "clean-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--cleanup-command",
            ": > clean-group-cleanup-marker; exit 0",
            "--json",
            "--",
            "sh",
            "-c",
            "exit 0",
        ],
    );
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"cleanup_exit_code\":0"), "{stderr}");
    assert!(
        stderr.contains("\"final_lease_state\":\"released\""),
        "{stderr}"
    );
    assert!(cleanup_marker.exists());
    holder.kill().unwrap();
    holder.wait().unwrap();

    let contender_path = temp.path().join("after-cleanup.json");
    std::fs::write(
        &contender_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "after-clean-groups",
            "repository": "owner/repo",
            "worktree_fingerprint": "after-clean-groups",
            "run_id": "after-clean-groups",
            "ttl_seconds": 15,
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "clean-pool",
                "units": 1,
                "limit": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let acquired = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            contender_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
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
    let mut child = Command::new(CLI)
        .args([
            "advanced",
            "resources",
            "run",
            request_path.to_str().unwrap(),
            "--json",
            "--",
            "sh",
            "-c",
            "trap 'wait \"$sleeper\" 2>/dev/null; exit 42' TERM; sleep 10 & sleeper=$!; printf 'child-ready\\n'; wait \"$sleeper\"",
        ])
        .current_dir(temp.path())
        .env("AETHYME_HOST_STATE_DIR", &state)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut readiness = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut readiness)
        .unwrap();
    assert_eq!(readiness, "child-ready\n");

    let listed = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    let leases: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(leases.as_array().map(Vec::len), Some(1));
    // SAFETY: `kill` takes the pid and signal by value and dereferences nothing
    // this crate owns, so `child.id() as i32` is the only requirement. The
    // target is the child this test just spawned.
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
            "advanced",
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
            "advanced",
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

    let listed = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    assert!(listed.status.success());
    let inventory: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(inventory[0]["request_id"], "cli-run-1");
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("ownership_token"));

    let released = run(
        temp.path(),
        &state,
        &[
            "advanced",
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

    let empty = run(
        temp.path(),
        &state,
        &["advanced", "resources", "list", "--json"],
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&empty.stdout).unwrap(),
        serde_json::json!([])
    );
}

/// Issue #139: an operator reaching for a lease id got "No such file or
/// directory", which reads as a broken command rather than a wrong argument.
#[test]
fn release_given_a_lease_id_points_at_reconcile() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let request = temp.path().join("request.json");
    let grant_path = temp.path().join("grant.json");
    std::fs::write(
        &request,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "only",
            "repository": "owner/repo",
            "worktree_fingerprint": "only",
            "run_id": "only",
            "ttl_seconds": 60,
            "resources": [{
                "key": "workers",
                "kind": "capacity",
                "pool": "host-work",
                "units": 1,
                "limit": 2
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let acquired = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            request.to_str().unwrap(),
            "--grant-out",
            grant_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "acquire failed: {}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let grant: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
    let lease_id = grant["lease"]["lease_id"].as_str().unwrap();

    let released = run(
        temp.path(),
        &state,
        &["advanced", "resources", "release", lease_id],
    );
    assert!(!released.status.success(), "release must reject a lease id");
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&released.stdout),
        String::from_utf8_lossy(&released.stderr)
    );
    assert!(
        message.contains("not a lease id") && message.contains("reconcile"),
        "release must explain the argument and name the reclaim path: {message}"
    );
    assert!(
        !message.contains("No such file or directory"),
        "the raw file error is what misled the operator: {message}"
    );
}

#[cfg(unix)]
#[test]
fn acquire_dead_exclusive_holder_is_structured_and_immediately_nonretryable() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let first_request = temp.path().join("first.json");
    let second_request = temp.path().join("second.json");
    let mut child = Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let request = |id: &str, holder_pid: u32| {
        serde_json::json!({
            "schema_version": 1,
            "request_id": id,
            "repository": "owner/repo",
            "worktree_fingerprint": id,
            "run_id": id,
            "ttl_seconds": 60,
            "holder_pid": holder_pid,
            "resources": [{
                "key": "worktree",
                "kind": "exclusive_key",
                "name": "shared-worktree"
            }]
        })
    };
    std::fs::write(
        &first_request,
        serde_json::to_vec_pretty(&request("first", dead_pid)).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &second_request,
        serde_json::to_vec_pretty(&request("second", std::process::id())).unwrap(),
    )
    .unwrap();

    let acquired = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            first_request.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let grant: serde_json::Value = serde_json::from_slice(&acquired.stdout).unwrap();
    let lease_id = grant["lease"]["lease_id"].as_str().unwrap();

    let reaped = run(temp.path(), &state, &["gc", "reap", "--json"]);
    assert!(
        reaped.status.success(),
        "{}",
        String::from_utf8_lossy(&reaped.stderr)
    );
    let reap_report: serde_json::Value = serde_json::from_slice(&reaped.stdout).unwrap();
    assert_eq!(reap_report["retained_quarantined_leases"], 1);
    assert_eq!(reap_report["leases"][0]["lease_id"], lease_id);
    assert_eq!(reap_report["leases"][0]["state"], "quarantined");

    let blocked = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "acquire",
            second_request.to_str().unwrap(),
            "--wait",
            "30s",
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(75));
    let failure: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(failure["code"], "orphaned_holder");
    assert_eq!(failure["retryable"], false);
    assert!(failure["waited_ms"].as_u64().unwrap() < 1_000);
    assert_eq!(failure["conflicts"][0]["code"], "orphaned_holder");
    assert_eq!(failure["conflicts"][0]["owning_lease"], lease_id);

    let explained = run(
        temp.path(),
        &state,
        &[
            "advanced",
            "resources",
            "explain",
            second_request.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        explained.status.success(),
        "{}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let explanation: serde_json::Value = serde_json::from_slice(&explained.stdout).unwrap();
    assert_eq!(explanation["wait"]["waitable"], false);
    assert_eq!(explanation["wait"]["reason"], "orphaned_holder");
    assert_eq!(
        explanation["blockers"][0]["leases"][0]["lease_id"],
        lease_id
    );
    assert_eq!(
        explanation["blockers"][0]["leases"][0]["state"],
        "quarantined"
    );
    assert!(!String::from_utf8_lossy(&blocked.stdout).contains("ownership_token"));
}
