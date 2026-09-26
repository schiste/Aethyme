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
/// Each test process gets its own block of 100 ports, chosen from its pid,
/// so two runs of this file at once (two broker gates, or a gate and a
/// developer's `cargo test`) no longer race for the same ports. Within a
/// process the ranges below stay disjoint, as before.
fn port_base() -> u16 {
    // 20000..=49900, clear of 4173 and of the usual ephemeral range.
    20000 + (std::process::id() % 300) as u16 * 100
}

struct Ports {
    singular: u16,
    singular_end: u16,
    per_worktree: u16,
    per_worktree_end: u16,
    held: u16,
    marker: u16,
    parallel: u16,
    parallel_end: u16,
}

fn ports() -> Ports {
    let base = port_base();
    Ports {
        singular: base,
        singular_end: base + 26,
        per_worktree: base + 30,
        per_worktree_end: base + 56,
        held: base + 60,
        marker: base + 70,
        parallel: base + 73,
        parallel_end: base + 76,
    }
}

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
    git(&root, &["branch", "aethyme/integration"]);
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
    let ports = ports();
    let singular_port = ports.singular;
    let singular_port_end = ports.singular_end;
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {singular_port}\nport_end = {singular_port_end}\n"
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
    assert_eq!(by_key("port")["value"], singular_port.to_string());
}

#[test]
fn per_worktree_plans_a_namespace_and_a_bounded_slot_instead_of_a_singleton() {
    let ports = ports();
    let per_worktree_port = ports.per_worktree;
    let per_worktree_port_end = ports.per_worktree_end;
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'per_worktree'\nport = {per_worktree_port}\nport_end = \
         {per_worktree_port_end}\npool_limit = 3\n"
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
    let ports = ports();
    let held_port = ports.held;
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {held_port}\n"
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
                {"key": "port", "kind": "tcp_port", "start": held_port, "end": held_port}
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
    assert_eq!(running[0]["port"], held_port.to_string());
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
        message.contains("already running") && message.contains(&held_port.to_string()),
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

/// A console command that stays up until the test writes `release` (capped
/// at 60 s), then exits cleanly. A fixed `sleep` let the console exit before
/// a loaded machine had even registered it.
fn hold_until(release: &Path) -> String {
    format!(
        "i=0; while [ ! -e '{}' ] && [ $i -lt 1200 ]; do sleep 0.05; i=$((i+1)); done",
        release.display()
    )
}

fn wait_for_running(root: &Path, state: &Path, expected: usize) -> serde_json::Value {
    // A deadline, not a poll count: each poll spawns the CLI, and on a
    // machine running other builds 80 polls could elapse before two console
    // processes had registered. 60 s is never reached when the machine is idle.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let status = run(root, state, &["console", "list", "--json"]);
        if status.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&status.stdout)
            && value["running"].as_array().is_some_and(|running| {
                running.len() == expected && running.iter().all(|row| row["marker"].is_object())
            })
        {
            return value;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let status = run(root, state, &["console", "list", "--json"]);
    panic!(
        "expected {expected} running console(s), got {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn managed_console_publishes_and_lists_its_exact_revision_marker() {
    let ports = ports();
    let marker_port = ports.marker;
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {marker_port}\n"
    )));
    let root = temp.path().join("repo");
    let release = state.join("release-console");
    let marker_command = format!(
        "test -s \"$AETHYME_CONSOLE_MARKER\" && {{ {}; }}",
        hold_until(&release)
    );
    let mut child = Command::new(CLI)
        .args([
            "console",
            "run",
            "--json",
            "--",
            "/bin/sh",
            "-c",
            &marker_command,
        ])
        .current_dir(&root)
        .env("AETHYME_HOST_STATE_DIR", &state)
        .spawn()
        .unwrap();

    let status = wait_for_running(&root, &state, 1);
    let running = status["running"].as_array().unwrap();
    let serving = &running[0];
    assert_eq!(status["identity"]["branch"], "main");
    assert_eq!(status["identity"]["commit"].as_str().unwrap().len(), 40);
    assert_eq!(status["identity"]["dirty"], false);
    assert_eq!(status["identity"]["integration_relation"], "current");
    assert_eq!(serving["port"], marker_port.to_string());
    assert_eq!(serving["branch"], "main");
    assert_eq!(serving["commit"].as_str().unwrap().len(), 40);
    assert_eq!(serving["dirty"], false);
    assert_eq!(serving["integration_relation"], "current");
    assert_eq!(serving["parallel"], false);
    let marker_path = Path::new(serving["marker"]["path"].as_str().unwrap());
    assert!(marker_path.is_file(), "marker path was not published");
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(marker_path).unwrap()).expect("marker JSON");
    assert_eq!(marker["marker_digest"], serving["marker"]["digest"]);
    assert_eq!(marker["commit"], serving["commit"]);
    assert_eq!(marker["port"], marker_port);
    assert_eq!(marker["integration_branch"], "aethyme/integration");
    assert_eq!(
        Path::new(marker["worktree"].as_str().unwrap()),
        std::fs::canonicalize(&root).unwrap()
    );

    let alias = json(&run(&root, &state, &["console", "status", "--json"]));
    assert_eq!(alias["running"], status["running"]);
    std::fs::write(&release, b"").unwrap();
    child.wait().unwrap();
    let stopped = json(&run(&root, &state, &["console", "list", "--json"]));
    assert!(stopped["running"].as_array().unwrap().is_empty());
    assert!(
        !marker_path.exists(),
        "clean shutdown must remove its marker"
    );
}

#[test]
fn allow_parallel_keeps_both_processes_in_the_registry_on_distinct_ports() {
    let ports = ports();
    let parallel_port = ports.parallel;
    let parallel_port_end = ports.parallel_end;
    let (temp, state) = repo(Some(&format!(
        "[console]\nmode = 'singular'\nport = {parallel_port}\nport_end = {parallel_port_end}\n"
    )));
    let root = temp.path().join("repo");
    let release_first = state.join("release-first");
    let release_second = state.join("release-second");
    let first_command = hold_until(&release_first);
    let second_command = hold_until(&release_second);
    let mut first = Command::new(CLI)
        .args(["console", "run", "--", "/bin/sh", "-c", &first_command])
        .current_dir(&root)
        .env("AETHYME_HOST_STATE_DIR", &state)
        .spawn()
        .unwrap();
    wait_for_running(&root, &state, 1);

    let mut second = Command::new(CLI)
        .args([
            "console",
            "run",
            "--allow-parallel",
            "--",
            "/bin/sh",
            "-c",
            &second_command,
        ])
        .current_dir(&root)
        .env("AETHYME_HOST_STATE_DIR", &state)
        .spawn()
        .unwrap();
    let status = wait_for_running(&root, &state, 2);
    let running = status["running"].as_array().unwrap();
    let ports: std::collections::BTreeSet<String> = running
        .iter()
        .map(|row| row["port"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ports.len(), 2, "parallel consoles must not share a port");
    assert_eq!(
        running.iter().filter(|row| row["parallel"] == true).count(),
        1
    );
    assert!(running.iter().all(|row| {
        row["commit"]
            .as_str()
            .is_some_and(|commit| commit.len() == 40)
    }));

    std::fs::write(&release_second, b"").unwrap();
    assert!(second.wait().unwrap().success());
    std::fs::write(&release_first, b"").unwrap();
    assert!(first.wait().unwrap().success());
    let stopped = json(&run(&root, &state, &["console", "list", "--json"]));
    assert!(stopped["running"].as_array().unwrap().is_empty());
}
