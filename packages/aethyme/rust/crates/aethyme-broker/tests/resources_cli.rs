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
