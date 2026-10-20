//! `--json` shape snapshots for session lifecycle and the broker command
//! surfaces audited in the L0 CLI baseline, driven through the real router.
//!
//! Only the type skeleton is stored (keys and leaf types), so the snapshot
//! is stable across runs while any added, removed or retyped field shows up
//! as a diff under `tests/snapshots/json-shapes/`.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use support::snapshots::{assert_snapshots, json_shape};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A host-state directory created fresh for this test process and removed
/// with it, so nothing a previous run left behind shows up in `status`.
///
/// Passed canonicalized: macOS reaches the temp directory through the
/// `/var` -> `/private/var` symlink, and through a symlinked host-state path
/// `status` reports the session's own worktree as unclaimed, which Linux
/// does not (a separate defect, not what this snapshot pins).
fn host_state() -> &'static (tempfile::TempDir, PathBuf) {
    static STATE: std::sync::OnceLock<(tempfile::TempDir, PathBuf)> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("host state tempdir");
        let canonical = dir.path().canonicalize().expect("canonical host state");
        (dir, canonical)
    })
}

fn aethyme(cwd: &Path, args: &[&str]) -> Output {
    aethyme_with_host_state(cwd, args, &host_state().1)
}

fn aethyme_with_host_state(cwd: &Path, args: &[&str], host_state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(cwd)
        .env_remove("AETHYME_REPO")
        .env_remove("AETHYME_AGENT")
        .env_remove("AETHYME_BROKER_DB")
        // Pin the session holder to this test process. Otherwise the holder
        // is whatever agent runtime launched the suite (a live `claude`
        // locally, none in CI), and lease liveness changes the JSON shape.
        .env("AETHYME_AGENT_PID", std::process::id().to_string())
        // Pin the host-state directory too. Left to the host, a Linux runner
        // has none and `start` reports `worktree_placement.fallback_reason`,
        // while a workstation has one and does not, so the shape depended on
        // where the suite ran (#598).
        .env("AETHYME_HOST_STATE_DIR", host_state)
        // And the worktree root, which an operator may export to an external
        // disk and which a Linux runner never sets.
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("XDG_STATE_HOME")
        .env(
            "AETHYME_CHAU7_MCP_BRIDGE",
            "/__aethyme_test_no_chau7_bridge__",
        )
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme")
}

fn json(cwd: &Path, args: &[&str]) -> serde_json::Value {
    let output = aethyme(cwd, args);
    assert!(
        output.status.success(),
        "aethyme {args:?} failed ({:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "aethyme {args:?} did not print JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn json_with_host_state(cwd: &Path, args: &[&str], host_state: &Path) -> serde_json::Value {
    let output = aethyme_with_host_state(cwd, args, host_state);
    assert!(
        output.status.success(),
        "aethyme {args:?} failed ({:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "aethyme {args:?} did not print JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn json_with_exit(
    cwd: &Path,
    args: &[&str],
    host_state: &Path,
    expected_exit: i32,
) -> serde_json::Value {
    json_with_exit_kind(
        cwd,
        args,
        host_state,
        expected_exit,
        JsonExitKind::ErrorEnvelope,
    )
}

fn json_with_report_exit(
    cwd: &Path,
    args: &[&str],
    host_state: &Path,
    expected_exit: i32,
) -> serde_json::Value {
    json_with_exit_kind(
        cwd,
        args,
        host_state,
        expected_exit,
        JsonExitKind::ExistingReport,
    )
}

#[derive(Clone, Copy)]
enum JsonExitKind {
    ErrorEnvelope,
    ExistingReport,
}

fn json_with_exit_kind(
    cwd: &Path,
    args: &[&str],
    host_state: &Path,
    expected_exit: i32,
    kind: JsonExitKind,
) -> serde_json::Value {
    let output = aethyme_with_host_state(cwd, args, host_state);
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "aethyme {args:?}:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if expected_exit != 0 {
        assert!(
            output.stderr.is_empty(),
            "JSON error stays on stdout: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "aethyme {args:?} did not print JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    if expected_exit != 0 {
        match kind {
            JsonExitKind::ErrorEnvelope => {
                assert_eq!(value["schema"], "aethyme.broker-error/v1");
                assert_eq!(value["error"]["code"], expected_exit);
            }
            JsonExitKind::ExistingReport => {
                assert_ne!(value["schema"], "aethyme.broker-error/v1");
            }
        }
    }
    value
}

fn shape(value: &serde_json::Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&json_shape(value)).expect("serialize shape")
    )
}

/// `StatusAdvice` identifiers are `Option<i64>`. Which advice item is first
/// depends on the observed status, so represent their stable nullable type
/// after checking every emitted value against the contract.
fn status_shape(value: &serde_json::Value) -> String {
    let advice = value
        .get("advice")
        .and_then(serde_json::Value::as_array)
        .expect("status advice is an array");
    assert!(
        !advice.is_empty(),
        "status includes its recorded-observations advice"
    );
    for (index, item) in advice.iter().enumerate() {
        let item = item
            .as_object()
            .unwrap_or_else(|| panic!("status advice item {index} is an object: {item}"));
        for field_name in ["session_id", "queue_entry_id"] {
            let field = item.get(field_name).unwrap_or_else(|| {
                panic!("status advice item {index} includes {field_name}: {item:?}")
            });
            assert!(
                field.is_null() || field.as_i64().is_some(),
                "status advice item {index} field {field_name} is null or i64: {field}"
            );
        }
    }

    let mut shape = json_shape(value);
    let advice_shape = shape
        .get_mut("advice")
        .and_then(serde_json::Value::as_array_mut)
        .expect("status advice shape is an array");
    let first = advice_shape
        .first_mut()
        .expect("nonempty status advice has a shape");
    let first = first
        .as_object_mut()
        .expect("status advice shape item is an object");
    for field_name in ["session_id", "queue_entry_id"] {
        assert!(
            first.contains_key(field_name),
            "status shape includes {field_name}"
        );
        first.insert(
            field_name.to_string(),
            serde_json::Value::String("integer|null".into()),
        );
    }
    format!(
        "{}\n",
        serde_json::to_string_pretty(&shape).expect("serialize status shape")
    )
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    tmp
}

fn fixture_with_local_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let origin = tmp.path().join("origin.git");
    let host = tmp.path().join("host-state");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&origin).unwrap();
    std::fs::create_dir_all(&host).unwrap();
    git(&origin, &["init", "--bare", "-q", "-b", "main"]);
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join("src/app.txt"), "base\n").unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        "gate-runs.txt\n.aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n.aethyme/locks/\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    let host = host.canonicalize().expect("canonical host state");
    (tmp, repo, host)
}

#[test]
fn session_lifecycle_json_shapes_match_snapshots() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let mut entries = Vec::new();

    let started = json(
        &repo,
        &[
            "broker",
            "start",
            "--task",
            "shape fixture",
            "--short-name",
            "Shape",
            "--json",
        ],
    );
    entries.push(("start".to_string(), shape(&started)));
    let session = started["id"]
        .as_i64()
        .or_else(|| started["session"]["id"].as_i64())
        .unwrap_or_else(|| panic!("start --json carries a session id: {started}"));
    let worktree = PathBuf::from(
        started["worktree_path"]
            .as_str()
            .or_else(|| started["session"]["worktree_path"].as_str())
            .unwrap_or_else(|| panic!("start --json carries a worktree: {started}")),
    );
    let session_arg = session.to_string();

    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "done"]);

    let submitted = json(
        &worktree,
        &["broker", "submit", "--session", &session_arg, "--json"],
    );
    entries.push(("submit".to_string(), shape(&submitted)));

    let status = json(&repo, &["broker", "status", "--json"]);
    entries.push(("status".to_string(), status_shape(&status)));

    let finished = json(
        &repo,
        &["broker", "finish", "--session", &session_arg, "--json"],
    );
    entries.push(("finish".to_string(), shape(&finished)));

    let adopted_path = tmp.path().join("adopted");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted",
            adopted_path.to_str().unwrap(),
        ],
    );
    let adopted = json(
        &adopted_path,
        &[
            "broker",
            "start",
            "--adopt",
            "--task",
            "adopt fixture",
            "--short-name",
            "Adopt fixture",
            "--json",
        ],
    );
    entries.push(("adopt".to_string(), shape(&adopted)));

    let refusal = aethyme(
        &repo,
        &["broker", "advanced", "start", "--task", "refused", "--json"],
    );
    assert_eq!(refusal.status.code(), Some(2));
    assert!(
        refusal.stderr.is_empty(),
        "JSON refusal writes only stdout: {}",
        String::from_utf8_lossy(&refusal.stderr)
    );
    let refusal: serde_json::Value = serde_json::from_slice(&refusal.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&refusal.stdout)));
    assert_eq!(refusal["schema"], "aethyme.broker-error/v1");
    assert_eq!(refusal["error"]["code"], 2);
    entries.push(("error".to_string(), shape(&refusal)));

    assert_snapshots("json-shapes", &entries);
}

#[test]
fn other_broker_json_shapes_match_snapshots() {
    let (tmp, repo, host_state) = fixture_with_local_origin();
    let mut entries = Vec::new();
    let repo_text = repo.to_str().unwrap();

    let deploy_plan = json_with_host_state(
        &repo,
        &["deploy", "plan", "--repo", repo_text, "--json"],
        &host_state,
    );
    entries.push(("deploy-plan".to_string(), shape(&deploy_plan)));

    let alias_refusal = json_with_exit(
        &repo,
        &[
            "broker",
            "advanced",
            "start",
            "--task",
            "refused",
            "--json-output",
        ],
        &host_state,
        2,
    );
    entries.push(("error-json-output-alias".to_string(), shape(&alias_refusal)));

    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority = 'disabled'\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".aethyme/gates.toml"),
        "[[gate]]\nname = 'shape-gate'\ncommand = 'true'\ntriggers = ['src/**']\n",
    )
    .unwrap();
    git(
        &repo,
        &["add", ".aethyme/config.toml", ".aethyme/gates.toml"],
    );
    git(&repo, &["commit", "-qm", "configure fixture gate"]);
    git(&repo, &["push", "-q", "origin", "main"]);

    // `.cargo/config.toml` provides the test-only gate escape to this child.
    // The configured escape is effective only for this owned TempDir fixture;
    // the broker does not record human trust for a durable repository here.
    let started = json_with_host_state(
        &repo,
        &[
            "broker",
            "start",
            "--task",
            "shape fixture",
            "--short-name",
            "Shape",
            "--json",
        ],
        &host_state,
    );
    let session = started["id"]
        .as_i64()
        .or_else(|| started["session"]["id"].as_i64())
        .unwrap_or_else(|| panic!("start --json carries a session id: {started}"));
    let session_arg = session.to_string();
    let worktree = PathBuf::from(
        started["worktree_path"]
            .as_str()
            .or_else(|| started["session"]["worktree_path"].as_str())
            .unwrap_or_else(|| panic!("start --json carries a worktree: {started}")),
    );

    let reused = json_with_host_state(
        &worktree,
        &[
            "broker",
            "start",
            "--reuse",
            "--task",
            "reuse fixture",
            "--json",
        ],
        &host_state,
    );
    assert_eq!(
        reused["id"]
            .as_i64()
            .or_else(|| reused["session"]["id"].as_i64()),
        Some(session)
    );
    assert_eq!(reused["outcome"], "reused");
    entries.push(("start-reuse".to_string(), shape(&reused)));

    std::fs::write(worktree.join("src/app.txt"), "changed\n").unwrap();
    git(&worktree, &["add", "src/app.txt"]);
    git(&worktree, &["commit", "-qm", "change fixture source"]);

    let gates_affected = json_with_host_state(
        &worktree,
        &[
            "broker",
            "advanced",
            "gates",
            "affected",
            "--session",
            &session_arg,
            "--json",
        ],
        &host_state,
    );
    assert!(
        gates_affected
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "the tracked source change selects the fixture gate: {gates_affected}"
    );
    entries.push(("gates-affected".to_string(), shape(&gates_affected)));

    let gates_run = json_with_host_state(
        &worktree,
        &[
            "broker",
            "advanced",
            "gates",
            "run",
            "--session",
            &session_arg,
            "--no-cache",
            "--json",
        ],
        &host_state,
    );
    assert!(
        gates_run.as_array().is_some_and(|items| !items.is_empty()),
        "the fixture gate produces a JSON status row: {gates_run}"
    );
    entries.push(("gates-run".to_string(), shape(&gates_run)));

    let prepare = json_with_exit(
        &worktree,
        &[
            "broker",
            "submit",
            "prepare",
            "--session",
            &session_arg,
            "--json",
        ],
        &host_state,
        1,
    );
    entries.push(("submit-prepare-refusal".to_string(), shape(&prepare)));

    let submitted = json_with_host_state(
        &worktree,
        &["broker", "submit", "--session", &session_arg, "--json"],
        &host_state,
    );
    let entry = submitted["entry"]["id"]
        .as_i64()
        .or_else(|| submitted["entry_id"].as_i64())
        .unwrap_or_else(|| panic!("submit --json carries an entry id: {submitted}"));
    let entry_arg = entry.to_string();
    let ship_plan = json_with_host_state(
        &repo,
        &[
            "broker", "advanced", "ship", "plan", "--entry", &entry_arg, "--json",
        ],
        &host_state,
    );
    entries.push(("ship-plan".to_string(), shape(&ship_plan)));

    git(&repo, &["branch", "--unset-upstream", "main"]);
    git(&repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
    let push_no_upstream = json_with_exit(
        &repo,
        &["broker", "push", "--session", &session_arg, "--json"],
        &host_state,
        3,
    );
    entries.push((
        "push-no-upstream-refusal".to_string(),
        shape(&push_no_upstream),
    ));
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);

    let finished = json_with_host_state(
        &repo,
        &["broker", "finish", "--session", &session_arg, "--json"],
        &host_state,
    );
    assert_eq!(finished["status"], "cleaned");

    let adopted_path = tmp.path().join("adopted");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted",
            adopted_path.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = json_with_host_state(
        &adopted_path,
        &[
            "broker",
            "start",
            "--adopt",
            "--task",
            "adopted fixture",
            "--json",
        ],
        &host_state,
    );
    let adopted_session = adopted["id"].as_i64().unwrap();
    let adopted_arg = adopted_session.to_string();
    let push_adopted = json_with_exit(
        &adopted_path,
        &["broker", "push", "--session", &adopted_arg, "--json"],
        &host_state,
        3,
    );
    entries.push((
        "push-adopted-branch-refusal".to_string(),
        shape(&push_adopted),
    ));

    std::fs::write(adopted_path.join("unsubmitted.txt"), "unsubmitted\n").unwrap();
    git(&adopted_path, &["add", "unsubmitted.txt"]);
    git(
        &adopted_path,
        &["commit", "-qm", "unsubmitted fixture change"],
    );
    let blocked = json_with_report_exit(
        &adopted_path,
        &["broker", "finish", "--session", &adopted_arg, "--json"],
        &host_state,
        3,
    );
    assert_eq!(blocked["status"], "blocked");
    assert_eq!(blocked["closed"], false);
    assert_eq!(blocked["error"]["code"], 3);
    entries.push(("finish-blocked".to_string(), shape(&blocked)));

    let closed = json_with_host_state(
        &repo,
        &[
            "broker",
            "finish",
            "close",
            "--session",
            &adopted_arg,
            "--json",
        ],
        &host_state,
    );
    entries.push(("finish-close".to_string(), shape(&closed)));

    let cleanup_refusal = json_with_exit(
        &repo,
        &["broker", "finish", "cleanup", &adopted_arg, "--json"],
        &host_state,
        3,
    );
    entries.push((
        "finish-cleanup-refusal".to_string(),
        shape(&cleanup_refusal),
    ));

    let cleanup_all = json_with_host_state(
        &repo,
        &["broker", "finish", "cleanup", "--all-cleaned", "--json"],
        &host_state,
    );
    entries.push(("finish-cleanup-all".to_string(), shape(&cleanup_all)));

    let gc_usage = json_with_exit(&repo, &["broker", "gc", "--json"], &host_state, 1);
    entries.push(("gc-usage-refusal".to_string(), shape(&gc_usage)));
    let gc_plan = json_with_host_state(&repo, &["broker", "gc", "plan", "--json"], &host_state);
    entries.push(("gc-plan".to_string(), shape(&gc_plan)));
    let gc_plan_alias = json_with_host_state(
        &repo,
        &["broker", "gc", "plan", "--json-output"],
        &host_state,
    );
    entries.push((
        "gc-plan-json-output-alias".to_string(),
        shape(&gc_plan_alias),
    ));
    let gc_storage =
        json_with_host_state(&repo, &["broker", "gc", "storage", "--json"], &host_state);
    entries.push(("gc-storage".to_string(), shape(&gc_storage)));

    let hooks_usage = json_with_exit(
        &repo,
        &["broker", "advanced", "hooks", "--json"],
        &host_state,
        1,
    );
    entries.push(("hooks-usage-refusal".to_string(), shape(&hooks_usage)));
    let hooks = json_with_host_state(
        &repo,
        &["broker", "advanced", "hooks", "status", "--json"],
        &host_state,
    );
    assert!(
        hooks.as_array().is_some_and(|items| !items.is_empty()),
        "hooks status reports its managed hooks as an array: {hooks}"
    );
    entries.push(("hooks-status".to_string(), shape(&hooks)));

    assert_snapshots("json-command-shapes", &entries);
}
