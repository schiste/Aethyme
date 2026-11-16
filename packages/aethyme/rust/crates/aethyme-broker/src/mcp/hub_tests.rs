//! The hub in-process, against a scripted fake MCP server (POSIX sh).
//!
//! Each test runs its own hub in a private temporary directory with short
//! timings set through `HubConfig` (never the environment), and speaks the
//! hub protocol over its socket the way `connect` does. A hub waits a long
//! startup grace for its first attach, so a loaded runner cannot make it
//! exit before a test begins.

use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::hub::{
    HubConfig, HubPaths, UpstreamRecord, build_identity, process_start, reap_recorded_upstreams,
    serve, write_record,
};
use super::jsonrpc::{self, read_message};
use super::mux::{INVALID_REQUEST, METHOD_NOT_FOUND};

/// A fake MCP server that behaves like real ones: it declares server
/// capabilities only. The hub always sends numeric ids and progress tokens,
/// so `sed` can pick them out. It logs every line it receives.
const FAKE_SERVER: &str = r#"#!/bin/sh
log="$FAKE_LOG"
printf 'started %s\n' "$$" >> "$log"
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$1" "$2"; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    initialize)
      if [ -z "$FAKE_HANG_INIT" ]; then
        reply "$id" '{"capabilities":{"resources":{"subscribe":true},"tools":{"listChanged":true}},"protocolVersion":"2025-06-18","serverInfo":{"name":"fake","version":"1"}}'
      fi
      ;;
    resources/subscribe|resources/unsubscribe) reply "$id" '{}' ;;
    tools/call)
      tool=$(printf '%s' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
      case "$tool" in
        pid) reply "$id" "{\"text\":\"$$\"}" ;;
        mode) reply "$id" "{\"text\":\"$FAKE_MODE\"}" ;;
        notify)
          printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
          reply "$id" '{}' ;;
        progress)
          token=$(printf '%s' "$line" | sed -n 's/.*"progressToken":\([0-9][0-9]*\).*/\1/p')
          printf '{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1,"progressToken":%s}}\n' "$token"
          reply "$id" '{}' ;;
        log)
          printf '{"jsonrpc":"2.0","method":"notifications/message","params":{"data":"private arguments","level":"info"}}\n'
          reply "$id" '{}' ;;
        touch)
          printf '{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":"file:///a"}}\n'
          reply "$id" '{}' ;;
        roots)
          printf '{"jsonrpc":"2.0","id":900,"method":"roots/list","params":{}}\n'
          IFS= read -r answer
          printf 'answer %s\n' "$answer" >> "$log"
          reply "$id" '{}' ;;
        big)
          big=$(printf '%6000s' '' | tr ' ' x)
          reply "$id" "{\"text\":\"$big\"}" ;;
        slow) sleep 1; reply "$id" '{"late":true}' ;;
        crash) exit 3 ;;
      esac
      ;;
  esac
done
"#;

fn test_config(paths: HubPaths, events: PathBuf) -> HubConfig {
    HubConfig {
        paths,
        events_dir: Some(events),
        idle_timeout: Duration::from_millis(300),
        empty_grace: Duration::from_millis(300),
        startup_grace: Duration::from_secs(120),
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(80),
        init_timeout: Duration::from_secs(15),
        max_connections: 64,
        client_queue: 256,
        client_queue_bytes: 64 * 1024 * 1024,
        upstream_queue: 1024,
        upstream_queue_bytes: 64 * 1024 * 1024,
        per_client_in_flight: 64,
        global_in_flight: 1024,
        register_leases: false,
        handle_signals: false,
    }
}

struct TestHub {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: HubPaths,
    script: PathBuf,
    thread: Option<thread::JoinHandle<std::io::Result<()>>>,
}

impl TestHub {
    fn start(adjust: impl FnOnce(&mut HubConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub_dir = root.join("hub");
        std::fs::create_dir(&hub_dir).unwrap();
        std::fs::set_permissions(&hub_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let script = root.join("fake-server.sh");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = HubPaths::new(&hub_dir);
        let mut config = test_config(paths.clone(), root.clone());
        adjust(&mut config);
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

    fn status(&self) -> Value {
        let mut client = Client::connect(self);
        client.send(&json!({"hub": 1, "op": "status"}));
        client.recv().unwrap()
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

    /// Attach and initialize with these client capabilities; the hello reply
    /// and the initialize answer.
    fn attach_with(
        hub: &TestHub,
        key: &str,
        log: &Path,
        extra: &[(&str, &str)],
        protocol: &str,
        capabilities: Value,
    ) -> (Self, Value, Option<Value>) {
        let mut client = Self::connect(hub);
        client.send(&hub.hello(key, log, extra));
        let hello = client.recv().expect("hello reply");
        if hello["ok"] != true {
            return (client, hello, None);
        }
        client.send(&json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": {"protocolVersion": protocol, "capabilities": capabilities,
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

    fn attach(
        hub: &TestHub,
        key: &str,
        log: &Path,
        extra: &[(&str, &str)],
        protocol: &str,
    ) -> (Self, Value, Option<Value>) {
        Self::attach_with(hub, key, log, extra, protocol, json!({}))
    }

    fn call(&mut self, id: Value, tool: &str) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": {}}
        }));
    }

    fn request(&mut self, id: Value, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }
}

/// A distinct 64-character hex key per test letter.
fn key(letter: char) -> String {
    format!("{:02x}", u32::from(letter)).repeat(32)
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

#[test]
fn two_clients_share_one_upstream_initialized_by_the_hub_itself() {
    let hub = TestHub::start(|_| {});
    let log = hub.log("shared");
    // The first client declares capabilities; the upstream never sees them.
    let (mut first, hello, initialized) = Client::attach_with(
        &hub,
        &key('a'),
        &log,
        &[],
        PROTOCOL,
        json!({"roots": {"listChanged": true}, "sampling": {}}),
    );
    assert_eq!(hello["ok"], true, "{hello}");
    let initialized = initialized.unwrap();
    assert_eq!(initialized["id"], "init");
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL);
    let (mut second, _, initialized) = Client::attach(&hub, &key('a'), &log, &[], PROTOCOL);
    assert_eq!(initialized.unwrap()["id"], "init");

    first.call(json!(1), "pid");
    second.call(json!(1), "pid");
    let one = first.recv().unwrap();
    let two = second.recv().unwrap();
    assert_eq!(one["id"], 1);
    assert_eq!(two["id"], 1);
    assert_eq!(one["result"]["text"], two["result"]["text"]);
    second.call(json!("1"), "pid");
    assert_eq!(second.recv().unwrap()["id"], "1");

    let log_text = read_log(&log);
    assert_eq!(log_text.matches("started ").count(), 1, "{log_text}");
    let initialize_line = log_text
        .lines()
        .find(|line| line.contains("\"method\":\"initialize\""))
        .unwrap();
    assert!(
        initialize_line.contains("\"capabilities\":{}"),
        "{initialize_line}"
    );
    assert!(
        initialize_line.contains("aethyme-mcp-hub"),
        "{initialize_line}"
    );
    assert!(!initialize_line.contains("roots"), "{initialize_line}");
    assert_eq!(log_text.matches("notifications/initialized").count(), 1);

    let report = hub.status();
    assert_eq!(report["upstreams"][0]["clients"], 2);
    assert_eq!(
        report["upstreams"][0]["pid"].to_string(),
        one["result"]["text"].as_str().unwrap()
    );
    assert_eq!(report["counters"][&key('a')[..12]]["requests"], 3);
    drop((first, second));
    assert!(
        hub.join(Duration::from_secs(10)),
        "the idle hub did not stop"
    );
}

#[test]
fn server_requests_are_refused_and_never_reach_a_client() {
    let hub = TestHub::start(|_| {});
    let log = hub.log("roots");
    let (mut client, _, _) = Client::attach_with(
        &hub,
        &key('b'),
        &log,
        &[],
        PROTOCOL,
        json!({"roots": {"listChanged": true}}),
    );
    // A client's roots notification is swallowed.
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/roots/list_changed"}));
    client.call(json!(1), "roots");
    let reply = client.recv().unwrap();
    assert_eq!(
        reply["id"], 1,
        "the client must not see the server request: {reply}"
    );
    let log_text = read_log(&log);
    let answer = log_text
        .lines()
        .find(|line| line.starts_with("answer "))
        .unwrap();
    assert!(
        answer.contains(&format!("\"code\":{METHOD_NOT_FOUND}")),
        "{answer}"
    );
    assert!(!log_text.contains("roots/list_changed"), "{log_text}");
    assert_eq!(
        hub.status()["counters"][&key('b')[..12]]["refused_server_requests"],
        1
    );
}

#[test]
fn progress_maps_back_and_list_changed_fans_out() {
    let hub = TestHub::start(|_| {});
    let log = hub.log("fanout");
    let (mut first, _, _) = Client::attach(&hub, &key('c'), &log, &[], PROTOCOL);
    let (mut second, _, _) = Client::attach(&hub, &key('c'), &log, &[], PROTOCOL);
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
}

#[test]
fn logging_and_subscriptions_stay_per_client() {
    let hub = TestHub::start(|config| config.idle_timeout = Duration::from_secs(10));
    let log = hub.log("session");
    let (mut first, _, _) = Client::attach(&hub, &key('d'), &log, &[], PROTOCOL);
    let (mut second, _, _) = Client::attach(&hub, &key('d'), &log, &[], PROTOCOL);
    let (mut third, _, _) = Client::attach(&hub, &key('d'), &log, &[], PROTOCOL);

    first.request(json!(1), "logging/setLevel", json!({"level": "debug"}));
    assert_eq!(
        first.recv().unwrap(),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}})
    );
    first.call(json!(2), "log");
    // The log line is dropped; only the reply arrives.
    assert_eq!(first.recv().unwrap()["id"], 2);

    let subscribe = json!({"uri": "file:///a"});
    first.request(json!(3), "resources/subscribe", subscribe.clone());
    assert_eq!(first.recv().unwrap()["id"], 3);
    second.request(json!(3), "resources/subscribe", subscribe.clone());
    assert_eq!(second.recv().unwrap()["id"], 3);
    third.call(json!(4), "touch");
    assert_eq!(
        third.recv().unwrap()["id"],
        4,
        "not subscribed: only its reply"
    );
    for client in [&mut first, &mut second] {
        let updated = client.recv().unwrap();
        assert_eq!(updated["method"], "notifications/resources/updated");
        assert_eq!(updated["params"]["uri"], "file:///a");
    }
    first.request(json!(5), "resources/unsubscribe", subscribe.clone());
    assert_eq!(first.recv().unwrap()["id"], 5);
    let log_text = read_log(&log);
    assert!(!log_text.contains("logging/setLevel"), "{log_text}");
    assert_eq!(
        log_text.matches("resources/subscribe").count(),
        1,
        "{log_text}"
    );
    assert_eq!(log_text.matches("resources/unsubscribe").count(), 0);
    // The last subscriber leaves by disconnecting: the upstream is unsubscribed.
    drop(second);
    assert!(wait_for(
        || read_log(&log).matches("resources/unsubscribe").count() == 1,
        Duration::from_secs(10)
    ));
    assert_eq!(
        hub.status()["counters"][&key('d')[..12]]["dropped_log_messages"],
        1
    );
}

#[test]
fn a_disconnect_mid_request_cancels_upstream_and_drops_the_late_reply() {
    let hub = TestHub::start(|config| config.idle_timeout = Duration::from_secs(5));
    let log = hub.log("cancel");
    let (mut leaving, _, _) = Client::attach(&hub, &key('e'), &log, &[], PROTOCOL);
    let (mut staying, _, _) = Client::attach(&hub, &key('e'), &log, &[], PROTOCOL);
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
    staying.call(json!(1), "pid");
    let reply = staying.recv().unwrap();
    assert_eq!(reply["id"], 1);
    assert!(reply["result"].get("late").is_none());
}

#[test]
fn a_crash_fails_requests_retryably_restarts_and_turns_unhealthy() {
    let hub = TestHub::start(|config| {
        config.idle_timeout = Duration::from_secs(5);
        config.empty_grace = Duration::from_secs(5);
    });
    let log = hub.log("crash");
    let (mut client, _, _) = Client::attach(&hub, &key('f'), &log, &[], PROTOCOL);
    for round in 0..4 {
        client.call(json!(round), "crash");
        let error = client.recv().unwrap();
        assert_eq!(error["id"], round, "{error}");
        assert_eq!(error["error"]["code"], jsonrpc::RETRYABLE);
        assert_eq!(error["error"]["data"]["retryable"], true);
        let changed = client.recv().unwrap();
        assert_eq!(changed["method"], "notifications/tools/list_changed");
    }
    assert_eq!(read_log(&log).matches("started ").count(), 5);
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
    let (_, hello, _) = Client::attach(&hub, &key('f'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], false);
    assert_eq!(hello["reason"], "unhealthy");
}

#[test]
fn requests_during_a_restart_are_held_until_the_upstream_is_ready() {
    let hub = TestHub::start(|config| {
        config.idle_timeout = Duration::from_secs(5);
        config.initial_backoff = Duration::from_millis(500);
        config.max_backoff = Duration::from_millis(500);
    });
    let log = hub.log("held");
    let (mut client, _, _) = Client::attach(&hub, &key('g'), &log, &[], PROTOCOL);
    client.call(json!(1), "pid");
    let before = client.recv().unwrap()["result"]["text"].clone();
    client.call(json!(2), "crash");
    assert_eq!(client.recv().unwrap()["error"]["code"], jsonrpc::RETRYABLE);
    // Sent while the upstream is down: held, then answered by the new one.
    client.call(json!(3), "pid");
    let changed = client.recv().unwrap();
    assert_eq!(changed["method"], "notifications/tools/list_changed");
    let after = client.recv().unwrap();
    assert_eq!(after["id"], 3, "{after}");
    assert_ne!(after["result"]["text"], before);
    // The held request reached the new process after its initialize.
    let log_text = read_log(&log);
    let restarted = log_text.rfind("started ").unwrap();
    let tail = &log_text[restarted..];
    let initialized = tail.find("notifications/initialized").unwrap();
    let call = tail.find("tools/call").unwrap();
    assert!(initialized < call, "{tail}");
}

#[test]
fn an_initialize_that_never_answers_marks_the_key_unhealthy() {
    let hub = TestHub::start(|config| {
        config.init_timeout = Duration::from_millis(500);
        config.idle_timeout = Duration::from_secs(5);
    });
    let log = hub.log("hang");
    let (_, hello, answer) =
        Client::attach(&hub, &key('h'), &log, &[("FAKE_HANG_INIT", "1")], PROTOCOL);
    assert_eq!(hello["ok"], true);
    assert_eq!(
        answer.unwrap(),
        json!({"aethyme_hub": "fallback", "reason": "initialize_timeout"})
    );
    let (_, hello, _) = Client::attach(&hub, &key('h'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], false);
    assert_eq!(hello["reason"], "unhealthy");
}

#[test]
fn an_idle_upstream_stops_and_then_the_hub_exits() {
    let hub = TestHub::start(|_| {});
    let log = hub.log("idle");
    let (mut client, _, _) = Client::attach(&hub, &key('i'), &log, &[], PROTOCOL);
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
    let hub = TestHub::start(|_| {});
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
fn another_protocol_version_or_another_spec_means_a_private_server() {
    let hub = TestHub::start(|config| config.idle_timeout = Duration::from_secs(5));
    let log = hub.log("protocol");
    let (_first, _, _) = Client::attach(&hub, &key('3'), &log, &[], PROTOCOL);
    let (_, hello, answer) = Client::attach(&hub, &key('3'), &log, &[], "2024-11-05");
    assert_eq!(hello["ok"], true);
    assert_eq!(
        answer.unwrap(),
        json!({"aethyme_hub": "fallback", "reason": "protocol_version"})
    );
    // Same key, different argv/cwd/env: refused before anything starts.
    let (_, hello, _) = Client::attach(&hub, &key('3'), &log, &[("EXTRA", "1")], PROTOCOL);
    assert_eq!(hello["ok"], false);
    assert_eq!(hello["reason"], "spec_mismatch");
}

#[test]
fn batches_reused_ids_and_too_many_requests_get_errors_not_silence() {
    let hub = TestHub::start(|config| {
        config.idle_timeout = Duration::from_secs(5);
        config.per_client_in_flight = 1;
    });
    let log = hub.log("errors");
    let (mut client, _, _) = Client::attach(&hub, &key('4'), &log, &[], PROTOCOL);
    client.send(&json!([{"jsonrpc": "2.0", "id": 1, "method": "tools/list"}]));
    let batch = client.recv().unwrap();
    assert_eq!(batch["error"]["code"], INVALID_REQUEST);
    client.call(json!(2), "slow");
    // Same id again while in flight, and over the per-client cap.
    client.call(json!(2), "pid");
    let error = client.recv().unwrap();
    assert_eq!(error["id"], 2);
    assert!(error.get("error").is_some(), "{error}");
    client.call(json!(3), "pid");
    let busy = client.recv().unwrap();
    assert_eq!(busy["id"], 3);
    assert_eq!(busy["error"]["code"], jsonrpc::RETRYABLE);
    assert_eq!(client.recv().unwrap()["result"]["late"], true);
}

#[test]
fn a_client_over_its_byte_bound_is_disconnected_and_others_keep_working() {
    let hub = TestHub::start(|config| {
        config.idle_timeout = Duration::from_secs(5);
        config.client_queue_bytes = 3000;
    });
    let log = hub.log("overflow");
    let (mut greedy, _, _) = Client::attach(&hub, &key('5'), &log, &[], PROTOCOL);
    let (mut other, _, _) = Client::attach(&hub, &key('5'), &log, &[], PROTOCOL);
    greedy.call(json!(1), "big");
    assert!(
        greedy.recv().is_none(),
        "over 3000 queued bytes: disconnected"
    );
    other.call(json!(1), "pid");
    assert_eq!(other.recv().unwrap()["id"], 1);
    assert!(hub.status()["counters"][&key('5')[..12]]["queue_overflows"] == 1);
}

#[test]
fn connections_are_capped_and_another_build_is_refused() {
    let hub = TestHub::start(|config| config.max_connections = 2);
    // Let the start-up probe's connection close first.
    thread::sleep(Duration::from_millis(300));
    let log = hub.log("cap");
    let (_first, hello, _) = Client::attach(&hub, &key('6'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], true);
    let (_second, hello, _) = Client::attach(&hub, &key('6'), &log, &[], PROTOCOL);
    assert_eq!(hello["ok"], true);
    let mut extra = Client::connect(&hub);
    extra.send(&hub.hello(&key('6'), &log, &[]));
    let refused = extra.recv().unwrap();
    assert_eq!(refused["reason"], "too_many_connections");

    let other = TestHub::start(|_| {});
    let mut client = Client::connect(&other);
    let mut hello = other.hello(&key('7'), &log, &[]);
    hello["build"] = json!("another build");
    client.send(&hello);
    let reply = client.recv().unwrap();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["reason"], "build_mismatch");
}

#[test]
fn events_are_recorded_without_bodies() {
    let hub = TestHub::start(|_| {});
    let log = hub.log("events");
    let (mut client, _, _) = Client::attach(&hub, &key('8'), &log, &[], PROTOCOL);
    client.call(json!("secret-request-id"), "pid");
    client.recv().unwrap();
    let events = read_log(&hub.root.join(super::telemetry::EVENTS_FILE));
    assert!(events.contains("upstream_started"), "{events}");
    assert!(!events.contains("secret-request-id"), "{events}");
    assert!(!events.contains("tools/call"), "{events}");
}

#[test]
fn recorded_upstreams_are_stopped_only_when_pid_and_start_time_match() {
    let dir = tempfile::tempdir().unwrap();
    let spawn_sleeper = || {
        std::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap()
    };
    let mut matching = spawn_sleeper();
    let mut different = spawn_sleeper();
    let started = process_start(matching.id()).unwrap();
    write_record(
        dir.path(),
        "matching",
        &UpstreamRecord {
            pid: matching.id(),
            process_group: matching.id(),
            started,
        },
    )
    .unwrap();
    write_record(
        dir.path(),
        "different",
        &UpstreamRecord {
            pid: different.id(),
            process_group: different.id(),
            started: "not its start time".into(),
        },
    )
    .unwrap();
    let stopped = reap_recorded_upstreams(dir.path());
    assert_eq!(stopped, vec![matching.id()]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        if matching.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        matching.kill().unwrap();
    }
    matching.wait().unwrap();
    assert!(exited, "the recorded group was not stopped");
    assert!(
        different.try_wait().unwrap().is_none(),
        "a pid alone must not be enough"
    );
    different.kill().unwrap();
    different.wait().unwrap();
    // Every record is forgotten either way.
    assert_eq!(
        std::fs::read_dir(dir.path().join("upstreams"))
            .unwrap()
            .count(),
        0
    );
}
