//! Wake on request (#1214): routes, the held request, the authenticated wake
//! endpoint and static serving, against a proxy on an ephemeral port.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{Lifecycle, RESERVATION_GRANT, UpOptions, read_grant};
use super::proxy::{ProxyConfig, serve};
use super::routes::{lookup, route_table};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentPhase, EnvironmentStore};
use super::{Loaded, parse_loaded};
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

    /// A proxy on an ephemeral loopback port. Stops when dropped.
    fn proxy(&self) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let config = Arc::new(ProxyConfig {
            root: self.root(),
            store_path: self.store(),
            host_db: self.db(),
            token: "t".repeat(64),
            holder_pid: None,
        });
        let flag = stop.clone();
        std::thread::spawn(move || serve(listener, config, flag));
        Proxy { port, stop }
    }
}

struct Proxy {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Proxy {
    fn request(&self, raw: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn get(&self, host: &str, path: &str) -> String {
        self.request(&format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        ))
    }
}

fn snapshot(text: &str, worktree: &Path) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        &file.profiles[0].name,
        "t",
        &CompileContext {
            repository: "repo-proxy".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: None,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap()
}

/// A python http.server component on its own port range. Tests have
/// separate host databases, so the coordinator cannot keep parallel tests
/// off each other's ports: every test that binds one gets its own range.
fn web(port_start: u16) -> String {
    format!(
        r#"
schema = 1
[ports]
start = {port_start}
end = {end}
[[profiles]]
name = "p"
[[profiles.components]]
name = "web"
runtime = "process"
listen = true
route = true
start = "exec python3 -m http.server --bind 127.0.0.1 $AETHYME_ENV_PORT_WEB"
health = {{ http = "/", timeout_seconds = 20, interval_ms = 100 }}
"#,
        end = port_start + 49
    )
}

fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn up(host: &Host, text: &str, worktree: &Path) -> (Lifecycle, String) {
    let snapshot = snapshot(text, worktree);
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(
            &snapshot,
            worktree,
            None,
            None,
            &UpOptions {
                wait: Duration::from_secs(1),
                reconfigure: false,
                recreate: Vec::new(),
            },
        )
        .unwrap();
    (lifecycle, record.env_id)
}

#[test]
fn routes_name_reserved_ports_only_while_active() {
    if !python_available() {
        eprintln!("SKIP routes: python3 not available");
        return;
    }
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    let (mut lifecycle, id) = up(&host, &web(43100), worktree.path());
    let record = lifecycle.store.get(&id).unwrap().unwrap();
    let namespace = record.namespace.clone().unwrap();
    let reserved: u16 = read_grant(&host.root(), &id, RESERVATION_GRANT)
        .unwrap()
        .lease
        .allocations
        .iter()
        .find(|a| a.key == "port_web")
        .unwrap()
        .value
        .parse()
        .unwrap();
    let routes = route_table(&lifecycle.store, &host.root()).unwrap();
    let hosts: Vec<&str> = routes.iter().map(|r| r.host.as_str()).collect();
    assert!(
        hosts.contains(&format!("web.{namespace}.localhost").as_str()),
        "{hosts:?}"
    );
    assert!(
        hosts.contains(&format!("{namespace}.localhost").as_str()),
        "primary host"
    );
    assert!(
        routes.iter().all(|r| r.upstream == Some(reserved)),
        "only the reserved port"
    );
    let found = lookup(&routes, &format!("WEB.{namespace}.localhost:4180")).unwrap();
    assert_eq!(found.env_id, id, "case-insensitive, port ignored");

    lifecycle.park(&id, "test").unwrap();
    let parked = route_table(&lifecycle.store, &host.root()).unwrap();
    assert!(
        parked
            .iter()
            .all(|r| r.upstream.is_none() && r.state == "parked")
    );
    lifecycle.down(&id, None).unwrap();
    assert!(
        route_table(&lifecycle.store, &host.root())
            .unwrap()
            .is_empty()
    );
}

/// #1214 acceptance: a request to a parked environment's host is held, the
/// environment wakes, and the request succeeds.
#[test]
fn a_request_to_a_parked_environment_wakes_it_and_succeeds() {
    if !python_available() {
        eprintln!("SKIP wake on request: python3 not available");
        return;
    }
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::write(
        worktree.path().join("hello.txt"),
        "hello from the environment",
    )
    .unwrap();
    let (mut lifecycle, id) = up(&host, &web(43150), worktree.path());
    let namespace = lifecycle
        .store
        .get(&id)
        .unwrap()
        .unwrap()
        .namespace
        .unwrap();
    lifecycle.park(&id, "test").unwrap();
    let proxy = host.proxy();

    let response = proxy.get(&format!("web.{namespace}.localhost"), "/hello.txt");
    assert!(
        response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"),
        "{response}"
    );
    assert!(
        response.ends_with("hello from the environment"),
        "{response}"
    );
    assert_eq!(
        lifecycle.store.get(&id).unwrap().unwrap().phase,
        EnvironmentPhase::Active,
        "the held request woke it"
    );
    assert!(
        super::activity::last_touch_ms(&host.root(), &id).is_some(),
        "proxied requests are activity"
    );

    let unknown = proxy.get("nobody.localhost", "/");
    assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
    lifecycle.down(&id, None).unwrap();
}

#[test]
fn the_wake_endpoint_needs_the_token_and_never_creates() {
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    let text = r#"
schema = 1
[ports]
start = 43200
end = 43299
[[profiles]]
name = "p"
[[profiles.components]]
name = "web"
runtime = "process"
listen = true
route = true
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 5, interval_ms = 50 }
"#;
    let (mut lifecycle, id) = up(&host, text, worktree.path());
    let namespace = lifecycle
        .store
        .get(&id)
        .unwrap()
        .unwrap()
        .namespace
        .unwrap();
    lifecycle.park(&id, "test").unwrap();
    let proxy = host.proxy();
    let token = "t".repeat(64);

    let anonymous = proxy.request(&format!(
        "POST /__aethyme/wake/{namespace} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n"
    ));
    assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous}");
    let wrong = proxy.request(&format!(
        "POST /__aethyme/wake/{namespace} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
        "u".repeat(64)
    ));
    assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
    let unknown = proxy.request(&format!(
        "POST /__aethyme/wake/env-nope HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\r\n"
    ));
    assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
    assert_eq!(
        lifecycle.store.list(None).unwrap().len(),
        1,
        "nothing was created"
    );

    let woken = proxy.request(&format!("POST /__aethyme/wake/{namespace} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\r\n"));
    assert!(woken.starts_with("HTTP/1.1 200"), "{woken}");
    assert_eq!(
        lifecycle.store.get(&id).unwrap().unwrap().phase,
        EnvironmentPhase::Active
    );

    let routes = proxy.get("x", "/__aethyme/routes");
    assert!(
        routes.starts_with("HTTP/1.1 200") && routes.contains("aethyme.env-routes/v1"),
        "{routes}"
    );
    let tag = routes
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .unwrap()
        .to_string();
    let unchanged = proxy.request(&format!(
        "GET /__aethyme/routes HTTP/1.1\r\nHost: x\r\nIf-None-Match: {tag}\r\n\r\n"
    ));
    assert!(unchanged.starts_with("HTTP/1.1 304"), "{unchanged}");
    lifecycle.down(&id, None).unwrap();
}

#[test]
fn static_components_are_served_without_a_process_and_stay_inside_their_directory() {
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(worktree.path().join("dist/assets")).unwrap();
    std::fs::write(worktree.path().join("dist/index.html"), "<h1>static</h1>").unwrap();
    std::fs::write(worktree.path().join("dist/assets/app.js"), "console.log(1)").unwrap();
    std::fs::write(worktree.path().join("secret.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(
        worktree.path().join("secret.txt"),
        worktree.path().join("dist/leak.txt"),
    )
    .unwrap();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
"#;
    let (mut lifecycle, id) = up(&host, text, worktree.path());
    let namespace = lifecycle
        .store
        .get(&id)
        .unwrap()
        .unwrap()
        .namespace
        .unwrap();
    let proxy = host.proxy();
    let site = format!("site.{namespace}.localhost");
    let index = proxy.get(&site, "/");
    assert!(
        index.starts_with("HTTP/1.1 200") && index.ends_with("<h1>static</h1>"),
        "{index}"
    );
    assert!(index.contains("text/html"), "{index}");
    let script = proxy.get(&site, "/assets/app.js?v=1");
    assert!(
        script.contains("text/javascript") && script.ends_with("console.log(1)"),
        "{script}"
    );
    for escape in ["/../secret.txt", "/leak.txt"] {
        let refused = proxy.get(&site, escape);
        assert!(refused.starts_with("HTTP/1.1 404"), "{escape}: {refused}");
    }
    // Without spa_fallback a client-side route is not a file.
    let route = proxy.get(&site, "/o/settings");
    assert!(route.starts_with("HTTP/1.1 404"), "{route}");
    lifecycle.down(&id, None).unwrap();
}

/// #1291: with `spa_fallback`, a client-side route loads `index.html`; a
/// missing asset (it has an extension) and an escape stay 404.
#[test]
fn a_single_page_app_falls_back_to_index_html() {
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(worktree.path().join("dist/assets")).unwrap();
    std::fs::write(worktree.path().join("dist/index.html"), "<h1>app</h1>").unwrap();
    std::fs::write(worktree.path().join("dist/health"), "healthy").unwrap();
    std::fs::File::create(worktree.path().join("dist/oversized"))
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    std::fs::create_dir(worktree.path().join("outside")).unwrap();
    std::fs::write(worktree.path().join("outside/private"), "outside").unwrap();
    for (target, link) in [
        ("outside/private", "dist/refused"),
        ("outside", "dist/refused-parent"),
        ("dist/absent-target", "dist/dangling"),
    ] {
        std::os::unix::fs::symlink(worktree.path().join(target), worktree.path().join(link))
            .unwrap();
    }
    std::fs::write(worktree.path().join("dist/assets/app.js"), "console.log(1)").unwrap();
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
spa_fallback = true
"#;
    let (mut lifecycle, id) = up(&host, text, worktree.path());
    let namespace = lifecycle
        .store
        .get(&id)
        .unwrap()
        .unwrap()
        .namespace
        .unwrap();
    let proxy = host.proxy();
    let site = format!("site.{namespace}.localhost");
    for route in ["/o/settings", "/o/settings?tab=2", "/deep/link/"] {
        let page = proxy.get(&site, route);
        assert!(
            page.starts_with("HTTP/1.1 200") && page.ends_with("<h1>app</h1>"),
            "{route}: {page}"
        );
        assert!(page.contains("text/html"), "{route}: {page}");
    }
    let script = proxy.get(&site, "/assets/app.js");
    assert!(script.ends_with("console.log(1)"), "{script}");
    let health = proxy.get(&site, "/health");
    assert!(health.ends_with("healthy"), "{health}");
    for missing in [
        "/missing.js",
        "/assets/gone.css",
        "/../escape",
        "/oversized",
        "/refused",
        "/refused-parent/missing-route",
        "/dangling",
    ] {
        let refused = proxy.get(&site, missing);
        assert!(refused.starts_with("HTTP/1.1 404"), "{missing}: {refused}");
    }
    lifecycle.down(&id, None).unwrap();
}

/// #1296 acceptance: one address serves a static app and forwards `/api` to
/// the api component, waking it when parked; the prefix is kept, `/apix` is
/// not under `/api`, and an `http://` target receives its own Host header.
#[test]
fn per_path_routes_share_one_address() {
    if !python_available() {
        eprintln!("SKIP per-path routes: python3 not available");
        return;
    }
    // An external upstream that echoes the Host header it received.
    let external = TcpListener::bind("127.0.0.1:0").unwrap();
    let external_port = external.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in external.incoming().take(1) {
            let mut stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let host = request
                .lines()
                .find_map(|line| line.strip_prefix("Host: "))
                .unwrap_or("none")
                .to_string();
            let line = request.lines().next().unwrap_or_default().to_string();
            let body = format!("{line}|{host}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(worktree.path().join("dist")).unwrap();
    std::fs::write(worktree.path().join("dist/index.html"), "from-static").unwrap();
    std::fs::create_dir_all(worktree.path().join("api")).unwrap();
    std::fs::write(worktree.path().join("api/hello.txt"), "from-api").unwrap();
    let text = format!(
        r#"
schema = 1
[ports]
start = 43250
end = 43299
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
paths = [{{ prefix = "/api", to = "api" }}, {{ prefix = "/ext", to = "http://127.0.0.1:{external_port}" }}]
[[profiles.components]]
name = "api"
runtime = "process"
listen = true
route = true
start = "exec python3 -m http.server --bind 127.0.0.1 $AETHYME_ENV_PORT_API"
health = {{ http = "/", timeout_seconds = 20, interval_ms = 100 }}
"#
    );
    let (mut lifecycle, id) = up(&host, &text, worktree.path());
    let namespace = lifecycle
        .store
        .get(&id)
        .unwrap()
        .unwrap()
        .namespace
        .unwrap();
    lifecycle.park(&id, "test").unwrap();
    let proxy = host.proxy();
    let site = format!("site.{namespace}.localhost");

    let api = proxy.get(&site, "/api/hello.txt");
    assert!(api.ends_with("from-api"), "the prefix is kept: {api}");
    assert_eq!(
        lifecycle.store.get(&id).unwrap().unwrap().phase,
        EnvironmentPhase::Active,
        "a parked path target wakes like a host route"
    );
    let index = proxy.get(&site, "/index.html");
    assert!(index.ends_with("from-static"), "{index}");
    let not_api = proxy.get(&site, "/apix");
    assert!(
        not_api.starts_with("HTTP/1.1 404"),
        "/apix is not under /api: {not_api}"
    );
    let ext = proxy.get(&site, "/ext/x?y=1");
    assert!(
        ext.ends_with(&format!(
            "GET /ext/x?y=1 HTTP/1.1|127.0.0.1:{external_port}"
        )),
        "{ext}"
    );
    lifecycle.down(&id, None).unwrap();
}
