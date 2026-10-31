//! `aethyme env` through the built router (#1209, #1222).
//!
//! Every process gets its own host state and home, so no case touches the
//! real environment store or a real supervisor.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use aethyme_testkit::aethyme_bin;

const ENVIRONMENTS: &str = r#"
schema = 1
[pools.envs]
limit = 2

[[profiles]]
name = "backend"
units = 1

[[profiles.components]]
name = "api"
runtime = "process"
listen = true
route = true
start = "python3 -m http.server $AETHYME_ENV_PORT_API"
health = { http = "/" }
"#;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    home: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) {
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
}

impl Fixture {
    fn new(environments: Option<&str>) -> Self {
        let fixture = Self {
            repo: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        let repo = fixture.repo.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        if let Some(text) = environments {
            std::fs::write(repo.join(".aethyme/environments.toml"), text).unwrap();
        }
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "init"]);
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(aethyme_bin());
        command
            .args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env("AETHYME_ENV_SUPERVISOR_LINGER_SECONDS", "1")
            .env("AETHYME_ENV_SUPERVISOR_TICK_MS", "50")
            .env_remove("XDG_STATE_HOME")
            .env_remove("AETHYME_WORKTREE_ROOT");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, serde_json::Value) {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.run(&args);
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: stdout is not one JSON object ({error}):\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value)
    }
}

#[test]
fn validate_and_plan_are_read_only_and_versioned() {
    let fixture = Fixture::new(Some(ENVIRONMENTS));
    let (code, value) = fixture.json(&["env", "validate"]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["schema"], "aethyme.env-validate/v1");
    assert_eq!(value["valid"], true);
    let (code, value) = fixture.json(&["env", "plan"]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["schema"], "aethyme.env-plan/v1");
    assert_eq!(value["host_state"], "absent");
    assert_eq!(value["start_order"], serde_json::json!(["api"]));
    assert!(
        std::fs::read_dir(fixture.state.path())
            .unwrap()
            .next()
            .is_none(),
        "validate and plan write no host state"
    );
}

#[test]
fn an_invalid_file_exits_4_with_located_diagnostics() {
    let fixture = Fixture::new(Some(
        &ENVIRONMENTS.replace("health = { http = \"/\" }\n", ""),
    ));
    let (code, value) = fixture.json(&["env", "validate"]);
    assert_eq!(code, 4, "{value}");
    let diagnostic = &value["diagnostics"][0];
    assert_eq!(diagnostic["code"], "route_without_health");
    assert_eq!(diagnostic["profile"], "backend");
    assert_eq!(diagnostic["component"], "api");
}

#[test]
fn a_repository_without_environments_is_refused_with_next_action() {
    let fixture = Fixture::new(None);
    let (code, value) = fixture.json(&["env", "validate"]);
    assert_eq!(code, 3);
    assert_eq!(value["schema"], "aethyme.env-error/v1");
    assert_eq!(value["code"], "not_configured");
    assert!(
        value["next_action"]
            .as_str()
            .unwrap()
            .contains("environments.toml")
    );
}

#[test]
fn status_on_a_fresh_host_reports_nothing_and_an_absent_supervisor() {
    let fixture = Fixture::new(Some(ENVIRONMENTS));
    let (code, value) = fixture.json(&["env", "status"]);
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["environments"], serde_json::json!([]));
    assert_eq!(value["supervisor"]["state"], "absent");
    let (code, value) = fixture.json(&["env", "logs", "env-0123456789ab"]);
    assert_eq!(code, 3, "{value}");
    assert_eq!(value["code"], "not_found");
}

/// #1222 acceptance: ten concurrent starters run exactly one supervisor, and
/// it exits on its own once nothing needs it.
#[test]
fn concurrent_ensure_calls_start_exactly_one_supervisor_that_lingers_out() {
    let fixture = Fixture::new(Some(ENVIRONMENTS));
    let children: Vec<_> = (0..10)
        .map(|_| {
            fixture
                .command(&["env", "supervise", "--ensure", "--json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut pids = std::collections::BTreeSet::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["supervisor"]["state"], "running", "{value}");
        pids.insert(value["supervisor"]["pid"].as_i64().unwrap());
    }
    assert_eq!(pids.len(), 1, "one supervisor for ten callers: {pids:?}");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, value) = fixture.json(&["env", "status"]);
        if value["supervisor"]["state"] == "absent" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "supervisor did not linger out: {value}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// #1211 through the router: `up` reports ports and variables and runs under
/// a supervisor; `down` destroys and releases; a second `down` is a no-op.
#[test]
fn up_then_down_round_trips_through_the_cli() {
    let environments = r#"
schema = 1
[[profiles]]
name = "backend"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "touch provisioned.marker"
destroy = "rm -f provisioned.marker"
[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["db"]
listen = true
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 10, interval_ms = 50 }
"#;
    let fixture = Fixture::new(Some(environments));
    let (code, up) = fixture.json(&["env", "up", "--wait", "5"]);
    assert_eq!(code, 0, "{up}");
    assert_eq!(up["schema"], "aethyme.env-up/v1");
    assert_eq!(up["phase"], "active");
    let env_id = up["env_id"].as_str().unwrap().to_string();
    assert!(up["variables"]["AETHYME_ENV_PORT_API"].is_string(), "{up}");
    assert!(fixture.repo.path().join("provisioned.marker").exists());
    let (_, status) = fixture.json(&["env", "status", &env_id]);
    assert_eq!(status["supervisor"]["state"], "running", "{status}");

    let (code, again) = fixture.json(&["env", "up"]);
    assert_eq!(code, 0, "{again}");
    assert_eq!(
        again["env_id"],
        env_id.as_str(),
        "a second up is the same environment"
    );

    let (code, down) = fixture.json(&["env", "down", &env_id]);
    assert_eq!(code, 0, "{down}");
    assert_eq!(down["phase"], "destroyed");
    assert!(!fixture.repo.path().join("provisioned.marker").exists());
    let (code, _) = fixture.json(&["env", "down", &env_id]);
    assert_eq!(code, 0, "down is idempotent");

    let (code, refused) = fixture.json(&["env", "destroy", &env_id]);
    assert_eq!(code, 2, "{refused}");
    assert_eq!(refused["code"], "usage");
}

/// #1212 through the router: park returns capacity, wake keeps the ports.
#[test]
fn park_then_wake_keeps_ports_through_the_cli() {
    let environments = r#"
schema = 1
[pools.envs]
limit = 1
[[profiles]]
name = "backend"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
listen = true
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 10, interval_ms = 50 }
"#;
    let fixture = Fixture::new(Some(environments));
    let (code, up) = fixture.json(&["env", "up", "--wait", "5"]);
    assert_eq!(code, 0, "{up}");
    let env_id = up["env_id"].as_str().unwrap().to_string();
    let (code, parked) = fixture.json(&["env", "park", &env_id]);
    assert_eq!(code, 0, "{parked}");
    assert_eq!(parked["schema"], "aethyme.env-park/v1");
    assert_eq!(parked["phase"], "parked");
    assert_eq!(parked["units_to_wake"], 1);
    let (code, woken) = fixture.json(&["env", "wake", &env_id, "--wait", "5"]);
    assert_eq!(code, 0, "{woken}");
    assert_eq!(woken["phase"], "active");
    assert_eq!(
        woken["ports"], up["ports"],
        "the reservation, and so the ports, survive"
    );
    let (code, _) = fixture.json(&["env", "down", &env_id]);
    assert_eq!(code, 0);
}

/// #1213 through the router: `env exec` passes the environment's variables
/// to the command, propagates its exit status and keeps stdout clean.
#[test]
fn exec_runs_a_command_with_the_environment_and_its_status() {
    let environments = r#"
schema = 1
[[profiles]]
name = "backend"
[[profiles.components]]
name = "api"
runtime = "process"
listen = true
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 10, interval_ms = 50 }
"#;
    let fixture = Fixture::new(Some(environments));
    let (code, up) = fixture.json(&["env", "up", "--wait", "5"]);
    assert_eq!(code, 0, "{up}");
    let env_id = up["env_id"].as_str().unwrap().to_string();
    let port = up["ports"]["api"].as_str().unwrap().to_string();
    let output = fixture.run(&[
        "env",
        "exec",
        &env_id,
        "--",
        "sh",
        "-c",
        "printf %s \"$AETHYME_ENV_PORT_API\"",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), port);
    let failing = fixture.run(&["env", "exec", &env_id, "--", "sh", "-c", "exit 7"]);
    assert_eq!(
        failing.status.code(),
        Some(7),
        "the command's status is the exit status"
    );
    let usage = fixture.run(&["env", "exec", &env_id, "sh"]);
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("aethyme.env-error/v1"));
    let (code, _) = fixture.json(&["env", "down", &env_id]);
    assert_eq!(code, 0);
}

/// #1216 through the router: changes raise the minimum profile; a lower one
/// is refused with the rules, a higher one needs a reason.
#[test]
fn inferred_minimum_is_raise_only_through_the_cli() {
    let environments = r#"
schema = 1
default_profile = "static"
[[profiles]]
name = "static"
[[profiles.components]]
name = "web"
runtime = "hooks"
provision = "true"
destroy = "true"
[[profiles]]
name = "isolated"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
[[profiles]]
name = "huge"
[[profiles.components]]
name = "all"
runtime = "hooks"
provision = "true"
destroy = "true"
[[rules]]
name = "schema"
paths = ["**/migrations/**"]
min_profile = "isolated"
"#;
    let fixture = Fixture::new(Some(environments));
    std::fs::create_dir_all(fixture.repo.path().join("app/migrations")).unwrap();
    std::fs::write(
        fixture.repo.path().join("app/migrations/0001.sql"),
        "create table t();\n",
    )
    .unwrap();
    let (code, inferred) = fixture.json(&["env", "infer"]);
    assert_eq!(code, 0, "{inferred}");
    assert_eq!(inferred["schema"], "aethyme.env-infer/v1");
    assert_eq!(inferred["inference"]["minimum"], "isolated");
    assert_eq!(
        inferred["inference"]["fired"][0]["paths"][0],
        "app/migrations/0001.sql"
    );

    let (code, refused) = fixture.json(&["env", "up", "--profile", "static"]);
    assert_eq!(code, 3, "{refused}");
    assert_eq!(refused["code"], "below_inferred_minimum");
    assert!(refused["message"].as_str().unwrap().contains("schema"));

    let (code, unexplained) = fixture.json(&["env", "up", "--profile", "huge"]);
    assert_eq!(code, 2, "{unexplained}");
    assert_eq!(unexplained["code"], "reason_required");

    let (code, up) = fixture.json(&["env", "up"]);
    assert_eq!(code, 0, "{up}");
    assert_eq!(
        up["profile"], "isolated",
        "no --profile means the inferred minimum"
    );
    let (code, _) = fixture.json(&["env", "down", up["env_id"].as_str().unwrap()]);
    assert_eq!(code, 0);
}

/// #1275: `broker gc plan` lists the environment build cache and proposes
/// the unused build; `gc apply` evicts it and never the one in use.
#[test]
fn gc_reclaims_an_unused_environment_build_and_never_a_used_one() {
    let environments = r#"
schema = 1
[[profiles]]
name = "site"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
build = 'mkdir -p dist && cp web/app.txt dist/index.html'
cache_inputs = ["web/**"]
"#;
    let fixture = Fixture::new(Some(environments));
    let app = fixture.repo.path().join("web/app.txt");
    std::fs::create_dir_all(app.parent().unwrap()).unwrap();
    std::fs::write(&app, "v1").unwrap();
    let (code, up) = fixture.json(&["env", "up", "--wait", "5"]);
    assert_eq!(code, 0, "{up}");
    let old = up["env_id"].as_str().unwrap().to_string();
    let (code, down) = fixture.json(&["env", "down", &old]);
    assert_eq!(code, 0, "{down}");
    let builds = fixture.state.path().join("environments/cache/builds");
    let entries = |dir: &Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    };
    let repository = entries(&builds).pop().unwrap();
    let unused = entries(&repository.join("site")).pop().unwrap();
    std::fs::write(&app, "v2").unwrap();
    let (code, up) = fixture.json(&["env", "up", "--wait", "5"]);
    assert_eq!(code, 0, "{up}");
    let live = up["env_id"].as_str().unwrap().to_string();

    let gc = |args: &[&str]| {
        let output = fixture
            .command(args)
            .env("AETHYME_ENV_CACHE_MAX_BYTES", "0")
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    };
    // A broker that has placed a worktree knows its root history, which a
    // complete (applicable) plan needs.
    let started = fixture.run(&["broker", "start", "--task", "gc", "--short-name", "gc"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let plan = gc(&["broker", "gc", "plan", "--full-scan", "--json"]);
    let cache = &plan["environment_cache"];
    assert_eq!(cache["build_entries"], 2, "{plan}");
    assert_eq!(cache["in_use_entries"], 1, "{plan}");
    assert!(
        plan["environment_caches"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "{plan}"
    );

    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64, "{plan}");
    let applied = gc(&[
        "broker",
        "gc",
        "apply",
        "--confirm",
        digest,
        "--full-scan",
        "--json",
    ]);
    assert_eq!(
        applied["environment_caches_reclaimed"],
        serde_json::json!([unused.to_string_lossy()]),
        "{applied}"
    );
    assert!(!unused.exists());
    assert_eq!(
        entries(&repository.join("site")).len(),
        1,
        "the used build is kept"
    );

    let (code, down) = fixture.json(&["env", "down", &live]);
    assert_eq!(code, 0, "{down}");
}
