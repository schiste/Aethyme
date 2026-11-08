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

/// Destroy whatever a test left running, even when it panicked: a leaked
/// `python3 -m http.server` keeps its port, and once a test's fixed range is
/// full every later run on the host fails with "no broker-free and
/// OS-bindable port remains".
impl Drop for Host {
    fn drop(&mut self) {
        let records = match EnvironmentStore::open(&self.store()).and_then(|s| s.list(None)) {
            Ok(records) => records,
            Err(_) => return,
        };
        let mut lifecycle = self.lifecycle();
        for record in records {
            if record.phase.retains_worktree() {
                let _ = lifecycle.down(&record.env_id, None);
            }
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
fn shared_preview_accepts_a_wait_timeout() {
    let options = super::cli::parse_options(
        "shared",
        &[
            "preview".into(),
            "baseline".into(),
            "--wait".into(),
            "42".into(),
        ],
    );
    assert_eq!(options.ok().unwrap().wait_seconds, 42);
}

#[test]
fn unmerged_shared_preview_is_worktree_owned_and_preserves_serving_baseline() {
    if !python_available() {
        eprintln!("SKIP shared preview: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(45000);
    let mut lifecycle = host.lifecycle();
    let host_config = HostEnvironmentsFile::default();
    let serving = lifecycle
        .shared_up(
            repo.path(),
            REPOSITORY,
            "baseline",
            &host_config,
            Duration::from_secs(5),
        )
        .unwrap();
    let mut text = std::fs::read_to_string(repo.path().join(".aethyme/environments.toml")).unwrap();
    text.push_str(
        "\n[[shared]]\nname = \"candidate\"\nprofile = \"backend\"\nref = \"default_branch\"\n",
    );
    let loaded: super::Loaded<super::spec::EnvironmentsFile> = super::parse_loaded(
        PathBuf::from(".aethyme/environments.toml"),
        ".aethyme/environments.toml",
        &text,
    );
    let file = loaded.value.unwrap();
    let fingerprint = crate::console::worktree_fingerprint(repo.path());
    let context = super::compile::CompileContext {
        repository: REPOSITORY.into(),
        owner: EnvironmentOwner::Worktree {
            fingerprint: fingerprint.clone(),
        },
        worktree_fingerprint: fingerprint,
        source_commit: None,
        source_dirty: true,
        source_digest: "unmerged".into(),
    };
    let snapshot =
        super::shared::preview_snapshot(&file, &host_config, "candidate", &context).unwrap();
    assert_ne!(snapshot.env_id, serving.env_id);
    assert!(matches!(snapshot.owner, EnvironmentOwner::Worktree { .. }));
    let preview = lifecycle
        .up(
            &snapshot,
            repo.path(),
            Some(repo.path()),
            None,
            &super::lifecycle::UpOptions {
                wait: Duration::from_secs(5),
                reconfigure: false,
                recreate: vec![],
            },
        )
        .unwrap();
    assert_eq!(preview.phase, EnvironmentPhase::Active);
    assert_eq!(
        lifecycle
            .shared_record(repo.path(), REPOSITORY, "baseline")
            .unwrap()
            .unwrap()
            .env_id,
        serving.env_id
    );
    lifecycle.down(&preview.env_id, None).unwrap();
    assert_eq!(
        lifecycle
            .shared_record(repo.path(), REPOSITORY, "baseline")
            .unwrap()
            .unwrap()
            .phase,
        EnvironmentPhase::Active
    );
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

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
    // More requests in flight when the old server stops, which is what used to
    // reach a client as a reset. Only the client above reads revisions, in order.
    let pressure: Vec<_> = (0..4)
        .map(|_| {
            let (route, client_stop) = (route.clone(), client_stop.clone());
            std::thread::spawn(move || {
                let mut responses = Vec::new();
                while !client_stop.load(Ordering::SeqCst) {
                    responses.push(get(proxy_port, &route, "/version.txt").unwrap_or_default());
                    std::thread::sleep(Duration::from_millis(5));
                }
                responses
            })
        })
        .collect();
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
    let pressure: Vec<String> = pressure
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect();
    stop.store(true, Ordering::SeqCst);
    let results = results.lock().unwrap().clone();

    let failed: Vec<&String> = results
        .iter()
        .chain(&pressure)
        .filter(|r| !(r.starts_with("HTTP/1.0 200") || r.starts_with("HTTP/1.1 200")))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} request(s) failed, first: {:?}",
        failed.len(),
        results.len() + pressure.len(),
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
        last_budget_outcomes: None,
        budget_outcomes: None,
        last_prebuild: None,
        prebuild: None,
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
    let repo = repository(44050);
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
    let repo = repository(44100);
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
    let repo = repository(44150);
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

/// #1297: with `redeploy_on = "inputs"`, a commit that leaves the inputs
/// alone records the revision and restarts nothing; a commit that changes
/// them redeploys.
#[test]
fn a_shared_environment_on_inputs_ignores_unrelated_commits() {
    if !python_available() {
        eprintln!("SKIP redeploy on inputs: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(44000);
    let first = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &environments(44000, SERVE).replacen(
            "min_redeploy_interval_seconds = 3600\n",
            "min_redeploy_interval_seconds = 3600\nredeploy_on = \"inputs\"\ninputs = [\"version.txt\"]\n",
            1,
        ),
        "redeploy on inputs",
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
    let redeploy = |lifecycle: &mut Lifecycle| {
        lifecycle
            .shared_redeploy(
                repo.path(),
                REPOSITORY,
                "baseline",
                &none,
                Duration::from_secs(20),
                true,
            )
            .unwrap()
    };

    let unrelated = commit(repo.path(), "notes.txt", "n", "unrelated");
    let outcome = redeploy(&mut lifecycle);
    assert!(
        matches!(&outcome, Redeploy::Unchanged { commit } if *commit == unrelated),
        "{outcome:?}"
    );
    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(after.phase, EnvironmentPhase::Active);
    assert_eq!(
        after.snapshot.source.commit.as_deref(),
        Some(first.as_str()),
        "nothing was redeployed"
    );
    let history = lifecycle.store.transitions(&before.env_id, 50).unwrap();
    assert!(
        history.iter().any(|t| t.reason == "revision recorded"),
        "{history:?}"
    );
    assert!(
        history.iter().all(|t| t.reason != "redeployed"),
        "{history:?}"
    );

    let v2 = commit(repo.path(), "version.txt", "v2", "v2");
    let outcome = redeploy(&mut lifecycle);
    assert!(
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v2),
        "{outcome:?}"
    );
    for record in lifecycle.store.list(None).unwrap() {
        if record.phase.retains_worktree() {
            lifecycle
                .shared_destroy(repo.path(), REPOSITORY, "baseline")
                .unwrap();
            break;
        }
    }
}

fn stateful(port_start: u16) -> String {
    environments(port_start, SERVE).replacen(
        "min_redeploy_interval_seconds = 3600\n",
        "min_redeploy_interval_seconds = 3600\nstateful = true\n",
        1,
    )
}

fn with_build(text: &str, build: &str) -> String {
    text.replace(
        "route = true\n",
        &format!("route = true\nbuild = '{build}'\n"),
    )
}

/// The export directories of the `baseline` shared environment.
fn exports(host: &Host) -> Vec<String> {
    let home = host.root().join("shared").join(REPOSITORY).join("baseline");
    let mut names: Vec<String> = std::fs::read_dir(home)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn short(sha: &str) -> String {
    sha[..12].to_string()
}

/// #1523 and #1524: a failed build leaves no export behind, and the next
/// successful redeploy clears the error it supersedes (keeping it in the
/// history) instead of reporting a fixed failure forever.
#[test]
fn a_successful_redeploy_clears_the_error_and_failed_exports() {
    if !python_available() {
        eprintln!("SKIP error clearing: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(44200);
    let first = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &stateful(44200),
        "stateful",
    );
    let none = HostEnvironmentsFile::default();
    let mut lifecycle = host.lifecycle();
    let before = up_baseline(&mut lifecycle, repo.path());
    // Parked, as in the report: a swap while parked wakes nothing, so no
    // wake clears the error on the way.
    lifecycle.park(&before.env_id, "test").unwrap();
    let redeploy = |lifecycle: &mut Lifecycle| {
        lifecycle
            .shared_redeploy(
                repo.path(),
                REPOSITORY,
                "baseline",
                &none,
                Duration::from_secs(20),
                true,
            )
            .unwrap()
    };

    let broken = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &with_build(&stateful(44200), "echo cannot build >&2; exit 4"),
        "broken build",
    );
    let outcome = redeploy(&mut lifecycle);
    assert!(
        matches!(&outcome, Redeploy::RolledBack { failed, .. } if *failed == broken),
        "{outcome:?}"
    );
    let failed = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    let error = failed.last_error.clone().unwrap();
    assert_eq!(error["failed_commit"], broken.as_str());
    // #1522: the failure names the build log run that holds its output.
    let log = PathBuf::from(error["log"]["path"].as_str().unwrap());
    assert!(log.ends_with("api.build.log"), "{error}");
    let offset = error["log"]["offset"].as_u64().unwrap() as usize;
    let text = std::fs::read_to_string(&log).unwrap();
    let run = &text[offset..];
    assert!(
        run.starts_with("[aethyme 20")
            && run.lines().next().unwrap().ends_with("api: build: started"),
        "the run starts at its timestamped marker: {run}"
    );
    assert!(run.contains("cannot build"), "{run}");
    let runtime = std::fs::read_to_string(log.with_file_name("api.log")).unwrap_or_default();
    assert!(
        !runtime.contains("cannot build"),
        "build output stays out of the runtime log: {runtime}"
    );
    assert_eq!(
        exports(&host),
        vec![short(&first)],
        "the failed revision's export is removed"
    );

    let fixed = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &stateful(44200),
        "fixed",
    );
    let outcome = redeploy(&mut lifecycle);
    assert!(
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == fixed),
        "{outcome:?}"
    );
    let after = lifecycle.store.get(&before.env_id).unwrap().unwrap();
    assert_eq!(
        after.phase,
        EnvironmentPhase::Parked,
        "swapped while parked"
    );
    assert_eq!(
        after.snapshot.source.commit.as_deref(),
        Some(fixed.as_str())
    );
    assert_eq!(after.last_error, None, "the superseded error is cleared");
    let history = lifecycle.store.transitions(&before.env_id, 50).unwrap();
    let redeployed = history
        .iter()
        .rev()
        .find(|t| t.reason == "redeployed")
        .expect("a redeployed note");
    assert_eq!(
        redeployed.detail.as_ref().unwrap()["resolved_error"]["failed_commit"],
        broken.as_str(),
        "the cleared error stays in the history: {redeployed:?}"
    );
    assert_eq!(exports(&host), vec![short(&fixed)]);
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
    assert!(exports(&host).is_empty(), "destroy leaves no export");
}

/// #1524: exports an earlier removal could not finish (and interrupted
/// staging directories) are swept by the next redeploy, even one that
/// changes nothing; the serving export and unrelated entries stay.
#[test]
fn a_redeploy_sweeps_exports_no_slot_runs_from() {
    if !python_available() {
        eprintln!("SKIP export sweep: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(44250);
    let mut lifecycle = host.lifecycle();
    let record = up_baseline(&mut lifecycle, repo.path());
    let serving = short(record.snapshot.source.commit.as_deref().unwrap());
    let home = host.root().join("shared").join(REPOSITORY).join("baseline");
    for stale in ["0b9f56ec37ca", "5761376cc747.staging"] {
        std::fs::create_dir_all(home.join(stale).join("venv/lib")).unwrap();
        std::fs::write(home.join(stale).join("venv/lib/x.py"), "x").unwrap();
    }
    std::fs::create_dir_all(home.join("not-an-export")).unwrap();

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
    assert!(matches!(outcome, Redeploy::Unchanged { .. }), "{outcome:?}");
    assert_eq!(
        exports(&host),
        vec![serving, "not-an-export".to_string()],
        "only the serving export and non-export entries remain"
    );
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

/// #1524: a parked shared environment woken (by a request) while its next
/// revision builds is swapped from the phase it is in by then, instead of
/// failing with "cannot go from active to snapshot swap".
#[test]
fn a_wake_during_the_build_does_not_fail_the_swap() {
    if !python_available() {
        eprintln!("SKIP wake during build: python3 not available");
        return;
    }
    let host = Host::new();
    let repo = repository(44300);
    commit(
        repo.path(),
        ".aethyme/environments.toml",
        &stateful(44300),
        "stateful",
    );
    let mut lifecycle = host.lifecycle();
    let record = up_baseline(&mut lifecycle, repo.path());
    lifecycle.park(&record.env_id, "test").unwrap();

    let signals = tempfile::tempdir().unwrap();
    let started = signals.path().join("build-started");
    let release = signals.path().join("release-build");
    let build = format!(
        "touch {}; while [ ! -e {} ]; do sleep 0.1; done",
        started.display(),
        release.display()
    );
    let v2 = commit(
        repo.path(),
        ".aethyme/environments.toml",
        &with_build(&stateful(44300), &build),
        "v2 with a build",
    );
    let waker = {
        let mut other = host.lifecycle();
        let env_id = record.env_id.clone();
        let (started, release) = (started.clone(), release.clone());
        std::thread::spawn(move || {
            while !started.exists() {
                std::thread::sleep(Duration::from_millis(50));
            }
            let woken = other.wake(&env_id, Duration::from_secs(10));
            std::fs::write(&release, b"").unwrap();
            woken.map(|r| r.phase)
        })
    };
    let outcome = lifecycle
        .shared_redeploy(
            repo.path(),
            REPOSITORY,
            "baseline",
            &HostEnvironmentsFile::default(),
            Duration::from_secs(20),
            true,
        )
        .unwrap();
    assert_eq!(
        waker.join().unwrap().unwrap(),
        EnvironmentPhase::Active,
        "the wake ran during the build"
    );
    assert!(
        matches!(&outcome, Redeploy::Redeployed { to, .. } if *to == v2),
        "{outcome:?}"
    );
    let after = lifecycle.store.get(&record.env_id).unwrap().unwrap();
    assert_eq!(
        after.phase,
        EnvironmentPhase::Active,
        "woken at the new revision"
    );
    assert_eq!(after.snapshot.source.commit.as_deref(), Some(v2.as_str()));
    lifecycle
        .shared_destroy(repo.path(), REPOSITORY, "baseline")
        .unwrap();
}

/// Tests in these files bind real loopback ports from fixed ranges, and each
/// test has its own host database, so only disjoint ranges keep parallel tests
/// apart. Three ranges here once started inside the 43900 test's range, and
/// `route_snapshot_rebuilds_when_new_serving_owner_appears_after_initial_list`
/// failed in CI with "Address already in use" when it ran beside that test.
/// The same start in the same file is one range (one test may reuse its own).
#[test]
fn fixed_port_ranges_of_parallel_environment_tests_are_disjoint() {
    let sources = [
        (
            "shared_tests.rs",
            include_str!("shared_tests.rs"),
            "repository(",
            50,
        ),
        ("proxy_tests.rs", include_str!("proxy_tests.rs"), "web(", 50),
        (
            "park_tests.rs",
            include_str!("park_tests.rs"),
            "one_unit(",
            20,
        ),
    ];
    let mut ranges = std::collections::BTreeSet::new();
    for (file, text, call, width) in sources {
        assert!(
            text.contains(&format!("end = port_start + {}", width - 1)),
            "{file}: its port helper no longer spans {width} ports; update this guard"
        );
        for (index, _) in text.match_indices(call) {
            let rest = &text[index + call.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && rest[digits.len()..].starts_with(')') {
                let start: u32 = digits.parse().unwrap();
                ranges.insert((start, start + width - 1, file));
            }
        }
    }
    let ranges: Vec<_> = ranges.into_iter().collect();
    assert!(
        ranges.len() > 20,
        "the guard found too few ranges: {ranges:?}"
    );
    for pair in ranges.windows(2) {
        let ((start, end, file), (next_start, next_end, next_file)) = (pair[0], pair[1]);
        assert!(
            end < next_start,
            "{file} range {start}..={end} overlaps {next_file} range {next_start}..={next_end}"
        );
    }
}
