//! Lease release requests, acknowledgements and `leases wait` (#359).
//!
//! `sleep` processes stand in for agent runtimes through `AETHYME_AGENT_PID`,
//! as in `lease_liveness_cli`, so killing one is a holder going away.

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

fn fixture(config: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
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

    fn start_as(&self, agent: &str, task: &str) -> i64 {
        self.json_as(agent, &["start", "--task", task, "--json"])["id"]
            .as_i64()
            .unwrap()
    }

    fn holder_as(&self, agent: &str) -> i64 {
        let session = self.start_as(agent, "holder");
        self.json_as(
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
        session
    }

    fn request(&self, agent: &str, requester: i64) -> serde_json::Value {
        let report = self.json_as(
            agent,
            &[
                "advanced",
                "leases",
                "request-release",
                PATH,
                "--session",
                &requester.to_string(),
                "--reason",
                "needs the file",
                "--json",
            ],
        );
        let requests = report["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 1, "{report}");
        requests[0].clone()
    }

    /// `leases wait` as `agent`: the exit code and the reported outcome.
    fn wait(&self, agent: &str, session: i64, timeout: &str) -> (Option<i32>, String) {
        let output = self.run_as(
            agent,
            &[
                "advanced",
                "leases",
                "wait",
                PATH,
                "--session",
                &session.to_string(),
                "--timeout",
                timeout,
                "--json",
            ],
        );
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "wait --json: {error}: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        (
            output.status.code(),
            report["outcome"].as_str().unwrap().to_string(),
        )
    }

    fn holds(&self, session: i64) -> bool {
        let listed = self.json_as("0", &["advanced", "leases", "--json"]);
        listed["leases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lease| lease["session_id"] == session && lease["path"] == PATH)
    }
}

const VERIFY_ONLY: &str = "schema = 1\n[promote]\nmode = \"verify-only\"\n";

#[test]
fn the_holder_sees_the_request_and_an_ack_releases_the_path() {
    let fx = fixture(VERIFY_ONLY);
    let (holder, requester) = (Agent::spawn(), Agent::spawn());
    let holding = fx.holder_as(&holder.pid());
    let asking = fx.start_as(&requester.pid(), "asker");

    let request = fx.request(&requester.pid(), asking);
    assert_eq!(request["state"], "pending");
    assert_eq!(request["holder_session_id"], holding);
    let id = request["request_id"].as_i64().unwrap();

    let status = fx.json_as("0", &["status", "--json"]);
    let advice = status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "lease.release-requested")
        .unwrap_or_else(|| panic!("no request advice: {status}"));
    assert_eq!(advice["session_id"], holding);
    assert!(
        advice["commands"][0]
            .as_str()
            .unwrap()
            .contains(&format!("leases ack {id} --session {holding}")),
        "{advice}"
    );
    assert!(
        status["lease_release_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["request_id"] == id && row["state"] == "pending"),
        "{status}"
    );

    let acked = fx.json_as(
        &holder.pid(),
        &[
            "advanced",
            "leases",
            "ack",
            &id.to_string(),
            "--session",
            &holding.to_string(),
            "--json",
        ],
    );
    assert_eq!(acked["state"], "acked");
    assert!(!fx.holds(holding));
    // The ack is audited like a finish release (#358): one reasoned
    // `lease.released` naming the lease generation.
    let store = aethyme_broker::BrokerStore::open_in_repo(&fx.repo).unwrap();
    let released: Vec<serde_json::Value> = store
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "lease.released" && event.session_id == Some(holding))
        .filter_map(|event| serde_json::from_str(event.payload_json.as_deref()?).ok())
        .collect();
    assert!(
        released
            .iter()
            .any(|payload| payload["reason"] == "request_acked"
                && payload["path"] == PATH
                && payload["lease_id"].is_i64()
                && payload["created_at"].is_i64()),
        "{released:?}"
    );
    assert_eq!(
        fx.wait(&requester.pid(), asking, "5"),
        (Some(0), "released".into())
    );
}

#[test]
fn a_decline_keeps_the_lease_and_ends_the_wait() {
    let fx = fixture(VERIFY_ONLY);
    let (holder, requester) = (Agent::spawn(), Agent::spawn());
    let holding = fx.holder_as(&holder.pid());
    let asking = fx.start_as(&requester.pid(), "asker");
    let id = fx.request(&requester.pid(), asking)["request_id"]
        .as_i64()
        .unwrap();

    let declined = fx.json_as(
        &holder.pid(),
        &[
            "advanced",
            "leases",
            "decline",
            &id.to_string(),
            "--session",
            &holding.to_string(),
            "--reason",
            "mid-refactor",
            "--json",
        ],
    );
    assert_eq!(declined["state"], "declined");
    assert_eq!(declined["resolution_reason"], "mid-refactor");
    assert!(fx.holds(holding));
    assert_eq!(
        fx.wait(&requester.pid(), asking, "5"),
        (Some(11), "declined".into())
    );
}

#[test]
fn only_the_holder_may_answer_and_only_once() {
    let fx = fixture(VERIFY_ONLY);
    let (holder, requester) = (Agent::spawn(), Agent::spawn());
    let holding = fx.holder_as(&holder.pid());
    let asking = fx.start_as(&requester.pid(), "asker");
    let id = fx.request(&requester.pid(), asking)["request_id"]
        .as_i64()
        .unwrap()
        .to_string();

    let by_requester = fx.run_as(
        &requester.pid(),
        &[
            "advanced",
            "leases",
            "ack",
            &id,
            "--session",
            &asking.to_string(),
        ],
    );
    assert_eq!(by_requester.status.code(), Some(3));
    assert!(fx.holds(holding));

    let ack = [
        "advanced",
        "leases",
        "ack",
        &id,
        "--session",
        &holding.to_string(),
    ];
    assert!(fx.run_as(&holder.pid(), &ack).status.success());
    let again = fx.run_as(&holder.pid(), &ack);
    assert_eq!(again.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&again.stderr).contains("already acked"));
}

#[test]
fn a_holder_gone_past_the_grace_is_granted_against_during_the_wait() {
    let fx = fixture(
        "schema = 1\n[promote]\nmode = \"verify-only\"\n[leases]\nstale_grace_minutes = 0\n",
    );
    let (mut holder, requester) = (Agent::spawn(), Agent::spawn());
    let holding = fx.holder_as(&holder.pid());
    let asking = fx.start_as(&requester.pid(), "asker");
    let request = fx.request(&requester.pid(), asking);
    assert_eq!(
        request["state"], "pending",
        "a running holder is never granted against"
    );

    holder.stop();
    fx.json_as("0", &["status", "--json"]);
    assert_eq!(
        fx.wait(&requester.pid(), asking, "10"),
        (Some(10), "granted".into())
    );
    assert!(!fx.holds(holding));
    let status = fx.json_as("0", &["status", "--json"]);
    let row = status["lease_release_requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request_id"] == request["request_id"])
        .cloned()
        .unwrap();
    assert_eq!(row["state"], "granted");
    assert_eq!(row["resolution_reason"], "holder_stale");
}

#[test]
fn a_holder_of_unknown_liveness_is_never_granted_against() {
    let fx = fixture(
        "schema = 1\n[promote]\nmode = \"verify-only\"\n[leases]\nstale_grace_minutes = 0\n",
    );
    let requester = Agent::spawn();
    let holding = fx.holder_as("0");
    let asking = fx.start_as(&requester.pid(), "asker");
    assert_eq!(fx.request(&requester.pid(), asking)["state"], "pending");

    assert_eq!(
        fx.wait(&requester.pid(), asking, "1"),
        (Some(12), "timeout".into())
    );
    assert!(fx.holds(holding));
}
