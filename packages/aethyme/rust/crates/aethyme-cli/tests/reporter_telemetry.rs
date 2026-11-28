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
        "--allow-main-checkout",
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
            // A zero timing is omitted from a row: absent means 0.
            rows[0]["phase_timings_ms"][phase].is_null()
                || rows[0]["phase_timings_ms"][phase].is_u64(),
            "{phase} timing malformed: {appended}"
        );
    }
    // An empty list is omitted from a row.
    assert!(
        rows[0]["over_budget_phases"].is_null() || rows[0]["over_budget_phases"].is_array(),
        "{appended}"
    );
    let before = after_affected.into_bytes();

    let stateful_args = ["broker", "status", "--full", "--json"];
    assert_success(&aethyme(temp.path(), &stateful_args), &stateful_args);
    assert!(
        fs::read(&metrics_path)
            .expect("stateful command metric")
            .len()
            > before.len(),
        "state-refreshing status should remain observable"
    );
}

#[cfg(unix)]
mod interrupted {
    use super::{aethyme, assert_success, init_git_repo};
    use std::fs;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn metric_rows(root: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(root.join(".aethyme/logs/command-metrics.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("metric rows are JSON"))
            .collect()
    }

    fn initialised_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        init_git_repo(temp.path());
        let init_args = ["init", "--json"];
        assert_success(&aethyme(temp.path(), &init_args), &init_args);
        temp
    }

    /// Run `broker status --json` held inside the `test_command_hold` span,
    /// wait for its heartbeat to name that span, then send `signal`.
    fn interrupt_held_command(root: &Path, signal: libc::c_int) -> serde_json::Value {
        interrupt_held_command_with(root, signal, &[])
    }

    fn interrupt_held_command_with(
        root: &Path,
        signal: libc::c_int,
        env: &[(&str, &str)],
    ) -> serde_json::Value {
        let rows_before = metric_rows(root).len();
        let mut command = Command::new(env!("CARGO_BIN_EXE_aethyme"));
        command
            .args(["broker", "status", "--json"])
            .current_dir(root)
            .env(
                "AETHYME_CHAU7_MCP_BRIDGE",
                "/__aethyme_test_no_chau7_bridge__",
            )
            .env("AETHYME_TEST_COMMAND_HOLD_MS", "120000")
            .env("AETHYME_COMMAND_HEARTBEAT_MS", "100")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // SAFETY: runs in the child between fork and exec and calls only
        // `signal`, which is async-signal-safe. A harness that started this
        // test with the signals ignored would otherwise pass that on, and an
        // ignored signal is deliberately left ignored.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn aethyme");
        let heartbeat = root.join(format!(".aethyme/run/commands/{}.json", child.id()));
        let deadline = Instant::now() + Duration::from_secs(60);
        let record = loop {
            if let Ok(bytes) = fs::read(&heartbeat)
                && let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && record["in_span"] == "test_command_hold"
            {
                break record;
            }
            assert!(
                Instant::now() < deadline,
                "no heartbeat naming the held span at {}",
                heartbeat.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(record["command"], "status");
        assert_eq!(record["pid"], child.id());
        assert_eq!(
            record["open_spans"],
            serde_json::json!(["test_command_hold"])
        );
        // Long enough for the held span to have measurable elapsed time.
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: `kill` takes the pid and signal by value; the target is the
        // child this test spawned and has not reaped.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let output = child.wait_with_output().expect("wait for aethyme");
        assert_eq!(
            output.status.signal(),
            Some(signal),
            "the command still dies of the signal: {:?}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!heartbeat.exists(), "the heartbeat is removed");
        // Only this command's rows: a background worker may append its own.
        let rows = status_rows_since(root, rows_before);
        assert_eq!(rows.len(), 1, "one row: {rows:?}");
        rows.into_iter().next().unwrap()
    }

    fn status_rows_since(root: &Path, before: usize) -> Vec<serde_json::Value> {
        metric_rows(root)
            .into_iter()
            .skip(before)
            .filter(|row| row["command"] == "status")
            .collect()
    }

    fn assert_interrupted_row(row: &serde_json::Value, signal: &str, exit: u64) {
        assert_eq!(row["command"], "status", "{row}");
        assert_eq!(row["exit"], exit, "{row}");
        assert_eq!(row["outcome"], "interrupted", "{row}");
        let interrupted = &row["interrupted"];
        assert_eq!(interrupted["signal"], signal, "{row}");
        assert_eq!(interrupted["in_span"], "test_command_hold", "{row}");
        assert_eq!(
            interrupted["open_spans"],
            serde_json::json!(["test_command_hold"]),
            "{row}"
        );
        let open_elapsed = interrupted["open_span_elapsed_ms"]["test_command_hold"]
            .as_u64()
            .expect("open span elapsed");
        assert!(open_elapsed > 0, "{row}");
        let phase = row["phase_timings_ms"]["test_command_hold"]
            .as_u64()
            .expect("the open span's time is in the phase timings");
        assert!(phase > 0, "{row}");
        // One occurrence beside a timing is left implicit.
        assert_eq!(
            row["phase_counts"]["test_command_hold"]
                .as_u64()
                .unwrap_or(1),
            1,
            "{row}"
        );
        assert!(row["duration_ms"].as_u64().unwrap() >= phase, "{row}");
        assert_eq!(row["attribution"]["schema_version"], 1, "{row}");
        assert!(row["cpu_ms"]["user"].is_u64(), "{row}");
        assert!(row["output_bytes"].is_u64(), "{row}");
        // A null session id is omitted from a row.
        assert!(
            row["session_id"].is_null() || row["session_id"].is_i64(),
            "{row}"
        );
    }

    #[test]
    fn sigint_writes_the_row_naming_the_open_span() {
        let temp = initialised_repo();
        let row = interrupt_held_command(temp.path(), libc::SIGINT);
        assert_interrupted_row(&row, "SIGINT", 130);
    }

    #[test]
    fn sigterm_writes_the_row_naming_the_open_span() {
        let temp = initialised_repo();
        let row = interrupt_held_command(temp.path(), libc::SIGTERM);
        assert_interrupted_row(&row, "SIGTERM", 143);
    }

    /// A Ctrl-C also kills the foreground child, so the command can return
    /// while its row is still being written. It must still die of the signal
    /// and write one row, not a normal row as well (or instead).
    #[test]
    fn a_command_returning_after_the_signal_still_dies_of_it() {
        let temp = initialised_repo();
        let row = interrupt_held_command_with(
            temp.path(),
            libc::SIGINT,
            &[
                ("AETHYME_TEST_COMMAND_HOLD_RETURNS_ON_SIGNAL", "1"),
                ("AETHYME_TEST_INTERRUPT_FLUSH_DELAY_MS", "500"),
            ],
        );
        assert_eq!(row["exit"], 130, "{row}");
        assert_eq!(row["outcome"], "interrupted", "{row}");
        assert_eq!(row["interrupted"]["signal"], "SIGINT", "{row}");
        // One occurrence beside a timing is left implicit.
        assert_eq!(
            row["phase_counts"]["test_command_hold"]
                .as_u64()
                .unwrap_or(1),
            1,
            "{row}"
        );
    }

    #[test]
    fn a_normal_exit_writes_one_uninterrupted_row_and_no_heartbeat() {
        let temp = initialised_repo();
        let rows_before = metric_rows(temp.path()).len();
        let args = ["broker", "status", "--json"];
        let output = aethyme(temp.path(), &args);
        let rows = status_rows_since(temp.path(), rows_before);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row["command"], "status", "{row}");
        assert_eq!(
            row["exit"].as_i64(),
            output.status.code().map(i64::from),
            "{row}"
        );
        assert!(row.get("interrupted").is_none(), "{row}");
        assert_ne!(row["outcome"], "interrupted", "{row}");
        let commands = temp.path().join(".aethyme/run/commands");
        assert!(
            fs::read_dir(&commands).map_or(true, |mut entries| entries.next().is_none()),
            "no heartbeat left behind"
        );
    }
}
