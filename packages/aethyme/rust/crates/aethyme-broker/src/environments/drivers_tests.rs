//! Driver conformance (#1210): start -> healthy -> stop -> start -> destroy
//! leaves nothing behind. `process` and `hooks` always run; container
//! engines run only with `AETHYME_ENV_ENGINE_TESTS=1` and a reachable engine,
//! because they pull images and need a daemon. A skip is printed, not hidden.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::drivers::{
    ComponentIdentity, DriverContext, Health, driver_for, probe, project_name, wait_healthy,
};
use super::engine::resolve;
use super::exec::{Bindings, DriverError, environment_variables, parse_receipt};
use super::identity::{Observation, ProcessIdentity};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::{Loaded, parse_loaded};

fn compiled(text: &str, profile: &str) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        profile,
        "t",
        &CompileContext {
            repository: "repo-test".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: "f".into(),
            },
            worktree_fingerprint: "f".into(),
            source_commit: None,
            source_dirty: true,
            source_digest: String::new(),
        },
    )
    .unwrap()
}

struct Harness {
    worktree: tempfile::TempDir,
    state: tempfile::TempDir,
    environment: CompiledEnvironment,
    bindings: Bindings,
}

impl Harness {
    fn new(text: &str, profile: &str) -> Self {
        let worktree = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let environment = compiled(text, profile);
        let bindings = Bindings {
            namespace: format!("{}-ns", environment.env_id),
            worktree: worktree.path().to_path_buf(),
            ports: BTreeMap::new(),
            receipts: BTreeMap::new(),
            template_ids: BTreeMap::new(),
        };
        Self {
            worktree,
            state,
            environment,
            bindings,
        }
    }

    fn ctx(&self, component: &str) -> DriverContext<'_> {
        DriverContext {
            environment: &self.environment,
            component: self
                .environment
                .components
                .iter()
                .find(|c| c.name == component)
                .unwrap(),
            bindings: &self.bindings,
            attempt: 1,
            directory: self.state.path().join(&self.environment.env_id),
            hook_timeout: Duration::from_secs(20),
        }
    }

    fn free_port(&mut self, component: &str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        self.bindings.ports.insert(component.into(), port);
        port
    }
}

const PROCESS: &str = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "srv"
runtime = "process"
listen = true
start = "exec python3 -m http.server --bind 127.0.0.1 $AETHYME_ENV_PORT_SRV"
health = { http = "/", timeout_seconds = 20, interval_ms = 100 }
"#;

fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn http_probe_reports_socket_timeout_configuration_errors() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();

    match super::drivers::probe_http(&mut stream, "/", Duration::ZERO) {
        Health::NotYet(reason) => assert!(reason.contains("read timeout"), "{reason}"),
        Health::Healthy => panic!("an invalid socket timeout must not pass the probe"),
    }
}

#[test]
fn process_conformance_start_health_stop_start_destroy_leaves_nothing() {
    if !python_available() {
        eprintln!("SKIP process conformance: python3 not available");
        return;
    }
    let mut harness = Harness::new(PROCESS, "p");
    harness.free_port("srv");
    let ctx = harness.ctx("srv");
    let driver = driver_for(ctx.component.runtime);
    let first = driver.start(&ctx).unwrap();
    wait_healthy(driver.as_ref(), &ctx, &first).unwrap();
    assert_eq!(driver.observe(&ctx, &first), Observation::Alive);
    driver.stop(&ctx, &first).unwrap();
    assert_eq!(driver.observe(&ctx, &first), Observation::Gone);
    assert!(
        matches!(probe(&ctx), Health::NotYet(_)),
        "nothing listens after stop"
    );

    let second = driver.start(&ctx).unwrap();
    wait_healthy(driver.as_ref(), &ctx, &second).unwrap();
    assert_ne!(first, second, "a restart is a new identity");
    driver.destroy(&ctx, Some(&second)).unwrap();
    assert_eq!(driver.observe(&ctx, &second), Observation::Gone);
    let log = std::fs::read_to_string(ctx.log()).unwrap();
    assert!(log.contains("[aethyme] srv: start"), "{log}");
}

#[test]
fn stop_escalates_to_sigkill_for_a_group_that_ignores_sigterm() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "stubborn"
runtime = "process"
stop_grace_seconds = 1
start = "trap '' TERM; while :; do sleep 1; done"
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("stubborn");
    let driver = driver_for(ctx.component.runtime);
    let identity = driver.start(&ctx).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let started = std::time::Instant::now();
    driver.stop(&ctx, &identity).unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "SIGTERM grace was honoured"
    );
    assert_eq!(driver.observe(&ctx, &identity), Observation::Gone);
}

#[test]
fn an_unproven_identity_is_never_signalled() {
    let harness = Harness::new(PROCESS, "p");
    let ctx = harness.ctx("srv");
    let mut victim = {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap()
    };
    let real = ProcessIdentity::capture(victim.id() as i32).unwrap();
    let recycled = ComponentIdentity::Process(ProcessIdentity {
        start_time: real.start_time.wrapping_add(1),
        ..real
    });
    let driver = driver_for(ctx.component.runtime);
    match driver.stop(&ctx, &recycled) {
        Err(error @ DriverError::Unproven { .. }) => assert_eq!(error.code(), "identity_unproven"),
        other => panic!("{other:?}"),
    }
    assert!(
        victim.try_wait().unwrap().is_none(),
        "the unrelated process is untouched"
    );
    victim.kill().unwrap();
    victim.wait().unwrap();
}

#[test]
fn a_component_that_exits_at_once_reports_its_log() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "crash"
runtime = "process"
start = "echo boom-from-start >&2; exit 3"
health = { command = "false", timeout_seconds = 5, interval_ms = 50 }
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("crash");
    let driver = driver_for(ctx.component.runtime);
    let result = driver
        .start(&ctx)
        .and_then(|identity| wait_healthy(driver.as_ref(), &ctx, &identity));
    match result {
        Err(DriverError::StartExited { tail, .. }) => {
            assert!(tail.contains("boom-from-start"), "{tail}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn hooks_pass_receipts_attempts_and_a_cleared_environment() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
pass_env = ["AETHYME_TEST_PASSED"]
env = { LITERAL = "yes" }
provision = "touch db.created; echo \"AETHYME_RECEIPT {\\\"database\\\":\\\"env_$AETHYME_ENV_HOOK_ATTEMPT\\\",\\\"leak\\\":\\\"${AETHYME_TEST_SECRET:-none}\\\",\\\"passed\\\":\\\"$AETHYME_TEST_PASSED\\\",\\\"literal\\\":\\\"$LITERAL\\\"}\""
destroy = "rm -f db.created"
"#;
    // SAFETY: test-only; no other test reads or writes these two names, and
    // the hook below only reads them through a spawned child's environment.
    unsafe {
        std::env::set_var("AETHYME_TEST_SECRET", "s3cret");
        std::env::set_var("AETHYME_TEST_PASSED", "through");
    }
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("db");
    let driver = driver_for(ctx.component.runtime);
    let receipt = driver.provision(&ctx).unwrap().expect("receipt");
    assert_eq!(receipt["database"], "env_1");
    assert_eq!(
        receipt["leak"], "none",
        "only allowlisted and pass_env variables are inherited"
    );
    assert_eq!(receipt["passed"], "through");
    assert_eq!(receipt["literal"], "yes");
    assert!(
        harness.worktree.path().join("db.created").exists(),
        "hooks run from the worktree root"
    );
    driver.destroy(&ctx, None).unwrap();
    assert!(!harness.worktree.path().join("db.created").exists());
}

#[test]
fn a_failing_destroy_is_an_error_with_the_log_tail() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "echo cannot-drop >&2; exit 7"
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("db");
    match driver_for(ctx.component.runtime).destroy(&ctx, None) {
        Err(error @ DriverError::HookFailed { .. }) => {
            assert_eq!(error.code(), "hook_failed");
            assert!(error.to_string().contains("cannot-drop"), "{error}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_hook_past_its_deadline_is_killed() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "sleep 30"
destroy = "true"
"#;
    let harness = Harness::new(text, "p");
    let mut ctx = harness.ctx("db");
    ctx.hook_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    match driver_for(ctx.component.runtime).provision(&ctx) {
        Err(DriverError::Timeout { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn receipts_must_be_small_json_objects_and_the_last_one_wins() {
    assert_eq!(parse_receipt("hello\n").unwrap(), None);
    assert_eq!(
        parse_receipt("AETHYME_RECEIPT {\"a\":1}\nAETHYME_RECEIPT {\"a\":2}\n").unwrap(),
        Some(serde_json::json!({"a": 2}))
    );
    assert!(parse_receipt("AETHYME_RECEIPT [1]\n").is_err());
    assert!(parse_receipt("AETHYME_RECEIPT {nope\n").is_err());
    let big = format!("AETHYME_RECEIPT {{\"a\":\"{}\"}}\n", "x".repeat(20_000));
    assert!(parse_receipt(&big).is_err());
}

#[test]
fn variables_name_the_environment_ports_urls_and_receipts() {
    let mut harness = Harness::new(PROCESS, "p");
    let port = harness.free_port("srv");
    harness
        .bindings
        .receipts
        .insert("srv".into(), serde_json::json!({"k": "v"}));
    let ctx = harness.ctx("srv");
    let vars = environment_variables(ctx.environment, ctx.component, ctx.bindings, 2);
    assert_eq!(vars["AETHYME_ENV_PORT_SRV"], port.to_string());
    assert_eq!(
        vars["AETHYME_ENV_URL_SRV"],
        format!("http://127.0.0.1:{port}")
    );
    assert_eq!(vars["AETHYME_ENV_RECEIPT_SRV"], "{\"k\":\"v\"}");
    assert_eq!(vars["AETHYME_ENV_HOOK_ATTEMPT"], "2");
    assert_eq!(vars["AETHYME_ENV_NAMESPACE"], ctx.bindings.namespace);
}

#[test]
fn compose_project_names_are_engine_safe() {
    assert_eq!(project_name("env-abc123-XYZ.9"), "env-abc123-xyz-9");
    assert_eq!(project_name("_-env"), "env");
}

fn engine_tests_enabled() -> bool {
    std::env::var("AETHYME_ENV_ENGINE_TESTS").as_deref() == Ok("1")
}

#[test]
fn container_conformance_when_an_engine_is_enabled() {
    if !engine_tests_enabled() {
        eprintln!(
            "SKIP container conformance: set AETHYME_ENV_ENGINE_TESTS=1 with docker or podman running"
        );
        return;
    }
    let Ok(engine) = resolve(None) else {
        eprintln!("SKIP container conformance: no engine on PATH");
        return;
    };
    if !engine.available() {
        eprintln!(
            "SKIP container conformance: {} is not answering",
            engine.engine.as_str()
        );
        return;
    }
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "box"
runtime = "container"
image = "alpine:3.20"
args = ["sleep", "300"]
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("box");
    let driver = driver_for(ctx.component.runtime);
    let first = driver.start(&ctx).unwrap();
    assert_eq!(driver.observe(&ctx, &first), Observation::Alive);
    driver.stop(&ctx, &first).unwrap();
    driver.destroy(&ctx, Some(&first)).unwrap();
    assert_eq!(driver.observe(&ctx, &first), Observation::Gone);
}

#[test]
fn container_destroy_reports_secret_file_removal_failure() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "box"
runtime = "container"
image = "alpine:3.20"
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("box");
    std::fs::create_dir_all(&ctx.directory).unwrap();
    let secret_path = ctx.directory.join("box.env");
    std::fs::create_dir(&secret_path).unwrap();

    let error = driver_for(ctx.component.runtime)
        .destroy(&ctx, None)
        .expect_err("failed secret cleanup must be reported");
    assert!(matches!(error, DriverError::Io { .. }), "{error:?}");
    assert!(
        secret_path.is_dir(),
        "the blocked secret path remains visible"
    );
}

#[cfg(unix)]
#[test]
fn container_destroy_attempts_hook_when_secret_cleanup_fails() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "box"
runtime = "container"
image = "alpine:3.20"
destroy = "echo hook-ran > destroy-hook-sentinel"
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("box");
    std::fs::create_dir_all(&ctx.directory).unwrap();
    let secret_path = ctx.directory.join("box.env");
    std::fs::create_dir(&secret_path).unwrap();

    let error = driver_for(ctx.component.runtime)
        .destroy(&ctx, None)
        .expect_err("failed secret cleanup must be reported");
    assert!(matches!(error, DriverError::Io { .. }), "{error:?}");
    assert!(
        secret_path.is_dir(),
        "the blocked secret path remains visible"
    );
    assert_eq!(
        std::fs::read_to_string(harness.worktree.path().join("destroy-hook-sentinel"))
            .unwrap()
            .trim(),
        "hook-ran",
        "the destroy hook still runs after generated-secret cleanup fails"
    );
}

#[cfg(unix)]
#[test]
fn container_destroy_preserves_hook_error_when_secret_cleanup_also_fails() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "box"
runtime = "container"
image = "alpine:3.20"
destroy = "echo hook-ran > destroy-hook-sentinel; echo hook-failed >&2; exit 7"
"#;
    let harness = Harness::new(text, "p");
    let ctx = harness.ctx("box");
    std::fs::create_dir_all(&ctx.directory).unwrap();
    let secret_path = ctx.directory.join("box.env");
    std::fs::create_dir(&secret_path).unwrap();

    let error = driver_for(ctx.component.runtime)
        .destroy(&ctx, None)
        .expect_err("neither cleanup nor hook failure may report success");
    assert!(
        matches!(error, DriverError::HookFailed { .. }),
        "the hook failure remains the primary error: {error:?}"
    );
    assert!(error.to_string().contains("hook-failed"), "{error}");
    assert!(
        secret_path.is_dir(),
        "the failed generated-secret path remains"
    );
    assert_eq!(
        std::fs::read_to_string(harness.worktree.path().join("destroy-hook-sentinel"))
            .unwrap()
            .trim(),
        "hook-ran"
    );
}

#[test]
fn compose_conformance_when_an_engine_is_enabled() {
    if !engine_tests_enabled() {
        eprintln!(
            "SKIP compose conformance: set AETHYME_ENV_ENGINE_TESTS=1 with docker or podman running"
        );
        return;
    }
    let Ok(engine) = resolve(None) else {
        eprintln!("SKIP compose conformance: no engine on PATH");
        return;
    };
    if !engine.available() {
        eprintln!(
            "SKIP compose conformance: {} is not answering",
            engine.engine.as_str()
        );
        return;
    }
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "stack"
runtime = "compose"
file = "compose.yml"
"#;
    let harness = Harness::new(text, "p");
    std::fs::write(
        harness.worktree.path().join("compose.yml"),
        "services:\n  one:\n    image: alpine:3.20\n    command: [\"sleep\", \"300\"]\n",
    )
    .unwrap();
    let ctx = harness.ctx("stack");
    let driver = driver_for(ctx.component.runtime);
    let first = driver.start(&ctx).unwrap();
    assert_eq!(driver.observe(&ctx, &first), Observation::Alive);
    driver.stop(&ctx, &first).unwrap();
    let second = driver.start(&ctx).unwrap();
    driver.destroy(&ctx, Some(&second)).unwrap();
    assert_eq!(driver.observe(&ctx, &second), Observation::Gone);
}
