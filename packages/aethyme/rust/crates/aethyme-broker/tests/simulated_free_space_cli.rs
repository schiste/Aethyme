//! `AETHYME_TEST_AVAILABLE_BYTES`: the suite's free-space reading is
//! simulated, so its outcome does not depend on the disk of the host it runs
//! on.
//!
//! Each test states the reading it needs, low or generous, and asserts the
//! broker decided on it: a low reading refuses the gate and raises the status
//! headroom row on a host with plenty of space, and a generous one admits the
//! gate on a host with none.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::TEST_AVAILABLE_BYTES_ENV;

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const LOW: &str = "1024";
const GENEROUS: &str = "1099511627776";

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
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let host = tmp.path().join("host");
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::create_dir_all(&host).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/gates.toml\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"echo\"\ncommand = \"echo ok\"\ntriggers = [\"**\"]\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        _tmp: tmp,
    }
}

impl Fixture {
    fn run(&self, available: &str, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env(TEST_AVAILABLE_BYTES_ENV, available)
            .output()
            .unwrap()
    }

    fn gate(&self, available: &str) -> (bool, String) {
        let output = self.run(
            available,
            &["advanced", "gates", "run", "--all", "--no-cache"],
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    fn advice_ids(&self, available: &str) -> Vec<String> {
        let output = self.run(available, &["status", "--json"]);
        assert!(output.status.success(), "{output:?}");
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        status["advice"]
            .as_array()
            .unwrap_or_else(|| panic!("no advice array: {status}"))
            .iter()
            .filter_map(|row| row["id"].as_str().map(str::to_string))
            .collect()
    }
}

#[test]
fn a_low_reading_refuses_the_gate_and_raises_the_headroom_row() {
    let fx = fixture();
    let (passed, text) = fx.gate(LOW);
    assert!(!passed, "a gate admitted on 1 KiB free: {text}");
    assert!(text.contains("free"), "the refusal names the disk: {text}");
    assert!(
        fx.advice_ids(LOW)
            .iter()
            .any(|id| id == "host.gate-headroom"),
        "status is silent about a starved volume"
    );
}

#[test]
fn a_generous_reading_admits_the_gate_and_keeps_status_quiet() {
    let fx = fixture();
    let (passed, text) = fx.gate(GENEROUS);
    assert!(passed, "{text}");
    assert!(
        !fx.advice_ids(GENEROUS)
            .iter()
            .any(|id| id == "host.gate-headroom"),
        "a simulated 1 TiB still reported a starved volume"
    );
}
