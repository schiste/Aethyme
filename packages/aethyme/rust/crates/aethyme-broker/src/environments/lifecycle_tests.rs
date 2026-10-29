//! Lifecycle (#1211): up/down, rollback, destroy-before-release, orphan
//! sweep from the recorded commit. Every test has its own host database,
//! environment store and repository; nothing touches real host state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{
    Lifecycle, LifecycleError, UpOptions, destroy_for_worktree_with, environment_owning_lease,
};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentPhase, EnvironmentStore};
use super::{Loaded, parse_loaded};
use crate::managed_services::ManagedServicePhase;
use crate::resources::{HostLeaseState, HostResourceCoordinator};

struct Host {
    _dir: tempfile::TempDir,
    store: PathBuf,
    db: PathBuf,
    root: PathBuf,
    markers: PathBuf,
}

struct ProcessGroupCleanup {
    pgid: i32,
    armed: bool,
}

impl ProcessGroupCleanup {
    fn new(pgid: i32) -> Self {
        Self { pgid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // SAFETY: this group is read from the exact ProcessIdentity created
        // by the component under test; it cannot be the test process group.
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        Self {
            store: dir.path().join("environments.db"),
            db: dir.path().join("host.db"),
            root: dir.path().join("environments"),
            markers,
            _dir: dir,
        }
    }

    fn lifecycle(&self) -> Lifecycle {
        Lifecycle {
            store: EnvironmentStore::open(&self.store).unwrap(),
            coordinator: HostResourceCoordinator::open(&self.db).unwrap(),
            root: self.root.clone(),
            holder_pid: None,
            hook_timeout: Duration::from_secs(20),
            actor: "test".into(),
        }
    }

    fn active_leases(&self, env_id: &str) -> usize {
        HostResourceCoordinator::open(&self.db)
            .unwrap()
            .list(false)
            .unwrap()
            .iter()
            .filter(|l| l.run_id == env_id && l.state == HostLeaseState::Active)
            .count()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository whose destroy script lives in the tree, committed.
fn repository(markers: &Path, environments: &str) -> (tempfile::TempDir, String) {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.path().join("scripts")).unwrap();
    std::fs::write(
        repo.path().join("scripts/drop.sh"),
        "rm -f \"$MARKERS/$AETHYME_ENV_COMPONENT.provisioned\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::write(
        repo.path().join(".aethyme/environments.toml"),
        environments.replace("@MARKERS@", &markers.to_string_lossy()),
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    (repo, head)
}

fn snapshot(
    text: &str,
    profile: &str,
    worktree: &Path,
    commit: Option<String>,
) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        profile,
        "t",
        &CompileContext {
            repository: "repo-test".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: commit,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap()
}

fn options() -> UpOptions {
    UpOptions {
        wait: Duration::from_secs(2),
        reconfigure: false,
    }
}

const STACK: &str = r#"
schema = 1
[pools.envs]
limit = 2

[[profiles]]
name = "backend"
units = 1

[[profiles.components]]
name = "db"
runtime = "hooks"
env = { MARKERS = "@MARKERS@" }
provision = 'touch "$MARKERS/db.provisioned"; echo "AETHYME_RECEIPT {\"database\":\"$AETHYME_ENV_NAMESPACE\"}"'
destroy = "sh scripts/drop.sh"

[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["db"]
listen = true
env = { MARKERS = "@MARKERS@" }
start = 'test -n "$AETHYME_ENV_RECEIPT_DB" && exec sleep 300'
health = { command = "true", timeout_seconds = 5, interval_ms = 50 }
"#;

#[test]
fn up_reserves_provisions_and_starts_and_a_second_up_is_the_same_environment() {
    let host = Host::new();
    let (repo, head) = repository(&host.markers, STACK);
    let text = std::fs::read_to_string(repo.path().join(".aethyme/environments.toml")).unwrap();
    let snapshot = snapshot(&text, "backend", repo.path(), Some(head));
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, repo.path(), Some(repo.path()), None, &options())
        .unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Active);
    assert!(
        record
            .namespace
            .as_deref()
            .is_some_and(|ns| ns.starts_with(&record.env_id))
    );
    assert!(host.markers.join("db.provisioned").exists());
    assert_eq!(
        host.active_leases(&record.env_id),
        2,
        "reservation and capacity"
    );
    let components = lifecycle.store.components(&record.env_id).unwrap();
    let db = components.iter().find(|c| c.component == "db").unwrap();
    assert_eq!(
        db.receipt.as_ref().unwrap()["database"],
        record.namespace.clone().unwrap().as_str()
    );
    let api = components.iter().find(|c| c.component == "api").unwrap();
    assert_eq!(
        api.phase,
        ManagedServicePhase::Running,
        "the receipt reached the start command"
    );

    let again = lifecycle
        .up(&snapshot, repo.path(), Some(repo.path()), None, &options())
        .unwrap();
    assert_eq!(again.env_id, record.env_id);
    assert_eq!(again.namespace, record.namespace);
    assert_eq!(
        host.active_leases(&record.env_id),
        2,
        "no second reservation"
    );
    lifecycle.down(&record.env_id, None).unwrap();
}

#[test]
fn down_destroys_in_reverse_order_and_releases_leases_only_then() {
    let host = Host::new();
    let (repo, head) = repository(&host.markers, STACK);
    let text = std::fs::read_to_string(repo.path().join(".aethyme/environments.toml")).unwrap();
    let snapshot = snapshot(&text, "backend", repo.path(), Some(head));
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, repo.path(), Some(repo.path()), None, &options())
        .unwrap();
    let api_identity: super::drivers::ComponentIdentity = serde_json::from_value(
        lifecycle
            .store
            .components(&record.env_id)
            .unwrap()
            .into_iter()
            .find(|c| c.component == "api")
            .unwrap()
            .identity
            .unwrap(),
    )
    .unwrap();
    let destroyed = lifecycle.down(&record.env_id, None).unwrap();
    assert_eq!(destroyed.phase, EnvironmentPhase::Destroyed);
    assert!(
        !host.markers.join("db.provisioned").exists(),
        "destroy hook ran"
    );
    assert_eq!(host.active_leases(&record.env_id), 0);
    assert!(
        !host.root.join(&record.env_id).exists(),
        "grants and logs removed"
    );
    if let super::drivers::ComponentIdentity::Process(process) = api_identity {
        assert_eq!(process.observe(), super::identity::Observation::Gone);
    }
    // Re-creating the same id gets fresh leases (request ids are per instance).
    let recreated = lifecycle
        .up(&snapshot, repo.path(), Some(repo.path()), None, &options())
        .unwrap();
    assert_eq!(recreated.env_id, record.env_id);
    assert_eq!(recreated.phase, EnvironmentPhase::Active);
    lifecycle.down(&record.env_id, None).unwrap();
}

#[test]
fn a_failed_provision_rolls_back_what_it_provisioned() {
    let host = Host::new();
    let text = format!(
        r#"
schema = 1
[pools.envs]
limit = 2
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "first"
runtime = "hooks"
provision = 'touch "{m}/first"'
destroy = 'rm -f "{m}/first"'
[[profiles.components]]
name = "second"
runtime = "hooks"
depends_on = ["first"]
provision = "echo second-cannot-provision >&2; exit 9"
destroy = "true"
"#,
        m = host.markers.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    match lifecycle.up(&snapshot, worktree.path(), None, None, &options()) {
        Err(LifecycleError::Driver(error)) => assert_eq!(error.code(), "hook_failed"),
        other => panic!("{other:?}"),
    }
    let record = lifecycle.store.get(&snapshot.env_id).unwrap().unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Failed);
    assert_eq!(record.last_error.as_ref().unwrap()["code"], "hook_failed");
    assert!(
        !host.markers.join("first").exists(),
        "first was rolled back"
    );
    assert_eq!(
        host.active_leases(&record.env_id),
        1,
        "capacity released, reservation kept"
    );
    let first = lifecycle
        .store
        .components(&record.env_id)
        .unwrap()
        .into_iter()
        .find(|c| c.component == "first")
        .unwrap();
    assert_eq!(
        first.phase,
        ManagedServicePhase::Intent,
        "a retry provisions it again"
    );
    lifecycle.down(&record.env_id, None).unwrap();
    assert_eq!(host.active_leases(&record.env_id), 0);
}

#[test]
fn an_unhealthy_start_stops_what_started_and_keeps_provisioned_data() {
    let host = Host::new();
    let text = format!(
        r#"
schema = 1
[pools.envs]
limit = 2
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = 'touch "{m}/db"'
destroy = 'rm -f "{m}/db"'
[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["db"]
start = "exec sleep 300"
health = {{ command = "false", timeout_seconds = 1, interval_ms = 50 }}
"#,
        m = host.markers.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    match lifecycle.up(&snapshot, worktree.path(), None, None, &options()) {
        Err(LifecycleError::Driver(error)) => assert_eq!(error.code(), "unhealthy"),
        other => panic!("{other:?}"),
    }
    let record = lifecycle.store.get(&snapshot.env_id).unwrap().unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Failed);
    assert!(
        host.markers.join("db").exists(),
        "provisioned data is kept for a retry"
    );
    assert_eq!(host.active_leases(&record.env_id), 1, "capacity released");
    lifecycle.down(&record.env_id, None).unwrap();
    assert!(!host.markers.join("db").exists());
}

#[test]
fn a_failed_rollback_stop_keeps_identity_and_capacity_for_down_retry() {
    let host = Host::new();
    let text = format!(
        r#"
schema = 1
[pools.envs]
limit = 2
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "worker"
runtime = "process"
env = {{ MARKERS = "{m}" }}
start = 'sleep 300 & echo "$$" > "$MARKERS/worker-leader.pid"; echo "$!" > "$MARKERS/worker-child.pid"; sleep 0.2'
health = {{ command = 'test -s "$MARKERS/worker-leader.pid" && test -s "$MARKERS/worker-child.pid"', timeout_seconds = 2, interval_ms = 50 }}
[[profiles.components]]
name = "failed"
runtime = "process"
depends_on = ["worker"]
env = {{ MARKERS = "{m}" }}
start = 'leader=$(cat "$MARKERS/worker-leader.pid"); while kill -0 "$leader" 2>/dev/null; do sleep 0.01; done; exit 9'
health = {{ command = "false", timeout_seconds = 2, interval_ms = 50 }}
"#,
        m = host.markers.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    let up = lifecycle.up(&snapshot, worktree.path(), None, None, &options());

    let child_pid: i32 = std::fs::read_to_string(host.markers.join("worker-child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: the PID came from the background child started by this fixture.
    let process_group = unsafe { libc::getpgid(child_pid) };
    assert!(process_group > 0, "fixture child has no process group");
    let mut cleanup = ProcessGroupCleanup::new(process_group);

    let failed_start = match up {
        Err(LifecycleError::Driver(error)) => error,
        other => panic!("expected the second start to fail, got {other:?}"),
    };
    assert_eq!(failed_start.code(), "start_exited");
    let record = lifecycle.store.get(&snapshot.env_id).unwrap().unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Failed);
    assert!(
        record.capacity_lease_id.is_some(),
        "capacity remains leased"
    );
    assert_eq!(host.active_leases(&record.env_id), 2);
    let start_error = record.last_error.as_ref().unwrap();
    assert_eq!(start_error["component"], "failed");
    assert_eq!(start_error["rollback_errors"][0]["component"], "worker");
    assert_eq!(
        start_error["rollback_errors"][0]["code"],
        "identity_unproven"
    );

    let worker = lifecycle
        .store
        .components(&record.env_id)
        .unwrap()
        .into_iter()
        .find(|component| component.component == "worker")
        .unwrap();
    assert_eq!(worker.phase, ManagedServicePhase::Running);
    let saved_identity = worker.identity.clone().expect("running identity retained");
    let identity: super::drivers::ComponentIdentity =
        serde_json::from_value(saved_identity.clone()).unwrap();
    let super::drivers::ComponentIdentity::Process(process) = identity else {
        panic!("worker should have a process identity");
    };
    assert_eq!(process.pgid, process_group);
    assert_eq!(process.observe(), super::identity::Observation::Unproven);

    assert!(matches!(
        lifecycle.up(&snapshot, worktree.path(), None, None, &options()),
        Err(LifecycleError::WrongPhase {
            phase: "failed",
            ..
        })
    ));
    let refused_retry = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(refused_retry.phase_generation, record.phase_generation);
    assert_eq!(refused_retry.capacity_lease_id, record.capacity_lease_id);
    assert_eq!(host.active_leases(&record.env_id), 2);
    assert_eq!(
        lifecycle
            .store
            .components(&record.env_id)
            .unwrap()
            .into_iter()
            .find(|component| component.component == "worker")
            .unwrap()
            .identity,
        Some(saved_identity.clone())
    );

    let first_down = lifecycle.down(&record.env_id, None);
    assert!(first_down.is_err(), "down must retry the retained identity");
    let after_failed_down = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(after_failed_down.phase, EnvironmentPhase::DestroyFailed);
    assert_eq!(host.active_leases(&record.env_id), 2);
    let worker_after_down = lifecycle
        .store
        .components(&record.env_id)
        .unwrap()
        .into_iter()
        .find(|component| component.component == "worker")
        .unwrap();
    assert_eq!(worker_after_down.phase, ManagedServicePhase::Running);
    assert_eq!(worker_after_down.identity, Some(saved_identity));

    // Resolve the test's deliberately orphaned child; the same retained
    // identity is now provably gone, so a second down can finish cleanup.
    // SAFETY: this is the process group verified from the stored identity.
    unsafe {
        libc::killpg(process_group, libc::SIGKILL);
    }
    for _ in 0..80 {
        if !super::identity::group_exists(process_group) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!super::identity::group_exists(process_group));
    cleanup.disarm();
    let destroyed = lifecycle.down(&record.env_id, None).unwrap();
    assert_eq!(destroyed.phase, EnvironmentPhase::Destroyed);
    assert_eq!(host.active_leases(&record.env_id), 0);
}

#[test]
fn a_failed_destroy_keeps_the_leases_and_generic_release_is_refused() {
    let host = Host::new();
    let gate = host.markers.join("allow-destroy");
    let text = format!(
        r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = 'test -f "{gate}" || {{ echo destroy-blocked >&2; exit 4; }}'
"#,
        gate = gate.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, worktree.path(), None, None, &options())
        .unwrap();
    assert!(lifecycle.down(&record.env_id, None).is_err());
    let failed = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(failed.phase, EnvironmentPhase::DestroyFailed);
    assert!(
        failed.last_error.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("destroy-blocked")
    );
    assert_eq!(
        host.active_leases(&record.env_id),
        1,
        "never released without destroy"
    );
    let lease = failed.reservation_lease_id.unwrap();
    assert_eq!(
        environment_owning_lease(&host.store, &lease)
            .unwrap()
            .as_deref(),
        Some(record.env_id.as_str()),
        "resources release/reconcile can see the owner and refuse"
    );

    std::fs::write(&gate, "").unwrap();
    let retried = lifecycle.down(&record.env_id, None).unwrap();
    assert_eq!(retried.phase, EnvironmentPhase::Destroyed);
    assert_eq!(host.active_leases(&record.env_id), 0);
    assert_eq!(environment_owning_lease(&host.store, &lease).unwrap(), None);
}

#[test]
fn abandon_releases_without_destroy_and_records_what_may_have_leaked() {
    let host = Host::new();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = 'echo "AETHYME_RECEIPT {\"database\":\"x\"}"'
destroy = "exit 1"
"#;
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, worktree.path(), None, None, &options())
        .unwrap();
    let abandoned = lifecycle
        .down(&record.env_id, Some("database dropped by hand"))
        .unwrap();
    assert_eq!(abandoned.phase, EnvironmentPhase::Destroyed);
    assert_eq!(host.active_leases(&record.env_id), 0);
    let history = lifecycle.store.transitions(&record.env_id, 50).unwrap();
    let last = history.last().unwrap();
    assert_eq!(last.reason, "abandoned");
    let detail = last.detail.as_ref().unwrap();
    assert_eq!(detail["reason"], "database dropped by hand");
    assert_eq!(detail["possibly_leaked"][0]["receipt"]["database"], "x");
}

/// #1211 acceptance: the owner dies and the worktree is deleted by hand;
/// the sweep still runs every destroy, from an export of the recorded
/// commit, and releases the leases.
#[test]
fn the_sweep_destroys_an_orphan_from_its_recorded_commit() {
    let host = Host::new();
    let (repo, head) = repository(&host.markers, STACK);
    let worktrees = tempfile::tempdir().unwrap();
    let linked = worktrees.path().join("wt");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &linked.to_string_lossy(),
            &head,
        ],
    );
    let text = std::fs::read_to_string(linked.join(".aethyme/environments.toml")).unwrap();
    let snapshot = snapshot(&text, "backend", &linked, Some(head));
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, &linked, Some(repo.path()), None, &options())
        .unwrap();
    assert!(host.markers.join("db.provisioned").exists());

    std::fs::remove_dir_all(&linked).unwrap();
    let acted = lifecycle.sweep_orphans().unwrap();
    assert_eq!(acted, vec![record.env_id.clone()]);
    let swept = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(
        swept.phase,
        EnvironmentPhase::Destroyed,
        "{:?}",
        swept.last_error
    );
    assert!(
        !host.markers.join("db.provisioned").exists(),
        "scripts/drop.sh ran from the exported commit"
    );
    assert_eq!(host.active_leases(&record.env_id), 0);
}

#[test]
fn worktree_removal_destroys_its_environments_or_is_refused() {
    let host = Host::new();
    let gate = host.markers.join("allow");
    let text = format!(
        r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = 'test -f "{gate}"'
"#,
        gate = gate.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, "p", worktree.path(), None);
    let record = host
        .lifecycle()
        .up(&snapshot, worktree.path(), None, None, &options())
        .unwrap();
    let refused = destroy_for_worktree_with(&host.store, &host.db, &host.root, worktree.path());
    assert!(
        refused
            .as_ref()
            .is_err_and(|reason| reason.contains(&record.env_id)),
        "{refused:?}"
    );
    std::fs::write(&gate, "").unwrap();
    let destroyed =
        destroy_for_worktree_with(&host.store, &host.db, &host.root, worktree.path()).unwrap();
    assert_eq!(destroyed, vec![record.env_id.clone()]);
    let other = tempfile::tempdir().unwrap();
    assert!(
        destroy_for_worktree_with(&host.store, &host.db, &host.root, other.path())
            .unwrap()
            .is_empty(),
        "unrelated worktrees are untouched"
    );
}

#[test]
fn ownership_and_worktree_removal_fail_closed_for_unreadable_environment_store() {
    let worktree = tempfile::tempdir().unwrap();

    let missing = Host::new();
    assert_eq!(
        environment_owning_lease(&missing.store, "lease").unwrap(),
        None
    );
    assert!(
        destroy_for_worktree_with(&missing.store, &missing.db, &missing.root, worktree.path())
            .unwrap()
            .is_empty(),
        "a confirmed-absent store remains a no-op"
    );

    let corrupt = Host::new();
    std::fs::write(&corrupt.store, b"not a SQLite database").unwrap();
    assert!(environment_owning_lease(&corrupt.store, "lease").is_err());
    assert!(
        destroy_for_worktree_with(&corrupt.store, &corrupt.db, &corrupt.root, worktree.path(),)
            .is_err()
    );

    let incompatible = Host::new();
    let connection = rusqlite::Connection::open(&incompatible.store).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); \
             INSERT INTO meta(key, value) VALUES ('schema_version', '999');",
        )
        .unwrap();
    drop(connection);
    assert!(environment_owning_lease(&incompatible.store, "lease").is_err());
    assert!(
        destroy_for_worktree_with(
            &incompatible.store,
            &incompatible.db,
            &incompatible.root,
            worktree.path(),
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn read_only_environment_store_rejects_symlink_paths() {
    let host = Host::new();
    let _lifecycle = host.lifecycle();
    let alias = host._dir.path().join("environments-alias.db");
    std::os::unix::fs::symlink(&host.store, &alias).unwrap();
    assert!(EnvironmentStore::open_read_only(&alias).is_err());
}

#[test]
fn a_changed_profile_needs_reconfigure() {
    let host = Host::new();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
"#;
    let worktree = tempfile::tempdir().unwrap();
    let first = snapshot(text, "p", worktree.path(), None);
    let mut lifecycle = host.lifecycle();
    lifecycle
        .up(&first, worktree.path(), None, None, &options())
        .unwrap();
    let changed = snapshot(
        &text.replace("destroy = \"true\"", "destroy = \"true # v2\""),
        "p",
        worktree.path(),
        None,
    );
    match lifecycle.up(&changed, worktree.path(), None, None, &options()) {
        Err(error @ LifecycleError::ReconfigureRequired { .. }) => {
            assert_eq!(error.code(), "reconfigure_required")
        }
        other => panic!("{other:?}"),
    }
    let reconfigured = lifecycle
        .up(
            &changed,
            worktree.path(),
            None,
            None,
            &UpOptions {
                wait: Duration::from_secs(2),
                reconfigure: true,
            },
        )
        .unwrap();
    assert_eq!(reconfigured.profile_digest, changed.digest);
    lifecycle.down(&reconfigured.env_id, None).unwrap();
}

#[test]
fn a_live_operation_fences_destroy_resume_until_its_lock_is_released() {
    let host = Host::new();
    let (repo, head) = repository(&host.markers, STACK);
    let text = std::fs::read_to_string(repo.path().join(".aethyme/environments.toml")).unwrap();
    let snapshot = snapshot(&text, "backend", repo.path(), Some(head));
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, repo.path(), Some(repo.path()), None, &options())
        .unwrap();
    lifecycle
        .store
        .transition(
            &record.env_id,
            record.phase_generation,
            EnvironmentPhase::Destroying,
            "fixture interrupted destroy",
            "test",
            None,
        )
        .unwrap();
    let operation =
        crate::file_lock::try_acquire_environment_operation_lock(&host.root, &record.env_id)
            .unwrap();
    let before = host.active_leases(&record.env_id);
    let error = lifecycle.down(&record.env_id, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lifecycle operation in progress"),
        "{error}"
    );
    assert_eq!(host.active_leases(&record.env_id), before);
    assert!(host.markers.join("db.provisioned").exists());
    assert_eq!(
        lifecycle.store.get(&record.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroying
    );
    drop(operation);
    assert_eq!(
        lifecycle.down(&record.env_id, None).unwrap().phase,
        EnvironmentPhase::Destroyed
    );
    assert_eq!(host.active_leases(&record.env_id), 0);
    assert!(!host.markers.join("db.provisioned").exists());
}

#[test]
fn destroy_refuses_a_replacement_repository_at_the_recorded_worktree_path() {
    let host = Host::new();
    let (repo, head) = repository(&host.markers, STACK);
    let worktrees = tempfile::tempdir().unwrap();
    let linked = worktrees.path().join("wt");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &linked.to_string_lossy(),
            &head,
        ],
    );
    let text = std::fs::read_to_string(linked.join(".aethyme/environments.toml")).unwrap();
    let snapshot = snapshot(&text, "backend", &linked, Some(head));
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(&snapshot, &linked, Some(repo.path()), None, &options())
        .unwrap();
    let leases = host.active_leases(&record.env_id);
    let original = worktrees.path().join("original");
    std::fs::rename(&linked, &original).unwrap();
    std::fs::create_dir(&linked).unwrap();
    git(&linked, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(linked.join("scripts")).unwrap();
    std::fs::write(
        linked.join("scripts/drop.sh"),
        "touch \"$MARKERS/replacement-hook-ran\"\n",
    )
    .unwrap();
    assert!(lifecycle.down(&record.env_id, None).is_err());
    assert!(!host.markers.join("replacement-hook-ran").exists());
    assert!(host.markers.join("db.provisioned").exists());
    assert_eq!(host.active_leases(&record.env_id), leases);
    assert_eq!(
        lifecycle.store.get(&record.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::DestroyFailed
    );
    std::fs::remove_dir_all(&linked).unwrap();
    std::fs::rename(original, &linked).unwrap();
    assert_eq!(
        lifecycle.down(&record.env_id, None).unwrap().phase,
        EnvironmentPhase::Destroyed
    );
    assert_eq!(host.active_leases(&record.env_id), 0);
}
