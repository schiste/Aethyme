use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, BrokerOpenOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) {
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
}

fn fixture_at(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "README.md", ".gitignore"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .env("AETHYME_HOST_CACHE_DIR", state.with_file_name("host-cache"))
        .env_remove("AETHYME_WORKTREE_ROOT")
        .output()
        .unwrap()
}

fn run_with_host_state(repo: &Path, state: &Path, args: &[&str]) -> Output {
    // Fresh placement uses this selected base only when it already exists.
    std::fs::create_dir_all(state).unwrap();
    run(repo, state, args)
}

fn run_with_new_database_and_root(
    repo: &Path,
    state: &Path,
    cache: &Path,
    database: &Path,
    root_container: &Path,
    args: &[&str],
) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .env("AETHYME_HOST_CACHE_DIR", cache)
        .env("AETHYME_BROKER_DB", database)
        .env("AETHYME_WORKTREE_ROOT", root_container)
        .output()
        .unwrap()
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn sqlite_count_if_present(path: &Path, query: &str) -> u64 {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => panic!("cannot inspect SQLite fixture {}: {error}", path.display()),
    };
    assert!(
        metadata.file_type().is_file(),
        "SQLite fixture is not a regular file: {}",
        path.display()
    );
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row(query, [], |row| row.get(0))
        .unwrap()
}

fn worktree_root_registry_path(state: &Path) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(state) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!(
            "cannot inspect host-state fixture {}: {error}",
            state.display()
        ),
    };
    let mut registries = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!(
                "cannot inspect host-state entry under {}: {error}",
                state.display()
            )
        });
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("worktree-roots-")
        {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().unwrap_or_else(|error| {
            panic!(
                "cannot inspect worktree-root registry path {}: {error}",
                path.display()
            )
        });
        assert!(
            file_type.is_dir(),
            "worktree-root registry catalog is not a directory: {}",
            path.display()
        );
        registries.push(path);
    }
    assert!(
        registries.len() <= 1,
        "expected at most one worktree-root registry catalog under {}, found {}",
        state.display(),
        registries.len()
    );
    registries
        .pop()
        .map(|directory| directory.join("registry.sqlite3"))
}

fn bind_managed_root_fixture(repo: &Path, state: &Path, cache: &Path, root: &Path) {
    std::fs::create_dir_all(state).unwrap();
    std::fs::create_dir_all(cache).unwrap();
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    let options = BrokerOpenOptions::default()
        .with_host_state_dir(state)
        .with_host_cache_dir(cache)
        .with_worktree_root(root);
    let mut broker = Broker::open_with_options(repo, options).unwrap();
    let bootstrap = broker.start_worktree("fixture root binding", None).unwrap();
    let branch = bootstrap.branch.clone();
    broker.close(bootstrap.id).unwrap();
    broker.cleanup(bootstrap.id, false).unwrap();
    if !git_output(repo, &["branch", "--list", &branch]).is_empty() {
        git(repo, &["branch", "-D", &branch]);
    }
    assert!(root.join(".aethyme-worktree-root.json").is_file());
}

fn run_with_storage_paths(
    repo: &Path,
    host_state: &Path,
    preparation_cache: &Path,
    args: &[&str],
) -> Output {
    run_with_host_paths(repo, host_state, Some(preparation_cache), args)
}

fn run_without_preparation_cache(repo: &Path, host_state: &Path, args: &[&str]) -> Output {
    run_with_host_paths(repo, host_state, None, args)
}

fn run_with_host_paths(
    repo: &Path,
    host_state: &Path,
    preparation_cache: Option<&Path>,
    args: &[&str],
) -> Output {
    let mut command = common::broker_cli(CLI, args);
    command
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .env(
            "AETHYME_HOST_CACHE_DIR",
            host_state.with_file_name("host-cache"),
        );
    if let Some(preparation_cache) = preparation_cache {
        command.env("AETHYME_PREPARATION_CACHE_ROOT", preparation_cache);
    }
    command
        .env(
            "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
            host_state.join("host-operations.db"),
        )
        .env("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT", "true")
        .env(
            "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
            host_state.join("collaboration"),
        )
        .output()
        .unwrap()
}

fn snapshot_status(repository: &Path) -> serde_json::Value {
    let broker = Broker::open_snapshot(repository).unwrap();
    let status = serde_json::to_value(broker.status_current_snapshot(0).unwrap()).unwrap();
    assert!(status["summary"].is_object(), "{status}");
    assert!(status["agents"].is_array(), "{status}");
    assert!(status["integration_branch"].is_string(), "{status}");
    status
}

struct LegacyRootFixture {
    repository: PathBuf,
    host_state: PathBuf,
    preparation_cache: PathBuf,
    worktree_root: PathBuf,
    worktree: PathBuf,
    database: PathBuf,
    repository_uuid_marker: PathBuf,
    root_marker: PathBuf,
}

fn synthetic_downgrade_legacy_root_fixture(owner: &Path) -> LegacyRootFixture {
    let repository = owner.join("project");
    let host_state = owner.join("host-state");
    let preparation_cache = owner.join("preparation-cache");
    fixture_at(&repository);
    std::fs::create_dir_all(&host_state).unwrap();
    std::fs::create_dir_all(&preparation_cache).unwrap();
    let started = json(run_with_storage_paths(
        &repository,
        &host_state,
        &preparation_cache,
        &["start", "--task", "existing legacy root", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let worktree_root = PathBuf::from(started["worktree_placement"]["root"].as_str().unwrap());
    let database = repository.join(".aethyme/broker.db");
    let root_marker = worktree_root.join(".aethyme-worktree-root.json");
    let repository_uuid_marker = repository.join(".git/aethyme-repository-uuid");
    assert!(worktree.is_dir());
    assert!(root_marker.is_file());
    assert!(repository_uuid_marker.is_file());
    std::fs::write(worktree.join("legacy-retained.txt"), "keep this checkout\n").unwrap();

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('fixture.legacy-retained', 'kept')",
            [],
        )
        .unwrap();
    connection
        .execute_batch(
            "DROP TABLE repository_worktree_binding;
             UPDATE meta SET value = '52' WHERE key = 'schema_version';
             UPDATE meta SET value = '52' WHERE key = 'min_compatible_schema';
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    drop(connection);
    std::fs::remove_file(&repository_uuid_marker).unwrap();
    std::fs::remove_dir_all(&preparation_cache).unwrap();

    LegacyRootFixture {
        repository,
        host_state,
        preparation_cache,
        worktree_root,
        worktree,
        database,
        repository_uuid_marker,
        root_marker,
    }
}

#[derive(serde::Serialize)]
struct V028WorktreeRootMarker {
    schema_version: u32,
    repository_key: String,
    repository_root: PathBuf,
}

/// A schema-52 database built from the released v0.8.28 migrations.
///
/// The current CLI is used only to create a fixture-owned linked checkout and
/// its auxiliary host root registration. The database is then replaced by the
/// frozen v0.8.28 migration SQL and an old-shaped session row. The marker is
/// serialized using the v0.8.28 field order; a retained producer-parity receipt
/// confirms this byte format against the pinned released CLI.
fn v028_schema52_root_fixture(owner: &Path) -> LegacyRootFixture {
    let repository = owner.join("project");
    let host_state = owner.join("host-state");
    let preparation_cache = owner.join("preparation-cache");
    fixture_at(&repository);
    std::fs::create_dir_all(&host_state).unwrap();
    std::fs::create_dir_all(&preparation_cache).unwrap();

    let started = json(run_with_storage_paths(
        &repository,
        &host_state,
        &preparation_cache,
        &["start", "--task", "schema52 fixture bootstrap", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let worktree_root = PathBuf::from(started["worktree_placement"]["root"].as_str().unwrap());
    let database = repository.join(".aethyme/broker.db");
    let root_marker = worktree_root.join(".aethyme-worktree-root.json");
    let repository_uuid_marker = repository.join(".git/aethyme-repository-uuid");
    assert!(worktree.is_dir());
    assert!(root_marker.is_file());

    let prior_marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&root_marker).unwrap()).unwrap();
    let marker = V028WorktreeRootMarker {
        schema_version: 1,
        repository_key: prior_marker["repository_key"].as_str().unwrap().to_owned(),
        repository_root: repository.canonicalize().unwrap(),
    };
    let mut marker_bytes = serde_json::to_vec_pretty(&marker).unwrap();
    marker_bytes.push(b'\n');
    assert_eq!(
        std::fs::read(&root_marker).unwrap(),
        marker_bytes,
        "the current marker writer must match the frozen v0.8.28 serialization model"
    );
    std::fs::write(&root_marker, &marker_bytes).unwrap();

    if repository_uuid_marker.exists() {
        std::fs::remove_file(&repository_uuid_marker).unwrap();
    }
    if preparation_cache.exists() {
        std::fs::remove_dir_all(&preparation_cache).unwrap();
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", database.display()));
        if sidecar.exists() {
            std::fs::remove_file(sidecar).unwrap();
        }
    }
    if database.exists() {
        std::fs::remove_file(&database).unwrap();
    }

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(include_str!("fixtures/v028-schema52.sql"))
        .unwrap();
    let branch = started["branch"].as_str().unwrap();
    let worktree_text = worktree.to_string_lossy().into_owned();
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(head.status.success());
    let diff_base = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    let now = 1_792_638_398_165_i64;
    connection
        .execute(
            "INSERT INTO sessions (
                 worktree_path, branch, origin, status, task, diff_base,
                 adoption_base, adopted_head, repository_schema,
                 deployment_state_digest, aethyme_version, gate_definition_digest,
                 repository_contract_backfilled, pid, command, log_path,
                 agent_identity, repository_name, tab_name, ai_provider,
                 short_name, created_at, updated_at, last_activity_at
             ) VALUES (
                 ?1, ?2, 'spawned', 'active', 'v0.8.28 retained session',
                 ?3, ?3, ?3, NULL,
                 'a332ca058fadabb85cc900a167a99df743dd0aa2db949edee7ad064a3ada8343',
                 '0.8.28', NULL, 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                 'legacy fixture', ?4, ?4, ?4
             )",
            rusqlite::params![worktree_text, branch, diff_base, now],
        )
        .unwrap();
    let session_id = connection.last_insert_rowid();
    let registered_payload = serde_json::json!({
        "origin": "spawned",
        "branch": branch,
        "worktree_path": worktree.to_string_lossy(),
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

    LegacyRootFixture {
        repository,
        host_state,
        preparation_cache,
        worktree_root,
        worktree,
        database,
        repository_uuid_marker,
        root_marker,
    }
}

type EventRow = (i64, i64, i64, String, Option<i64>, Option<String>);

fn event_rows(database: &Path) -> Vec<EventRow> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json FROM events ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn read_meta(database: &Path, key: &str) -> String {
    rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
    .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .unwrap()
}

fn has_table(database: &Path, table: &str) -> bool {
    rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
    .query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn session_count_for_worktree(database: &Path, worktree: &Path) -> i64 {
    rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
    .query_row(
        "SELECT count(*) FROM sessions WHERE worktree_path = ?1",
        [worktree.to_string_lossy().as_ref()],
        |row| row.get(0),
    )
    .unwrap()
}

fn session_rows(database: &Path) -> Vec<Vec<rusqlite::types::Value>> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let mut statement = connection
        .prepare("SELECT * FROM sessions ORDER BY id")
        .unwrap();
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get::<_, rusqlite::types::Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn checkout_limit_counts_other_roots_and_preserves_reuse_without_allocating() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        "[worktrees]\nmax_checkouts = 2\n",
    )
    .unwrap();
    let started = json(run_with_host_state(
        &repo,
        &state,
        &["start", "--task", "first", "--json"],
    ));
    let existing = started["worktree_path"].as_str().unwrap();
    let other = tmp.path().join("another-root/checkout");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            other.to_str().unwrap(),
            "HEAD",
        ],
    );
    let before = git_output(&repo, &["worktree", "list", "--porcelain"]);
    let refused = run(&repo, &state, &["start", "--task", "over limit", "--json"]);
    assert!(!refused.status.success());
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        error.contains("linked checkout limit reached (2/2)"),
        "{error}"
    );
    assert!(error.contains("--reuse"), "{error}");
    assert_eq!(
        git_output(&repo, &["worktree", "list", "--porcelain"]),
        before
    );
    assert!(other.exists(), "admission never removes another checkout");
    let reused = json(run(
        &repo,
        &state,
        &["start", "--reuse", existing, "--task", "reuse", "--json"],
    ));
    assert_eq!(reused["outcome"], "reused");
    assert_eq!(
        git_output(&repo, &["worktree", "list", "--porcelain"]),
        before
    );
}

#[test]
fn explicit_zero_checkout_limit_permits_new_allocations() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        "[worktrees]\nmax_checkouts = 0\n",
    )
    .unwrap();
    for task in ["first", "second"] {
        json(run_with_host_state(
            &repo,
            &state,
            &["start", "--task", task, "--json"],
        ));
    }
    assert_eq!(
        git_output(&repo, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        3,
        "two linked checkouts plus the primary"
    );
}

#[test]
fn start_places_worktree_outside_repository_under_a_private_owned_root() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);

    let started = json(run_with_host_state(
        &repo,
        &state,
        &["start", "--task", "scanner isolation", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let root = PathBuf::from(started["worktree_placement"]["root"].as_str().unwrap());

    assert!(worktree.starts_with(state.canonicalize().unwrap()));
    assert!(!worktree.starts_with(repo.canonicalize().unwrap()));
    assert_eq!(worktree.parent(), Some(root.as_path()));
    assert_eq!(started["worktree_placement"]["source"], "host_state");
    assert_eq!(started["worktree_placement"]["outside_repository"], true);

    let marker = root.join(".aethyme-worktree-root.json");
    let marker_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(marker_json["schema_version"], 1);
    assert_eq!(
        marker_json["repository_root"],
        repo.canonicalize().unwrap().to_string_lossy().as_ref()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn worktree_root_plan_is_read_only_and_distinguishes_same_named_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("one/project");
    let second = tmp.path().join("two/project");
    let state = tmp.path().join("not-created");
    fixture_at(&first);
    fixture_at(&second);

    let first_plan = json(run(
        &first,
        &state,
        &["advanced", "worktree-root", "--json"],
    ));
    let second_plan = json(run(
        &second,
        &state,
        &["advanced", "worktree-root", "--json"],
    ));

    assert_ne!(first_plan["repository_key"], second_plan["repository_key"]);
    assert!(
        first_plan["repository_key"]
            .as_str()
            .unwrap()
            .starts_with("project-")
    );
    assert!(!state.exists(), "planning must not create host state");
}

#[test]
fn starts_from_a_broker_worktree_create_siblings_instead_of_nesting() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);

    let first = json(run_with_host_state(
        &repo,
        &state,
        &["start", "--task", "first", "--json"],
    ));
    let first_path = PathBuf::from(first["worktree_path"].as_str().unwrap());
    let second = json(run(
        &first_path,
        &state,
        &["start", "--task", "second", "--json"],
    ));
    let second_path = PathBuf::from(second["worktree_path"].as_str().unwrap());

    assert_eq!(first_path.parent(), second_path.parent());
    assert!(!second_path.starts_with(&first_path));
}

#[test]
fn explicit_root_inside_a_linked_worktree_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    let cache = tmp.path().join("cache");
    let database = tmp.path().join("broker.db");
    fixture_at(&repo);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let linked_checkout = tmp.path().join("unmanaged-checkout");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/unmanaged-checkout",
            linked_checkout.to_str().unwrap(),
            "main",
        ],
    );
    let plan = json(run_with_new_database_and_root(
        &repo,
        &state,
        &cache,
        &database,
        &linked_checkout,
        &["advanced", "worktree-root", "--json"],
    ));
    let selected_root = PathBuf::from(plan["preferred_root"].as_str().unwrap());
    assert!(selected_root.starts_with(&linked_checkout));
    assert!(!selected_root.exists());
    let output = run_with_new_database_and_root(
        &repo,
        &state,
        &cache,
        &database,
        &linked_checkout,
        &["start", "--task", "nested", "--json"],
    );

    assert!(!output.status.success());
    assert!(
        json_error::message(&output).contains("refusing nested broker worktree path"),
        "{}",
        json_error::message(&output)
    );
    assert!(!selected_root.exists(), "refused nested root was allocated");
}

#[test]
fn host_state_inside_a_linked_worktree_falls_back_without_creating_its_worktrees_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let linked_checkout = tmp.path().join("unmanaged-checkout");
    let cache = tmp.path().join("cache");
    let database = tmp.path().join("broker.db");
    let available_bytes = tmp.path().join("available-bytes");
    fixture_at(&repo);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/unmanaged-checkout",
            linked_checkout.to_str().unwrap(),
            "main",
        ],
    );
    let linked_checkout = linked_checkout.canonicalize().unwrap();
    let host_state = linked_checkout.join(".aethyme/host-state");
    std::fs::create_dir_all(&host_state).unwrap();
    let host_state = host_state.canonicalize().unwrap();
    std::fs::create_dir_all(&cache).unwrap();

    let plan = json(
        common::broker_cli(CLI, &["advanced", "worktree-root", "--json"])
            .current_dir(&repo)
            .env("AETHYME_HOST_STATE_DIR", &host_state)
            .env("AETHYME_HOST_CACHE_DIR", &cache)
            .env("AETHYME_BROKER_DB", &database)
            .env_remove("AETHYME_WORKTREE_ROOT")
            .env_remove("AETHYME_GATE_BROKER_DATABASES")
            .output()
            .unwrap(),
    );
    let selected_root = PathBuf::from(plan["preferred_root"].as_str().unwrap());
    let worktrees_parent = host_state.join("worktrees");
    assert!(selected_root.starts_with(&host_state));
    assert!(
        !worktrees_parent.exists(),
        "planning created the nested parent"
    );

    std::fs::write(&available_bytes, "68719476736\n").unwrap();
    let output = common::broker_cli(CLI, &["start", "--task", "nested host state", "--json"])
        .current_dir(&repo)
        .env("AETHYME_HOST_STATE_DIR", &host_state)
        .env("AETHYME_HOST_CACHE_DIR", &cache)
        .env("AETHYME_BROKER_DB", &database)
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env(
            aethyme_broker::TEST_AVAILABLE_BYTES_ENV,
            format!("@{}", available_bytes.display()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "start should use the safe repository fallback: {}",
        json_error::message(&output)
    );
    let started = json(output);
    assert_eq!(
        started["worktree_placement"]["source"],
        "repository_fallback"
    );
    assert!(
        started["worktree_placement"]["fallback_reason"]
            .as_str()
            .unwrap()
            .contains("refusing nested broker worktree path"),
        "{started}"
    );
    assert!(
        !worktrees_parent.exists(),
        "nested host-state worktrees parent was created"
    );
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(!worktree.starts_with(&linked_checkout));
    assert!(
        git_output(&linked_checkout, &["status", "--porcelain"]).is_empty(),
        "linked checkout changed"
    );
}

#[test]
fn unavailable_host_placement_uses_reported_registered_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let state = tmp.path().join("state");
    fixture_at(&repo);
    std::fs::create_dir(&state).unwrap();
    // Placement can fail while its independent coordination domain remains
    // healthy. Losing the entire host-state domain is a different failure.
    std::fs::write(state.join("worktrees"), "blocked\n").unwrap();

    let started = json(run(
        &repo,
        &state,
        &["start", "--task", "fallback", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());

    assert!(worktree.starts_with(repo.canonicalize().unwrap().join(".aethyme/worktrees")));
    assert_eq!(
        started["worktree_placement"]["source"],
        "repository_fallback"
    );
    assert_eq!(started["worktree_placement"]["outside_repository"], false);
    assert!(
        started["worktree_placement"]["fallback_reason"]
            .as_str()
            .unwrap()
            .contains("cannot prepare broker worktree root")
    );
    let registry = std::fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("worktree-roots-")
        })
        .unwrap()
        .join("registry.sqlite3");
    let history =
        rusqlite::Connection::open_with_flags(registry, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let payload: String = history
        .query_row("SELECT payload FROM roots", [], |row| row.get(0))
        .unwrap();
    let retained: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        retained["root"],
        worktree.parent().unwrap().to_str().unwrap()
    );
    assert_eq!(retained["source"], "repository_fallback");
}

#[test]
fn unavailable_history_refuses_before_fallback_root_checkout_or_agent_creation() {
    for agent in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("project");
        let blocked_state = tmp.path().join("not-a-directory");
        let sentinel = tmp.path().join("agent-ran");
        fixture_at(&repo);
        std::fs::write(&blocked_state, "preserve blocker\n").unwrap();
        let args = if agent {
            vec![
                "start",
                "--task",
                "unavailable history",
                "--cmd",
                "printf ran > \"$AETHYME_TEST_AGENT_SENTINEL\"",
                "--json",
            ]
        } else {
            vec!["start", "--task", "unavailable history", "--json"]
        };
        let output = common::broker_cli(CLI, &args)
            .current_dir(&repo)
            .env("AETHYME_HOST_STATE_DIR", &blocked_state)
            .env_remove("AETHYME_WORKTREE_ROOT")
            .env("AETHYME_TEST_AGENT_SENTINEL", &sentinel)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = json_error::message(&output);
        assert!(
            error.contains("cannot retain worktree storage discovery"),
            "{error}"
        );
        assert!(
            !repo.join(".aethyme/worktrees").exists(),
            "history refusal must precede root creation"
        );
        assert!(!sentinel.exists(), "unadmitted agent executed");
        assert_eq!(
            std::fs::read_to_string(&blocked_state).unwrap(),
            "preserve blocker\n"
        );
        let branches = Command::new("git")
            .args(["for-each-ref", "--format=%(refname)", "refs/heads/agent/"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(branches.status.success());
        assert!(branches.stdout.is_empty(), "unadmitted branch was created");
        let database = rusqlite::Connection::open_with_flags(
            repo.join(".aethyme/broker.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let sessions: u64 = database
            .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 0, "unadmitted session was registered");
    }
}

#[test]
fn cleanup_accepts_a_worktree_owned_by_the_external_root_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("project");
    let root = tmp.path().join("owned-worktrees");
    fixture_at(&repo);

    let mut broker = common::open_fixture_broker(&repo, tmp.path())
        .unwrap()
        .with_worktree_root(&root);
    let session = broker
        .start_worktree("cleanup external checkout", None)
        .unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    assert!(worktree.exists());

    broker.close(session.id).unwrap();
    broker.cleanup(session.id, false).unwrap();

    assert!(!worktree.exists());
    assert!(root.join(".aethyme-worktree-root.json").is_file());
}

/// A placement override must not bypass the disk reserve or launch an agent
/// before the checkout has been admitted. All state remains fixture-owned.
#[test]
fn low_space_refuses_host_and_environment_placement_before_checkout_or_agent_launch() {
    for override_root in [false, true] {
        for agent in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("project");
            let state = tmp.path().join("state");
            let cache = tmp.path().join("cache");
            let root_container = tmp.path().join("override-worktrees");
            let sentinel = tmp.path().join("agent-ran");
            fixture_at(&repo);
            std::fs::create_dir_all(&state).unwrap();
            std::fs::create_dir_all(&cache).unwrap();
            let selected_container = if override_root {
                root_container.clone()
            } else {
                state.join("worktrees")
            };
            std::fs::create_dir_all(&selected_container).unwrap();
            let mut plan_command =
                common::broker_cli(CLI, &["advanced", "worktree-root", "--json"]);
            plan_command
                .current_dir(&repo)
                .env("AETHYME_HOST_STATE_DIR", &state)
                .env("AETHYME_HOST_CACHE_DIR", &cache);
            if override_root {
                plan_command.env("AETHYME_WORKTREE_ROOT", &root_container);
            }
            let plan = json(plan_command.output().unwrap());
            let selected_root = PathBuf::from(plan["preferred_root"].as_str().unwrap());
            assert!(!selected_root.exists(), "the selected root starts absent");
            let args = if agent {
                vec![
                    "start",
                    "--task",
                    "low space",
                    "--cmd",
                    "printf ran > \"$AETHYME_TEST_AGENT_SENTINEL\"",
                    "--json",
                ]
            } else {
                vec!["start", "--task", "low space", "--json"]
            };
            let mut command = common::broker_cli(CLI, &args);
            command
                .current_dir(&repo)
                .env("AETHYME_HOST_STATE_DIR", &state)
                .env("AETHYME_HOST_CACHE_DIR", &cache)
                .env_remove("AETHYME_BROKER_DB")
                .env_remove("AETHYME_GATE_BROKER_DATABASES")
                .env("AETHYME_TEST_AGENT_SENTINEL", &sentinel)
                .env(aethyme_broker::TEST_AVAILABLE_BYTES_ENV, "1024");
            if override_root {
                command.env("AETHYME_WORKTREE_ROOT", &root_container);
            }
            let output = command.output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.status.success(),
                "low-space start was admitted: {text}"
            );
            assert!(text.contains("broker recovery reserve"), "{text}");
            assert!(
                !selected_root.exists(),
                "selected worktree root allocated before admission"
            );
            assert!(
                !repo.join(".aethyme/worktrees").exists(),
                "fallback bypassed the reserve"
            );
            assert!(!sentinel.exists(), "an unadmitted agent executed");
            // Command metrics are emitted for refused commands too. Every
            // other entry, including any session agent log, is forbidden.
            assert!(
                std::fs::read_dir(repo.join(".aethyme/logs"))
                    .map(|mut entries| {
                        entries.all(|entry| entry.unwrap().file_name() == "command-metrics.jsonl")
                    })
                    .unwrap_or_else(|error| {
                        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                        true
                    }),
                "unadmitted agent allocated a log"
            );
            let branches = Command::new("git")
                .args(["for-each-ref", "--format=%(refname)", "refs/heads/agent/"])
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(branches.status.success());
            assert!(
                branches.stdout.is_empty(),
                "an unadmitted session branch was created"
            );
        }
    }
}

/// A remote refresh can consume the remaining space after initial placement.
/// Recheck the volume after the actual fetch and before worktree allocation.
#[cfg(unix)]
#[test]
fn a_refresh_cannot_spend_the_reserve_checked_before_it_started() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::Builder::new()
        .prefix("reserve path's ")
        .tempdir()
        .unwrap();
    let repo = tmp.path().join("project");
    let origin = tmp.path().join("origin.git");
    let state = tmp.path().join("state");
    let cache = tmp.path().join("cache");
    let root = tmp.path().join("worktrees");
    let bytes = tmp.path().join("available-bytes");
    let upload_pack = tmp.path().join("upload-pack");
    let sentinel = tmp.path().join("agent-ran");
    fixture_at(&repo);
    git(&repo, &["clone", "--bare", ".", origin.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["fetch", "-q", "origin"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let plan = json(
        common::broker_cli(CLI, &["advanced", "worktree-root", "--json"])
            .current_dir(&repo)
            .env("AETHYME_HOST_STATE_DIR", &state)
            .env("AETHYME_HOST_CACHE_DIR", &cache)
            .env("AETHYME_WORKTREE_ROOT", &root)
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_GATE_BROKER_DATABASES")
            .output()
            .unwrap(),
    );
    let actual_root = PathBuf::from(plan["preferred_root"].as_str().unwrap());
    assert!(!root.exists(), "read-only planning allocated the placement");
    // Environment overrides select an existing container; planning above did not create it.
    std::fs::create_dir_all(&root).unwrap();
    bind_managed_root_fixture(&repo, &state, &cache, &actual_root);
    std::fs::write(&bytes, "68719476736\n").unwrap();
    std::fs::write(
        &upload_pack,
        "#!/bin/sh\nprintf '0\\n' > \"$AETHYME_TEST_RESERVE_BYTES_PATH\" || exit 1\nexec git-upload-pack \"$@\"\n",
    ).unwrap();
    std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Git interprets uploadpack as a shell command, not an executable pathname.
    let quoted_upload_pack = upload_pack.to_str().unwrap().replace('\'', "'\\''");
    let upload_pack_command = format!("'{quoted_upload_pack}'");
    git(
        &repo,
        &["config", "remote.origin.uploadpack", &upload_pack_command],
    );
    let output = common::broker_cli(
        CLI,
        &[
            "start",
            "--task",
            "refresh consumes reserve",
            "--cmd",
            "printf ran > \"$AETHYME_TEST_AGENT_SENTINEL\"",
            "--json",
        ],
    )
    .current_dir(&repo)
    .env("AETHYME_HOST_STATE_DIR", &state)
    .env("AETHYME_HOST_CACHE_DIR", &cache)
    .env("AETHYME_WORKTREE_ROOT", &root)
    .env_remove("AETHYME_BROKER_DB")
    .env_remove("AETHYME_GATE_BROKER_DATABASES")
    .env("AETHYME_TEST_AGENT_SENTINEL", &sentinel)
    .env("AETHYME_TEST_RESERVE_BYTES_PATH", &bytes)
    .env(
        aethyme_broker::TEST_AVAILABLE_BYTES_ENV,
        format!("@{}", bytes.display()),
    )
    .output()
    .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&bytes).unwrap(),
        "0\n",
        "the actual refresh did not exercise the late headroom change: {text}"
    );
    assert!(
        !output.status.success(),
        "checkout allocated after reserve was spent: {text}"
    );
    assert!(text.contains("broker recovery reserve"), "{text}");
    assert!(!sentinel.exists(), "agent ran before late admission");
    // Refusal still records command metrics; it must not create an agent log.
    assert!(
        std::fs::read_dir(repo.join(".aethyme/logs"))
            .map(|mut entries| {
                entries.all(|entry| entry.unwrap().file_name() == "command-metrics.jsonl")
            })
            .unwrap_or_else(|error| {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                true
            }),
        "agent log allocated before late admission"
    );
    let mut entries: Vec<_> = std::fs::read_dir(&actual_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![
            std::ffi::OsString::from(".aethyme-worktree-root.json"),
            std::ffi::OsString::from(".cargo")
        ],
        "a checkout was created despite late refusal"
    );
    assert!(actual_root.join(".cargo/config.toml").is_file());
    let branches = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/agent/"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(branches.status.success());
    assert!(
        branches.stdout.is_empty(),
        "an unadmitted agent branch was created"
    );
}

/// Selecting a root is not authority to adopt unrelated files, bypass its
/// ownership witness, or change permissions before refusing it.
#[test]
fn unowned_or_invalid_roots_refuse_before_permissions_checkout_or_agent_changes() {
    for case in [
        "unmarked",
        "oversized",
        "broken-link",
        "valid-link",
        "writable-mode",
    ] {
        if cfg!(not(unix)) && (case.ends_with("link") || case == "writable-mode") {
            continue;
        }
        for agent in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("project");
            let state = tmp.path().join("state");
            let cache = tmp.path().join("cache");
            let container = tmp.path().join("worktrees");
            let sentinel = tmp.path().join("agent-ran");
            fixture_at(&repo);
            std::fs::create_dir_all(&state).unwrap();
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::create_dir_all(&container).unwrap();
            let plan = json(
                common::broker_cli(CLI, &["advanced", "worktree-root", "--json"])
                    .current_dir(&repo)
                    .env("AETHYME_HOST_STATE_DIR", &state)
                    .env("AETHYME_HOST_CACHE_DIR", &cache)
                    .env("AETHYME_WORKTREE_ROOT", &container)
                    .output()
                    .unwrap(),
            );
            let root = PathBuf::from(plan["preferred_root"].as_str().unwrap());
            if case == "unmarked" {
                std::fs::create_dir_all(&root).unwrap();
            } else {
                bind_managed_root_fixture(&repo, &state, &cache, &root);
            }
            let payload = root.join("preserve.txt");
            std::fs::write(&payload, b"unrelated contents").unwrap();
            let marker = root.join(".aethyme-worktree-root.json");
            let external = tmp.path().join("external-marker.json");
            if case == "unmarked" && marker.exists() {
                std::fs::remove_file(&marker).unwrap();
            }
            if case == "oversized" {
                let owner = serde_json::json!({
                    "schema_version": 1,
                    "repository_key": plan["repository_key"],
                    "repository_root": repo.canonicalize().unwrap(),
                });
                let mut bytes = serde_json::to_vec(&owner).unwrap();
                assert!(bytes.len() < 8193);
                bytes.resize(8193, b' ');
                // Valid JSON beyond the limit isolates the byte bound from
                // parsing and owner equality checks.
                assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_ok());
                std::fs::write(&marker, bytes).unwrap();
            }
            #[cfg(unix)]
            if case.ends_with("link") {
                assert!(
                    marker.is_file(),
                    "{case}: managed setup should create a regular marker"
                );
                std::fs::remove_file(&marker).unwrap();
                if case == "valid-link" {
                    let owner = serde_json::json!({
                        "schema_version": 1,
                        "repository_key": plan["repository_key"],
                        "repository_root": repo.canonicalize().unwrap(),
                    });
                    std::fs::write(&external, serde_json::to_vec(&owner).unwrap()).unwrap();
                }
                std::os::unix::fs::symlink(&external, &marker).unwrap();
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let expected_mode = match case {
                    "unmarked" | "oversized" => 0o750,
                    "writable-mode" => 0o770,
                    _ => 0o700,
                };
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(expected_mode))
                    .unwrap();
            }
            let before_entries: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            let before_marker = std::fs::read(&marker).ok();
            let before_external = std::fs::read(&external).ok();
            let sessions_before = sqlite_count_if_present(
                &repo.join(".aethyme/broker.db"),
                "SELECT count(*) FROM sessions",
            );
            let roots_before = worktree_root_registry_path(&state)
                .as_deref()
                .map(|path| sqlite_count_if_present(path, "SELECT count(*) FROM roots"))
                .unwrap_or(0);
            if case != "unmarked" {
                assert!(sessions_before > 0, "{case}: bound fixture has no sessions");
                assert!(
                    roots_before > 0,
                    "{case}: bound fixture has no registry root"
                );
            }
            let args = if agent {
                vec![
                    "start",
                    "--task",
                    "unowned root",
                    "--cmd",
                    "printf ran > \"$AETHYME_TEST_AGENT_SENTINEL\"",
                    "--json",
                ]
            } else {
                vec!["start", "--task", "unowned root", "--json"]
            };
            let output = common::broker_cli(CLI, &args)
                .current_dir(&repo)
                .env("AETHYME_HOST_STATE_DIR", &state)
                .env("AETHYME_HOST_CACHE_DIR", &cache)
                .env("AETHYME_WORKTREE_ROOT", &container)
                .env("AETHYME_TEST_AGENT_SENTINEL", &sentinel)
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.status.success(), "{case}/{agent}: {text}");
            let expected = match case {
                "unmarked" => "fresh binding refuses an existing root",
                "writable-mode" => "cannot verify bound worktree-root permissions",
                _ => "invalid ownership marker",
            };
            assert!(text.contains(expected), "{case}/{agent}: {text}");
            assert_eq!(std::fs::read(&payload).unwrap(), b"unrelated contents");
            assert_eq!(std::fs::read(&marker).ok(), before_marker);
            assert_eq!(std::fs::read(&external).ok(), before_external);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                    match case {
                        "unmarked" | "oversized" => 0o750,
                        "writable-mode" => 0o770,
                        _ => 0o700,
                    },
                    "{case}: refusing ownership must precede chmod"
                );
                if case.ends_with("link") {
                    assert!(
                        std::fs::symlink_metadata(&marker)
                            .unwrap()
                            .file_type()
                            .is_symlink()
                    );
                    assert_eq!(std::fs::read_link(&marker).unwrap(), external);
                }
            }
            let after_entries: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            let mut before_entries = before_entries;
            let mut after_entries = after_entries;
            before_entries.sort();
            after_entries.sort();
            assert_eq!(
                after_entries, before_entries,
                "root contents changed after refusal"
            );
            assert!(!sentinel.exists(), "unadmitted agent ran");
            let sessions_after = sqlite_count_if_present(
                &repo.join(".aethyme/broker.db"),
                "SELECT count(*) FROM sessions",
            );
            assert_eq!(
                sessions_after, sessions_before,
                "refused start registered a session"
            );
            let roots_after = worktree_root_registry_path(&state)
                .as_deref()
                .map(|path| sqlite_count_if_present(path, "SELECT count(*) FROM roots"))
                .unwrap_or(0);
            assert_eq!(
                roots_after, roots_before,
                "refused root changed ownership history"
            );
            let branches = Command::new("git")
                .args(["for-each-ref", "--format=%(refname)", "refs/heads/agent/"])
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(branches.status.success());
            assert!(branches.stdout.is_empty(), "unadmitted branch was created");
        }
    }
}

#[test]
fn synthetic_downgrade_status_leaves_database_root_and_cache_unchanged() {
    let owner = tempfile::tempdir().unwrap();
    let fixture = synthetic_downgrade_legacy_root_fixture(owner.path());
    assert_eq!(read_meta(&fixture.database, "schema_version"), "52");
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "52");
    assert!(!has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());

    let database_before = std::fs::read(&fixture.database).unwrap();
    let root_entries_before = std::fs::read_dir(&fixture.worktree_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let root_marker_before = std::fs::read(&fixture.root_marker).unwrap();
    let retained_worktree_file = fixture.worktree.join("legacy-retained.txt");
    let retained_worktree_before = std::fs::read(&retained_worktree_file).unwrap();
    let session_rows_before = session_rows(&fixture.database);

    let _status = snapshot_status(&fixture.repository);

    assert_eq!(std::fs::read(&fixture.database).unwrap(), database_before);
    assert_eq!(read_meta(&fixture.database, "schema_version"), "52");
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "52");
    assert_eq!(
        read_meta(&fixture.database, "fixture.legacy-retained"),
        "kept"
    );
    assert!(!has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(
        std::fs::read(&fixture.root_marker).unwrap(),
        root_marker_before
    );
    assert_eq!(
        std::fs::read_dir(&fixture.worktree_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        root_entries_before
    );
    assert_eq!(
        std::fs::read(retained_worktree_file).unwrap(),
        retained_worktree_before
    );
    assert_eq!(
        session_rows(&fixture.database),
        session_rows_before,
        "read-only status must not change any retained session row"
    );
    assert_eq!(
        session_count_for_worktree(&fixture.database, &fixture.worktree),
        1
    );
}

#[test]
fn synthetic_downgrade_plan_apply_preserves_existing_sessions_and_allows_start_and_adopt() {
    let owner = tempfile::tempdir().unwrap();
    let fixture = synthetic_downgrade_legacy_root_fixture(owner.path());
    let root_marker_before = std::fs::read(&fixture.root_marker).unwrap();
    let root_entries_before = std::fs::read_dir(&fixture.worktree_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let retained_worktree_file = fixture.worktree.join("legacy-retained.txt");
    let session_rows_before = session_rows(&fixture.database);

    let refused_start = run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &["start", "--task", "must review legacy root", "--json"],
    );
    let refusal_text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused_start.stdout),
        String::from_utf8_lossy(&refused_start.stderr)
    );
    assert!(!refused_start.status.success(), "{refusal_text}");
    assert!(refusal_text.contains("legacy"), "{refusal_text}");
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert_eq!(
        read_meta(&fixture.database, "fixture.legacy-retained"),
        "kept"
    );
    assert!(has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(
        std::fs::read(&fixture.root_marker).unwrap(),
        root_marker_before
    );
    assert_eq!(
        std::fs::read(&retained_worktree_file).unwrap(),
        b"keep this checkout\n"
    );
    assert_eq!(
        session_rows(&fixture.database),
        session_rows_before,
        "refused start preserves legacy session records"
    );
    assert_eq!(
        session_count_for_worktree(&fixture.database, &fixture.worktree),
        1
    );

    let root_arg = fixture.worktree_root.to_string_lossy().into_owned();
    let plan_args = [
        "advanced",
        "worktree-root",
        "binding",
        "plan",
        root_arg.as_str(),
        "keyed_container",
        "--json",
    ];
    let plan = json(run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &plan_args,
    ));
    assert_eq!(plan["worktree_root"], root_arg);
    assert_eq!(plan["layout"], "keyed_container");
    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(
        std::fs::read(&fixture.root_marker).unwrap(),
        root_marker_before
    );
    assert_eq!(
        std::fs::read_dir(&fixture.worktree_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        root_entries_before
    );
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert_eq!(session_rows(&fixture.database), session_rows_before);

    let apply_args = [
        "advanced",
        "worktree-root",
        "binding",
        "apply",
        root_arg.as_str(),
        "keyed_container",
        "--confirm",
        digest,
        "--json",
    ];
    let binding = json(run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &apply_args,
    ));
    assert_eq!(binding["state"], "bound");
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert_eq!(
        read_meta(&fixture.database, "fixture.legacy-retained"),
        "kept"
    );
    assert!(fixture.repository_uuid_marker.is_file());
    assert_eq!(
        std::fs::read(&fixture.root_marker).unwrap(),
        root_marker_before
    );
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(
        std::fs::read(&retained_worktree_file).unwrap(),
        b"keep this checkout\n"
    );
    assert_eq!(session_rows(&fixture.database), session_rows_before);
    assert_eq!(
        session_count_for_worktree(&fixture.database, &fixture.worktree),
        1
    );

    std::fs::create_dir_all(&fixture.preparation_cache).unwrap();
    let started = json(run_with_storage_paths(
        &fixture.repository,
        &fixture.host_state,
        &fixture.preparation_cache,
        &["start", "--task", "start after reviewed binding", "--json"],
    ));
    assert_eq!(
        started["worktree_placement"]["root"], root_arg,
        "start keeps using the reviewed legacy root"
    );
    assert!(fixture.preparation_cache.is_dir());
    assert_eq!(
        std::fs::read(&retained_worktree_file).unwrap(),
        b"keep this checkout\n"
    );
    assert!(session_rows(&fixture.database).starts_with(&session_rows_before));
    assert_eq!(
        session_count_for_worktree(&fixture.database, &fixture.worktree),
        1
    );

    let checkout_owner = tempfile::tempdir().unwrap();
    let checkout = checkout_owner.path().join("adopted");
    git(
        &fixture.repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/adopt-after-binding",
            checkout.to_str().unwrap(),
            "main",
        ],
    );
    let checkout_arg = checkout.to_string_lossy().into_owned();
    let adopted = json(run_with_storage_paths(
        &fixture.repository,
        &fixture.host_state,
        &fixture.preparation_cache,
        &[
            "start",
            "--adopt",
            checkout_arg.as_str(),
            "--task",
            "adopt after reviewed binding",
            "--json",
        ],
    ));
    assert_eq!(
        PathBuf::from(adopted["worktree_path"].as_str().unwrap()),
        checkout
    );
    assert_eq!(
        std::fs::read(&retained_worktree_file).unwrap(),
        b"keep this checkout\n"
    );
    assert!(session_rows(&fixture.database).starts_with(&session_rows_before));
    assert_eq!(
        session_count_for_worktree(&fixture.database, &fixture.worktree),
        1
    );
    let total_sessions: i64 = rusqlite::Connection::open_with_flags(
        &fixture.database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
    .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
    .unwrap();
    assert!(
        total_sessions >= 3,
        "legacy and new session rows must be retained"
    );
}

#[cfg(unix)]
#[test]
fn recovery_plan_inspects_incomplete_repository_uuid_marker_without_settling_it() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let owner = tempfile::tempdir().unwrap();
    let repository = owner.path().join("project");
    let host_state = owner.path().join("host-state");
    let preparation_cache = owner.path().join("preparation-cache");
    fixture_at(&repository);
    std::fs::create_dir_all(&host_state).unwrap();
    std::fs::create_dir_all(&preparation_cache).unwrap();
    let started = json(run_with_storage_paths(
        &repository,
        &host_state,
        &preparation_cache,
        &["start", "--task", "recovery plan prefix", "--json"],
    ));
    let database = repository.join(".aethyme/broker.db");
    let marker = repository.join(".git/aethyme-repository-uuid");
    let worktree_root = PathBuf::from(started["worktree_placement"]["root"].as_str().unwrap());
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(marker.is_file());
    assert!(worktree.starts_with(&worktree_root));
    // Recreate the supported interrupted-publication prefix: the reservation,
    // root marker, Cargo defaults and registry row remain, with no checkout yet.
    std::fs::remove_dir_all(&worktree).unwrap();

    let connection = rusqlite::Connection::open(&database).unwrap();
    let changed = connection
        .execute(
            "UPDATE repository_worktree_binding
             SET state = 'fresh_reserved', repository_uuid = NULL,
                 worktree_identity_json = NULL
             WHERE binding_id = 1 AND state = 'bound'",
            [],
        )
        .unwrap();
    assert_eq!(changed, 1);
    drop(connection);

    let temporary =
        repository.join(".git/.aethyme-repository-uuid.0123456789abcdef0123456789abcdef.tmp");
    let expected_marker = b"AETHYME-REPO-UUID-V1\nf47ac10b-58cc-4372-a567-0e02b2c3d479\n";
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(expected_marker).unwrap();
    file.sync_all().unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert!(!marker.exists());

    let plan = json(run_with_storage_paths(
        &repository,
        &host_state,
        &preparation_cache,
        &["advanced", "worktree-root", "recovery", "plan", "--json"],
    ));
    assert_eq!(plan["binding"]["state"], "fresh_reserved");
    assert_eq!(plan["can_apply"], true);
    assert!(
        !marker.exists(),
        "planning must not settle the marker prefix"
    );
    assert_eq!(std::fs::read(&temporary).unwrap(), expected_marker);
    // Fresh staged storage has the v60 task-operation-guard compatibility floor.
    assert_eq!(read_meta(&database, "min_compatible_schema"), "60");
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let (state, repository_uuid): (String, Option<String>) = connection
        .query_row(
            "SELECT state, repository_uuid FROM repository_worktree_binding
             WHERE binding_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "fresh_reserved");
    assert_eq!(repository_uuid, None);
    assert_eq!(started["worktree_placement"]["root"], plan["worktree_root"]);
}

#[test]
fn v028_schema52_snapshot_status_preserves_the_released_session_root_and_cache() {
    let owner = tempfile::tempdir().unwrap();
    let fixture = v028_schema52_root_fixture(owner.path());
    assert_eq!(read_meta(&fixture.database, "schema_version"), "52");
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "52");
    assert!(!has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());

    let database_before = std::fs::read(&fixture.database).unwrap();
    let sessions_before = session_rows(&fixture.database);
    let events_before = event_rows(&fixture.database);
    let marker_before = std::fs::read(&fixture.root_marker).unwrap();
    let root_entries_before = std::fs::read_dir(&fixture.worktree_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let _status = snapshot_status(&fixture.repository);

    assert_eq!(std::fs::read(&fixture.database).unwrap(), database_before);
    assert_eq!(read_meta(&fixture.database, "schema_version"), "52");
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "52");
    assert!(!has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(std::fs::read(&fixture.root_marker).unwrap(), marker_before);
    assert_eq!(session_rows(&fixture.database), sessions_before);
    assert_eq!(event_rows(&fixture.database), events_before);
    assert_eq!(
        std::fs::read_dir(&fixture.worktree_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>(),
        root_entries_before
    );
}

#[test]
fn v028_schema52_first_write_requires_reviewed_binding_and_preserves_the_ledger() {
    let owner = tempfile::tempdir().unwrap();
    let fixture = v028_schema52_root_fixture(owner.path());
    let sessions_before = session_rows(&fixture.database);
    let events_before = event_rows(&fixture.database);
    let marker_before = std::fs::read(&fixture.root_marker).unwrap();
    let original_worktree_file = fixture.worktree.join("README.md");
    let original_worktree_file_before = std::fs::read(&original_worktree_file).unwrap();

    let refused_start = run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &["start", "--task", "must review legacy root", "--json"],
    );
    let refusal_text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused_start.stdout),
        String::from_utf8_lossy(&refused_start.stderr)
    );
    assert!(!refused_start.status.success(), "{refusal_text}");
    assert!(refusal_text.contains("legacy"), "{refusal_text}");
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert!(has_table(&fixture.database, "repository_worktree_binding"));
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(session_rows(&fixture.database), sessions_before);
    let events_after_refusal = event_rows(&fixture.database);
    assert!(events_after_refusal.starts_with(&events_before));
    assert_eq!(events_after_refusal.len(), events_before.len() + 1);
    let failed_start = events_after_refusal.last().unwrap();
    assert_eq!(failed_start.3, "broker.command.failed");
    let failed_start_payload: serde_json::Value =
        serde_json::from_str(failed_start.5.as_deref().unwrap()).unwrap();
    assert_eq!(failed_start_payload["command_surface"], "broker.start");
    assert_eq!(failed_start_payload["failure_class"], "command_failed");
    assert_eq!(std::fs::read(&fixture.root_marker).unwrap(), marker_before);
    assert_eq!(
        std::fs::read(&original_worktree_file).unwrap(),
        original_worktree_file_before
    );

    let root_arg = fixture.worktree_root.to_string_lossy().into_owned();
    let plan = json(run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &[
            "advanced",
            "worktree-root",
            "binding",
            "plan",
            root_arg.as_str(),
            "keyed_container",
            "--json",
        ],
    ));
    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert!(!fixture.repository_uuid_marker.exists());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(session_rows(&fixture.database), sessions_before);
    assert_eq!(event_rows(&fixture.database), events_after_refusal);
    assert_eq!(std::fs::read(&fixture.root_marker).unwrap(), marker_before);

    let bound = json(run_without_preparation_cache(
        &fixture.repository,
        &fixture.host_state,
        &[
            "advanced",
            "worktree-root",
            "binding",
            "apply",
            root_arg.as_str(),
            "keyed_container",
            "--confirm",
            digest,
            "--json",
        ],
    ));
    assert_eq!(bound["state"], "bound");
    assert_eq!(
        read_meta(&fixture.database, "schema_version"),
        aethyme_broker::BROKER_STORAGE_CURRENT_SCHEMA.to_string()
    );
    // The v60 compatibility floor fences pre-task-operation-guard writers.
    assert_eq!(read_meta(&fixture.database, "min_compatible_schema"), "60");
    assert!(fixture.repository_uuid_marker.is_file());
    assert!(!fixture.preparation_cache.exists());
    assert_eq!(session_rows(&fixture.database), sessions_before);
    assert_eq!(event_rows(&fixture.database), events_after_refusal);
    assert_eq!(std::fs::read(&fixture.root_marker).unwrap(), marker_before);

    std::fs::create_dir_all(&fixture.preparation_cache).unwrap();
    let started = json(run_with_storage_paths(
        &fixture.repository,
        &fixture.host_state,
        &fixture.preparation_cache,
        &["start", "--task", "start after binding", "--json"],
    ));
    assert_eq!(started["worktree_placement"]["root"], root_arg);
    assert!(fixture.preparation_cache.is_dir());
    assert!(session_rows(&fixture.database).starts_with(&sessions_before));
    assert!(event_rows(&fixture.database).starts_with(&events_after_refusal));

    let checkout_owner = tempfile::tempdir().unwrap();
    let checkout = checkout_owner.path().join("adopted");
    git(
        &fixture.repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/v028-adopt-after-binding",
            checkout.to_str().unwrap(),
            "main",
        ],
    );
    let checkout_arg = checkout.to_string_lossy().into_owned();
    let adopted = json(run_with_storage_paths(
        &fixture.repository,
        &fixture.host_state,
        &fixture.preparation_cache,
        &[
            "start",
            "--adopt",
            checkout_arg.as_str(),
            "--task",
            "adopt after binding",
            "--json",
        ],
    ));
    assert_eq!(
        PathBuf::from(adopted["worktree_path"].as_str().unwrap()),
        checkout
    );
    assert!(session_rows(&fixture.database).starts_with(&sessions_before));
    assert!(event_rows(&fixture.database).starts_with(&events_after_refusal));
}

#[path = "common/json_error.rs"]
mod json_error;
