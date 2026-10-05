//! A session is held by the agent process that started it (#393).
//!
//! Two `sleep` processes stand in for two agent runtimes; `AETHYME_AGENT_PID`
//! names which one each command comes from, so the tests do not depend on a
//! real `claude` or `codex` being an ancestor of the test runner.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use aethyme_broker::{BrokerStore, events};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const HELD: &str = "is held by another live agent process";

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

/// A stand-in agent runtime: a process that stays alive until dropped.
struct Agent(Child);

impl Agent {
    fn spawn() -> Self {
        Self(Command::new("sleep").arg("600").spawn().unwrap())
    }

    fn pid(&self) -> String {
        self.0.id().to_string()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        _tmp: tmp,
    }
}

impl Fixture {
    /// Run the CLI as the agent with pid `agent` (`"0"`: unidentified).
    fn run_as(&self, agent: &str, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env("AETHYME_AGENT_PID", agent)
            .output()
            .unwrap()
    }

    fn start_as(&self, agent: &str) -> i64 {
        let output = self.run_as(agent, &["start", "--task", "held work", "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        started["id"].as_i64().unwrap()
    }

    fn holder_event(&self, session: i64) -> serde_json::Value {
        let store = BrokerStore::open_in_repo(&self.repo).unwrap();
        let event = store
            .latest_session_holder_event(session)
            .unwrap()
            .expect("a holder is recorded");
        assert_eq!(event.kind, events::SESSION_HOLDER_BOUND);
        serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn refused(output: &Output) -> bool {
    stderr(output).contains(HELD)
}

/// Every command that acts for a session, as `--session` arguments.
fn acting_commands(session: &str) -> Vec<Vec<String>> {
    [
        vec!["submit", "--session", session],
        vec!["finish", "--session", session],
        vec!["sync", "--session", session],
        vec![
            "advanced",
            "git",
            "--session",
            session,
            "--reason",
            "test",
            "--",
            "status",
        ],
        vec![
            "advanced",
            "gh",
            "--session",
            session,
            "--repo",
            "o/n",
            "--reason",
            "test",
            "--",
            "pr",
            "list",
        ],
    ]
    .into_iter()
    .map(|args| args.into_iter().map(str::to_string).collect())
    .collect()
}

#[test]
fn another_live_agent_is_refused_and_told_who_holds_the_session() {
    let fx = fixture();
    let holder = Agent::spawn();
    let intruder = Agent::spawn();
    let session = fx.start_as(&holder.pid());
    assert_eq!(fx.holder_event(session)["reason"], "registered");
    assert_eq!(
        fx.holder_event(session)["pid"].as_i64(),
        Some(i64::from(holder.0.id()))
    );

    for args in acting_commands(&session.to_string()) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = fx.run_as(&intruder.pid(), &args);
        assert!(
            refused(&output),
            "{args:?} was not refused: {}",
            stderr(&output)
        );
        assert_eq!(output.status.code(), Some(3), "{args:?}");
        assert!(
            stderr(&output).contains(&format!("pid {}", holder.pid())),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("--take-over"),
            "{}",
            stderr(&output)
        );
    }
    // Reusing the worktree's session is the same attachment.
    let worktree = fx.holder_worktree(session);
    let reuse = common::broker_cli(CLI, &["start", "--reuse", "--task", "again"])
        .current_dir(&worktree)
        .env("AETHYME_HOST_STATE_DIR", &fx.host)
        .env("AETHYME_AGENT_PID", intruder.pid())
        .output()
        .unwrap();
    assert!(refused(&reuse), "start --reuse: {}", stderr(&reuse));

    // The holder itself is never refused.
    let own = fx.run_as(&holder.pid(), &["sync", "--session", &session.to_string()]);
    assert!(!refused(&own), "{}", stderr(&own));
}

#[test]
fn take_over_moves_the_session_and_records_it() {
    let fx = fixture();
    let holder = Agent::spawn();
    let successor = Agent::spawn();
    let session = fx.start_as(&holder.pid());
    let id = session.to_string();

    let taken = fx.run_as(&successor.pid(), &["sync", "--session", &id, "--take-over"]);
    assert!(!refused(&taken), "{}", stderr(&taken));
    let bound = fx.holder_event(session);
    assert_eq!(bound["reason"], "take_over");
    assert_eq!(bound["pid"].as_i64(), Some(i64::from(successor.0.id())));
    assert_eq!(
        bound["previous_pid"].as_i64(),
        Some(i64::from(holder.0.id()))
    );

    // The previous holder is now the one refused.
    let former = fx.run_as(&holder.pid(), &["sync", "--session", &id]);
    assert!(refused(&former), "{}", stderr(&former));
}

#[test]
fn an_unidentified_caller_is_let_through_unbound() {
    let fx = fixture();
    let holder = Agent::spawn();
    let session = fx.start_as(&holder.pid());
    let before = fx.holder_event(session);

    let output = fx.run_as("0", &["sync", "--session", &session.to_string()]);
    assert!(!refused(&output), "{}", stderr(&output));
    assert_eq!(fx.holder_event(session), before, "nothing was rebound");
}

#[test]
fn a_dead_holder_is_replaced_by_the_next_agent() {
    let fx = fixture();
    let holder = Agent::spawn();
    let session = fx.start_as(&holder.pid());
    let holder_pid = holder.pid();
    drop(holder);

    let next = Agent::spawn();
    let output = fx.run_as(&next.pid(), &["sync", "--session", &session.to_string()]);
    assert!(!refused(&output), "{}", stderr(&output));
    let bound = fx.holder_event(session);
    assert_eq!(bound["reason"], "holder_gone");
    assert_eq!(bound["previous_pid"].as_i64(), holder_pid.parse().ok());
}

impl Fixture {
    fn holder_worktree(&self, session: i64) -> PathBuf {
        let store = BrokerStore::open_in_repo(&self.repo).unwrap();
        PathBuf::from(store.session(session).unwrap().worktree_path)
    }
}
