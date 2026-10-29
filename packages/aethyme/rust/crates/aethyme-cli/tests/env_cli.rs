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
            .env_remove("XDG_STATE_HOME");
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
