//! A dev server is a host resource, and the two regimes that want it —
//! one operator on one bookmarkable URL, and a fleet of agent worktrees
//! serving side by side — want opposite defaults. These tests pin the
//! observable difference between the modes rather than their internals.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

/// A port is the one thing these tests cannot each get a private copy of.
/// Every other input is per-test -- its own checkout, its own state dir --
/// but `plan` proves a port is free by binding it, and that probe reaches
/// the real machine. The harness runs this file's tests concurrently, so
/// three tests that plan a port from one shared range race for the same
/// probe, and `singular` -- whose range is exactly one port -- is always the
/// one with nowhere to fall back to. Disjoint ranges keep the concurrency
/// out of the assertions. Off 4173 as well: that is the Vite preview
/// default, and an operator serving one should not fail this suite.
const SINGULAR_PORT: u16 = 45173;
const SINGULAR_PORT_END: u16 = 45199;
const PER_WORKTREE_PORT: u16 = 45273;
const PER_WORKTREE_PORT_END: u16 = 45299;
const HELD_PORT: u16 = 45373;

fn run(cwd: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(cwd)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A checkout with an optional `[console]` section, returned with its state dir.
fn repo(console_section: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main", "repo"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["config", "user.name", "Test"]);
    std::fs::write(
        root.join(".aethyme/config.toml"),
        console_section.unwrap_or(""),
    )
    .unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "init"]);
    let state = temp.path().join("state");
    (temp, state)
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_repository_that_configured_nothing_gets_the_singular_mode() {
    let (temp, state) = repo(None);
    let root = temp.path().join("repo");
    let status = json(&run(&root, &state, &["console", "status", "--json"]));
    assert_eq!(status["identity"]["mode"], "singular");
    assert_eq!(status["identity"]["canonical"], true);
    assert_eq!(status["running"].as_array().unwrap().len(), 0);
}

/// `status` is the answer to "why is my dev server not the one I am editing",
/// so the primary checkout and an agent worktree must not read the same.
#[test]
fn a_linked_worktree_is_reported_as_not_canonical() {
    let (temp, state) = repo(None);
    let root = temp.path().join("repo");
    let linked = temp.path().join("linked");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    let status = json(&run(&linked, &state, &["console", "status", "--json"]));
    assert_eq!(status["identity"]["canonical"], false);
    // One repository, one key: a console started here contends with one
    // started in the primary checkout.
    let canonical = json(&run(&root, &state, &["console", "status", "--json"]));
    assert_eq!(
        status["identity"]["repository"],
        canonical["identity"]["repository"]
    );
}

#[test]
fn singular_plans_one_exclusive_key_and_one_pinned_port() {
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {SINGULAR_PORT}\nport_end = {SINGULAR_PORT_END}\n"
    )));
    let root = temp.path().join("repo");
    let plan = json(&run(&root, &state, &["console", "plan", "--json"]));
    let proposed = plan["proposed"].as_array().unwrap();
    let by_key = |key: &str| {
        proposed
            .iter()
            .find(|a| a["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {proposed:?}"))
            .clone()
    };
    assert_eq!(by_key("console")["kind"], "exclusive_key");
    assert_eq!(by_key("port")["value"], SINGULAR_PORT.to_string());
}

#[test]
fn per_worktree_plans_a_namespace_and_a_bounded_slot_instead_of_a_singleton() {
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'per_worktree'\nport = {PER_WORKTREE_PORT}\nport_end = \
         {PER_WORKTREE_PORT_END}\npool_limit = 3\n"
    )));
    let root = temp.path().join("repo");
    let plan = json(&run(&root, &state, &["console", "plan", "--json"]));
    let keys: Vec<&str> = plan["proposed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"namespace"), "{keys:?}");
    assert!(keys.contains(&"slot"), "{keys:?}");
    assert!(
        !keys.contains(&"console"),
        "per_worktree must not take the singleton key, or it would serialise the fleet: {keys:?}"
    );
}

/// The whole value of `singular` is that the second launch fails loudly
/// instead of quietly answering on another port.
#[test]
fn singular_refuses_a_second_console_and_names_the_one_already_serving() {
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {HELD_PORT}\n"
    )));
    let root = temp.path().join("repo");
    let identity = json(&run(&root, &state, &["console", "status", "--json"]));
    let repository = identity["identity"]["repository"].as_str().unwrap();

    let held = temp.path().join("held.json");
    let grant = temp.path().join("grant.json");
    std::fs::write(
        &held,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "request_id": "already-serving",
            "repository": repository,
            "worktree_fingerprint": "other-worktree",
            "run_id": "already-serving",
            "ttl_seconds": 120,
            "holder_pid": 4242,
            "resources": [
                {"key": "console", "kind": "exclusive_key", "name": format!("console:{repository}")},
                {"key": "port", "kind": "tcp_port", "start": HELD_PORT, "end": HELD_PORT}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let acquired = run(
        &root,
        &state,
        &[
            "resources",
            "acquire",
            held.to_str().unwrap(),
            "--grant-out",
            grant.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        acquired.status.success(),
        "{}",
        String::from_utf8_lossy(&acquired.stderr)
    );

    let status = json(&run(&root, &state, &["console", "status", "--json"]));
    let running = status["running"].as_array().unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0]["port"], HELD_PORT.to_string());
    assert_eq!(
        running[0]["canonical"], false,
        "the holder is another worktree, and saying which one is the point"
    );

    let refused = run(
        &root,
        &state,
        &["console", "run", "--", "/bin/sh", "-c", "exit 0"],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("already running") && message.contains(&HELD_PORT.to_string()),
        "a bare resource conflict does not tell the operator where to look: {message}"
    );
}

/// Opting out must still leave one spelling of "start the console" working,
/// or the mode is a trap rather than an option.
#[test]
fn unmanaged_reserves_nothing_and_still_runs_the_command() {
    let (temp, state) = repo(Some("[console]\nmode = 'unmanaged'\n"));
    let root = temp.path().join("repo");
    let marker = temp.path().join("ran");
    let script = format!("touch {}", marker.to_str().unwrap());
    let output = run(
        &root,
        &state,
        &["console", "run", "--", "/bin/sh", "-c", &script],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists());
    let leases = json(&run(
        &root,
        &state,
        &["resources", "list", "--all", "--json"],
    ));
    let empty = leases["leases"]
        .as_array()
        .is_none_or(|rows| rows.is_empty());
    assert!(
        empty,
        "unmanaged must not appear in the inventory: {leases}"
    );
}

/// A non-zero dev server exit must not be reported as a clean shutdown.
#[test]
fn a_failing_command_fails_the_console() {
    let (temp, state) = repo(Some("[console]\nmode = 'unmanaged'\n"));
    let root = temp.path().join("repo");
    let output = run(
        &root,
        &state,
        &["console", "run", "--", "/bin/sh", "-c", "exit 3"],
    );
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn an_unknown_action_names_the_ones_that_exist() {
    let (temp, state) = repo(None);
    let root = temp.path().join("repo");
    let output = run(&root, &state, &["console", "restart"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("status"), "{message}");
    assert!(message.contains("plan"), "{message}");
    assert!(message.contains("run"), "{message}");
}
