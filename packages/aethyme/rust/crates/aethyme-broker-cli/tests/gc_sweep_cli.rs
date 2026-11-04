//! Host-level GC sweeps: orphaned worktree roots and reclaimable build caches.
//!
//! These run through the CLI shim so each case gets its own process and can set
//! `AETHYME_WORKTREE_ROOT` without mutating shared state in a threaded harness.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;
#[path = "common/json_error.rs"]
mod json_error;

fn git_output(repo: &Path, args: &[&str]) -> Output {
    git_output_with_env(
        repo,
        args,
        std::iter::empty::<(std::ffi::OsString, std::ffi::OsString)>(),
    )
}

fn git_output_with_env(
    repo: &Path,
    args: &[&str],
    extra_env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Output {
    let mut command = Command::new("git");
    command.envs(extra_env).args(args).current_dir(repo);
    clear_git_environment(&mut command);
    command
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn clear_git_environment(command: &mut Command) {
    let git_keys = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect::<Vec<_>>();
    for key in git_keys {
        command.env_remove(key);
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = git_output(repo, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_selector_overrides(repo: &Path) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vec![
        ("GIT_DIR".into(), repo.join(".git").into_os_string()),
        ("GIT_WORK_TREE".into(), repo.as_os_str().to_owned()),
        (
            "GIT_INDEX_FILE".into(),
            repo.join(".git/index").into_os_string(),
        ),
    ]
}

#[test]
fn fixture_git_commands_ignore_configured_git_selectors() {
    let fixture_repo = tempfile::tempdir().unwrap();
    let selected_repo = tempfile::tempdir().unwrap();
    for (repo, content) in [
        (fixture_repo.path(), "fixture before\n"),
        (selected_repo.path(), "selected before\n"),
    ] {
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("selector.txt"), content).unwrap();
        git(repo, &["add", "selector.txt"]);
        git(repo, &["commit", "-qm", "initial"]);
    }
    std::fs::write(fixture_repo.path().join("selector.txt"), "fixture staged\n").unwrap();
    std::fs::write(
        selected_repo.path().join("selector.txt"),
        "selected staged\n",
    )
    .unwrap();

    let added = git_output_with_env(
        fixture_repo.path(),
        &["add", "selector.txt"],
        git_selector_overrides(selected_repo.path()),
    );
    assert!(
        added.status.success(),
        "git add: {}",
        String::from_utf8_lossy(&added.stderr)
    );

    let fixture_index = git_output(fixture_repo.path(), &["show", ":selector.txt"]);
    assert!(fixture_index.status.success());
    assert_eq!(
        String::from_utf8_lossy(&fixture_index.stdout),
        "fixture staged\n"
    );
    let selected_index = git_output(selected_repo.path(), &["show", ":selector.txt"]);
    assert!(selected_index.status.success());
    assert_eq!(
        String::from_utf8_lossy(&selected_index.stdout),
        "selected before\n",
        "Git selectors from the caller must not redirect fixture writes"
    );
}

fn fixture(retention: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        repo.path().join(".gitignore"),
        "/.aethyme/\n/rust/target/\n/rust/build/\n",
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    let head = git_output(repo.path(), &["rev-parse", "HEAD"]);
    assert!(
        head.status.success(),
        "read fixture HEAD: {}",
        String::from_utf8_lossy(&head.stderr)
    );
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(repo.path(), &["config", "remote.origin.url", "."]);
    git(
        repo.path(),
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(repo.path(), &["config", "branch.main.remote", "origin"]);
    git(
        repo.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    // This suite tests artifact cleanup and reclaim, not whole-checkout removal.
    // Set that policy explicitly instead of relying on an absent remote ref to
    // keep otherwise eligible fixture worktrees around.
    std::fs::write(
        repo.path().join(".aethyme/config.toml"),
        "[cleanup]\nauto_remove = false\n",
    )
    .unwrap();
    std::fs::write(repo.path().join(".aethyme/broker.toml"), retention).unwrap();
    discover_artifact_sweep_namespace(repo.path(), container.path());
    (repo, container)
}

/// Discover the namespace from the same CLI environment used by every test
/// command. The empty fixture has no sweep candidates, so this bootstrap only
/// registers its captured storage context; its per-context state is then
/// removed to leave the fixture as an otherwise fresh sweep database.
fn discover_artifact_sweep_namespace(repo: &Path, container: &Path) {
    let config = repo.join(".aethyme/broker.toml");
    let original = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();
    let output = run(repo, container, &["gc", "sweep", "--json"]);
    assert!(
        output.status.success(),
        "fixture namespace discovery: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let registry = raw_meta(repo, "gc.artifact_sweep.v2.context_registry")
        .expect("the CLI sweep registers its captured storage context");
    let namespaces: Vec<String> = serde_json::from_str(&registry).unwrap();
    assert_eq!(
        namespaces.len(),
        1,
        "a new CLI fixture must register exactly its one captured namespace"
    );
    let namespace = &namespaces[0];
    assert!(
        namespace.len() == 64
            && namespace
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "the fixture registry contains one canonical namespace"
    );
    let prefix = format!("gc.artifact_sweep.v2.{namespace}.");
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .execute(
            "DELETE FROM meta WHERE substr(key, 1, ?1) = ?2",
            rusqlite::params![prefix.len() as i64, prefix],
        )
        .unwrap();
    std::fs::write(config, original).unwrap();
}

fn run(repo: &Path, container: &Path, args: &[&str]) -> Output {
    // `gc reclaim` keeps worktrees whose Git index changed within the last
    // three hours. These fixtures model sessions whose agents left long ago,
    // so their indexes are aged first; `run_as_is` exercises the guard itself.
    if args.starts_with(&["gc", "reclaim"]) {
        age_worktree_indexes(repo);
    }
    run_as_is(repo, container, args)
}

fn run_as_is(repo: &Path, container: &Path, args: &[&str]) -> Output {
    run_from_fixture(repo, repo, container, args)
}

fn command_from_fixture(owner: &Path, cwd: &Path, container: &Path, args: &[&str]) -> Command {
    command_from_fixture_with_env(
        owner,
        cwd,
        container,
        args,
        std::iter::empty::<(std::ffi::OsString, std::ffi::OsString)>(),
    )
}

fn command_from_fixture_with_env(
    owner: &Path,
    cwd: &Path,
    container: &Path,
    args: &[&str],
    extra_env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Command {
    let owner = owner.canonicalize().expect("canonicalize fixture owner");
    let cwd = cwd
        .canonicalize()
        .expect("canonicalize fixture working tree");
    let container = container
        .canonicalize()
        .expect("canonicalize fixture worktree root");
    let mut command = common::broker_cli(CLI, args);
    command
        .envs(extra_env)
        .current_dir(cwd)
        .env("AETHYME_WORKTREE_ROOT", &container)
        .env(
            "AETHYME_HOST_STATE_DIR",
            owner.join(".git/aethyme-test-state"),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            owner.join(".git/aethyme-test-cache"),
        );
    clear_git_environment(&mut command);
    command
}

fn run_from_fixture(owner: &Path, cwd: &Path, container: &Path, args: &[&str]) -> Output {
    command_from_fixture(owner, cwd, container, args)
        .output()
        .unwrap()
}

fn assert_reclaim_usage_refusal_before_state_creation(args: &[&str], expected: &str) {
    let owner = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    let output = command_from_fixture_with_env(
        owner.path(),
        owner.path(),
        container.path(),
        args,
        [("AETHYME_MEASURE_OUTPUT".into(), "0".into())],
    )
    .output()
    .unwrap();
    assert!(!output.status.success(), "unexpected success: {output:?}");
    assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");
    let message = json_error::message(&output);
    assert!(message.contains(expected), "{message}");

    assert!(
        !owner.path().join(".aethyme").exists(),
        "reclaim selector refusal must precede repository database creation"
    );
    assert!(
        !owner.path().join(".git/aethyme-test-state").exists(),
        "reclaim selector refusal must precede host-state snapshot creation"
    );
    assert!(
        !owner.path().join(".git/aethyme-test-cache").exists(),
        "reclaim selector refusal must precede host-cache creation"
    );
    assert!(
        std::fs::read_dir(container.path())
            .unwrap()
            .next()
            .is_none(),
        "reclaim selector refusal must precede root scanning or review-snapshot writes"
    );
}

#[test]
fn invalid_reclaim_selectors_are_refused_before_opening_state_or_scanning_the_root() {
    assert_reclaim_usage_refusal_before_state_creation(
        &["gc", "reclaim", "purge", "--json"],
        "unknown reclaim action",
    );
    assert_reclaim_usage_refusal_before_state_creation(
        &["gc", "reclaim", "apply", "--json"],
        "requires --confirm",
    );
    assert_reclaim_usage_refusal_before_state_creation(
        &["gc", "reclaim", "plan", "extra", "--json"],
        "exactly one action",
    );

    let malformed_digest = format!("{}x", "a".repeat(63));
    assert_reclaim_usage_refusal_before_state_creation(
        &[
            "gc",
            "reclaim",
            "apply",
            "--confirm",
            malformed_digest.as_str(),
            "--json",
        ],
        "64-character hexadecimal SHA-256",
    );
}

/// Date every linked worktree's index four hours back.
fn age_worktree_indexes(repo: &Path) {
    let Ok(entries) = std::fs::read_dir(repo.join(".git/worktrees")) else {
        return;
    };
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(4 * 3600);
    for entry in entries.flatten() {
        let index = entry.path().join("index");
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&index) {
            file.set_modified(then).unwrap();
        }
    }
}

#[test]
fn gc_abandon_without_a_journal_refuses_without_creating_an_archive() {
    let (repo, container) = fixture("");
    let digest = "a".repeat(64);
    let output = run(
        repo.path(),
        container.path(),
        &["gc", "abandon", "--confirm", digest.as_str()],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no pending journal exists"),
        "unexpected refusal: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!repo.path().join(".aethyme/gc-journal.json").exists());
    assert!(!repo.path().join(".aethyme/gc-journal-archives").exists());

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "forget-abandoned", "--confirm", digest.as_str()],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no matching abandoned journal exists"),
        "unexpected refusal: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!repo.path().join(".aethyme/gc-journal-archives").exists());
}

fn plan_json(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["gc", "plan", "--json"]);
    assert!(
        output.status.success(),
        "gc plan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn full_scan_plan_json(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["gc", "plan", "--full-scan", "--json"]);
    assert!(
        output.status.success(),
        "gc plan --full-scan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn reclaim_plan_json(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["gc", "reclaim", "plan", "--json"]);
    assert!(
        output.status.success(),
        "reclaim plan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn legacy_unbound_retained_worktree_fixture(repo: &Path, container: &Path) -> (PathBuf, i64) {
    let worktree = container.join("legacy-retained");
    let worktree_arg = worktree.to_string_lossy().into_owned();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "legacy-retained-session",
            &worktree_arg,
            "HEAD",
        ],
    );

    let database = repo.join(".aethyme/broker.db");
    let existing = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        existing
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the disposable bootstrap database must not contain session records"
    );
    let bootstrap_binding: (String, Option<Vec<u8>>) = existing
        .query_row(
            "SELECT state, worktree_root FROM repository_worktree_binding",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        bootstrap_binding,
        ("fresh_unbound".to_owned(), None),
        "the disposable bootstrap row must not carry root authority"
    );
    drop(existing);
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", database.display()));
        if sidecar.exists() {
            std::fs::remove_file(sidecar).unwrap();
        }
    }
    std::fs::remove_file(&database).unwrap();
    let repository_uuid_marker = repo.join(".git/aethyme-repository-uuid");
    if repository_uuid_marker.exists() {
        std::fs::remove_file(repository_uuid_marker).unwrap();
    }

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(include_str!("fixtures/v028-schema52.sql"))
        .unwrap();
    let head = git_output(repo, &["rev-parse", "HEAD"]);
    assert!(
        head.status.success(),
        "read fixture HEAD: {}",
        String::from_utf8_lossy(&head.stderr)
    );
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let worktree_path = worktree.canonicalize().unwrap();
    let worktree_path_text = worktree_path.to_string_lossy().into_owned();
    let now = epoch_ms();
    connection
        .execute(
            "INSERT INTO sessions (
                 worktree_path, branch, origin, status, task, diff_base,
                 adoption_base, adopted_head, repository_schema,
                 deployment_state_digest, aethyme_version, gate_definition_digest,
                 repository_contract_backfilled, pid, command, log_path, exit_code,
                 agent_identity, repository_name, tab_name, ai_provider, short_name,
                 created_at, updated_at, last_activity_at
             ) VALUES (
                 ?1, 'legacy-retained-session', 'spawned', 'exited',
                 'legacy retained session', ?2, ?2, ?2, NULL,
                 'a332ca058fadabb85cc900a167a99df743dd0aa2db949edee7ad064a3ada8343',
                 '0.8.28', NULL, 0, NULL, NULL, NULL, 0, NULL, NULL, NULL, NULL,
                 'legacy retained fixture', ?3, ?3, ?3
             )",
            rusqlite::params![worktree_path_text, head, now],
        )
        .unwrap();
    let session_id = connection.last_insert_rowid();
    let registered_payload = serde_json::json!({
        "origin": "spawned",
        "branch": "legacy-retained-session",
        "worktree_path": worktree_path,
    })
    .to_string();
    connection
        .execute(
            "INSERT INTO events (schema_version, ts, kind, session_id, payload_json)
             VALUES (1, ?1, 'session.registered', ?2, ?3)",
            rusqlite::params![now, session_id, registered_payload],
        )
        .unwrap();
    drop(connection);

    (worktree, session_id)
}

#[derive(Debug, Eq, PartialEq)]
struct LegacySessionSnapshot {
    worktree_path: String,
    branch: String,
    task: Option<String>,
    diff_base: Option<String>,
    adoption_base: Option<String>,
    adopted_head: Option<String>,
    status: String,
    exit_code: Option<i64>,
    updated_at: i64,
    last_activity_at: i64,
}

fn legacy_session_row(repo: &Path, session_id: i64) -> LegacySessionSnapshot {
    let connection = rusqlite::Connection::open_with_flags(
        repo.join(".aethyme/broker.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    connection
        .query_row(
            "SELECT worktree_path, branch, task, diff_base, adoption_base, adopted_head,
                    status, exit_code, updated_at, last_activity_at
             FROM sessions WHERE id = ?1",
            [session_id],
            |row| {
                Ok(LegacySessionSnapshot {
                    worktree_path: row.get(0)?,
                    branch: row.get(1)?,
                    task: row.get(2)?,
                    diff_base: row.get(3)?,
                    adoption_base: row.get(4)?,
                    adopted_head: row.get(5)?,
                    status: row.get(6)?,
                    exit_code: row.get(7)?,
                    updated_at: row.get(8)?,
                    last_activity_at: row.get(9)?,
                })
            },
        )
        .unwrap()
}

#[test]
fn reclaim_plan_warns_but_prints_when_snapshot_storage_is_unavailable() {
    let (repo, container) = fixture("");
    let started = run(
        repo.path(),
        container.path(),
        &["start", "--task", "bind reclaim test root", "--json"],
    );
    assert!(
        started.status.success(),
        "start: stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let root_output = run(
        repo.path(),
        container.path(),
        &["advanced", "worktree-root", "--json"],
    );
    assert!(root_output.status.success());
    let worktree_root =
        serde_json::from_slice::<serde_json::Value>(&root_output.stdout).unwrap()["preferred_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap();

    let first = reclaim_plan_json(repo.path(), container.path());
    let digest = first["digest"].as_str().unwrap();
    let snapshot = worktree_root.join(format!(".aethyme-reclaim-plan-{digest}.json"));
    std::fs::remove_file(&snapshot).unwrap();
    std::fs::create_dir(&snapshot).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json"],
    );
    assert!(
        output.status.success(),
        "reclaim plan should remain available: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["digest"], digest);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("continuing with the digest-bound plan"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reclaim_refuses_legacy_unbound_root_before_planning_or_apply() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (worktree, session_id) =
        legacy_unbound_retained_worktree_fixture(repo.path(), container.path());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    let artifact = target.join("legacy-cache.bin");
    std::fs::write(&artifact, b"retained legacy worktree cache\n").unwrap();
    age_worktree_indexes(repo.path());

    let root_marker = container.path().join(".aethyme-worktree-root.json");
    assert!(
        !root_marker.exists(),
        "the legacy root is not durably bound"
    );
    let has_reclaim_snapshot = || {
        std::fs::read_dir(container.path())
            .unwrap()
            .map(|entry| entry.unwrap())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aethyme-reclaim-plan-")
            })
    };
    assert!(!has_reclaim_snapshot());
    let session_before = legacy_session_row(repo.path(), session_id);
    assert_eq!(session_before.status, "exited");

    let plan = run_as_is(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json"],
    );
    let plan_diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        !plan.status.success(),
        "legacy plan unexpectedly succeeded: {plan_diagnostic}"
    );
    assert!(
        plan_diagnostic
            .contains("legacy root requires explicit digest-confirmed binding plan/apply"),
        "unexpected refusal: {plan_diagnostic}"
    );
    assert!(artifact.is_file());
    assert_eq!(
        std::fs::read(&artifact).unwrap(),
        b"retained legacy worktree cache\n"
    );
    assert_eq!(legacy_session_row(repo.path(), session_id), session_before);
    assert!(
        !has_reclaim_snapshot(),
        "a refused plan must not write a review snapshot"
    );
    assert!(
        !root_marker.exists(),
        "a refused plan must not create a root marker"
    );

    let confirmation = "a".repeat(64);
    let apply = run_as_is(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &confirmation],
    );
    let apply_diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(
        !apply.status.success(),
        "legacy apply unexpectedly succeeded: {apply_diagnostic}"
    );
    assert!(
        apply_diagnostic
            .contains("legacy root requires explicit digest-confirmed binding plan/apply"),
        "unexpected refusal: {apply_diagnostic}"
    );
    assert!(artifact.is_file());
    assert_eq!(
        std::fs::read(&artifact).unwrap(),
        b"retained legacy worktree cache\n"
    );
    assert_eq!(legacy_session_row(repo.path(), session_id), session_before);
    assert!(
        !has_reclaim_snapshot(),
        "a refused apply must not write a review snapshot"
    );
    assert!(
        !root_marker.exists(),
        "a refused apply must not create a root marker"
    );
}

#[test]
fn session_scoped_reclaim_refuses_out_of_root_sessions_with_typed_json_error() {
    let (repo, container) = fixture("");
    let started = run_as_is(
        repo.path(),
        container.path(),
        &["start", "--task", "legacy reclaim scope", "--json"],
    );
    assert!(
        started.status.success(),
        "start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let session_id = session["id"].as_i64().unwrap();
    let session_id_text = session_id.to_string();
    let original_worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());

    let finished = run_as_is(
        repo.path(),
        container.path(),
        &["finish", "--session", &session_id_text, "--keep-worktree"],
    );
    assert!(
        finished.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&finished.stderr)
    );

    let legacy_root = tempfile::tempdir().unwrap();
    let legacy_worktree = legacy_root.path().join("legacy-session");
    let original_arg = original_worktree.to_string_lossy().into_owned();
    let legacy_arg = legacy_worktree.to_string_lossy().into_owned();
    git(
        repo.path(),
        &["worktree", "move", &original_arg, &legacy_arg],
    );
    let target = legacy_worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    let artifact = target.join("legacy-cache.bin");
    std::fs::write(&artifact, b"preserve this legacy cache\n").unwrap();
    let legacy_path = legacy_worktree.canonicalize().unwrap();
    let legacy_path_text = legacy_path.to_string_lossy().into_owned();
    rusqlite::Connection::open(repo.path().join(".aethyme/broker.db"))
        .unwrap()
        .execute(
            "UPDATE sessions SET worktree_path = ?2 WHERE id = ?1",
            rusqlite::params![session_id, legacy_path_text],
        )
        .unwrap();

    let read_root_plan = || {
        let output = run_as_is(
            repo.path(),
            container.path(),
            &["advanced", "worktree-root", "--json"],
        );
        assert!(
            output.status.success(),
            "worktree-root: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let root_identity = |plan: &serde_json::Value| {
        (
            plan["repository_root"].as_str().unwrap().to_owned(),
            plan["repository_key"].as_str().unwrap().to_owned(),
            plan["preferred_root"].as_str().map(str::to_owned),
            plan["preferred_source"].as_str().map(str::to_owned),
            plan["root_container"].as_str().map(str::to_owned),
        )
    };
    let binding_before = read_root_plan();
    let binding_identity_before = root_identity(&binding_before);
    let bound_root = original_worktree.parent().unwrap().canonicalize().unwrap();
    assert_eq!(
        PathBuf::from(binding_before["preferred_root"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        bound_root,
        "the root plan matches the root that held the started worktree"
    );
    let has_reclaim_snapshot = || {
        std::fs::read_dir(&bound_root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aethyme-reclaim-plan-")
            })
    };
    assert!(!has_reclaim_snapshot());

    let plan = run_as_is(
        repo.path(),
        container.path(),
        &[
            "gc",
            "reclaim",
            "plan",
            "--session",
            &session_id_text,
            "--json",
        ],
    );
    assert_eq!(plan.status.code(), Some(3), "{plan:?}");
    let plan_message = json_error::message(&plan);
    assert!(
        plan_message.contains("not an available direct child"),
        "{plan_message}"
    );
    assert!(
        plan_message.contains("bound worktree root"),
        "{plan_message}"
    );
    assert!(
        plan_message.contains("legacy-root reclamation is unsupported"),
        "{plan_message}"
    );

    let confirmation = "a".repeat(64);
    let apply = run_as_is(
        repo.path(),
        container.path(),
        &[
            "gc",
            "reclaim",
            "apply",
            "--confirm",
            &confirmation,
            "--session",
            &session_id_text,
            "--json",
        ],
    );
    assert_eq!(apply.status.code(), Some(3), "{apply:?}");
    let apply_message = json_error::message(&apply);
    assert_eq!(apply_message, plan_message);

    assert_eq!(
        std::fs::read(&artifact).unwrap(),
        b"preserve this legacy cache\n"
    );
    assert_eq!(root_identity(&read_root_plan()), binding_identity_before);
    assert!(
        !has_reclaim_snapshot(),
        "unsupported scope must not create a plan"
    );
}

/// A worktree root left behind by a repository that no longer exists.
fn stamp_root(container: &Path, key: &str, repository_root: &Path) -> PathBuf {
    let root = container.join(key);
    std::fs::create_dir_all(root.join("some-session")).unwrap();
    std::fs::write(root.join("some-session/leftover.txt"), "bytes\n").unwrap();
    std::fs::write(
        root.join(".aethyme-worktree-root.json"),
        serde_json::json!({
            "schema_version": 1,
            "repository_key": key,
            "repository_root": repository_root,
        })
        .to_string(),
    )
    .unwrap();
    root
}

/// Obtain saved filesystem evidence through real broker admission while the
/// origin exists; removing the owned origin then creates a genuine orphan.
fn registered_orphan(repo: &Path, container: &Path, origin: &Path) -> PathBuf {
    assert!(
        origin.starts_with(container),
        "origin must be fixture-owned"
    );
    assert!(!origin.exists());
    std::fs::create_dir_all(origin).unwrap();
    git(origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("README.md"), "registered fixture\n").unwrap();
    std::fs::write(origin.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(origin, &["add", "-A"]);
    git(origin, &["commit", "-qm", "init"]);
    std::fs::create_dir(origin.join(".aethyme")).unwrap();
    std::fs::write(
        origin.join(".aethyme/broker.toml"),
        "[retention]\nartifact_sweep_budget_ms = 0\n",
    )
    .unwrap();
    let started = run_from_fixture(
        repo,
        origin,
        container,
        &["start", "--task", "registered orphan", "--json"],
    );
    assert!(
        started.status.success(),
        "{}{}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let root = worktree.parent().unwrap().to_path_buf();
    assert!(root.starts_with(container.canonicalize().unwrap()));
    assert!(root.join(".aethyme-worktree-root.json").is_file());
    std::fs::create_dir(root.join("some-session")).unwrap();
    std::fs::write(root.join("some-session/leftover.txt"), "registered bytes\n").unwrap();
    std::fs::remove_dir_all(origin).unwrap();
    root
}

#[test]
fn orphaned_roots_are_swept_while_owned_roots_are_protected() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    let orphan = registered_orphan(repo.path(), container.path(), &missing);
    let owned = stamp_root(container.path(), "repo-owned", repo.path());

    let plan = full_scan_plan_json(repo.path(), container.path());
    let keys: Vec<&str> = plan["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|orphan| orphan["repository_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec![orphan.file_name().unwrap().to_str().unwrap()]);
    assert!(plan["orphans"][0]["estimated_bytes"].as_u64().unwrap() > 0);
    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(
        digest.len(),
        64,
        "complete inventory must be applicable: {plan:#}"
    );

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", digest, "--full-scan"],
    );
    assert!(
        output.status.success(),
        "gc apply: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !orphan.exists(),
        "the registered orphan should be reclaimed"
    );
    assert!(owned.exists(), "a root with a live owner must survive");
}

#[test]
fn unmarked_worktree_root_defers_orphan_removal() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    let orphan = registered_orphan(repo.path(), container.path(), &missing);
    let unmarked = container.path().join("repo-unmarked");
    std::fs::create_dir_all(&unmarked).unwrap();
    std::fs::write(unmarked.join("stray.txt"), "bytes\n").unwrap();

    let plan = full_scan_plan_json(repo.path(), container.path());
    assert_eq!(plan["digest"].as_str(), Some(""), "{plan:#}");
    assert!(
        plan["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check == "orphan_scan"),
        "unmarked roots must leave orphan discovery incomplete: {plan:#}"
    );
    assert!(
        plan["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["kind"] == "unmarked_worktree_root"),
        "a root with no breadcrumb must be reported, never removed blind"
    );

    let unissued_confirmation = "a".repeat(64);
    let output = run(
        repo.path(),
        container.path(),
        &[
            "gc",
            "apply",
            "--confirm",
            &unissued_confirmation,
            "--full-scan",
        ],
    );
    assert!(
        !output.status.success(),
        "GC must refuse apply when orphan discovery is incomplete"
    );
    assert!(
        orphan.exists(),
        "an incomplete plan must not remove a registered orphan"
    );
    let orphan_payload = std::fs::read(orphan.join("some-session/leftover.txt")).unwrap();
    assert_eq!(orphan_payload.as_slice(), b"registered bytes\n");
    assert!(unmarked.exists(), "an unmarked root must remain protected");
    let stray_payload = std::fs::read(unmarked.join("stray.txt")).unwrap();
    assert_eq!(stray_payload.as_slice(), b"bytes\n");
}

#[test]
fn reclaim_confirmation_binds_decisions_not_sizes_and_explains_changes() {
    let (repo, container) = fixture("");
    // Reclaim takes only Git-ignored output from worktrees a session records,
    // so the fixture is a real session closed with its checkout kept.
    let started = run(
        repo.path(),
        container.path(),
        &["start", "--task", "reclaim fixture", "--json"],
    );
    assert!(
        started.status.success(),
        "start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let id = session["id"].as_i64().unwrap().to_string();
    let finished = run(
        repo.path(),
        container.path(),
        &["finish", "--session", &id, "--keep-worktree"],
    );
    assert!(
        finished.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("artifact"), "small\n").unwrap();

    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();

    // The reviewed deletion decision is unchanged even though the build output
    // grew after the plan was printed.
    std::fs::write(target.join("artifact"), "larger build output\n").unwrap();
    let apply = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest],
    );
    assert!(
        apply.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(!target.exists(), "the reviewed candidate should be removed");

    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("artifact"), "small\n").unwrap();
    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    let added = worktree.join("rust/build");
    std::fs::create_dir_all(&added).unwrap();
    std::fs::write(added.join("artifact"), "new\n").unwrap();

    // A candidate that appeared after review changes the digest, but it only
    // withholds itself: the reviewed path goes, the new one waits for a plan.
    let applied = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest, "--json"],
    );
    assert!(
        applied.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(!target.exists(), "the reviewed candidate should be removed");
    assert!(
        added.exists(),
        "an unreviewed candidate must not be removed"
    );
    let not_reviewed = outcome["not_reviewed"].as_array().unwrap();
    assert_eq!(not_reviewed.len(), 1, "{outcome}");
    assert!(
        not_reviewed[0].as_str().unwrap().ends_with("rust/build"),
        "{outcome}"
    );
}

/// A closed session with its checkout kept and Git-ignored build output.
fn closed_session_with_target(repo: &Path, container: &Path, task: &str) -> (PathBuf, String) {
    let started = run(repo, container, &["start", "--task", task, "--json"]);
    assert!(
        started.status.success(),
        "start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let id = session["id"].as_i64().unwrap().to_string();
    let finished = run(
        repo,
        container,
        &["finish", "--session", &id, "--keep-worktree"],
    );
    assert!(
        finished.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("artifact"), "output\n").unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    (worktree, id)
}

/// A quiet session is not an idle worktree: someone who staged or committed
/// in it within the last three hours keeps its build output.
#[test]
fn a_worktree_worked_in_recently_keeps_its_build_output() {
    let (repo, container) = fixture("");
    let _ = closed_session_with_target(repo.path(), container.path(), "recent");
    let planned = run_as_is(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json"],
    );
    assert!(
        planned.status.success(),
        "reclaim plan: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    let candidate = plan["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| {
            candidate["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("rust/target"))
        })
        .unwrap_or_else(|| panic!("no rust/target candidate: {plan}"));
    assert_eq!(candidate["reclaimable"], false, "{candidate}");
    assert!(
        candidate["reason"]
            .as_str()
            .unwrap()
            .contains("Git index changed"),
        "{candidate}"
    );
    assert_eq!(plan["reclaimable_bytes"], 0, "{plan}");

    // The same worktree, idle for hours, is reclaimable.
    assert!(
        reclaim_plan_json(repo.path(), container.path())["reclaimable_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
}

/// Another agent's checkout gaining build output after review used to void
/// every reviewed deletion.
#[test]
fn a_kept_candidate_appearing_after_review_does_not_block_apply() {
    let (repo, container) = fixture("");
    let (worktree, _) = closed_session_with_target(repo.path(), container.path(), "reviewed");
    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();

    // No session records this directory, so its output is a kept candidate.
    let stray = worktree.parent().unwrap().join("stray/target");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("artifact"), "someone else's\n").unwrap();
    assert_ne!(
        reclaim_plan_json(repo.path(), container.path())["digest"],
        plan["digest"],
        "the fixture must actually change the digest"
    );

    let applied = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest],
    );
    assert!(
        applied.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!worktree.join("rust/target").exists());
    assert!(stray.exists(), "a kept candidate is never removed");
}

/// The review authorizes; whether the path may go now is decided afresh.
#[test]
fn a_reviewed_worktree_that_became_active_is_skipped() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (worktree, _) = closed_session_with_target(repo.path(), container.path(), "resumed");
    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    assert!(worktree.join("rust/target").exists());

    let adopted = run_from_fixture(
        repo.path(),
        &worktree,
        container.path(),
        &["start", "--adopt", "--task", "resume work", "--json"],
    );
    assert!(
        adopted.status.success(),
        "adopt: {}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    assert!(worktree.join("rust/target").exists());

    let applied = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest, "--json"],
    );
    assert!(
        applied.status.success(),
        "reclaim apply: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(
        worktree.join("rust/target").exists(),
        "an active session's build output must survive"
    );
    let skipped = outcome["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{outcome}");
    assert!(
        skipped[0].as_str().unwrap().contains("now kept"),
        "{outcome}"
    );
}

/// A failed transition in a read-only broker must not discard other active
/// sessions from the reclaim plan.
#[test]
fn reclaim_keeps_active_cache_when_another_session_needs_a_liveness_transition() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (active_id, active_worktree) = quiet_open_session(repo.path(), container.path(), 0);
    let (transition_id, transition_worktree) =
        quiet_open_session(repo.path(), container.path(), 24);

    // Give one row fresh liveness and another a deterministic transition from
    // stored Active to derived Idle/Stale. `run()` ages every Git index before
    // planning, so the cache decisions cannot use the recent-index guard.
    let fresh_at = epoch_ms();
    let stale_at = fresh_at - 24 * HOUR_MS;
    let db = rusqlite::Connection::open(repo.path().join(".aethyme/broker.db")).unwrap();
    db.execute(
        "UPDATE sessions SET status = 'active', last_activity_at = ?2, created_at = ?2 WHERE id = ?1",
        rusqlite::params![active_id, fresh_at],
    )
    .unwrap();
    db.execute(
        "UPDATE sessions SET status = 'active', last_activity_at = ?2, created_at = ?2 WHERE id = ?1",
        rusqlite::params![transition_id, stale_at],
    )
    .unwrap();
    drop(db);

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json"],
    );
    assert!(
        output.status.success(),
        "reclaim plan: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("process-use inspection is incomplete"),
        "the process-use snapshot must be complete for this regression: {stderr}"
    );

    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let candidates = plan["candidates"].as_array().unwrap();
    let candidate_for = |target: &Path| {
        let expected = target.canonicalize().unwrap();
        candidates
            .iter()
            .find(|candidate| {
                candidate["path"]
                    .as_str()
                    .and_then(|path| Path::new(path).canonicalize().ok())
                    .is_some_and(|path| path == expected)
            })
            .cloned()
            .unwrap_or_else(|| panic!("missing reclaim candidate for {}: {plan}", target.display()))
    };
    let active_candidate = candidate_for(&active_worktree.join("rust/target"));
    assert_eq!(
        active_candidate["reclaimable"], false,
        "a fresh active session must retain its cache: {active_candidate}"
    );
    assert!(
        active_candidate["reason"]
            .as_str()
            .unwrap()
            .contains("still active"),
        "the active-session guard, not another fence, must retain the cache: {active_candidate}"
    );

    let transition_candidate = candidate_for(&transition_worktree.join("rust/target"));
    assert_eq!(
        transition_candidate["reclaimable"], true,
        "the positive control proves old indexes and complete process-use inspection: {transition_candidate}"
    );
    assert_eq!(session_status(repo.path(), active_id), "active");
    assert_eq!(
        session_status(repo.path(), transition_id),
        "active",
        "read-only reclaim planning must not persist the derived Idle/Stale transition"
    );
}

/// Without a saved review there is nothing to narrow to.
#[test]
fn a_missing_review_still_requires_the_exact_digest() {
    let (repo, container) = fixture("");
    let (worktree, _) = closed_session_with_target(repo.path(), container.path(), "missing");
    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    let root = worktree.parent().unwrap();
    std::fs::remove_file(root.join(format!(".aethyme-reclaim-plan-{digest}.json"))).unwrap();
    std::fs::create_dir_all(root.join("stray/target")).unwrap();
    std::fs::write(root.join("stray/target/artifact"), "x\n").unwrap();

    let refused = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.contains("saved review is unavailable"), "{message}");
    assert!(worktree.join("rust/target").exists());
}

/// A review edited to name more paths no longer hashes to the confirmed
/// digest, so it authorizes nothing.
#[test]
fn a_tampered_review_authorizes_nothing() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (worktree, id) = closed_session_with_target(repo.path(), container.path(), "tampered");
    let (other, _) = closed_session_with_target(repo.path(), container.path(), "not reviewed");
    let scoped = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json", "--session", &id],
    );
    assert!(scoped.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&scoped.stdout).unwrap();
    let digest = plan["digest"].as_str().unwrap().to_string();
    let snapshot = worktree
        .parent()
        .unwrap()
        .join(format!(".aethyme-reclaim-plan-{digest}.json"));
    // The unscoped plan spells the other path exactly as the scan does.
    let other_target = reclaim_plan_json(repo.path(), container.path())["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["path"].as_str().unwrap().to_owned())
        .find(|path| path.contains("not-reviewed"))
        .expect("the unreviewed session's output is a candidate");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    // Kept in sorted order, so the digest is the only check that can notice.
    let decisions = saved["decisions"].as_array_mut().unwrap();
    decisions.push(serde_json::json!({ "path": other_target, "reclaimable": true }));
    decisions.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap()
            .cmp(right["path"].as_str().unwrap())
    });
    std::fs::write(&snapshot, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert!(worktree.join("rust/target").exists());
    assert!(other.join("rust/target").exists());

    let refused = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("saved review could not be read"),
        "{message}"
    );
    assert!(worktree.join("rust/target").exists());
    assert!(other.join("rust/target").exists());
}

/// One worktree can be reviewed and applied without the others holding still.
#[test]
fn a_session_scoped_plan_reviews_and_removes_one_worktree() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (first, first_id) = closed_session_with_target(repo.path(), container.path(), "first");
    let (second, _) = closed_session_with_target(repo.path(), container.path(), "second");
    let scoped = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json", "--session", &first_id],
    );
    assert!(
        scoped.status.success(),
        "scoped plan: {}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&scoped.stdout).unwrap();
    let candidates = plan["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{plan}");
    let digest = plan["digest"].as_str().unwrap().to_string();
    assert!(first.join("rust/target").exists());
    assert!(second.join("rust/target").exists());

    let applied = run(
        repo.path(),
        container.path(),
        &[
            "gc",
            "reclaim",
            "apply",
            "--confirm",
            &digest,
            "--session",
            &first_id,
        ],
    );
    assert!(
        applied.status.success(),
        "scoped apply: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!first.join("rust/target").exists());
    assert!(second.join("rust/target").exists(), "outside the scope");
}

/// A review covers what it scanned. A one-session plan never looked at the
/// rest of the root, and a whole-root plan was not reviewed as being about one
/// session, so neither confirmation is accepted for the other scope.
#[test]
fn a_reclaim_confirmation_is_refused_for_a_different_scope() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (first, first_id) = closed_session_with_target(repo.path(), container.path(), "first");
    let (second, _) = closed_session_with_target(repo.path(), container.path(), "second");
    let scoped = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json", "--session", &first_id],
    );
    assert!(scoped.status.success());
    let scoped: serde_json::Value = serde_json::from_slice(&scoped.stdout).unwrap();
    let scoped_digest = scoped["digest"].as_str().unwrap().to_string();
    assert!(
        scoped["scope"].as_str().unwrap().ends_with("first"),
        "{scoped}"
    );
    let whole = reclaim_plan_json(repo.path(), container.path());
    let whole_digest = whole["digest"].as_str().unwrap().to_string();
    assert!(whole.get("scope").is_none(), "{whole}");
    assert!(first.join("rust/target").exists());
    assert!(second.join("rust/target").exists());

    let unscoped_apply = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &scoped_digest],
    );
    assert!(!unscoped_apply.status.success());
    let message = String::from_utf8_lossy(&unscoped_apply.stderr);
    assert!(
        message.contains("but this apply covers the whole worktree root"),
        "{message}"
    );

    let scoped_apply = run(
        repo.path(),
        container.path(),
        &[
            "gc",
            "reclaim",
            "apply",
            "--confirm",
            &whole_digest,
            "--session",
            &first_id,
        ],
    );
    assert!(!scoped_apply.status.success());
    let message = String::from_utf8_lossy(&scoped_apply.stderr);
    assert!(
        message.contains("reviewed the whole worktree root"),
        "{message}"
    );

    assert!(first.join("rust/target").exists());
    assert!(second.join("rust/target").exists());
}

/// A session whose worktree is already gone has nothing to reclaim; that is
/// an answer, not a failure, so a loop over sessions keeps going.
#[test]
fn a_session_plan_for_a_removed_worktree_has_nothing_to_reclaim() {
    let (repo, container) = fixture("");
    let (worktree, id) = closed_session_with_target(repo.path(), container.path(), "removed");
    std::fs::remove_dir_all(&worktree).unwrap();

    let planned = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "plan", "--json", "--session", &id],
    );
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert!(plan["candidates"].as_array().unwrap().is_empty(), "{plan}");
    assert!(String::from_utf8_lossy(&planned.stderr).contains("nothing to reclaim"));
}

#[test]
fn reclaim_confirmation_keeps_digest_mismatch_primary_when_snapshot_is_unreadable() {
    let (repo, container) = fixture("");
    let started = run(
        repo.path(),
        container.path(),
        &[
            "start",
            "--task",
            "bind digest mismatch test root",
            "--json",
        ],
    );
    assert!(
        started.status.success(),
        "start: stdout={} stderr={}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let root_output = run(
        repo.path(),
        container.path(),
        &["advanced", "worktree-root", "--json"],
    );
    assert!(root_output.status.success());
    let worktree_root =
        serde_json::from_slice::<serde_json::Value>(&root_output.stdout).unwrap()["preferred_root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap();

    let plan = reclaim_plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_string();
    let snapshot = worktree_root.join(format!(".aethyme-reclaim-plan-{digest}.json"));
    std::fs::write(&snapshot, b"not a reclaim snapshot\n").unwrap();

    let added = worktree_root.join("session/build");
    std::fs::create_dir_all(&added).unwrap();
    std::fs::write(added.join("artifact"), "new\n").unwrap();

    let refused = run(
        repo.path(),
        container.path(),
        &["gc", "reclaim", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("confirmation does not match the current plan"),
        "{message}"
    );
    assert!(
        message.contains("saved review could not be read"),
        "{message}"
    );
}

#[test]
fn a_reappearing_repository_revokes_an_authorized_orphan_removal() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let missing = container.path().join("deleted-repository");
    let orphan = registered_orphan(repo.path(), container.path(), &missing);

    let plan = plan_json(repo.path(), container.path());
    let digest = plan["digest"].as_str().unwrap().to_owned();
    assert_eq!(plan["orphans"].as_array().unwrap().len(), 1);

    // The premise of the authorization was that nothing owns this tree.
    std::fs::create_dir_all(&missing).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", &digest],
    );
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Either guard is a correct refusal: the plan is re-derived when no journal
    // is outstanding, so the digest stops it before the per-item evidence check
    // that protects a resumed apply. What must hold either way is that an
    // authorization stops binding the moment its premise changes.
    assert!(
        rendered.contains("no longer matches current state")
            || rendered.contains("orphan evidence changed"),
        "expected the removal to be refused, got {rendered}"
    );
    assert!(orphan.exists(), "the reclaimed tree must survive");
}

/// Start a session, leave an unaccepted commit on it, and close it keeping the
/// worktree. The result is a retained worktree whose provenance is blocked.
fn blocked_session_with_build_cache(repo: &Path, container: &Path) -> (String, PathBuf) {
    let output = run(repo, container, &["start", "--task", "blocked", "--json"]);
    assert!(
        output.status.success(),
        "start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());

    // An unaccepted commit is what makes cleanup refuse this worktree.
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-qm", "work"]);

    let target = worktree.join("rust/target");
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();

    // `close` rather than `finish`: an unaccepted commit is exactly what makes
    // `finish` refuse, and a blocked-but-closed session is the case under test.
    let output = run(repo, container, &["finish", "close", "--session", &id]);
    assert!(
        output.status.success(),
        "close: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (id, worktree)
}

/// Turn the autonomous sweep on after setup.
///
/// Fixtures start with it disabled so the opens that build the scenario cannot
/// consume the sweep's cadence window before the assertion runs.
fn enable_sweep(repo: &Path) {
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();
}

#[test]
fn cleanup_resolve_plan_and_apply_skip_sweeping_unrelated_closed_checkouts() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (cleanup_id, cleanup_worktree) =
        blocked_session_with_build_cache(repo.path(), container.path());
    let (_unrelated_id, unrelated_worktree) =
        blocked_session_with_build_cache(repo.path(), container.path());
    let unrelated_cache = unrelated_worktree.join("rust/target");
    let cache_payload = unrelated_cache.join("debug/artifact.bin");
    assert!(cache_payload.is_file());

    enable_sweep(repo.path());
    let plan_output = run(
        repo.path(),
        container.path(),
        &[
            "finish",
            "cleanup",
            "resolve",
            &cleanup_id,
            "--archive",
            "--json",
        ],
    );
    assert!(
        plan_output.status.success(),
        "cleanup plan: {}",
        String::from_utf8_lossy(&plan_output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan_output.stdout).unwrap();
    let digest = plan["digest"].as_str().unwrap();
    assert!(
        cache_payload.is_file(),
        "planning cleanup for one session must not sweep an unrelated closed checkout"
    );
    // Setup kept the sweep disabled, so an eligible startup pass would leave
    // either a completed cadence stamp or a partial-progress stamp.
    assert!(meta(repo.path(), SWEEP_STAMP).is_none());
    assert!(meta(repo.path(), SWEEP_PARTIAL).is_none());

    let apply_output = run(
        repo.path(),
        container.path(),
        &[
            "finish",
            "cleanup",
            "resolve",
            &cleanup_id,
            "--archive",
            "--confirm",
            digest,
            "--json",
        ],
    );
    assert!(
        apply_output.status.success(),
        "cleanup apply: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    assert!(
        !cleanup_worktree.exists(),
        "apply should remove only the reviewed cleanup target"
    );
    assert!(
        cache_payload.is_file(),
        "applying cleanup for one session must not sweep an unrelated closed checkout"
    );
    assert!(meta(repo.path(), SWEEP_STAMP).is_none());
    assert!(meta(repo.path(), SWEEP_PARTIAL).is_none());
}

/// Run the sweep explicitly, with its full budget. Eligibility tests use this
/// rather than a broker open: an open spends at most a quarter of a second, so
/// on a loaded machine it can pause before reaching the case under test and a
/// negative assertion would pass for the wrong reason.
fn sweep(repo: &Path, container: &Path) -> serde_json::Value {
    let output = run(repo, container, &["gc", "sweep", "--json"]);
    assert!(
        output.status.success(),
        "gc sweep: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn raw_meta(repo: &Path, key: &str) -> Option<String> {
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok()
}

fn artifact_sweep_namespace(repo: &Path) -> String {
    let registry = raw_meta(repo, "gc.artifact_sweep.v2.context_registry")
        .expect("the fixture registered its captured storage context");
    let namespaces: Vec<String> = serde_json::from_str(&registry).unwrap();
    assert_eq!(
        namespaces.len(),
        1,
        "the test must identify its exact namespace, never choose among multiple contexts"
    );
    namespaces.into_iter().next().unwrap()
}

fn scoped_meta_key(repo: &Path, key: &str) -> String {
    let suffix = key
        .strip_prefix("gc.artifact_sweep.")
        .expect("artifact-sweep metadata keys use their known prefix");
    format!(
        "gc.artifact_sweep.v2.{}.{}",
        artifact_sweep_namespace(repo),
        suffix
    )
}

fn meta(repo: &Path, key: &str) -> Option<String> {
    raw_meta(repo, &scoped_meta_key(repo, key))
}

fn raw_meta_set(repo: &Path, key: &str, value: &str) {
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .unwrap();
}

fn scoped_meta_set(repo: &Path, key: &str, value: &str) {
    raw_meta_set(repo, &scoped_meta_key(repo, key), value);
}

fn interrupted_paths(repo: &Path) -> Vec<String> {
    match meta(repo, "gc.artifact_sweep.interrupted_path") {
        None => Vec::new(),
        Some(value) if value.is_empty() => Vec::new(),
        Some(value) if value.starts_with('[') => serde_json::from_str(&value).unwrap(),
        Some(value) => vec![value],
    }
}

fn clear_meta(repo: &Path, key: &str) {
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .execute(
            "DELETE FROM meta WHERE key = ?1",
            [scoped_meta_key(repo, key)],
        )
        .unwrap();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenSweepState {
    Completed,
    Partial,
}

/// Inline work is time bounded, so a faster machine may complete a fixture
/// that pauses on another machine. Check both valid terminal states directly.
fn assert_open_sweep_state(
    repo: &Path,
    target: &Path,
    target_key: &str,
    entries_before: usize,
) -> OpenSweepState {
    let partial_stamp = meta(repo, SWEEP_PARTIAL).expect("an eligible open records sweep state");
    let queued = interrupted_paths(repo);
    if partial_stamp == "0" {
        let cadence = meta(repo, SWEEP_STAMP).expect("a completed lap records its cadence");
        assert_ne!(cadence, "0", "a completed lap has a nonzero cadence stamp");
        assert!(!target.exists(), "a complete lap removed the cache");
        assert!(
            !queued.iter().any(|path| path == target_key),
            "a complete lap clears this cache's interrupted entry"
        );
        OpenSweepState::Completed
    } else {
        assert!(
            partial_stamp.parse::<i64>().is_ok_and(|stamp| stamp > 0),
            "a partial lap records a positive timestamp: {partial_stamp}"
        );
        let entries_after = count_entries(target);
        if target.exists() {
            assert!(
                target.join("CACHEDIR.TAG").is_file(),
                "a cache with entries remaining retains its witness"
            );
            if entries_after < entries_before {
                assert!(
                    queued.iter().any(|path| path == target_key),
                    "a cache with removed entries is durably queued for resume"
                );
            }
        } else {
            assert!(
                !queued.iter().any(|path| path == target_key),
                "an absent cache has no stale interrupted-path entry"
            );
        }
        // A pass can finish this cache and hit its budget while classifying
        // unrelated work, so its partial stamp can coexist with an absent path.
        OpenSweepState::Partial
    }
}

const SWEEP_STAMP: &str = "gc.artifact_sweep.last_run_ms";
const SWEEP_PARTIAL: &str = "gc.artifact_sweep.last_partial_ms";

#[test]
fn gc_plan_reports_large_ignored_directories_without_authorizing_them() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(repo.path().join(".git/info/exclude"), "ignored-cache/\n").unwrap();
    let declined = worktree.join("ignored-cache");
    std::fs::create_dir_all(&declined).unwrap();
    std::fs::write(
        declined.join("dataset.bin"),
        vec![0_u8; aethyme_broker::UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES as usize + 1],
    )
    .unwrap();

    let plan = plan_json(repo.path(), container.path());
    let reported = plan["declined_artifacts"].as_array().unwrap();
    assert_eq!(
        reported.len(),
        1,
        "expected one declined directory: {reported:?}"
    );
    assert_eq!(reported[0]["relative_dir"], "ignored-cache");
    let declined_bytes = reported[0]["estimated_bytes"].as_u64().unwrap();
    assert!(declined_bytes > aethyme_broker::UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES);
    assert_eq!(plan["estimated_declined_artifact_bytes"], declined_bytes);
    assert!(
        plan["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["relative_dir"] != "ignored-cache")
    );

    let reclaimable_from_candidates = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["estimated_bytes"].as_u64().unwrap())
        .chain(plan["files"].as_array().unwrap().iter().map(|file| {
            file["bytes_before"]
                .as_u64()
                .unwrap()
                .saturating_sub(file["bytes_after"].as_u64().unwrap())
        }))
        .chain(
            plan["worktrees"]
                .as_array()
                .unwrap()
                .iter()
                .map(|worktree| worktree["estimated_bytes"].as_u64().unwrap()),
        )
        .chain(
            plan["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|artifact| artifact["estimated_bytes"].as_u64().unwrap()),
        )
        .chain(
            plan["orphans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|orphan| orphan["estimated_bytes"].as_u64().unwrap()),
        )
        .sum::<u64>();
    assert_eq!(
        plan["estimated_reclaimable_bytes"],
        reclaimable_from_candidates
    );
    assert!(declined.exists(), "reporting must not remove opaque output");
}

#[test]
fn configured_artifact_directories_are_reclaimable_in_gc_and_reclaim() {
    let (repo, container) = fixture(
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartefact_directories = [\".aeptus-cache\"]\n",
    );
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(repo.path().join(".git/info/exclude"), ".aeptus-cache/\n").unwrap();
    let store = worktree.join(".aeptus-cache");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("index"), b"package-index").unwrap();

    let plan = plan_json(repo.path(), container.path());
    assert!(
        plan["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["relative_dir"] == ".aeptus-cache"),
        "configured cache should extend the built-in catalog: {}",
        serde_json::to_string_pretty(&plan).unwrap()
    );
    assert!(plan["declined_artifacts"].as_array().unwrap().is_empty());

    let reclaim_plan = reclaim_plan_json(repo.path(), container.path());
    assert!(
        reclaim_plan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["path"]
                .as_str()
                .unwrap()
                .ends_with("/.aeptus-cache")),
        "legacy reclaim should use the same configured catalog: {reclaim_plan}"
    );

    let digest = plan["digest"].as_str().unwrap();
    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", digest],
    );
    assert!(
        output.status.success(),
        "gc apply: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !store.exists(),
        "configured cache should be removed after review"
    );
    assert!(worktree.join("work.txt").exists());
}

#[test]
fn default_policy_reclaims_closed_session_build_caches() {
    let (repo, container) = fixture("");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    assert!(
        !target.exists(),
        "close should reclaim build caches immediately under the default policy"
    );
    assert!(worktree.exists(), "the retained checkout must remain");
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must remain"
    );
    let branch_output = git_output(&worktree, &["branch", "--show-current"]);
    assert!(branch_output.status.success());
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert!(
        !branch.is_empty(),
        "the retained checkout must remain on its branch"
    );
    let branch_ref = format!("refs/heads/{branch}");
    let branch_check = git_output(repo.path(), &["show-ref", "--verify", branch_ref.as_str()]);
    assert!(
        branch_check.status.success(),
        "the retained branch must remain: {}",
        String::from_utf8_lossy(&branch_check.stderr)
    );

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "closed-session build cache should be gone"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "default reclamation must preserve committed work"
    );
}

#[test]
fn explicit_opt_out_preserves_closed_session_build_caches() {
    let (repo, container) = fixture("[retention]\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(target.exists(), "explicit opt-out must preserve the cache");
}

#[test]
fn finish_keep_worktree_reclaims_build_artifacts() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let output = run(
        repo.path(),
        container.path(),
        &["start", "--task", "retained finish", "--json"],
    );
    assert!(
        output.status.success(),
        "start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();

    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();
    let output = run(
        repo.path(),
        container.path(),
        &["finish", "--session", &id, "--keep-worktree", "--json"],
    );
    assert!(
        output.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["closed"], true);
    assert_eq!(report["cleanup"]["kept"], true);
    assert_eq!(report["cleanup"]["worktree_removed"], false);
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("reclaimed build artifacts")),
        "finish should report the reclaim: {report}"
    );
    assert!(
        !target.exists(),
        "finish should reclaim ignored build output"
    );
    assert!(worktree.exists(), "finish must retain the checkout");
}

#[test]
fn unknown_retention_fields_warn_without_disabling_gc_or_status() {
    let (repo, container) =
        fixture("[retention]\nartifact_sweep_budget_ms = 0\nfuture_sweep_days = 14\n");

    let plan = plan_json(repo.path(), container.path());
    assert_eq!(plan["policy"]["artifact_sweep_budget_ms"], 0);
    assert_eq!(
        plan["retention_config_warnings"][0]["field"],
        "retention.future_sweep_days"
    );

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["cleanup_retention"]["retention_config"]["warnings"][0]["field"],
        "retention.future_sweep_days"
    );
    assert!(status["advice"].as_array().unwrap().iter().any(|advice| {
        advice["id"] == "retention.config"
            && advice["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str().unwrap().contains("future_sweep_days"))
    }));
}

#[test]
fn invalid_retention_config_is_explained_by_status_instead_of_hiding_the_error() {
    let (repo, container) = fixture("[retention]\nstartup_budget_ms = 0\n");

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        status["cleanup_retention"]["retention_config"]["error"]
            .as_str()
            .unwrap()
            .contains("startup_budget_ms")
    );
    assert!(status["advice"].as_array().unwrap().iter().any(|advice| {
        advice["id"] == "retention.config"
            && advice["summary"]
                .as_str()
                .unwrap()
                .contains("conservative defaults")
            && advice["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str().unwrap().contains("broker gc plan"))
    }));
}

#[test]
fn tracked_directory_with_a_cache_witness_is_never_reclaimed() {
    let (repo, container) = fixture("");
    let output = run(
        repo.path(),
        container.path(),
        &["start", "--task", "tracked target", "--json"],
    );
    assert!(output.status.success());
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("tracked.txt"), "repository content\n").unwrap();
    git(
        &worktree,
        &[
            "add",
            "-f",
            "rust/target/CACHEDIR.TAG",
            "rust/target/tracked.txt",
        ],
    );
    git(&worktree, &["commit", "-qm", "tracked target"]);
    let output = run(
        repo.path(),
        container.path(),
        &["finish", "close", "--session", &id],
    );
    assert!(output.status.success());

    let output = run(repo.path(), container.path(), &["status", "--json"]);
    assert!(output.status.success());
    assert!(
        target.join("tracked.txt").exists(),
        "a tracked directory must survive even when its name and witness resemble a cache"
    );
}

#[test]
fn build_caches_are_reclaimable_even_when_the_worktree_itself_is_blocked() {
    // The autonomous sweep is off so the plan is what is under test.
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());

    let plan = plan_json(repo.path(), container.path());
    let artifacts = plan["artifacts"].as_array().unwrap();
    assert_eq!(
        artifacts.len(),
        1,
        "expected one build cache, got {artifacts:?}"
    );
    assert_eq!(artifacts[0]["relative_dir"], "rust/target");
    assert!(artifacts[0]["estimated_bytes"].as_u64().unwrap() >= 4096);

    // The worktree is blocked from whole-worktree cleanup, yet its build cache
    // is still reclaimable: the block protects commits, which a cache has none of.
    assert!(
        plan["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|worktree| worktree["session_id"].as_i64().unwrap().to_string() != id),
        "a session with unaccepted commits must not be scheduled for removal"
    );
    let retained_summary = plan["blocker_summary"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["kind"] == "retention_age")
        .expect("a recently closed unproven worktree should be age-blocked");
    assert_eq!(retained_summary["count"], 1);
    assert!(
        retained_summary["retained_bytes"].as_u64().unwrap() > 0,
        "age blocker should account for the retained worktree bytes: {retained_summary}"
    );
    let worktree_summary = plan["worktree_blocker_summary"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["kind"] == "retention_age")
        .expect("worktree-specific summary should include the age blocker");
    assert_eq!(worktree_summary["count"], 1);
    assert!(worktree_summary["retained_bytes"].as_u64().unwrap() > 0);

    let digest = plan["digest"].as_str().unwrap();
    let output = run(
        repo.path(),
        container.path(),
        &["gc", "apply", "--confirm", digest],
    );
    assert!(
        output.status.success(),
        "gc apply: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !worktree.join("rust/target").exists(),
        "build cache should be gone"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
}

#[test]
fn the_autonomous_sweep_reclaims_build_caches_without_confirmation() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    assert!(target.exists());
    enable_sweep(repo.path());

    // Explicit refresh uses normal broker startup, which runs the sweep. An open
    // spends at most a quarter of a second, so on a loaded machine one may
    // pause; clearing the spacing stamp stands in for the minutes the next
    // continuation would otherwise wait.
    for _ in 0..40 {
        let output = run(repo.path(), container.path(), &["status", "--refresh"]);
        assert!(
            output.status.success(),
            "status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !target.exists() {
            break;
        }
        clear_meta(repo.path(), SWEEP_PARTIAL);
    }
    assert!(
        !target.exists(),
        "an idle closed session's build cache should be reclaimed unprompted"
    );
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
}

/// The sweep and `gc reclaim` read one catalog. Closed sessions used to keep
/// their `.venv` because only `gc reclaim` knew the name, and nothing knew
/// `.pnpm-store`. Names with a witness the sweep can check are swept; generic
/// ones and unwitnessed directories are left for a reviewed plan.
#[test]
fn the_sweep_uses_the_shared_catalog_and_its_witnesses() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(
        repo.path().join(".git/info/exclude"),
        ".venv/\n.pnpm-store/\nbuild/\ndist/\n",
    )
    .unwrap();
    let populate = |relative: &str, file: &str| {
        let dir = worktree.join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), vec![0_u8; 64]).unwrap();
        dir
    };
    let venv = populate("backend/.venv", "pyvenv.cfg");
    let store = populate(".pnpm-store", "index");
    let unwitnessed = populate("tools/.venv", "notes.txt");
    let build = populate("web/build", "bundle.js");
    let dist = populate("web/dist", "bundle.js");
    enable_sweep(repo.path());

    let report = sweep(repo.path(), container.path());
    assert!(
        !venv.exists(),
        "a witnessed .venv should be swept: {report}"
    );
    assert!(!store.exists(), "a pnpm store should be swept: {report}");
    assert!(
        unwitnessed.exists(),
        "a .venv without pyvenv.cfg is not one"
    );
    assert!(build.exists(), "build/ is reviewed-only");
    assert!(dist.exists(), "dist/ is reviewed-only");
    assert!(worktree.join("work.txt").exists());

    let reclaim_plan = reclaim_plan_json(repo.path(), container.path());
    let proposed = |suffix: &str| {
        reclaim_plan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| {
                candidate["reclaimable"] == true
                    && candidate["path"].as_str().unwrap().ends_with(suffix)
            })
    };
    assert!(proposed("/web/build"), "{reclaim_plan}");
    assert!(proposed("/web/dist"), "{reclaim_plan}");
}

#[test]
fn a_live_session_keeps_its_build_cache() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let output = run(
        repo.path(),
        container.path(),
        &["start", "--task", "live", "--json"],
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let target = worktree.join("rust/target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();

    // Without this the sweep would simply be disabled, and the assertion below
    // would hold for the wrong reason.
    enable_sweep(repo.path());
    let report = sweep(repo.path(), container.path());
    assert!(
        report["budget_ms"]
            .as_u64()
            .is_some_and(|budget| budget > 0),
        "the sweep must have a nonzero budget: {report}"
    );
    assert_eq!(
        report["complete"], true,
        "the sweep must actually run: {report}"
    );
    assert!(
        target.exists(),
        "a session still in use must keep its build cache"
    );
}

const HOUR_MS: i64 = 60 * 60 * 1000;

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Set a path's modification time to `hours_ago`.
fn backdate(path: &Path, hours_ago: i64) {
    let when = std::time::SystemTime::now()
        - std::time::Duration::from_millis((hours_ago * HOUR_MS) as u64);
    std::fs::File::open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Backdate a directory and every entry directly inside it; the sweep reads
/// exactly that far when deciding whether output is still being written.
fn backdate_tree_top(dir: &Path, hours_ago: i64) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        backdate(&entry.path(), hours_ago);
    }
    backdate(dir, hours_ago);
}

/// An open session whose agent has shown no evidence of work for
/// `quiet_hours`, holding a Cargo `target/` and an ignored `build/`.
///
/// Quiet means every signal the broker reads for liveness: the session row's
/// activity and creation times, and the mtimes of the worktree's Git index and
/// HEAD. A session started through the CLI records no pid, so it is judged on
/// the clock, as an adopted or abandoned agent is.
fn quiet_open_session(repo: &Path, container: &Path, quiet_hours: i64) -> (i64, PathBuf) {
    let output = run(repo, container, &["start", "--task", "quiet", "--json"]);
    assert!(
        output.status.success(),
        "start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = session["id"].as_i64().unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());

    let target = worktree.join("rust/target");
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
    std::fs::write(target.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();
    let build = worktree.join("rust/build");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join("bundle.js"), vec![0_u8; 64]).unwrap();
    backdate_tree_top(&target, quiet_hours);
    backdate_tree_top(&build, quiet_hours);

    let quiet_since = epoch_ms() - quiet_hours * HOUR_MS;
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    db.execute(
        "UPDATE sessions SET last_activity_at = ?2, created_at = ?2 WHERE id = ?1",
        rusqlite::params![id, quiet_since],
    )
    .unwrap();
    let name = worktree.file_name().unwrap();
    for file in ["index", "HEAD"] {
        let path = repo.join(".git/worktrees").join(name).join(file);
        if path.exists() {
            backdate(&path, quiet_hours);
        }
    }
    (id, worktree)
}

/// Push a session's recorded and Git-visible activity back by `quiet_hours`.
/// Broker commands refresh a worktree's index, which reads as agent activity.
fn make_quiet(repo: &Path, id: i64, worktree: &Path, quiet_hours: i64) {
    let quiet_since = epoch_ms() - quiet_hours * HOUR_MS;
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at = ?2 WHERE id = ?1",
            rusqlite::params![id, quiet_since],
        )
        .unwrap();
    let name = worktree.file_name().unwrap();
    for file in ["index", "HEAD"] {
        let path = repo.join(".git/worktrees").join(name).join(file);
        if path.exists() {
            backdate(&path, quiet_hours);
        }
    }
}

fn session_status(repo: &Path, id: i64) -> String {
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .query_row("SELECT status FROM sessions WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Autonomous sweeps preserve build caches from open sessions even when the
/// agent has been quiet for longer than the legacy idle window.
#[test]
fn an_idle_open_sessions_build_output_is_preserved_and_the_session_stays_open() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (id, worktree) = quiet_open_session(repo.path(), container.path(), 30);
    enable_sweep(repo.path());

    let report = sweep(repo.path(), container.path());
    assert!(
        worktree.join("rust/target/CACHEDIR.TAG").exists(),
        "an idle open session's Cargo cache stays protected: {report}"
    );
    assert!(
        worktree.join("rust/build/bundle.js").exists(),
        "build/ is reviewed-only and must survive the unattended sweep"
    );
    assert!(worktree.join("README.md").exists(), "the checkout stays");
    assert_ne!(
        session_status(repo.path(), id),
        "cleaned",
        "sweeping build output must not close the session"
    );
}

#[test]
fn inline_sweep_removes_closed_cache_and_preserves_an_idle_open_cache() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (closed_worktree, _) =
        closed_session_with_target(repo.path(), container.path(), "closed-cache");
    let (_open_id, open_worktree) = quiet_open_session(repo.path(), container.path(), 30);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\n\
         artifact_sweep_budget_ms = 5000\nidle_session_artifact_hours = 1\n",
    )
    .unwrap();

    let output = run(repo.path(), container.path(), &["status", "--refresh"]);

    assert!(
        output.status.success(),
        "status: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        closed_worktree.exists(),
        "automatic inline cleanup preserves the closed checkout"
    );
    assert!(
        !closed_worktree.join("rust/target").exists(),
        "the same inline sweep reclaims the eligible closed cache"
    );
    assert!(
        open_worktree.join("rust/target/CACHEDIR.TAG").exists(),
        "quiet open sessions are outside every autonomous cache scope"
    );
}

#[test]
fn a_legacy_idle_window_does_not_authorize_open_cache_removal() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = quiet_open_session(repo.path(), container.path(), 3);
    // The open session remains protected regardless of how old its output is.
    backdate_tree_top(&worktree.join("rust/target"), 30);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n\
         idle_session_artifact_hours = 1\n",
    )
    .unwrap();

    sweep(repo.path(), container.path());
    assert!(
        worktree.join("rust/target/CACHEDIR.TAG").exists(),
        "elapsed idle time must not make an open cache eligible"
    );
}

#[test]
fn a_zero_legacy_idle_window_keeps_open_cache_protected() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = quiet_open_session(repo.path(), container.path(), 30);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n\
         idle_session_artifact_hours = 0\n",
    )
    .unwrap();

    sweep(repo.path(), container.path());
    assert!(
        worktree.join("rust/target/CACHEDIR.TAG").exists(),
        "the legacy setting does not change open-session protection"
    );
}

/// A deep build write does not update the cache root or its immediate entries.
/// The unattended lane cannot use those shallow timestamps to prove an open
/// session's cache is idle, so it preserves the cache.
#[test]
fn a_nested_recent_write_in_an_open_cache_is_preserved() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = quiet_open_session(repo.path(), container.path(), 30);
    let target = worktree.join("rust/target");
    let deep = target.join("debug/incremental/hash");
    std::fs::create_dir_all(&deep).unwrap();
    backdate_tree_top(&target, 30);
    std::fs::write(deep.join("fresh.bin"), vec![0_u8; 16]).unwrap();
    enable_sweep(repo.path());

    sweep(repo.path(), container.path());
    assert!(
        worktree.join("rust/target/CACHEDIR.TAG").exists(),
        "a nested build write must not be missed by a shallow mtime check"
    );
}

fn count_entries(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                1 + count_entries(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

#[test]
fn a_bounded_open_leaves_a_truthful_state_and_explicit_sweep_finishes() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    for index in 0..3000_u32 {
        let bucket = target.join(format!("debug/deps/{}", index % 16));
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join(format!("{index}.rlib")), b"artifact").unwrap();
    }
    let target_key = target
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let before = count_entries(&target);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 50\n",
    )
    .unwrap();
    let first = run(repo.path(), container.path(), &["status", "--refresh"]);
    assert!(
        first.status.success(),
        "status: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_state = assert_open_sweep_state(repo.path(), &target, &target_key, before);
    let first_entries = count_entries(&target);
    let first_partial = meta(repo.path(), SWEEP_PARTIAL);
    let first_cadence = meta(repo.path(), SWEEP_STAMP);
    let first_queue = interrupted_paths(repo.path());

    // Repeated opens inside the spacing/cadence interval leave the observed
    // terminal state alone, whether this machine paused or completed the lap.
    for _ in 0..4 {
        let output = run(repo.path(), container.path(), &["status", "--refresh"]);
        assert!(
            output.status.success(),
            "status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            assert_open_sweep_state(repo.path(), &target, &target_key, before),
            first_state
        );
        assert_eq!(count_entries(&target), first_entries);
        assert_eq!(meta(repo.path(), SWEEP_PARTIAL), first_partial);
        assert_eq!(meta(repo.path(), SWEEP_STAMP), first_cadence);
        assert_eq!(interrupted_paths(repo.path()), first_queue);
    }

    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 30000\n",
    )
    .unwrap();
    let report = sweep(repo.path(), container.path());
    assert_eq!(report["complete"], true, "{report}");
    assert!(!target.exists(), "the explicit sweep finishes the cache");
    assert!(
        worktree.join("work.txt").exists(),
        "committed work must be untouched"
    );
    assert_eq!(meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("0"));
    assert_ne!(meta(repo.path(), SWEEP_STAMP).as_deref(), Some("0"));
    assert!(!interrupted_paths(repo.path()).contains(&target_key));
}

/// An orphaned root's owning database is gone, which is what makes it an
/// orphan, so no age is recoverable for it and only its size can order it.
/// Path order -- what the sweep used before -- is arbitrary with respect to
/// what a time-bounded `gc apply` should reach first (#176).
#[test]
fn the_largest_orphaned_root_is_planned_before_smaller_ones() {
    let (repo, container) =
        fixture("[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\n");
    let first = registered_orphan(
        repo.path(),
        container.path(),
        &container.path().join("deleted-first"),
    );
    let second = registered_orphan(
        repo.path(),
        container.path(),
        &container.path().join("deleted-second"),
    );
    // Whichever key sorts later receives the larger payload: alphabetical
    // order and measured size order must disagree independently of hashes.
    let mut roots = [first, second];
    roots.sort();
    let [small, large] = roots;
    std::fs::write(large.join("some-session/bulk.bin"), vec![b'x'; 512 * 1_024]).unwrap();

    let plan = plan_json(repo.path(), container.path());
    let keys: Vec<&str> = plan["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|orphan| orphan["repository_key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec![
            large.file_name().unwrap().to_str().unwrap(),
            small.file_name().unwrap().to_str().unwrap()
        ]
    );
    assert!(small.exists() && large.exists(), "planning removes nothing");
}

/// An orphaned root's bytes are retained bytes, and `doctor` takes the
/// recorded-size path, which never walks one. If those missing bytes did not
/// count against the total's completeness, `doctor` would report a floor as a
/// total and the budget would read as satisfied because nobody looked (#176).
#[test]
fn doctor_counts_an_unsized_orphaned_root_against_its_own_totals() {
    let (repo, container) = fixture(
        "[retention]\norphan_worktree_roots_days = 0\nartifact_sweep_budget_ms = 0\nroutine_size_budget_ms = 0\nretained_bytes_budget = 1073741824\n",
    );
    let missing = container.path().join("gone-repository");
    registered_orphan(repo.path(), container.path(), &missing);

    // The expensive audit sizes it, so the orphan is a real, non-zero cost.
    let plan = plan_json(repo.path(), container.path());
    assert_eq!(plan["orphans"].as_array().unwrap().len(), 1);
    assert!(plan["orphans"][0]["estimated_bytes"].as_u64().unwrap() > 0);
    assert_eq!(plan["unmeasured_directory_count"].as_u64().unwrap(), 0);
    assert_eq!(
        plan["budget_verdict"].as_str().unwrap(),
        "within",
        "{plan:#}"
    );

    let output = run(
        repo.path(),
        container.path(),
        &["status", "doctor", "--json"],
    );
    assert!(
        output.status.success(),
        "doctor: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let retention = &report["retention"];
    assert_eq!(retention["unmeasured_directory_count"].as_u64().unwrap(), 1);
    assert_eq!(retention["budget_verdict"].as_str().unwrap(), "unknown");
    assert!(
        !retention["over_retained_bytes_budget"].as_bool().unwrap(),
        "undecided is not a breach"
    );
}

/// Inline cleanup is bounded, so the same fixture may pause on one machine
/// and finish on another. Repeated opens inside the spacing or cadence window
/// must preserve whichever truthful state the first open recorded.
#[test]
fn broker_opens_preserve_the_completed_or_partial_sweep_state() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    populate_large_cache(&target);
    let target_key = target
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let before = count_entries(&target);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 250\n",
    )
    .unwrap();

    let first = run(repo.path(), container.path(), &["status", "--refresh"]);
    assert!(
        first.status.success(),
        "status: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_state = assert_open_sweep_state(repo.path(), &target, &target_key, before);
    let after_first = count_entries(&target);
    let partial_stamp = meta(repo.path(), SWEEP_PARTIAL);
    let cadence_stamp = meta(repo.path(), SWEEP_STAMP);
    let queue = interrupted_paths(repo.path());

    for _ in 0..10 {
        let output = run(repo.path(), container.path(), &["status", "--refresh"]);
        assert!(
            output.status.success(),
            "status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            assert_open_sweep_state(repo.path(), &target, &target_key, before),
            first_state
        );
        assert_eq!(
            count_entries(&target),
            after_first,
            "opens inside spacing/cadence do not continue the bounded pass"
        );
        assert_eq!(meta(repo.path(), SWEEP_PARTIAL), partial_stamp);
        assert_eq!(meta(repo.path(), SWEEP_STAMP), cadence_stamp);
        assert_eq!(interrupted_paths(repo.path()), queue);
    }
    assert!(
        worktree.join("work.txt").exists(),
        "a sweep never removes the checkout itself"
    );
}

/// The explicit sweep ignores the spacing and the cadence, and a finished lap
/// records the cadence stamp and clears the paused marker -- so ordinary opens
/// go back to skipping for a day.
#[test]
fn gc_sweep_drains_the_backlog_and_stamps_the_cadence() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    populate_large_cache(&target);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 50\n",
    )
    .unwrap();
    assert!(
        run(repo.path(), container.path(), &["status", "--refresh"])
            .status
            .success()
    );
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();

    // The paused marker would keep a broker open away; the explicit sweep
    // ignores it.
    assert_ne!(meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("0"));
    let report = sweep(repo.path(), container.path());
    assert_eq!(report["complete"], true, "{report}");
    assert!(!target.exists(), "{report}");

    let stamp = meta(repo.path(), SWEEP_STAMP).expect("a finished lap stamps the cadence");
    assert!(stamp.parse::<i64>().unwrap() > 0);
    assert_eq!(meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("0"));
}

#[test]
fn gc_sweep_reports_the_pressure_scaled_budget_when_disk_is_low() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n");
    let output = run(repo.path(), container.path(), &["gc", "sweep", "--json"]);
    assert!(output.status.success());
    let routine: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(routine["budget_ms"], 5000, "{routine}");
    assert_eq!(routine["disk_pressured"], false, "{routine}");

    let output = command_from_fixture(
        repo.path(),
        repo.path(),
        container.path(),
        &["gc", "sweep", "--json"],
    )
    .env("AETHYME_TEST_AVAILABLE_BYTES", "1024")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pressured: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(pressured["budget_ms"], 60000, "{pressured}");
    assert_eq!(pressured["disk_pressured"], true, "{pressured}");
}

#[test]
fn gc_sweep_json_reports_unscoped_legacy_debt_without_rewriting_it() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    raw_meta_set(repo.path(), SWEEP_PARTIAL, "17");

    let report = sweep(repo.path(), container.path());
    assert_eq!(
        report["legacy_unscoped_state"], "unknown_pending",
        "{report}"
    );
    assert_eq!(raw_meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("17"));
}

#[test]
fn gc_sweep_text_reports_unscoped_legacy_debt_without_rewriting_it() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    raw_meta_set(repo.path(), SWEEP_PARTIAL, "17");

    let output = run(repo.path(), container.path(), &["gc", "sweep"]);
    assert!(
        output.status.success(),
        "gc sweep: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(
            "legacy unscoped artifact-sweep state: unknown and still pending; this lap covers only the current storage context"
        ),
        "text report explains unresolved legacy state: {text}"
    );
    assert_eq!(raw_meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("17"));
}

/// The drain broker opens no longer do is paid by an agent that is leaving.
#[test]
fn finish_continues_an_unfinished_sweep() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let target = worktree.join("rust/target");
    populate_large_cache(&target);
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 50\n",
    )
    .unwrap();
    assert!(
        run(repo.path(), container.path(), &["status", "--refresh"])
            .status
            .success()
    );
    assert!(target.exists(), "the 50 ms open must leave a backlog");
    assert_ne!(meta(repo.path(), SWEEP_PARTIAL).as_deref(), Some("0"));

    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 30000\n",
    )
    .unwrap();
    let started = run(
        repo.path(),
        container.path(),
        &["start", "--task", "leaving", "--json"],
    );
    assert!(started.status.success());
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let output = run(
        repo.path(),
        container.path(),
        &["finish", "--session", &id, "--keep-worktree", "--json"],
    );
    assert!(
        output.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "finish should drain the other session's backlog: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A cache no single short pass can remove.
fn populate_large_cache(target: &Path) {
    for index in 0..3000_u32 {
        let bucket = target.join(format!("debug/deps/{}", index % 16));
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join(format!("{index}.rlib")), b"artifact").unwrap();
    }
}

/// A fresh CLI pass resumes a persisted canonical cache marker in a closed
/// session. Actual partial removal is covered by
/// gc::tests::inline_partial_cache_removal_respects_spacing_and_resumes_afterward.
#[test]
fn a_closed_session_with_a_persisted_interrupted_cache_is_resumed_by_the_next_pass() {
    let (repo, container) = fixture(
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n",
    );
    let (worktree, id) = closed_session_with_target(repo.path(), container.path(), "interrupted");
    let worktree = worktree.canonicalize().unwrap();
    let session_id = id.parse().unwrap();
    let initial_status = session_status(repo.path(), session_id);
    assert!(
        aethyme_broker::SessionStatus::parse(&initial_status)
            .unwrap()
            .is_closed(),
        "the cache belongs to a closed session, got {initial_status:?}"
    );
    assert!(worktree.is_dir(), "the closed checkout is retained");

    let target = worktree.join("rust/target").canonicalize().unwrap();
    assert!(
        target.starts_with(&worktree),
        "the resumed cache must be inside its closed session worktree"
    );
    assert!(target.join("artifact").is_file());
    backdate_tree_top(&target, 30);

    // Without its normal witness, ordinary discovery must not select this
    // payload; the persisted interrupted-path marker is the only resume path.
    let witness = target.join("CACHEDIR.TAG");
    assert!(witness.is_file());
    std::fs::remove_file(&witness).unwrap();
    backdate_tree_top(&target, 30);
    assert!(!witness.exists());
    let ordinary_plan = plan_json(repo.path(), container.path());
    let ordinary_artifacts = ordinary_plan["artifacts"].as_array().unwrap();
    assert!(
        ordinary_artifacts
            .iter()
            .all(|artifact| artifact["relative_dir"] != "rust/target"),
        "ordinary discovery must not select a witnessless cache: {ordinary_artifacts:?}"
    );

    let target_key = target.to_string_lossy().into_owned();
    remember_interrupted_tree(repo.path(), &target);
    assert_eq!(
        interrupted_paths(repo.path()),
        vec![target_key],
        "the persisted marker is present before the fresh CLI pass"
    );

    let selected_repo = tempfile::tempdir().unwrap();
    git(selected_repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(selected_repo.path().join("selector.txt"), "preserve\n").unwrap();
    git(selected_repo.path(), &["add", "selector.txt"]);
    git(selected_repo.path(), &["commit", "-qm", "initial"]);
    let selected_head = git_output(selected_repo.path(), &["rev-parse", "HEAD"]);
    assert!(selected_head.status.success());
    let selected_index = std::fs::read(selected_repo.path().join(".git/index")).unwrap();
    let selected_content = std::fs::read(selected_repo.path().join("selector.txt")).unwrap();

    enable_sweep(repo.path());
    let output = command_from_fixture_with_env(
        repo.path(),
        repo.path(),
        container.path(),
        &["gc", "sweep", "--json"],
        git_selector_overrides(selected_repo.path()),
    )
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "gc sweep: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["complete"].is_boolean(),
        "the sweep reports whether other cleanup work remains: {report}"
    );
    assert!(
        !target.exists(),
        "the next CLI pass resumes the cache: {report}"
    );
    assert!(
        !selected_repo.path().join(".aethyme").exists(),
        "the CLI child leaves the alternate repository untouched"
    );
    let selected_head_after = git_output(selected_repo.path(), &["rev-parse", "HEAD"]);
    assert!(selected_head_after.status.success());
    assert_eq!(selected_head_after.stdout, selected_head.stdout);
    assert_eq!(
        std::fs::read(selected_repo.path().join(".git/index")).unwrap(),
        selected_index
    );
    assert_eq!(
        std::fs::read(selected_repo.path().join("selector.txt")).unwrap(),
        selected_content
    );
    assert!(
        worktree.join("README.md").is_file(),
        "resuming cache cleanup preserves the closed worktree"
    );
    let final_status = session_status(repo.path(), session_id);
    assert!(
        aethyme_broker::SessionStatus::parse(&final_status)
            .unwrap()
            .is_closed(),
        "cache cleanup leaves the session closed, got {final_status:?}: {report}"
    );
    assert_eq!(
        meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
        Some("")
    );
    assert!(interrupted_paths(repo.path()).is_empty());
}

/// A cache witnessed only by being non-empty stops classifying once a paused
/// removal has emptied it; the remembered path is finished anyway.
#[test]
fn an_emptied_store_left_by_a_paused_removal_is_still_finished() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    std::fs::write(repo.path().join(".git/info/exclude"), ".pnpm-store/\n").unwrap();
    let store = worktree.join(".pnpm-store");
    std::fs::create_dir_all(&store).unwrap();
    scoped_meta_set(
        repo.path(),
        "gc.artifact_sweep.interrupted_path",
        store.to_string_lossy().as_ref(),
    );
    enable_sweep(repo.path());

    let report = sweep(repo.path(), container.path());
    assert!(!store.exists(), "{report}");
    assert!(worktree.join("work.txt").exists(), "committed work stays");
}

/// The remembered path is only ever finished inside a session worktree the
/// sweep is visiting; a path anywhere else is ignored.
#[test]
fn a_remembered_path_outside_the_session_worktree_is_never_touched() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, _worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let outside = tempfile::tempdir().unwrap();
    let precious = outside.path().join(".pnpm-store");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("keep"), "keep\n").unwrap();
    scoped_meta_set(
        repo.path(),
        "gc.artifact_sweep.interrupted_path",
        precious.to_string_lossy().as_ref(),
    );
    enable_sweep(repo.path());

    sweep(repo.path(), container.path());
    assert!(precious.join("keep").exists());
    assert_eq!(
        meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
        Some("")
    );
}

fn remember_interrupted_tree(repo: &Path, path: &Path) {
    scoped_meta_set(
        repo,
        "gc.artifact_sweep.interrupted_path",
        path.to_string_lossy().as_ref(),
    );
}

#[test]
fn a_remembered_parent_traversal_is_never_resumed() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (id, worktree) = quiet_open_session(repo.path(), container.path(), 30);
    std::fs::create_dir(worktree.join("a")).unwrap();
    let store = worktree.join(".pnpm-store");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("keep"), "keep\n").unwrap();
    std::fs::write(repo.path().join(".git/info/exclude"), ".pnpm-store/\na/\n").unwrap();
    remember_interrupted_tree(repo.path(), &worktree.join("a/../.pnpm-store"));
    make_quiet(repo.path(), id, &worktree, 30);
    enable_sweep(repo.path());
    sweep(repo.path(), container.path());
    assert!(
        store.join("keep").exists(),
        "an invalid remembered path must not bypass fresh-output protection"
    );
    assert_eq!(
        meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
        Some("")
    );
}

#[test]
fn a_valid_remembered_open_cache_stays_pending_and_untouched() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = quiet_open_session(repo.path(), container.path(), 30);
    let target = worktree.join("rust/target");
    let artifact = target.join("debug/artifact.bin");
    let before = std::fs::read(&artifact).unwrap();
    remember_interrupted_tree(repo.path(), &target);
    enable_sweep(repo.path());

    let report = sweep(repo.path(), container.path());
    assert!(
        target.is_dir(),
        "an open session's cache remains present: {report}"
    );
    assert_eq!(std::fs::read(&artifact).unwrap(), before);
    assert_eq!(
        meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
        Some(target.to_string_lossy().as_ref()),
        "a valid live marker remains pending until its session closes"
    );
}

#[test]
fn a_remembered_non_artifact_directory_is_never_removed() {
    let (repo, container) =
        fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
    let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
    let cache = worktree.join("rust/target");
    let precious = worktree.join("local-data");
    std::fs::create_dir(&precious).unwrap();
    std::fs::write(precious.join("keep"), "keep\n").unwrap();
    std::fs::write(repo.path().join(".git/info/exclude"), "local-data/\n").unwrap();
    remember_interrupted_tree(repo.path(), &precious);
    assert!(
        cache.exists(),
        "the unrelated eligible cache starts present"
    );
    enable_sweep(repo.path());
    sweep(repo.path(), container.path());
    assert!(precious.join("keep").exists());
    assert!(
        !cache.exists(),
        "a revoked marker must not starve eligible caches"
    );
    assert_eq!(
        meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
        Some("")
    );
    let payload: String = rusqlite::Connection::open(repo.path().join(".aethyme/broker.db"))
        .unwrap()
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'broker.gc.artifacts-swept' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["directories"], 1);
    let expected_path: String = precious.to_string_lossy().chars().take(128).collect();
    assert_eq!(
        payload["revoked_interrupted_paths"][0]["path"],
        expected_path
    );
    assert_eq!(
        payload["revoked_interrupted_paths"][0]["reason"],
        "not_unattended_artifact"
    );
}

#[test]
fn a_remembered_path_through_a_symlink_is_never_removed() {
    for points_outside in [false, true] {
        let (repo, container) =
            fixture("[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n");
        let (_id, worktree) = blocked_session_with_build_cache(repo.path(), container.path());
        let outside = tempfile::tempdir().unwrap();
        let real = if points_outside {
            outside.path().to_path_buf()
        } else {
            // Keep the canonical target beyond the ordinary discovery depth,
            // so only the remembered path could remove it.
            worktree.join("a/b/c/d/e/f/g")
        };
        let store = real.join(".pnpm-store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("keep"), "keep\n").unwrap();
        std::os::unix::fs::symlink(&real, worktree.join("link")).unwrap();
        std::fs::write(repo.path().join(".git/info/exclude"), "link/\na/\n").unwrap();
        remember_interrupted_tree(repo.path(), &worktree.join("link/.pnpm-store"));
        enable_sweep(repo.path());
        let report = sweep(repo.path(), container.path());
        assert_eq!(
            report["complete"], false,
            "a present symlink component keeps the sweep incomplete: {report}"
        );
        assert!(
            store.join("keep").exists(),
            "symlink target must survive (outside: {points_outside})"
        );
        let expected_path = worktree
            .join("link/.pnpm-store")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            meta(repo.path(), "gc.artifact_sweep.interrupted_path").as_deref(),
            Some(expected_path.as_str()),
            "a present symlink path remains queued until it is resolved"
        );
        assert!(
            meta(repo.path(), "gc.artifact_sweep.blocked_paths")
                .expect("the symlink blocker is reported")
                .contains("symlink_component")
        );
    }
}
