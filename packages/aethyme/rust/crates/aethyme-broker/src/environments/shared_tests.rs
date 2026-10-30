//! Shared environments (#1217): ref ownership, hold-and-swap redeploy with
//! no failed request, rollback, and default-branch-only removal.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::compile::EnvironmentOwner;
use super::lifecycle::{Lifecycle, destroy_for_worktree_with};
use super::proxy::{ProxyConfig, serve};
use super::shared::{Redeploy, consumer_variables};
use super::spec::HostEnvironmentsFile;
use super::store::{EnvironmentPhase, EnvironmentStore};
use crate::resources::HostResourceCoordinator;

struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("environments")
    }
    fn store(&self) -> PathBuf {
        self.dir.path().join("environments.db")
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("host.db")
    }
    fn lifecycle(&self) -> Lifecycle {
        Lifecycle {
            store: EnvironmentStore::open(&self.store()).unwrap(),
            coordinator: HostResourceCoordinator::open(&self.db()).unwrap(),
            root: self.root(),
            holder_pid: None,
            hook_timeout: Duration::from_secs(20),
            actor: "test".into(),
        }
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

fn environments(port_start: u16, start: &str) -> String {
    format!(
        r#"
schema = 1
[ports]
start = {port_start}
end = {end}
[[profiles]]
name = "backend"
[[profiles.components]]
name = "api"
runtime = "process"
listen = true
route = true
start = '{start}'
health = {{ http = "/version.txt", timeout_seconds = 20, interval_ms = 100 }}
[[profiles]]
name = "frontend"
uses_shared = ["baseline"]
[[profiles.components]]
name = "web"
runtime = "hooks"
provision = "true"
destroy = "true"
[[shared]]
name = "baseline"
profile = "backend"
ref = "default_branch"
min_redeploy_interval_seconds = 3600
"#,
        end = port_start + 49
    )
}

const SERVE: &str = "exec python3 -m http.server --bind 127.0.0.1 $AETHYME_ENV_PORT_API";

fn repository(port_start: u16) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
    std::fs::write(
        dir.join(".aethyme/environments.toml"),
        environments(port_start, SERVE),
    )
    .unwrap();
    std::fs::write(dir.join("version.txt"), "v1").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "v1"]);
    repo
}

fn commit(dir: &Path, path: &str, text: &str, message: &str) -> String {
    std::fs::write(dir.join(path), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

const REPOSITORY: &str = "repo-shared";

#[test]
fn a_shared_environment_is_ref_owned_and_survives_worktree_cleanup() {
    if !python_available() {
        eprintln!("SKIP shared ownership: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43400);
    let mut lifecycle = host.lifecycle();
    let none = HostEnvironmentsFile::default();
    let record = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Active);
    assert!(
        matches!(&record.owner, EnvironmentOwner::Ref { name } if name == "baseline@default_branch")
    );
    assert_eq!(
        record.snapshot.source.commit.as_deref(),
        Some(git(repo.path(), &["rev-parse", "HEAD"]).as_str())
    );
    let again = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(again.env_id, record.env_id, "one per host");

    // Session cleanup of any worktree never touches it.
    let session = tempfile::tempdir().unwrap();
    let touched =
        destroy_for_worktree_with(&host.store(), &host.db(), &host.root(), session.path()).unwrap();
    assert!(touched.is_empty());
    let after = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(after.phase, EnvironmentPhase::Active);

    let vars = consumer_variables(&host.root(), "baseline", &after);
    assert!(
        vars.iter()
            .any(|(k, v)| k == "AETHYME_SHARED_BASELINE_URL" && v.starts_with("http://127.0.0.1:"))
    );
    assert!(
        vars.iter()
            .any(|(k, _)| k == "AETHYME_SHARED_BASELINE_PORT_API")
    );
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

fn get(port: u16, host: &str, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

/// #1217 acceptance: advancing the default branch redeploys the baseline
/// without a single failed request through its route.
#[test]
fn a_redeploy_swaps_revisions_without_a_failed_request() {
    if !python_available() {
        eprintln!("SKIP hold-and-swap: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43450);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let before = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(5),
        )
        .unwrap();
    let namespace = before.namespace.clone().unwrap();
    let old_export = PathBuf::from(before.worktree_root.clone().unwrap());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let config = Arc::new(ProxyConfig {
        root: host.root(),
        store_path: host.store(),
        host_db: host.db(),
        token: "t".repeat(64),
        holder_pid: None,
    });
    let proxy_stop = stop.clone();
    std::thread::spawn(move || serve(listener, config, proxy_stop));

    let route = format!("api.{namespace}.localhost");
    let results: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let client_stop = Arc::new(AtomicBool::new(false));
    let client = {
        let (route, client_stop, results) = (route.clone(), client_stop.clone(), results.clone());
        std::thread::spawn(move || {
            while !client_stop.load(Ordering::SeqCst) {
                let response = get(proxy_port, &route, "/version.txt").unwrap_or_default();
                results.lock().unwrap().push(response);
                std::thread::sleep(Duration::from_millis(30));
            }
        })
    };
    let wait_for = |count: usize| {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while results.lock().unwrap().len() < count {
            assert!(std::time::Instant::now() < deadline, "the client stalled");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    // Traffic before, during and after the swap.
    wait_for(5);
    let v2 = commit(repo.path(), "version.txt", "v2", "v2");
    let outcome = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(10),
            true,
        )
        .unwrap();
    assert!(
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v2),
        "{outcome:?}"
    );
    let seen = results.lock().unwrap().len();
    wait_for(seen + 5);
    client_stop.store(true, Ordering::SeqCst);
    client.join().unwrap();
    stop.store(true, Ordering::SeqCst);
    let results = results.lock().unwrap().clone();

    let failed: Vec<&String> = results
        .iter()
        .filter(|r| !(r.starts_with("HTTP/1.0 200") || r.starts_with("HTTP/1.1 200")))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} request(s) failed, first: {:?}",
        failed.len(),
        results.len(),
        failed.first()
    );
    assert!(results.first().unwrap().ends_with("v1"));
    assert!(
        results.last().unwrap().ends_with("v2"),
        "the new revision serves"
    );

    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(
        after.namespace, before.namespace,
        "same namespace, so the same URL"
    );
    assert_eq!(
        after.reservation_lease_id, before.reservation_lease_id,
        "same ports"
    );
    assert!(!old_export.exists(), "the previous export is removed");
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

#[test]
fn a_failed_redeploy_keeps_the_old_revision_serving() {
    if !python_available() {
        eprintln!("SKIP rollback: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43500);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let before = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(5),
        )
        .unwrap();
    let v1 = before.snapshot.source.commit.clone().unwrap();
    let broken = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &environments(43500, "echo broken revision >&2; exit 3"),
        "break",
    );
    let outcome = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(10),
            true,
        )
        .unwrap();
    assert!(
        matches!(&outcome, Redeploy::RolledBack { failed, .. } if *failed == broken),
        "{outcome:?}"
    );
    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(
        after.phase,
        EnvironmentPhase::Active,
        "the old revision serves again"
    );
    assert_eq!(after.snapshot.source.commit.as_deref(), Some(v1.as_str()));
    let error = after.last_error.unwrap();
    assert_eq!(error["code"], "redeploy_failed");
    assert_eq!(error["failed_commit"], broken.as_str());
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

#[test]
fn only_removal_on_the_default_branch_destroys_it() {
    if !python_available() {
        eprintln!("SKIP default-branch removal: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43550);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(5),
        )
        .unwrap();
    let without = environments(43550, SERVE)
        .split("[[shared]]")
        .next()
        .unwrap()
        .to_string();

    git(repo.path(), &["checkout", "-qb", "feature"]);
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &without,
        "drop shared on a branch",
    );
    lifecycle
        .sync_shared(repo.path(), REPOSITORY, &none)
        .unwrap();
    assert_eq!(
        lifecycle.store.get(&record.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Active,
        "a feature branch cannot remove it"
    );

    git(repo.path(), &["checkout", "-q", "main"]);
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &without,
        "drop shared on main",
    );
    let actions = lifecycle
        .sync_shared(repo.path(), REPOSITORY, &none)
        .unwrap();
    assert!(
        actions.iter().any(|a| a.contains("destroyed baseline")),
        "{actions:?}"
    );
    assert_eq!(
        lifecycle.store.get(&record.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroyed
    );
}

/// Destroy trusts an export only when it is Aethyme's own export of the
/// recorded commit; anything else is re-exported from the commit instead.
#[test]
fn only_aethymes_own_export_of_the_recorded_commit_is_trusted() {
    let root = tempfile::tempdir().unwrap();
    let export = root.path().join("shared/repo/baseline/abc123");
    std::fs::create_dir_all(&export).unwrap();
    std::fs::write(export.join(super::shared::EXPORT_MARKER), "abc123def").unwrap();
    assert!(super::shared::is_shared_export_of(
        root.path(),
        &export,
        "abc123def"
    ));
    assert!(
        !super::shared::is_shared_export_of(root.path(), &export, "fffffff"),
        "a different recorded commit"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(
        elsewhere.path().join(super::shared::EXPORT_MARKER),
        "abc123def",
    )
    .unwrap();
    assert!(
        !super::shared::is_shared_export_of(root.path(), elsewhere.path(), "abc123def"),
        "outside Aethyme's shared directory"
    );
    std::fs::remove_file(export.join(super::shared::EXPORT_MARKER)).unwrap();
    assert!(
        !super::shared::is_shared_export_of(root.path(), &export, "abc123def"),
        "no marker"
    );
}

fn up_baseline(lifecycle: &mut Lifecycle, repo: &Path) -> super::store::EnvironmentRecord {
    lifecycle
        .shared_up(
            repo,
            REPOSITORY,
            "baseline",
            &HostEnvironmentsFile::default(),
            Duration::from_secs(5),
        )
        .unwrap()
}

/// A revision whose build fails never interrupts the serving one: it is
/// built in its own export first, so nothing is parked.
#[test]
fn a_failed_build_never_parks_the_serving_revision() {
    if !python_available() {
        eprintln!("SKIP failed build: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43600);
    let mut lifecycle = host.lifecycle();
    let before = up_baseline(&mut lifecycle, repo.path());
    let broken = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &environments(43600, SERVE).replace(
            "route = true\n",
            "route = true\nbuild = \"echo cannot build >&2; exit 4\"\n",
        ),
        "broken build",
    );
    let outcome = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &HostEnvironmentsFile::default(),
            Duration::from_secs(10),
            true,
        )
        .unwrap();
    assert!(
        matches!(&outcome, Redeploy::RolledBack { failed, .. } if *failed == broken),
        "{outcome:?}"
    );
    let transitions = lifecycle.store.transitions(&before.env_id, 50).unwrap();
    assert!(
        !transitions.iter().any(|t| t.to == EnvironmentPhase::Parked),
        "never parked: {transitions:?}"
    );
    assert!(
        transitions
            .iter()
            .any(|t| t.reason == "redeploy build failed")
    );
    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(after.phase, EnvironmentPhase::Active);
    assert_eq!(after.snapshot.source.commit, before.snapshot.source.commit);
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

/// A default-branch file that cannot be read is not "nothing declared".
#[test]
fn an_unreadable_declaration_destroys_nothing() {
    if !python_available() {
        eprintln!("SKIP unreadable declaration: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43650);
    let mut lifecycle = host.lifecycle();
    let record = up_baseline(&mut lifecycle, repo.path());
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        "schema = 1\n[[profiles]\nthis is not toml",
        "typo on main",
    );
    let actions = lifecycle
        .sync_shared(repo.path(), REPOSITORY, &HostEnvironmentsFile::default())
        .unwrap();
    assert!(
        actions.iter().any(|a| a.starts_with("skipped")),
        "{actions:?}"
    );
    assert_eq!(
        lifecycle.store.get(&record.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Active
    );
    lifecycle.down(&record.env_id, None).unwrap();
}

/// A swap marker left by a redeployer that died no longer holds requests.
#[test]
fn a_dead_redeployers_swap_marker_is_ignored() {
    let root = tempfile::tempdir().unwrap();
    let env_id = "env-0123456789ab";
    let directory = super::logs::environment_directory(root.path(), env_id).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let marker = directory.join(super::shared::SWAP_MARKER);
    let me = std::process::id() as i32;
    let alive = crate::gates::process_start_time(me);
    std::fs::write(
        &marker,
        format!(
            r#"{{"pid":{me},"start_time":{},"at":0}}"#,
            alive.unwrap_or(0)
        ),
    )
    .unwrap();
    assert_eq!(
        super::shared::swap_in_progress(root.path(), env_id),
        alive.is_some(),
        "a live owner holds"
    );
    std::fs::write(
        &marker,
        format!(
            r#"{{"pid":{me},"start_time":{},"at":0}}"#,
            alive.unwrap_or(0) + 1
        ),
    )
    .unwrap();
    assert!(
        !super::shared::swap_in_progress(root.path(), env_id),
        "a recycled pid does not"
    );
    std::fs::write(&marker, r#"{"pid":1,"start_time":null,"at":0}"#).unwrap();
    assert!(
        !super::shared::swap_in_progress(root.path(), env_id),
        "an unidentifiable owner expires"
    );
}

/// Changing a shared environment's profile on the default branch replaces
/// it; the old one is not kept forever beside the new one.
#[test]
fn a_profile_change_replaces_the_shared_environment() {
    if !python_available() {
        eprintln!("SKIP profile change: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43700);
    let mut lifecycle = host.lifecycle();
    let old = up_baseline(&mut lifecycle, repo.path());
    let changed = environments(43700, SERVE).replace(
        "[[profiles]]\nname = \"frontend\"",
        &format!(
            "[[profiles]]\nname = \"backend2\"\n[[profiles.components]]\nname = \"api\"\nruntime = \"process\"\nlisten = true\nroute = true\nstart = '{SERVE}'\nhealth = {{ http = \"/version.txt\", timeout_seconds = 20, interval_ms = 100 }}\n[[profiles]]\nname = \"frontend\""
        ),
    );
    let changed = changed.replace("profile = \"backend\"", "profile = \"backend2\"");
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &changed,
        "new profile",
    );
    let actions = lifecycle
        .sync_shared(repo.path(), REPOSITORY, &HostEnvironmentsFile::default())
        .unwrap();
    assert!(
        actions.iter().any(|a| a.contains("profile changed")),
        "{actions:?}"
    );
    assert!(
        !lifecycle
            .store
            .get(&old.env_id)
            .unwrap()
            .unwrap()
            .phase
            .retains_worktree()
    );
    let new = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    assert_eq!(new.profile, "backend2");
    assert_eq!(new.phase, EnvironmentPhase::Active);
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}
