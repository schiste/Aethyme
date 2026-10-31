//! Shared environments (#1217): ref ownership, hold-and-swap redeploy with
//! no failed request, rollback, and default-branch-only removal.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
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

    let vars = consumer_variables(&host.root(), "baseline", &after, None);
    assert!(
        vars.iter()
            .any(|(k, v)| k == "AETHYME_SHARED_BASELINE_URL" && v.starts_with("http://127.0.0.1:"))
    );
    let direct_url = vars
        .iter()
        .find(|(k, _)| k == "AETHYME_SHARED_BASELINE_URL")
        .map(|(_, v)| v.as_str())
        .unwrap();
    let alias = super::shared::alias_host(REPOSITORY, "baseline", "localhost");
    assert_eq!(
        super::routes::route_alias(&host.root(), &after.env_id),
        Some(alias.clone()),
        "the ref-owned environment advertises its stable alias"
    );
    let alias_file = host
        .root()
        .join(&after.env_id)
        .join(super::routes::ROUTE_ALIAS_FILE);
    // #1274: consumers use a loopback route through the proxy while keeping
    // the direct endpoint. Literal addresses make both branches independent
    // of the host's wildcard-localhost DNS support.
    std::fs::write(&alias_file, "127.0.0.1").unwrap();
    let proxied = consumer_variables(&host.root(), "baseline", &after, Some(4180));
    let url = proxied
        .iter()
        .find(|(k, _)| k == "AETHYME_SHARED_BASELINE_URL")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert_eq!(url, "http://127.0.0.1:4180", "use the loopback proxy route");
    assert!(
        proxied
            .iter()
            .any(|(k, v)| k == "AETHYME_SHARED_BASELINE_DIRECT_URL" && v == direct_url)
    );
    // A non-loopback literal must keep consumers on their direct endpoint;
    // this address is parsed locally and no connection is attempted.
    std::fs::write(&alias_file, "192.0.2.1").unwrap();
    let fallback = consumer_variables(&host.root(), "baseline", &after, Some(4180));
    assert!(
        fallback
            .iter()
            .any(|(k, v)| k == "AETHYME_SHARED_BASELINE_URL" && v == direct_url),
        "a non-loopback alias must fall back to the direct endpoint"
    );
    std::fs::write(&alias_file, &alias).unwrap();
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

/// The shared environment in a one-unit pool: no room for a second slot.
fn in_a_full_pool(text: &str) -> String {
    text.replacen(
        "[[profiles]]\nname = \"backend\"\n",
        "[pools.envs]\nlimit = 1\n[[profiles]]\nname = \"backend\"\nunits = 1\n",
        1,
    )
}

/// #1217 acceptance, and #1274's fallback: with no room for a second slot,
/// advancing the default branch redeploys the baseline by hold-and-swap
/// without a single failed request through its route.
#[test]
fn a_redeploy_swaps_revisions_without_a_failed_request() {
    if !python_available() {
        eprintln!("SKIP hold-and-swap: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43450);
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &in_a_full_pool(&environments(43450, SERVE)),
        "one-unit pool",
    );
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

/// #1274: waking a consumer brings its shared environments up first, so the
/// URLs it was given answer.
#[test]
fn waking_a_consumer_wakes_its_shared_environment() {
    if !python_available() {
        eprintln!("SKIP consumer wake: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43750);
    let mut lifecycle = host.lifecycle();
    let shared = up_baseline(&mut lifecycle, repo.path());

    let text = std::fs::read_to_string(repo.path().join(".aethyme/environments.toml")).unwrap();
    let loaded: super::Loaded<super::spec::EnvironmentsFile> =
        super::parse_loaded(PathBuf::from("t"), "t", &text);
    let file = loaded.value.unwrap();
    let fingerprint = crate::console::worktree_fingerprint(repo.path());
    let snapshot = super::compile::compile(
        &file,
        &HostEnvironmentsFile::default(),
        "frontend",
        "t",
        &super::compile::CompileContext {
            repository: REPOSITORY.into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: None,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap();
    let consumer = lifecycle
        .up(
            &snapshot,
            repo.path(),
            Some(repo.path()),
            None,
            &super::lifecycle::UpOptions {
                wait: Duration::from_secs(5),
                reconfigure: false,
                recreate: Vec::new(),
            },
        )
        .unwrap()
        .env_id;
    lifecycle.park(&consumer, "test").unwrap();
    lifecycle.park(&shared.env_id, "idle").unwrap();

    lifecycle.wake(&consumer, Duration::from_secs(10)).unwrap();
    assert_eq!(
        lifecycle.store.get(&shared.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Active,
        "the shared environment woke with its consumer"
    );
    lifecycle.down(&consumer, None).unwrap();
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

/// #1274: shared sync runs off the supervisor tick, one at a time, so a slow
/// redeploy never delays lease renewal.
#[test]
fn shared_sync_never_blocks_the_tick() {
    let mut work = super::operate::LifecycleWork {
        watch: super::supervisor::StoreWatch {
            store_path: PathBuf::from("/nonexistent/environments.db"),
        },
        proxy_started: true,
        last_sample: None,
        shared_sync: None,
    };
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let started = std::time::Instant::now();
    assert!(work.start_shared_sync(move || {
        wait.recv().ok();
    }));
    // A second start runs on its own thread so a blocking one fails the test
    // instead of hanging it.
    let (done, second) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started_again = work.start_shared_sync(|| {});
        done.send((work, started_again)).ok();
    });
    let Ok((mut work, started_again)) = second.recv_timeout(Duration::from_secs(2)) else {
        release.send(()).ok();
        panic!("starting a sync blocked while the previous one ran");
    };
    assert!(!started_again, "a second sync waits for the first");
    assert!(started.elapsed() < Duration::from_millis(500));
    release.send(()).unwrap();
    for _ in 0..100 {
        if work.start_shared_sync(|| {}) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the next sync starts once the first finished");
}

/// #1274: with room for a second slot, a redeploy is blue/green: the new
/// revision comes up beside the serving one, the alias moves, and no request
/// through it is held or fails; the old slot is destroyed, never parked.
#[test]
fn a_blue_green_redeploy_holds_nothing() {
    if !python_available() {
        eprintln!("SKIP blue/green: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43800);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let blue = up_baseline(&mut lifecycle, repo.path());

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

    let alias = super::shared::alias_host(REPOSITORY, "baseline", "localhost");
    let results: Arc<std::sync::Mutex<Vec<(std::time::Duration, String)>>> = Arc::default();
    let client_stop = Arc::new(AtomicBool::new(false));
    let client = {
        let (alias, client_stop, results) = (alias.clone(), client_stop.clone(), results.clone());
        std::thread::spawn(move || {
            while !client_stop.load(Ordering::SeqCst) {
                let started = std::time::Instant::now();
                let response = get(proxy_port, &alias, "/version.txt").unwrap_or_default();
                results.lock().unwrap().push((started.elapsed(), response));
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
        .map(|(_, r)| r)
        .filter(|r| !(r.starts_with("HTTP/1.0 200") || r.starts_with("HTTP/1.1 200")))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} failed: {:?}",
        failed.len(),
        results.len(),
        failed.first()
    );
    assert!(results.first().unwrap().1.ends_with("v1"));
    assert!(
        results.last().unwrap().1.ends_with("v2"),
        "the new slot serves"
    );
    let slowest = results.iter().map(|(d, _)| *d).max().unwrap();
    assert!(
        slowest < Duration::from_secs(1),
        "nothing was held: {slowest:?}"
    );

    let transitions = lifecycle.store.transitions(&blue.env_id, 100).unwrap();
    assert!(
        !transitions.iter().any(|t| t.to == EnvironmentPhase::Parked),
        "the serving slot was never parked: {transitions:?}"
    );
    assert_eq!(
        lifecycle.store.get(&blue.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroyed,
        "the previous slot is destroyed"
    );
    let green = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    assert_ne!(green.env_id, blue.env_id);
    assert_eq!(green.phase, EnvironmentPhase::Active);

    // The next redeploy flips back to the first slot.
    let v3 = commit(repo.path(), "version.txt", "v3", "v3");
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
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v3),
        "{outcome:?}"
    );
    let serving = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    assert_eq!(serving.env_id, blue.env_id, "back on the first slot");
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
    assert_eq!(
        lifecycle.store.get(&green.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroyed
    );
}

/// A serving-pointer flip between pointer capture and record selection must
/// rebuild the route table from the new active slot, never pair the new alias
/// with stale slot metadata from the initial list.
#[test]
fn route_snapshot_reselects_when_serving_pointer_flips_mid_snapshot() {
    if !python_available() {
        eprintln!("SKIP route snapshot flip: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43900);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let blue = up_baseline(&mut lifecycle, repo.path());
    let v2 = commit(repo.path(), "version.txt", "v2", "v2");

    let store_path = host.store();
    let root = host.root();
    let (snapshot_started, snapshot_waiting) = mpsc::channel();
    let (continue_snapshot, snapshot_continues) = mpsc::channel();
    let route_reader = std::thread::spawn(move || {
        let store = EnvironmentStore::open_read_only(&store_path)
            .unwrap()
            .unwrap();
        let mut pause_once = true;
        super::routes::route_table_with_test_hook(&store, &root, || {
            if pause_once {
                pause_once = false;
                snapshot_started.send(()).unwrap();
                snapshot_continues.recv().unwrap();
            }
        })
    });
    snapshot_waiting
        .recv_timeout(Duration::from_secs(5))
        .expect("route snapshot reached its captured serving pointer");

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
        "the pointer flip completed before the snapshot continued: {outcome:?}"
    );
    let green = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    continue_snapshot.send(()).unwrap();

    let routes = route_reader.join().unwrap().unwrap();
    let alias = super::shared::alias_host(REPOSITORY, "baseline", "localhost");
    let alias_route = routes
        .iter()
        .find(|route| route.host == alias)
        .expect("the serving slot retains the stable alias");
    assert_eq!(alias_route.env_id, green.env_id);
    assert_eq!(alias_route.state, "active");
    assert!(alias_route.upstream.is_some());
    assert!(
        !routes
            .iter()
            .any(|route| route.host == alias && route.env_id == blue.env_id),
        "the old slot must not receive the new serving alias"
    );
    let canonical_host = format!(
        "api.{}.{}",
        green.namespace.as_deref().unwrap(),
        green.snapshot.route_domain
    );
    assert!(
        routes
            .iter()
            .any(|route| route.host == canonical_host && route.env_id == green.env_id),
        "the new slot keeps its canonical component route"
    );
}

/// A new serving owner published after the initial database list must trigger
/// a complete snapshot rebuild, including that slot's canonical routes.
#[test]
fn route_snapshot_rebuilds_when_new_serving_owner_appears_after_initial_list() {
    if !python_available() {
        eprintln!("SKIP route snapshot owner publication: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43901);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let blue = up_baseline(&mut lifecycle, repo.path());
    let v2 = commit(repo.path(), "version.txt", "v2", "v2");

    let store_path = host.store();
    let root = host.root();
    let (snapshot_started, snapshot_waiting) = mpsc::channel();
    let (continue_snapshot, snapshot_continues) = mpsc::channel();
    let route_reader = std::thread::spawn(move || {
        let store = EnvironmentStore::open_read_only(&store_path)
            .unwrap()
            .unwrap();
        let mut pause_once = true;
        super::routes::route_table_with_list_hook(&store, &root, || {
            if pause_once {
                pause_once = false;
                snapshot_started.send(()).unwrap();
                snapshot_continues.recv().unwrap();
            }
        })
    });
    snapshot_waiting
        .recv_timeout(Duration::from_secs(5))
        .expect("route snapshot reached its initial record list");

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
        "the new owner was published before pointer capture: {outcome:?}"
    );
    let green = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    continue_snapshot.send(()).unwrap();

    let routes = route_reader.join().unwrap().unwrap();
    let alias = super::shared::alias_host(REPOSITORY, "baseline", "localhost");
    let alias_route = routes
        .iter()
        .find(|route| route.host == alias)
        .expect("the serving slot retains the stable alias");
    assert_eq!(alias_route.env_id, green.env_id);
    assert_eq!(alias_route.state, "active");
    assert!(alias_route.upstream.is_some());
    assert!(
        !routes
            .iter()
            .any(|route| route.host == alias && route.env_id == blue.env_id),
        "the old slot must not receive the new serving alias"
    );
    let canonical_host = format!(
        "api.{}.{}",
        green.namespace.as_deref().unwrap(),
        green.snapshot.route_domain
    );
    assert!(
        routes
            .iter()
            .any(|route| route.host == canonical_host && route.env_id == green.env_id),
        "the new slot keeps its canonical component route"
    );
}

/// A legacy singleton with no pointer gets its current owner published before
/// blue/green starts the second slot.
#[cfg(unix)]
#[test]
fn blue_green_publishes_current_owner_before_starting_second_slot() {
    if !python_available() {
        eprintln!("SKIP blue-green serving pointer guard: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43902);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let blue = up_baseline(&mut lifecycle, repo.path());
    let pointer = host
        .root()
        .join("shared")
        .join(REPOSITORY)
        .join("baseline")
        .join("serving");
    std::fs::remove_file(&pointer).unwrap();

    let guarded_start = format!(
        "test \"$(cat {})\" = baseline@default_branch && exec python3 -m http.server --bind 127.0.0.1 $AETHYME_ENV_PORT_API",
        pointer.display()
    );
    let guarded_config = environments(43902, &guarded_start);
    let v2 = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &guarded_config,
        "check serving pointer before starting green",
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
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v2),
        "green only starts after the current owner is explicit: {outcome:?}"
    );
    let serving = std::fs::read_to_string(&pointer).unwrap();
    assert_eq!(serving, "baseline@default_branch#b");
    assert_eq!(
        lifecycle.store.get(&blue.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroyed
    );
}

/// Changing a shared declaration's ref publishes the new healthy declared
/// owner before sync removes the old ref's last retained slot.
#[test]
fn shared_ref_change_replaces_stale_serving_owner_before_sync_cleanup() {
    if !python_available() {
        eprintln!("SKIP shared ref pointer migration: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43903);
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let blue = up_baseline(&mut lifecycle, repo.path());
    let v2 = commit(repo.path(), "version.txt", "v2", "v2");
    let redeployed = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(10),
            true,
        )
        .unwrap();
    assert!(matches!(&redeployed, Redeploy::Redeployed { to, .. } if *to == v2));
    assert_eq!(
        lifecycle.store.get(&blue.env_id).unwrap().unwrap().phase,
        EnvironmentPhase::Destroyed
    );
    let previous = lifecycle
        .shared_record(repo.path(), REPOSITORY, "baseline")
        .unwrap()
        .unwrap();
    let previous_owner = "baseline@default_branch#b";
    assert!(matches!(
        &previous.owner,
        EnvironmentOwner::Ref { name } if name == previous_owner
    ));
    let pointer = host
        .root()
        .join("shared")
        .join(REPOSITORY)
        .join("baseline")
        .join("serving");
    assert_eq!(std::fs::read_to_string(&pointer).unwrap(), previous_owner);

    let new_config =
        environments(43903, SERVE).replace("ref = \"default_branch\"", "ref = \"feature#b\"");
    let v3 = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &new_config,
        "move shared declaration to feature ref",
    );
    git(repo.path(), &["branch", "feature#b", v3.as_str()]);
    let new_record = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(matches!(
        &new_record.owner,
        EnvironmentOwner::Ref { name } if name == "baseline@feature#b"
    ));
    assert_eq!(
        std::fs::read_to_string(&pointer).unwrap(),
        "baseline@feature#b"
    );

    lifecycle
        .sync_shared(repo.path(), REPOSITORY, &none)
        .unwrap();
    assert_eq!(
        lifecycle
            .store
            .get(&previous.env_id)
            .unwrap()
            .unwrap()
            .phase,
        EnvironmentPhase::Destroyed,
        "sync removes the old ref's now-unselected serving slot"
    );
    let routes = super::routes::route_table(&lifecycle.store, &host.root()).unwrap();
    let alias = super::shared::alias_host(REPOSITORY, "baseline", "localhost");
    assert!(
        routes
            .iter()
            .any(|route| route.host == alias && route.env_id == new_record.env_id)
    );
}

#[cfg(unix)]
#[test]
fn serving_pointer_snapshot_detects_same_owner_atomic_replacement() {
    let host = Host::new();
    let owner = "baseline@default_branch";
    let pointer_dir = host.root().join("shared").join(REPOSITORY).join("baseline");
    std::fs::create_dir_all(&pointer_dir).unwrap();
    let pointer = pointer_dir.join("serving");
    std::fs::write(&pointer, owner).unwrap();
    let snapshot =
        super::shared::serving_pointer_snapshot(&host.root(), REPOSITORY, "baseline", Some(owner))
            .unwrap();

    // Match the production temp-file + rename publication while keeping the
    // plain owner bytes identical. The retained old descriptor prevents inode
    // reuse from making this replacement look unchanged.
    let staging = pointer_dir.join("serving.next");
    std::fs::write(&staging, owner).unwrap();
    std::fs::rename(&staging, &pointer).unwrap();

    assert_eq!(snapshot.owner(), Some(owner));
    assert!(
        !snapshot
            .is_current(&host.root(), REPOSITORY, "baseline")
            .unwrap()
    );
}

#[cfg(unix)]
#[test]
fn serving_pointer_snapshot_rejects_nonregular_and_oversized_files() {
    let host = Host::new();
    let pointer_dir = host.root().join("shared").join(REPOSITORY).join("baseline");
    std::fs::create_dir_all(&pointer_dir).unwrap();
    let pointer = pointer_dir.join("serving");

    std::fs::write(&pointer, vec![b'x'; 4097]).unwrap();
    assert!(
        super::shared::serving_pointer_snapshot(
            &host.root(),
            REPOSITORY,
            "baseline",
            Some("baseline@default_branch"),
        )
        .is_err()
    );

    std::fs::remove_file(&pointer).unwrap();
    std::fs::create_dir(&pointer).unwrap();
    assert!(
        super::shared::serving_pointer_snapshot(
            &host.root(),
            REPOSITORY,
            "baseline",
            Some("baseline@default_branch"),
        )
        .is_err()
    );
}

/// #1274: a stateful shared environment (a database) is never redeployed
/// blue/green, even with room for a second slot: the new slot would start
/// without its data. It swaps in place and keeps its environment.
#[test]
fn a_stateful_shared_environment_never_runs_blue_green() {
    if !python_available() {
        eprintln!("SKIP stateful: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(43850);
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &environments(43850, SERVE).replacen(
            "min_redeploy_interval_seconds = 3600\n",
            "min_redeploy_interval_seconds = 3600\nstateful = true\n",
            1,
        ),
        "stateful baseline",
    );
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
    let v2 = commit(repo.path(), "version.txt", "v2", "v2");
    let outcome = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &none,
            Duration::from_secs(20),
            true,
        )
        .unwrap();
    assert!(
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v2),
        "{outcome:?}"
    );
    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(after.phase, EnvironmentPhase::Active, "swapped in place");
    assert_eq!(after.snapshot.source.commit.as_deref(), Some(v2.as_str()));
    let live = lifecycle.store.list(None).unwrap();
    assert!(
        live.iter()
            .all(|r| !matches!(&r.owner, EnvironmentOwner::Ref { name } if name.ends_with("#b"))),
        "no second slot: {live:?}"
    );
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}
