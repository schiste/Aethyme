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
/// 12 GiB: above the 8 GiB a gate needs, below twice that.
const MARGINAL: &str = "12884901888";

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
    assert!(
        !fx.advice_ids(GENEROUS)
            .iter()
            .any(|id| id == "host.disk-low"),
        "a simulated 1 TiB still warned about free space"
    );
}

#[test]
fn a_marginal_reading_admits_the_gate_but_warns_before_it_would_refuse() {
    let fx = fixture();
    let (passed, text) = fx.gate(MARGINAL);
    assert!(passed, "12 GiB is above the gate threshold: {text}");
    let ids = fx.advice_ids(MARGINAL);
    assert!(
        ids.iter().any(|id| id == "host.disk-low"),
        "status stayed silent one large build away from refusing every gate: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id == "host.gate-headroom"),
        "a gate that starts must not be reported as refused: {ids:?}"
    );
}

/// A repository whose one gate writes `reading` into the free-space file the
/// broker reads, then fails: free disk drops while the gate runs.
fn starving_fixture(reading_after: &str) -> (Fixture, PathBuf) {
    let fx = fixture();
    let reading = fx.host.join("free-bytes");
    std::fs::write(&reading, GENEROUS).unwrap();
    std::fs::write(
        fx.repo.join(".aethyme/gates.toml"),
        format!(
            "[[gate]]\nname = \"starve\"\ncommand = \"printf {reading_after} > '{}'; exit 1\"\ntriggers = [\"**\"]\n",
            reading.display()
        ),
    )
    .unwrap();
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-qm", "starving gate"]);
    (fx, reading)
}

fn gate_json(fx: &Fixture, reading: &Path) -> serde_json::Value {
    std::fs::write(reading, GENEROUS).unwrap();
    let output = fx.run(
        &format!("@{}", reading.display()),
        &["advanced", "gates", "run", "--all", "--json"],
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    let outcomes = value
        .as_array()
        .cloned()
        .or_else(|| value["outcomes"].as_array().cloned())
        .or_else(|| value["gate_outcomes"].as_array().cloned())
        .unwrap_or_else(|| panic!("no outcomes: {value}"));
    assert_eq!(outcomes.len(), 1, "{value}");
    outcomes[0].clone()
}

/// A gate admitted with room to spare can still fill the disk while it runs.
/// What fails then is the host, so the run is resource contention, deferred
/// rather than rejected, and never served from the cache as a verdict (#288).
#[test]
fn a_gate_that_starves_after_it_starts_is_a_host_fault_not_a_verdict() {
    let (fx, reading) = starving_fixture(LOW);

    let first = gate_json(&fx, &reading);
    assert_eq!(first["status"], "error", "{first}");
    assert_eq!(first["failure_class"], "resource_contention", "{first}");
    assert_eq!(first["host_fault"], true, "{first}");
    assert_eq!(first["free_disk_bytes_end"], 1024, "{first}");
    let log = std::fs::read_to_string(first["log_path"].as_str().unwrap()).unwrap();
    assert!(log.contains("host starvation"), "{log}");

    let second = gate_json(&fx, &reading);
    assert_eq!(
        second["cached"], false,
        "a starved run was served as a verdict: {second}"
    );
}

/// The risk in the rule above is laundering a real failure. The same failing
/// gate on a disk that stays healthy is still a test failure, and cached.
#[test]
fn a_gate_that_fails_on_a_healthy_disk_is_still_a_verdict() {
    let (fx, reading) = starving_fixture(MARGINAL);

    let first = gate_json(&fx, &reading);
    assert_eq!(first["status"], "fail", "{first}");
    assert_eq!(first["failure_class"], "test_failure", "{first}");
    assert!(first.get("host_fault").is_none(), "{first}");

    let second = gate_json(&fx, &reading);
    assert_eq!(second["cached"], true, "{second}");
}
