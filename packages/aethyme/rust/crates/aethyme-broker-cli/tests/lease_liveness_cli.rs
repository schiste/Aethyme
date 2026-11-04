//! Lease liveness is bound to the holder process (#360).
//!
//! `sleep` processes stand in for agent runtimes, named through
//! `AETHYME_AGENT_PID` as in `session_holder_cli`, so killing one is the
//! holder going away.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const PATH: &str = "src/owned.rs";

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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

struct Agent(Child);

impl Agent {
    fn spawn() -> Self {
        Self(Command::new("sleep").arg("600").spawn().unwrap())
    }

    fn pid(&self) -> String {
        self.0.id().to_string()
    }

    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
}

/// A repository whose main checkout carries `config` as its broker config.
fn fixture(config: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(PATH), "fn main() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(repo.join(".aethyme/config.toml"), config).unwrap();
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        _tmp: tmp,
    }
}

impl Fixture {
    fn run_as(&self, agent: &str, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env("AETHYME_AGENT_PID", agent)
            .output()
            .unwrap()
    }

    fn json_as(&self, agent: &str, args: &[&str]) -> serde_json::Value {
        let output = self.run_as(agent, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Start a session as `agent` and claim `PATH` for it.
    fn holding_session(&self, agent: &str, task: &str) -> i64 {
        let started = self.json_as(agent, &["start", "--task", task, "--json"]);
        let session = started["id"].as_i64().unwrap();
        let claimed = self.run_as(
            agent,
            &[
                "advanced",
                "leases",
                "claim",
                PATH,
                "--session",
                &session.to_string(),
                "--json",
            ],
        );
        assert!(
            claimed.status.success(),
            "{}",
            String::from_utf8_lossy(&claimed.stderr)
        );
        session
    }

    /// The single explained lease on `PATH`.
    fn explain(&self) -> serde_json::Value {
        let report = self.json_as("0", &["advanced", "leases", "explain", PATH, "--json"]);
        let leases = report["leases"].as_array().unwrap();
        assert_eq!(leases.len(), 1, "{report}");
        leases[0].clone()
    }

    fn plan_conflicts(&self, planner: &str, session: i64) -> bool {
        let plan = self.json_as(
            planner,
            &[
                "advanced",
                "leases",
                "plan",
                PATH,
                "--session",
                &session.to_string(),
                "--json",
            ],
        );
        plan["would_conflict"].as_bool().unwrap()
    }
}

#[test]
fn a_running_holder_is_active_and_reported_in_status_and_explain() {
    let fx = fixture("schema = 1\n");
    let holder = Agent::spawn();
    let session = fx.holding_session(&holder.pid(), "holder");

    let lease = fx.explain();
    assert_eq!(lease["session_id"], session);
    assert_eq!(lease["liveness"], "active");
    assert_eq!(lease["liveness_evidence"]["basis"], "holder_running");
    assert_eq!(
        lease["liveness_evidence"]["holder_pid"].as_i64(),
        Some(i64::from(holder.0.id()))
    );
    assert_eq!(lease["liveness_evidence"]["holds"], true);

    let status = fx.json_as("0", &["status", "--json"]);
    let rows = status["lease_liveness"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["path"] == PATH)
        .unwrap_or_else(|| panic!("no liveness row in {status}"));
    assert_eq!(row["liveness"], "active");
}

#[test]
fn a_quiet_running_holder_is_idle_and_still_holds() {
    let fx = fixture("schema = 1\n[leases]\nidle_minutes = 0\n");
    let holder = Agent::spawn();
    fx.holding_session(&holder.pid(), "holder");
    std::thread::sleep(std::time::Duration::from_millis(20));

    let lease = fx.explain();
    assert_eq!(lease["liveness"], "idle");
    assert_eq!(lease["liveness_evidence"]["holds"], true);
}

#[test]
fn a_gone_holder_is_stale_and_keeps_its_hold_through_the_grace() {
    let fx = fixture("schema = 1\n");
    let mut holder = Agent::spawn();
    let planner = Agent::spawn();
    fx.holding_session(&holder.pid(), "holder");
    let planning = fx.json_as(&planner.pid(), &["start", "--task", "planner", "--json"]);
    let planning = planning["id"].as_i64().unwrap();
    assert!(fx.plan_conflicts(&planner.pid(), planning));

    holder.stop();
    // Status persists the first sighting, which starts the grace clock.
    fx.json_as("0", &["status", "--json"]);
    let first = fx.explain();
    assert_eq!(first["liveness"], "stale");
    assert_eq!(first["liveness_evidence"]["basis"], "holder_gone");
    assert_eq!(first["liveness_evidence"]["holds"], true);
    let stale_since = first["liveness_evidence"]["stale_since"].as_i64().unwrap();
    assert_eq!(
        first["liveness_evidence"]["grace_ends_at"].as_i64(),
        Some(stale_since + 15 * 60_000)
    );

    // The clock does not restart on later reads.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let later = fx.explain();
    assert_eq!(
        later["liveness_evidence"]["stale_since"].as_i64(),
        Some(stale_since)
    );
    assert!(
        fx.plan_conflicts(&planner.pid(), planning),
        "within the grace a stale lease still conflicts"
    );
}

#[test]
fn past_the_grace_a_stale_lease_no_longer_conflicts_but_is_listed() {
    let fx = fixture("schema = 1\n[leases]\nstale_grace_minutes = 0\n");
    let mut holder = Agent::spawn();
    let planner = Agent::spawn();
    let session = fx.holding_session(&holder.pid(), "holder");
    let planning = fx.json_as(&planner.pid(), &["start", "--task", "planner", "--json"]);
    let planning = planning["id"].as_i64().unwrap();
    assert!(fx.plan_conflicts(&planner.pid(), planning));

    holder.stop();
    fx.json_as("0", &["status", "--json"]);
    let lease = fx.explain();
    assert_eq!(lease["session_id"], session);
    assert_eq!(lease["liveness"], "stale");
    assert_eq!(lease["liveness_evidence"]["holds"], false);
    assert!(!fx.plan_conflicts(&planner.pid(), planning));

    let listed = fx.json_as("0", &["advanced", "leases", "--json"]);
    assert!(
        listed["leases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lease| lease["path"] == PATH),
        "a stale lease is still listed: {listed}"
    );
}

#[test]
fn an_unidentified_holder_is_unknown_and_never_treated_as_dead() {
    let fx = fixture("schema = 1\n[leases]\nstale_grace_minutes = 0\n");
    let planner = Agent::spawn();
    fx.holding_session("0", "unbound");
    let planning = fx.json_as(&planner.pid(), &["start", "--task", "planner", "--json"]);
    let planning = planning["id"].as_i64().unwrap();

    let lease = fx.explain();
    assert_eq!(lease["liveness"], "unknown");
    assert_eq!(lease["liveness_evidence"]["basis"], "no_recorded_holder");
    assert_eq!(lease["liveness_evidence"]["holds"], true);
    assert!(
        fx.plan_conflicts(&planner.pid(), planning),
        "unknown liveness keeps the lease conflicting"
    );
}
