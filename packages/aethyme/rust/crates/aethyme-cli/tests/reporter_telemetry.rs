use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn aethyme(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(cwd)
        .env(
            "AETHYME_CHAU7_MCP_BRIDGE",
            "/__aethyme_test_no_chau7_bridge__",
        )
        // The fixture adopts its own main checkout (#284 proposal 9).
        .env("AETHYME_ALLOW_MAIN_CHECKOUT", "1")
        .output()
        .expect("run aethyme")
}

fn assert_success(output: &Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "aethyme {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Aethyme Test"]);
    git(root, &["config", "user.email", "aethyme@example.test"]);
    fs::write(root.join("README.md"), "# Fixture\n").expect("write fixture");
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-qm", "initial"]);
}

#[test]
fn hooks_status_leaves_a_fresh_repo_trace_free() {
    let temp = tempfile::tempdir().expect("tempdir");
    init_git_repo(temp.path());

    let args = ["broker", "advanced", "hooks", "status", "--json"];
    let output = aethyme(temp.path(), &args);
    assert_success(&output, &args);
    assert!(
        !temp.path().join(".aethyme").exists(),
        "hooks status created repository-local state"
    );
}

#[test]
fn report_only_commands_do_not_append_metrics() {
    let temp = tempfile::tempdir().expect("tempdir");
    init_git_repo(temp.path());

    let init_args = ["init", "--json"];
    assert_success(&aethyme(temp.path(), &init_args), &init_args);
    fs::write(
        temp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"noop\"\ncommand = \"true\"\ncost = 1\ntriggers = [\"**/*\"]\n",
    )
    .expect("write fixture gate");
    let adopt_args = [
        "broker",
        "start",
        "--adopt",
        "--task",
        "fixture",
        "--short-name",
        "Fixture",
        "--json",
    ];
    assert_success(&aethyme(temp.path(), &adopt_args), &adopt_args);

    let metrics_path = temp.path().join(".aethyme/logs/command-metrics.jsonl");
    let before = fs::read(&metrics_path).expect("setup commands recorded metrics");
    for args in [
        &["broker", "advanced", "hooks", "status", "--json"][..],
        &["broker", "advanced", "queue", "--json"][..],
        &["broker", "advanced", "events", "--json"][..],
        &["broker", "advanced", "metrics", "--json"][..],
        // `insights` is a report over the same history `metrics` accounts for,
        // so it belongs in the same set: a cron job polling it must not appear
        // in the numbers it reads.
        &["broker", "advanced", "insights", "--json"][..],
        &["broker", "advanced", "insights", "--days", "0", "--json"][..],
        &["broker", "status", "doctor", "--json"][..],
        &[
            "broker",
            "advanced",
            "gates",
            "semantic",
            "--session",
            "1",
            "--json",
        ][..],
    ] {
        assert_success(&aethyme(temp.path(), args), args);
        assert_eq!(
            fs::read(&metrics_path).expect("metrics file remains present"),
            before,
            "report-only command appended telemetry: {args:?}"
        );
    }

    // `gates affected` is the one read-only exception (#481): it records its
    // phase timings so a stalled selection can be localized afterwards.
    let affected_args = [
        "broker",
        "advanced",
        "gates",
        "affected",
        "--session",
        "1",
        "--json",
    ];
    assert_success(&aethyme(temp.path(), &affected_args), &affected_args);
    let after_affected = fs::read_to_string(&metrics_path).expect("gates affected metric");
    let appended = &after_affected[before.len()..];
    let rows: Vec<serde_json::Value> = appended
        .lines()
        .map(|line| serde_json::from_str(line).expect("metric line is JSON"))
        .collect();
    assert_eq!(rows.len(), 1, "exactly one metric line: {appended}");
    assert_eq!(rows[0]["command"], "gates.affected", "{appended}");
    assert_eq!(rows[0]["phase_budget_ms"], 5_000, "{appended}");
    for phase in ["graph_read", "manifest", "selection", "lock_wait"] {
        assert!(
            rows[0]["phase_timings_ms"][phase].is_u64(),
            "{phase} timing missing: {appended}"
        );
    }
    assert!(rows[0]["over_budget_phases"].is_array(), "{appended}");
    let before = after_affected.into_bytes();

    let stateful_args = ["broker", "status", "--json"];
    assert_success(&aethyme(temp.path(), &stateful_args), &stateful_args);
    assert!(
        fs::read(&metrics_path)
            .expect("stateful command metric")
            .len()
            > before.len(),
        "state-refreshing status should remain observable"
    );
}
