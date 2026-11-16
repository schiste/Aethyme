//! The hub in-process, against a scripted fake MCP server (POSIX sh).
//!
//! Each test runs its own hub in a private temporary directory with short
//! timings set through `HubConfig` (never the environment), and speaks the
//! hub protocol over its socket the way `connect` does.

use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::hub::{HubConfig, HubPaths, build_identity, serve};
use super::jsonrpc::{self, read_message};

/// A fake MCP server. The hub always sends numeric ids and progress tokens,
/// so `sed` can pick them out. It logs every line it receives.
const FAKE_SERVER: &str = r#"#!/bin/sh
log="$FAKE_LOG"
printf 'started %s\n' "$$" >> "$log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    initialize)
      caps='{"tools":{"listChanged":true}}'
      if [ -n "$FAKE_NEEDS_SAMPLING" ]; then caps='{"sampling":{}}'; fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"capabilities":%s,"protocolVersion":"2025-06-18","serverInfo":{"name":"fake","version":"1"}}}\n' "$id" "$caps"
      ;;
    tools/call)
      tool=$(printf '%s' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
      case "$tool" in
        pid) printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"%s"}}\n' "$id" "$$" ;;
        mode) printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"%s"}}\n' "$id" "$FAKE_MODE" ;;
        notify)
          printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
          printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
        progress)
          token=$(printf '%s' "$line" | sed -n 's/.*"progressToken":\([0-9][0-9]*\).*/\1/p')
          printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1,"progressToken":%s}}\n' "$token"
          printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
        slow) sleep 1; printf '{"jsonrpc":"2.0","id":%s,"result":{"late":true}}\n' "$id" ;;
        crash) exit 3 ;;
      esac
      ;;
  esac
done
"#;

struct TestHub {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: HubPaths,
    script: PathBuf,
    thread: Option<thread::JoinHandle<std::io::Result<()>>>,
}

impl TestHub {
    fn start(idle: Duration, grace: Duration, backoff: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub_dir = root.join("hub");
        std::fs::create_dir(&hub_dir).unwrap();
        std::fs::set_permissions(&hub_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let script = root.join("fake-server.sh");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = HubPaths::new(&hub_dir);
        let config = HubConfig {
            paths: paths.clone(),
            events_dir: Some(root.clone()),
            idle_timeout: idle,
            empty_grace: grace,
            initial_backoff: backoff,
            max_backoff: backoff * 4,
            register_leases: false,
        };
        let thread = thread::spawn(move || serve(config));
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&paths.socket).is_err() {
            assert!(Instant::now() < deadline, "the hub did not start");
            thread::sleep(Duration::from_millis(20));
        }
        Self {
            _dir: dir,
            root,
            paths,
            script,
            thread: Some(thread),
        }
    }

    fn log(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.log"))
    }

    fn hello(&self, key: &str, log: &Path, extra: &[(&str, &str)]) -> Value {
        let mut env = vec![
            json!(["PATH", "/usr/bin:/bin"]),
            json!(["FAKE_LOG", log.to_string_lossy()]),
        ];
        env.extend(extra.iter().map(|(name, value)| json!([name, value])));
        json!({
            "hub": 1,
            "op": "attach",
            "build": build_identity(),
            "key": key,
            "name": "fake",
            "argv": [self.script.to_string_lossy()],
            "cwd": self.root,
            "env": env,
        })
    }

    /// Wait for the hub to stop by itself.
    fn join(mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while self
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.thread.take().map(|thread| thread.join()).is_some()
    }
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect(hub: &TestHub) -> Self {
        let stream = UnixStream::connect(&hub.paths.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Self { stream, reader }
    }

    fn send(&mut self, message: &Value) {
        self.stream.write_all(&jsonrpc::encode(message)).unwrap();
    }

    fn recv(&mut self) -> Option<Value> {
        let line = read_message(&mut self.reader).ok()??;
        serde_json::from_slice(&line).ok()
    }

    /// Attach and initialize; the first reply (hello) and the initialize
    /// answer.
    fn attach(
        hub: &TestHub,
        key: &str,
        log: &Path,
        extra: &[(&str, &str)],
        protocol: &str,
    ) -> (Self, Value, Option<Value>) {
        let mut client = Self::connect(hub);
        client.send(&hub.hello(key, log, extra));
        let hello = client.recv().expect("hello reply");
        if hello["ok"] != true {
            return (client, hello, None);
        }
        client.send(&json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": {"protocolVersion": protocol, "capabilities": {},
                       "clientInfo": {"name": "test", "version": "1"}}
        }));
        let initialized = client.recv();
        if initialized
            .as_ref()
            .is_some_and(|reply| reply.get("result").is_some())
        {
            client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        }
        (client, hello, initialized)
    }

    fn call(&mut self, id: Value, tool: &str) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": {}}
        }));
    }
}

fn key(digit: char) -> String {
    std::iter::repeat_n(digit, 64).collect()
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for(condition: impl Fn() -> bool, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    true
}

const PROTOCOL: &str = "2025-06-18";
const SHORT: Duration = Duration::from_millis(300);
/// Long enough that a hub never exits before a test's first attach.
const GRACE: Duration = Duration::from_secs(2);

#[test]
fn two_clients_share_one_upstream_and_their_ids_map_back() {
    let hub = TestHub::start(SHORT, GRACE, SHORT);
    let log = hub.log("shared");
    let (mut first, hello, initialized) = Client::attach(&hub, &key('a'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], true, "{hello}");
    let initialized = initialized.unwrap();
    assert_eq!(initialized["id"], "init");
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL);
    let (mut second, _, initialized) = Client::attach(&hub, &key('a'), &log, &[], PROTOCOL);
    assert_eq!(initialized.unwrap()["id"], "init");

    // Both use id 1; each gets its own answer back, from the same process.
    first.call(json!(1), "pid");
    second.call(json!(1), "pid");
    let one = first.recv().unwrap();
    let two = second.recv().unwrap();
    assert_eq!(one["id"], 1);
    assert_eq!(two["id"], 1);
    assert_eq!(one["result"]["text"], two["result"]["text"]);
    // A string id keeps its type.
    second.call(json!("1"), "pid");
    assert_eq!(second.recv().unwrap()["id"], "1");

    let log_text = read_log(&log);
    assert_eq!(log_text.matches("started ").count(), 1, "{log_text}");
    assert_eq!(log_text.matches("\"method\":\"initialize\"").count(), 1);
    assert_eq!(
        log_text.matches("notifications/initialized").count(),
        1,
        "client initialized notifications are swallowed: {log_text}"
    );

    // Status reports the upstream and its counters.
    let mut status = Client::connect(&hub);
    status.send(&json!({"hub": 1, "op": "status"}));
    let report = status.recv().unwrap();
    let upstream = &report["upstreams"][0];
    assert_eq!(upstream["clients"], 2);
    assert_eq!(
        upstream["pid"].to_string(),
        one["result"]["text"].as_str().unwrap()
    );
    assert_eq!(report["counters"][&key('a')[..12]]["requests"], 3);
    drop((first, second, status));
    assert!(
        hub.join(Duration::from_secs(10)),
        "the idle hub did not stop"
    );
}

#[test]
fn progress_maps_back_and_list_changed_fans_out() {
    let hub = TestHub::start(SHORT, GRACE, SHORT);
    let log = hub.log("fanout");
    let (mut first, _, _) = Client::attach(&hub, &key('b'), &log, &[], PROTOCOL);
    let (mut second, _, _) = Client::attach(&hub, &key('b'), &log, &[], PROTOCOL);
    first.send(&json!({
        "jsonrpc": "2.0", "id": "p1", "method": "tools/call",
        "params": {"name": "progress", "arguments": {}, "_meta": {"progressToken": "tok"}}
    }));
    let progress = first.recv().unwrap();
    assert_eq!(progress["method"], "notifications/progress");
    assert_eq!(progress["params"]["progressToken"], "tok");
    assert_eq!(first.recv().unwrap()["id"], "p1");

    second.call(json!(7), "notify");
    assert_eq!(
        second.recv().unwrap()["method"],
        "notifications/tools/list_changed"
    );
    assert_eq!(second.recv().unwrap()["id"], 7);
    assert_eq!(
        first.recv().unwrap()["method"],
        "notifications/tools/list_changed"
    );
    drop((first, second));
    assert!(hub.join(Duration::from_secs(10)));
}

#[test]
fn a_disconnect_mid_request_cancels_upstream_and_drops_the_late_reply() {
    let hub = TestHub::start(Duration::from_secs(5), GRACE, SHORT);
    let log = hub.log("cancel");
    let (mut leaving, _, _) = Client::attach(&hub, &key('c'), &log, &[], PROTOCOL);
    let (mut staying, _, _) = Client::attach(&hub, &key('c'), &log, &[], PROTOCOL);
    leaving.call(json!(9), "slow");
    thread::sleep(Duration::from_millis(200));
    leaving.stream.shutdown(std::net::Shutdown::Both).unwrap();
    drop(leaving);
    assert!(
        wait_for(
            || read_log(&log).contains("notifications/cancelled"),
            Duration::from_secs(10)
        ),
        "{}",
        read_log(&log)
    );
    // The staying client is served and never sees the late reply.
    staying.call(json!(1), "pid");
    let reply = staying.recv().unwrap();
    assert_eq!(reply["id"], 1);
    assert!(reply["result"].get("late").is_none());
}

#[test]
fn a_crash_fails_requests_retryably_restarts_and_turns_unhealthy() {
    let hub = TestHub::start(
        Duration::from_secs(5),
        Duration::from_secs(5),
        Duration::from_millis(20),
    );
    let log = hub.log("crash");
    let (mut client, _, _) = Client::attach(&hub, &key('d'), &log, &[], PROTOCOL);
    for round in 0..4 {
        client.call(json!(round), "crash");
        let error = client.recv().unwrap();
        assert_eq!(error["id"], round, "{error}");
        assert_eq!(error["error"]["code"], jsonrpc::RETRYABLE);
        assert_eq!(error["error"]["data"]["retryable"], true);
        // Restarted and initialized again: tools may have changed.
        let changed = client.recv().unwrap();
        assert_eq!(changed["method"], "notifications/tools/list_changed");
    }
    assert_eq!(read_log(&log).matches("started ").count(), 5);
    // The fifth crash within five minutes: unhealthy, clients disconnected.
    client.call(json!(4), "crash");
    let mut closed = false;
    for _ in 0..3 {
        match client.recv() {
            None => {
                closed = true;
                break;
            }
            Some(message) => assert!(message.get("error").is_some(), "{message}"),
        }
    }
    assert!(closed);
    let (_, hello, _) = Client::attach(&hub, &key('d'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], false);
    assert_eq!(hello["reason"], "unhealthy");
}

#[test]
fn an_idle_upstream_stops_and_then_the_hub_exits() {
    let hub = TestHub::start(SHORT, GRACE, SHORT);
    let log = hub.log("idle");
    let (mut client, _, _) = Client::attach(&hub, &key('e'), &log, &[], PROTOCOL);
    client.call(json!(1), "pid");
    let pid: i32 = client.recv().unwrap()["result"]["text"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    drop(client);
    assert!(hub.join(Duration::from_secs(10)), "the hub did not exit");
    let gone = wait_for(
        || {
            // SAFETY: signal 0 only checks whether `pid` exists; no signal is sent.
            unsafe { libc::kill(pid, 0) != 0 }
        },
        Duration::from_secs(10),
    );
    assert!(gone, "the upstream {pid} outlived its hub");
}

#[test]
fn different_keys_and_env_files_get_separate_upstreams_with_their_own_env() {
    let hub = TestHub::start(SHORT, GRACE, SHORT);
    let prod_log = hub.log("prod");
    let staging_log = hub.log("staging");
    let (mut prod, _, _) = Client::attach(
        &hub,
        &key('1'),
        &prod_log,
        &[("FAKE_MODE", "prod")],
        PROTOCOL,
    );
    let (mut staging, _, _) = Client::attach(
        &hub,
        &key('2'),
        &staging_log,
        &[("FAKE_MODE", "staging")],
        PROTOCOL,
    );
    prod.call(json!(1), "mode");
    staging.call(json!(1), "mode");
    assert_eq!(prod.recv().unwrap()["result"]["text"], "prod");
    assert_eq!(staging.recv().unwrap()["result"]["text"], "staging");
    prod.call(json!(2), "pid");
    staging.call(json!(2), "pid");
    assert_ne!(
        prod.recv().unwrap()["result"]["text"],
        staging.recv().unwrap()["result"]["text"]
    );
}

#[test]
fn another_protocol_version_or_client_features_mean_a_private_server() {
    let hub = TestHub::start(Duration::from_secs(5), GRACE, SHORT);
    let log = hub.log("protocol");
    let (_first, _, _) = Client::attach(&hub, &key('f'), &log, &[], PROTOCOL);
    let (_, hello, answer) = Client::attach(&hub, &key('f'), &log, &[], "2024-11-05");
    assert_eq!(hello["ok"], true);
    assert_eq!(
        answer.unwrap(),
        json!({"aethyme_hub": "fallback", "reason": "protocol_version"})
    );

    let sampling_log = hub.log("sampling");
    let (_, _, answer) = Client::attach(
        &hub,
        &key('9'),
        &sampling_log,
        &[("FAKE_NEEDS_SAMPLING", "1")],
        PROTOCOL,
    );
    assert_eq!(answer.unwrap()["aethyme_hub"], "fallback");
    let (_, hello, _) = Client::attach(&hub, &key('9'), &sampling_log, &[], PROTOCOL);
    assert_eq!(hello["ok"], false);
    assert_eq!(hello["reason"], "not_shareable");
}

#[test]
fn another_build_is_refused_and_events_are_recorded_without_bodies() {
    let hub = TestHub::start(SHORT, GRACE, SHORT);
    let log = hub.log("build");
    let mut client = Client::connect(&hub);
    let mut hello = hub.hello(&key('7'), &log, &[]);
    hello["build"] = json!("another build");
    client.send(&hello);
    let reply = client.recv().unwrap();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["reason"], "build_mismatch");

    let (mut client, _, _) = Client::attach(&hub, &key('7'), &log, &[], PROTOCOL);
    client.call(json!("secret-request-id"), "pid");
    client.recv().unwrap();
    let events = read_log(&hub.root.join(super::telemetry::EVENTS_FILE));
    assert!(events.contains("upstream_started"), "{events}");
    assert!(!events.contains("secret-request-id"), "{events}");
    assert!(!events.contains("tools/call"), "{events}");
}
