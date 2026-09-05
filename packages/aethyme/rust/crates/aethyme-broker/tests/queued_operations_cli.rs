//! A coordinated operation must be visible while it waits for the lock (#138).
//!
//! Before this, the record was created only after the lock was acquired, so a
//! queued command left no trace anywhere. A caller could not tell "queued behind
//! known work" from "died without registering", and re-issued it.

use std::path::Path;
use std::process::{Command, Output};

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
    assert!(output.status.success(), "git {args:?}");
}

fn run(repo: &Path, state: &Path, path: Option<&Path>, args: &[&str]) -> Output {
    let mut command = Command::new(CLI);
    command.args(args).current_dir(repo);
    command.env("AETHYME_HOST_STATE_DIR", state);
    if let Some(path) = path {
        let existing = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{}", path.display(), existing));
    }
    command.output().unwrap()
}

/// A `git` that sleeps for one specific tag name and is otherwise the real thing,
/// so exactly one coordinated operation holds the lock long enough to observe.
fn slow_git_shim(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let real = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    let shim = dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nif [ \"$1\" = tag ] && [ \"$2\" = slowmark ]; then sleep 6; fi\nexec {real} \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn session_id(output: &Output) -> i64 {
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["session"]["id"]
        .as_i64()
        .or_else(|| value["id"].as_i64())
        .unwrap_or_else(|| panic!("no session id in {value}"))
}

#[test]
fn an_operation_waiting_for_the_lock_is_visible_as_prepared() {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    slow_git_shim(bin.path());

    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);

    let started = run(
        repo.path(),
        state.path(),
        None,
        &["start", "--task", "queued visibility", "--json"],
    );
    assert!(
        started.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session = session_id(&started).to_string();

    // Holds the repository write lock for ~6 seconds.
    let mut holder = Command::new(CLI)
        .args([
            "git",
            "--session",
            &session,
            "--reason",
            "hold the lock for the test",
            "--",
            "tag",
            "slowmark",
        ])
        .current_dir(repo.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .spawn()
        .unwrap();

    // Wait until the holder actually owns the lock; otherwise the second command
    // can start and finish before the first ever acquires it.
    let mut holding = false;
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let listed = run(
            repo.path(),
            state.path(),
            None,
            &["operations", "list", "--json"],
        );
        if String::from_utf8_lossy(&listed.stdout).contains("\"running\"") {
            holding = true;
            break;
        }
    }
    assert!(holding, "the holder never took the lock");

    // A second operation that can only queue behind it.
    let waiter = Command::new(CLI)
        .args([
            "git",
            "--session",
            &session,
            "--reason",
            "queue behind the holder",
            "--",
            "tag",
            "queued-tag",
        ])
        .current_dir(repo.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // While the holder runs, the waiter must be recorded rather than invisible.
    // Matching on the waiter's own authorization reason keeps this from passing on
    // the holder's brief prepared window inside the lock.
    let mut saw_queued = false;
    let mut last = String::new();
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let listed = run(
            repo.path(),
            state.path(),
            None,
            &["operations", "list", "--json"],
        );
        let parsed: serde_json::Value =
            serde_json::from_slice(&listed.stdout).unwrap_or(serde_json::Value::Null);
        let operations = parsed["operations"].as_array().cloned().unwrap_or_default();
        last = operations
            .iter()
            .map(|o| format!("{}={}", o["id"], o["status"]))
            .collect::<Vec<_>>()
            .join(" ");
        if operations.iter().any(|operation| {
            operation["status"] == "prepared"
                && operation["authorization_reason"] == "queue behind the holder"
        }) {
            saw_queued = true;
            break;
        }
    }

    let waiter_out = waiter.wait_with_output().unwrap();
    holder.wait().unwrap();

    assert!(
        saw_queued,
        "the waiting operation must be recorded while it waits; saw {last}"
    );
    // And it must say what it is waiting for rather than pausing silently.
    let waited = String::from_utf8_lossy(&waiter_out.stderr);
    assert!(
        waited.contains("[coordination] waiting"),
        "the waiter must report the holder: {waited}"
    );
}

/// Issue #138: with two identical commands queued, the second would have fired
/// against state the first already changed -- an already-merged PR, in the
/// reported case.
#[test]
fn an_identical_pending_command_from_one_session_is_refused() {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    slow_git_shim(bin.path());

    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);

    let started = run(
        repo.path(),
        state.path(),
        None,
        &["start", "--task", "duplicate refusal", "--json"],
    );
    assert!(started.status.success());
    let session = session_id(&started).to_string();

    let mut holder = Command::new(CLI)
        .args([
            "git",
            "--session",
            &session,
            "--reason",
            "hold the lock for the test",
            "--",
            "tag",
            "slowmark",
        ])
        .current_dir(repo.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .spawn()
        .unwrap();

    let mut holding = false;
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let listed = run(
            repo.path(),
            state.path(),
            None,
            &["operations", "list", "--json"],
        );
        if String::from_utf8_lossy(&listed.stdout).contains("\"running\"") {
            holding = true;
            break;
        }
    }
    assert!(holding, "the holder never took the lock");

    let queued = |name: &str| {
        Command::new(CLI)
            .args([
                "git",
                "--session",
                &session,
                "--reason",
                "queue behind the holder",
                "--",
                "tag",
                name,
            ])
            .current_dir(repo.path())
            .env("AETHYME_HOST_STATE_DIR", state.path())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };

    let first = queued("dup-tag");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let second = queued("dup-tag");

    let second_out = second.wait_with_output().unwrap();
    let first_out = first.wait_with_output().unwrap();
    holder.wait().unwrap();

    assert!(
        !second_out.status.success(),
        "the duplicate must be refused, not queued"
    );
    let refusal = String::from_utf8_lossy(&second_out.stderr);
    assert!(
        refusal.contains("identical") && refusal.contains("pending"),
        "the refusal must say why: {refusal}"
    );
    // The original is untouched: refusing the duplicate must not cancel it.
    assert!(
        first_out.status.success(),
        "the first command must still run: {}",
        String::from_utf8_lossy(&first_out.stderr)
    );
}

/// Issue #138: a caller that would rather report honestly than park forever needs
/// a way to say so.
#[test]
fn no_wait_refuses_immediately_instead_of_queueing() {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    slow_git_shim(bin.path());

    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);

    let started = run(
        repo.path(),
        state.path(),
        None,
        &["start", "--task", "no wait", "--json"],
    );
    assert!(started.status.success());
    let session = session_id(&started).to_string();

    let mut holder = Command::new(CLI)
        .args([
            "git",
            "--session",
            &session,
            "--reason",
            "hold the lock for the test",
            "--",
            "tag",
            "slowmark",
        ])
        .current_dir(repo.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .spawn()
        .unwrap();

    let mut holding = false;
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let listed = run(
            repo.path(),
            state.path(),
            None,
            &["operations", "list", "--json"],
        );
        if String::from_utf8_lossy(&listed.stdout).contains("\"running\"") {
            holding = true;
            break;
        }
    }
    assert!(holding, "the holder never took the lock");

    let began = std::time::Instant::now();
    let refused = run(
        repo.path(),
        state.path(),
        None,
        &[
            "git",
            "--session",
            &session,
            "--reason",
            "do not queue",
            "--no-wait",
            "--",
            "tag",
            "nowait-tag",
        ],
    );
    let elapsed = began.elapsed();
    holder.wait().unwrap();

    assert!(!refused.status.success(), "--no-wait must refuse");
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "--no-wait must not queue: took {elapsed:?}"
    );
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        message.contains("write lock is busy"),
        "the refusal must name the cause: {message}"
    );
}
