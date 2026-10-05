//! #361: a gate runs binaries built from the tree under test, and those may
//! carry a migration no reviewed branch has. A gate must never be the write
//! that moves the operator's shared broker database.
//!
//! The gate children here are this test binary re-entered through
//! [`gate_child`], so the resolution they exercise is the crate's own code --
//! what a binary built from a dirty worktree would contain -- and the
//! "migration" is a schema bump past anything this crate ships.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{BROKER_DB_RELPATH, GitRepo, SCHEMA_VERSION};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const CHILD_MODE: &str = "AETHYME_TEST_GATE_CHILD";
const CHILD_RECORD: &str = "AETHYME_TEST_GATE_CHILD_RECORD";
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
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn run(repo: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .env_remove(aethyme_broker::BROKER_DB_ENV)
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join("tracked.txt"), "first\n").unwrap();
    std::fs::write(
        tmp.path().join(".gitignore"),
        ".aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n*.record\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[graph]\nauthority='disabled'\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join(".aethyme/gates.toml"), "").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "fixture"]);
    let adopted = run(
        tmp.path(),
        &["start", "--adopt", "--task", "live operator", "--json"],
    );
    assert!(adopted.status.success(), "{adopted:?}");
    tmp
}

fn shell(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

/// A gate step that re-enters this binary as [`gate_child`].
fn child_step(mode: &str, record: &Path, env_prefix: &str) -> String {
    format!(
        "{env_prefix} {CHILD_MODE}={mode} {CHILD_RECORD}={} {} --exact gate_child --ignored --quiet",
        shell(record),
        shell(&std::env::current_exe().unwrap()),
    )
}

fn write_gate(repo: &Path, command: &str) {
    std::fs::write(
        repo.join(".aethyme/gates.toml"),
        format!(
            "[[gate]]\nname = 'contract'\ncommand = {}\n",
            serde_json::to_string(command).unwrap()
        ),
    )
    .unwrap();
}

fn gates_run(repo: &Path) -> (bool, serde_json::Value) {
    let output = run(
        repo,
        &["advanced", "gates", "run", "--all", "--no-cache", "--json"],
    );
    let result = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    (output.status.success(), result)
}

struct HostState {
    schema_version: String,
    child_writes: i64,
    live_tasks: Vec<String>,
}

fn host_state(repo: &Path) -> HostState {
    let db = rusqlite::Connection::open_with_flags(
        repo.join(BROKER_DB_RELPATH),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let live_tasks = aethyme_broker::BrokerStore::open_snapshot_at(&repo.join(BROKER_DB_RELPATH))
        .unwrap()
        .live_sessions()
        .unwrap()
        .into_iter()
        .filter_map(|session| session.task)
        .collect();
    HostState {
        schema_version: db
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        child_writes: db
            .query_row(
                "SELECT count(*) FROM meta WHERE key = 'gate-child-wrote'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        live_tasks,
    }
}

fn assert_host_untouched(repo: &Path) {
    let state = host_state(repo);
    assert_eq!(state.schema_version, SCHEMA_VERSION.to_string());
    assert_eq!(state.child_writes, 0);
    assert_eq!(state.live_tasks, vec!["live operator".to_string()]);
}

fn record(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Resolution as binaries built before gate scopes existed performed it: the
/// override verbatim, else the checkout's own database.
fn legacy_resolution(cwd: &Path) -> PathBuf {
    match std::env::var_os(aethyme_broker::BROKER_DB_ENV).filter(|value| !value.is_empty()) {
        Some(pinned) => PathBuf::from(pinned),
        None => GitRepo::discover(cwd)
            .unwrap()
            .main_root()
            .unwrap()
            .join(BROKER_DB_RELPATH),
    }
}

/// Not a test: the process a gate spawns. `current` resolves with this tree's
/// code, `legacy` as an older binary would; both then open whatever they
/// resolved, migrate it past [`SCHEMA_VERSION`], and write a row -- the worst a
/// dirty-tree build can do to storage it is handed.
#[test]
#[ignore = "re-entered as a gate child by the other tests in this file"]
fn gate_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let record = PathBuf::from(std::env::var_os(CHILD_RECORD).unwrap());
    let cwd = std::env::current_dir().unwrap();
    let resolved = match mode.as_str() {
        "current" => {
            aethyme_broker::broker_db_path(&GitRepo::discover(&cwd).unwrap().main_root().unwrap())
                .map_err(|error| error.to_string())
        }
        "legacy" => Ok(legacy_resolution(&cwd)),
        other => panic!("unknown gate child mode {other}"),
    };
    let path = match resolved {
        Ok(path) => path,
        Err(refusal) => {
            std::fs::write(&record, format!("refused: {refusal}")).unwrap();
            return;
        }
    };
    drop(aethyme_broker::BrokerStore::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
        [(SCHEMA_VERSION + 1).to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('gate-child-wrote', '1')",
        [],
    )
    .unwrap();
    std::fs::write(&record, path.to_string_lossy().as_bytes()).unwrap();
}

#[test]
fn a_dirty_tree_migration_in_a_gate_lands_only_in_its_disposable_database() {
    let repo = fixture();
    // Uncommitted, like the session worktree that shipped the real migration.
    std::fs::write(repo.path().join("tracked.txt"), "dirty schema change\n").unwrap();
    let inherited = repo.path().join("inherited.record");
    let removed = repo.path().join("removed.record");
    let pinned = repo.path().join("pinned.record");
    let host_db = repo.path().canonicalize().unwrap().join(BROKER_DB_RELPATH);
    write_gate(
        repo.path(),
        &[
            child_step("current", &inherited, ""),
            // A child that strips the override used to fall through to the
            // checkout's shared database.
            child_step("current", &removed, "env -u AETHYME_BROKER_DB"),
            // One that names the shared database outright is refused.
            child_step(
                "current",
                &pinned,
                &format!("env AETHYME_BROKER_DB={}", shell(&host_db)),
            ),
        ]
        .join(" && "),
    );

    let (success, result) = gates_run(repo.path());
    assert!(success, "{result}");
    assert_eq!(result[0]["status"], "pass", "{result}");
    assert_host_untouched(repo.path());

    let target = &result[0]["broker_database"];
    assert_eq!(target["kind"], "disposable", "{result}");
    assert_eq!(target["retention"], "removed_on_gate_exit");
    assert_eq!(
        target["protected_repositories"],
        serde_json::json!([repo.path().canonicalize().unwrap()])
    );
    let disposable = PathBuf::from(target["path"].as_str().unwrap());
    assert_eq!(PathBuf::from(record(&inherited)), disposable);
    assert_eq!(PathBuf::from(record(&removed)), disposable);
    assert!(
        record(&pinned).starts_with("refused: gate refused the broker database"),
        "{}",
        record(&pinned)
    );
    assert!(!disposable.parent().unwrap().exists(), "not reclaimed");

    // The target is on record in the log before any of the command's output.
    let log = std::fs::read_to_string(result[0]["log_path"].as_str().unwrap()).unwrap();
    let line = log
        .lines()
        .find_map(|line| line.strip_prefix("aethyme gate environment: broker database "))
        .unwrap_or_else(|| panic!("no target line in {log}"));
    let logged: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(&logged, target);
}

#[test]
fn an_older_binary_that_keeps_the_gate_environment_stays_isolated() {
    let repo = fixture();
    let inherited = repo.path().join("legacy.record");
    write_gate(repo.path(), &child_step("legacy", &inherited, ""));

    let (success, result) = gates_run(repo.path());
    assert!(success, "{result}");
    assert_host_untouched(repo.path());
    assert_eq!(
        PathBuf::from(record(&inherited)),
        PathBuf::from(result[0]["broker_database"]["path"].as_str().unwrap())
    );
}

// Environment isolation cannot stop a child that discards its environment and
// predates the scope. What the gate can still guarantee is that such a run is
// never recorded as a verdict: it is invalidated, and the log says why.
#[test]
fn a_gate_whose_child_escapes_isolation_is_invalidated_not_passed() {
    let repo = fixture();
    let escaped = repo.path().join("escaped.record");
    write_gate(
        repo.path(),
        &child_step(
            "legacy",
            &escaped,
            "env -u AETHYME_BROKER_DB -u AETHYME_GATE_BROKER_DATABASES",
        ),
    );

    let (success, result) = gates_run(repo.path());
    assert!(!success, "{result}");
    assert_eq!(result[0]["status"], "error", "{result}");
    assert_eq!(result[0]["failure_class"], "environment", "{result}");
    let log = std::fs::read_to_string(result[0]["log_path"].as_str().unwrap()).unwrap();
    assert!(
        log.contains("aethyme invalidated this gate: shared broker database")
            && log.contains(&format!(
                "changed schema from {SCHEMA_VERSION} to {}",
                SCHEMA_VERSION + 1
            )),
        "{log}"
    );
}

#[test]
fn concurrent_gate_workers_receive_distinct_databases() {
    let repo = fixture();
    write_gate(
        repo.path(),
        r#"mkdir -p .aethyme/run/gate-workers
printf '%s %s\n' "$AETHYME_TEST_DB_SUFFIX" "$AETHYME_BROKER_DB" > "worker-$AETHYME_TEST_DB_SUFFIX.record"
touch ".aethyme/run/gate-workers/worker-$AETHYME_TEST_DB_SUFFIX.ready"
attempt=0
while [ "$attempt" -lt 5000 ]; do
    ready=$(find .aethyme/run/gate-workers -type f -name '*.ready' | wc -l)
    if [ "$ready" -ge 2 ]; then
        exit 0
    fi
    attempt=$((attempt + 1))
    sleep 0.01
done
echo 'timed out waiting for both gate workers' >&2
exit 1"#,
    );
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let repo = repo.path().to_path_buf();
            std::thread::spawn(move || {
                run(&repo, &["advanced", "gates", "run", "--all", "--no-cache"])
            })
        })
        .collect();
    for worker in workers {
        let output = worker.join().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(repo.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "record") {
            let line = record(&path);
            let (suffix, database) = line.trim().split_once(' ').unwrap();
            assert!(!suffix.is_empty() && database.contains(".aethyme/run/gates/broker-db-"));
            assert!(seen.insert(suffix.to_string()), "shared suffix {suffix}");
            assert!(
                seen.insert(database.to_string()),
                "shared database {database}"
            );
        }
    }
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert_host_untouched(repo.path());
}
