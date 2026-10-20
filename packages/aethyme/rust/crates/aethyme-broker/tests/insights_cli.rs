//! The `broker advanced insights` command end to end, through the real CLI.
//!
//! `tests/insights.rs` exercises the report computation directly; these tests
//! cover what only the command path can break: opening the store, refusing bad
//! flags, and leaving the database it reports on unchanged.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;
use common::{FixtureRepository, open_fixture_repository_broker};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run(broker: &Broker, repo: &Path, args: &[&str]) -> Output {
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(repo);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("bind child to fixture storage context");
    command.output().unwrap()
}

fn fixture() -> FixtureRepository {
    let fixture = FixtureRepository::new();
    let repo = fixture.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
    fixture
}

#[test]
fn insights_reports_from_a_repository_with_history() {
    let fixture = fixture();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let session = broker
        .adopt(fixture.path(), Some("insights fixture"))
        .unwrap();
    let now = broker.store().newest_event_ts();
    broker
        .store()
        .record_session_activity(session.id, now)
        .unwrap();

    let output = run(
        &broker,
        fixture.path(),
        &["advanced", "insights", "--days", "0", "--json"],
    );
    assert!(
        output.status.success(),
        "insights failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["funnel"]["registered"], 1);
    assert!(report["idle_gap_ms"].as_i64().is_some());

    let text = run(&broker, fixture.path(), &["advanced", "insights"]);
    assert!(
        text.status.success(),
        "insights (text) failed: {}",
        String::from_utf8_lossy(&text.stderr)
    );
}

#[test]
fn insights_leaves_the_event_log_it_reports_on_unchanged() {
    // A report that wrote a `broker.command.*` event or a metrics line would be
    // one of the things it reports on, and every poll would move the numbers.
    let fixture = fixture();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    broker
        .adopt(fixture.path(), Some("insights fixture"))
        .unwrap();
    let before = broker.store().newest_event_ts();

    let output = run(&broker, fixture.path(), &["advanced", "insights", "--json"]);
    assert!(output.status.success());
    assert_eq!(broker.store().newest_event_ts(), before);
}

#[test]
fn insights_counts_gate_cache_hits_recorded_in_the_event_log() {
    // The report used to read only the funnel's event prefixes, so `gate.cached`
    // never reached the cache totals and every repository reported zero cache
    // hits — a measured-looking zero for something it never read.
    let fixture = fixture();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    broker
        .store()
        .append_event(
            "gate.cached",
            None,
            Some(r#"{"gate":"test","saved_ms":1200}"#),
        )
        .unwrap();
    broker
        .store()
        .append_event(
            "gate.cached",
            None,
            Some(r#"{"gate":"test","saved_ms":800}"#),
        )
        .unwrap();

    let output = run(
        &broker,
        fixture.path(),
        &["advanced", "insights", "--days", "0", "--json"],
    );
    assert!(
        output.status.success(),
        "insights failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["gates"]["cached"], 2, "{}", report["gates"]);
    assert_eq!(report["gates"]["cached_saved_ms"], 2000);
}

#[test]
fn the_default_window_applies_to_gate_figures_as_well_as_the_funnel() {
    // The default report is the last 30 days. Gate, cache, coordination and
    // operation figures used to cover all history regardless, so the report put
    // a 30-day funnel beside lifetime gate numbers without saying so.
    let fixture = fixture();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    broker
        .store()
        .append_event(
            "gate.cached",
            None,
            Some(r#"{"gate":"test","saved_ms":500}"#),
        )
        .unwrap();
    let old = broker
        .store()
        .append_event(
            "gate.cached",
            None,
            Some(r#"{"gate":"test","saved_ms":9000}"#),
        )
        .unwrap();
    let ninety_days_ago = broker.store().newest_event_ts() - 90 * 86_400_000;
    broker
        .store()
        .set_event_timestamp_for_test(old, ninety_days_ago);

    let windowed = run(&broker, fixture.path(), &["advanced", "insights", "--json"]);
    assert!(windowed.status.success());
    let windowed: serde_json::Value = serde_json::from_slice(&windowed.stdout).unwrap();
    assert_eq!(windowed["gates"]["cached"], 1, "{}", windowed["gates"]);
    assert_eq!(windowed["gates"]["cached_saved_ms"], 500);

    let all = run(
        &broker,
        fixture.path(),
        &["advanced", "insights", "--days", "0", "--json"],
    );
    assert!(all.status.success());
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all["gates"]["cached"], 2);
}

#[test]
fn insights_refuses_a_negative_window() {
    // `--days -5` used to fall through the `days > 0` filter and silently report
    // all history: a typo answered as if it were `--days 0`.
    let fixture = fixture();
    let broker = open_fixture_repository_broker(&fixture).unwrap();

    let output = run(
        &broker,
        fixture.path(),
        &["advanced", "insights", "--days", "-5"],
    );
    // The broker reports a bad flag value as a usage message, which exits 1.
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--days"));
}
