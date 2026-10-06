//! A swap of the installed aethyme under a live session is reported (#293).
//!
//! `start` records the build that ran it; `status` compares that record with
//! the build running now. The test rewrites the record to an older build to
//! stand in for a `cargo install` or update that replaced the pair.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use aethyme_broker::{BrokerStore, events};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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

/// A stand-in agent runtime that keeps the session live until dropped.
struct Agent(Child);

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
    fn run_as(&self, agent: &str, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env("AETHYME_AGENT_PID", agent)
            .output()
            .unwrap()
    }

    fn replaced_advice(&self) -> Vec<serde_json::Value> {
        let output = self.run_as("0", &["status", "--json"]);
        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "status --json: {error}: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        status["advice"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["id"] == "install.replaced")
            .cloned()
            .collect()
    }
}

#[test]
fn status_reports_an_installed_build_that_changed_after_start() {
    let fx = fixture();
    let agent = Agent(Command::new("sleep").arg("600").spawn().unwrap());
    let started = fx.run_as(
        &agent.0.id().to_string(),
        &["start", "--task", "swap under me", "--json"],
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session = serde_json::from_slice::<serde_json::Value>(&started.stdout).unwrap()["id"]
        .as_i64()
        .unwrap();

    let mut store = BrokerStore::open_in_repo(&fx.repo).unwrap();
    let recorded = store
        .latest_session_install_event(session)
        .unwrap()
        .expect("start records the installed build");
    let payload: serde_json::Value =
        serde_json::from_str(recorded.payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        fx.replaced_advice().is_empty(),
        "an unchanged install is silent"
    );

    store
        .append_event(
            events::SESSION_INSTALL_RECORDED,
            Some(session),
            Some(
                &serde_json::json!({
                    "version": "0.0.1",
                    "describe": "v0.0.1",
                    "commit": "0000000000000000000000000000000000000000",
                    "path": "/old/aethyme",
                    "engine_banner": null
                })
                .to_string(),
            ),
        )
        .unwrap();
    drop(store);

    let advice = fx.replaced_advice();
    assert_eq!(advice.len(), 1, "{advice:?}");
    assert_eq!(advice[0]["session_id"], session);
    let evidence = advice[0]["evidence"].to_string();
    assert!(
        evidence.contains("recorded: ") && evidence.contains("running: "),
        "{evidence}"
    );
    assert!(
        advice[0]["summary"].as_str().unwrap().contains("0.0.1")
            || evidence.contains("0000000000000000000000000000000000000000"),
        "the old build is named: {advice:?}"
    );
}
