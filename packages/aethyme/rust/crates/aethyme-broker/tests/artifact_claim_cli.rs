//! Strict claims exercise only temporary synthetic repositories and commands.
#![cfg(unix)]
mod common;

use aethyme_broker::SessionStatus;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn isolated(command: &mut Command, home: &Path) {
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    command
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", home.join("empty.gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

fn git(root: &Path, home: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(root);
    isolated(&mut command, home);
    // Set author identity after inherited-GIT isolation.
    command
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli(root: &Path, cwd: &Path, home: &Path, session: i64, command: &[&str]) -> Command {
    let mut args = vec![
        "advanced".into(),
        "resources".into(),
        "artifact-run".into(),
        "generated.json".into(),
        "--session".into(),
        session.to_string(),
        "--json".into(),
        "--".into(),
    ];
    args.extend(command.iter().map(|value| (*value).to_owned()));
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let mut process = common::broker_cli_for_fixture(CLI, &refs, root);
    isolated(&mut process, home);
    process.current_dir(cwd);
    process
}

struct HeldRun {
    child: Child,
    done: PathBuf,
}
impl Drop for HeldRun {
    fn drop(&mut self) {
        if let Err(error) = std::fs::write(&self.done, b"done") {
            eprintln!("fixture completion signal: {error}");
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                other => {
                    eprintln!("owned fixture runner did not exit: {other:?}");
                    if let Err(error) = self.child.kill() {
                        eprintln!("owned fixture runner kill: {error}");
                    }
                    if let Err(error) = self.child.wait() {
                        eprintln!("owned fixture runner reap: {error}");
                    }
                    return;
                }
            }
        }
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn idle_owner_cannot_be_taken_over_and_wrong_worktree_never_dispatches() {
    let fixture = common::FixtureRepository::new();
    let root = fixture.path();
    let home = fixture.owner_path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("empty.gitconfig"), "").unwrap();
    git(root, &home, &["init", "-q", "-b", "main"]);
    git(root, &home, &["config", "user.name", "fixture"]);
    git(
        root,
        &home,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        root,
        &home,
        &[
            "config",
            "core.excludesFile",
            home.join("empty.gitconfig").to_str().unwrap(),
        ],
    );
    git(root, &home, &["commit", "--allow-empty", "-qm", "fixture"]);
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let first = broker.start_worktree("generator one", None).unwrap();
    let second = broker.start_worktree("generator two", None).unwrap();
    let ready = fixture.owner_path().join("ready");
    let done = fixture.owner_path().join("done");
    let sentinel = fixture.owner_path().join("second-dispatched");
    let script = "printf ready > \"$1\"; n=0; while [ ! -e \"$2\" ]; do n=$((n+1)); [ \"$n\" -lt 500 ] || exit 70; sleep 0.02; done";
    let mut held = HeldRun {
        child: cli(
            root,
            Path::new(&first.worktree_path),
            &home,
            first.id,
            &[
                "sh",
                "-c",
                script,
                "fixture",
                ready.to_str().unwrap(),
                done.to_str().unwrap(),
            ],
        )
        .spawn()
        .unwrap(),
        done: done.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready.exists() && Instant::now() < deadline {
        assert!(
            held.child.try_wait().unwrap().is_none(),
            "generator exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ready.exists(), "generator did not start");
    broker
        .store()
        .set_session_status(first.id, SessionStatus::Idle, None)
        .unwrap();
    let command = [
        "sh",
        "-c",
        "printf dispatched > \"$1\"",
        "fixture",
        sentinel.to_str().unwrap(),
    ];
    let refused = cli(
        root,
        Path::new(&second.worktree_path),
        &home,
        second.id,
        &command,
    )
    .output()
    .unwrap();
    assert_eq!(
        refused.status.code(),
        Some(75),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!sentinel.exists(), "contending generator was dispatched");
    let wrong = cli(
        root,
        Path::new(&second.worktree_path),
        &home,
        first.id,
        &command,
    )
    .output()
    .unwrap();
    assert!(!wrong.status.success());
    assert!(
        !sentinel.exists(),
        "wrong-worktree generator was dispatched"
    );
    let nested = Path::new(&first.worktree_path).join("nested");
    std::fs::create_dir(&nested).unwrap();
    let nested_refusal = cli(root, &nested, &home, first.id, &command)
        .output()
        .unwrap();
    assert!(!nested_refusal.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&nested_refusal.stdout),
        String::from_utf8_lossy(&nested_refusal.stderr)
    );
    assert!(diagnostic.contains("worktree root"), "{diagnostic}");
    assert!(
        !sentinel.exists(),
        "nested-worktree generator was dispatched"
    );
    std::fs::write(&done, b"done").unwrap();
    assert!(held.child.wait().unwrap().success());
    let completed = cli(
        root,
        Path::new(&second.worktree_path),
        &home,
        second.id,
        &command,
    )
    .output()
    .unwrap();
    success(&completed);
    assert!(
        sentinel.exists(),
        "released claim did not permit the next generator"
    );
}
