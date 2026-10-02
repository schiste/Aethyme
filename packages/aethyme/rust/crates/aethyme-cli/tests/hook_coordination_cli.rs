//! Leases as an in-context coordination channel: `aethyme hook` tells an
//! agent, before it edits a file, that another live session is changing it,
//! and hands it notes from other sessions at its turn boundaries. Driven
//! through the built router against a real broker with two sessions.
//!
//! The rendering is unit-tested beside the hook; this suite holds what only a
//! real repository can: implicit leases from an uncommitted change, the
//! once-per-change memory, the read-only `PostToolUse` path, and silence on
//! everything that should stay silent.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ROUTER: &str = env!("CARGO_BIN_EXE_aethyme");

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn lines(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        for file in ["shared.txt", "codex.txt", "third.txt", "mine.txt"] {
            std::fs::write(repo.join(file), lines(10)).unwrap();
        }
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let repo = repo.canonicalize().unwrap();
        Self { tmp, repo }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(ROUTER);
        command
            .current_dir(cwd)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("AETHYME_WORKTREE_ROOT", self.tmp.path().join("worktrees"))
            .env("AETHYME_HOST_CACHE_DIR", self.tmp.path().join("cache"))
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_UPDATE_CHECK", "off")
            .env(
                "AETHYME_CHAU7_MCP_BRIDGE",
                "/__aethyme_test_no_chau7_bridge__",
            )
            .env_remove("AETHYME_REPO")
            .env_remove("AETHYME_AGENT")
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_SESSION_TAB_NAME")
            .env_remove("AETHYME_CHAU7_TAB_NAME");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self
            .command(&self.repo)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "aethyme {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn start(&self, task: &str, short_name: &str) -> (i64, PathBuf) {
        let output = self.run(&[
            "broker",
            "start",
            "--task",
            task,
            "--short-name",
            short_name,
            "--json",
        ]);
        let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = started["id"]
            .as_i64()
            .or_else(|| started["session"]["id"].as_i64())
            .expect("session id");
        let worktree = started["worktree_path"]
            .as_str()
            .or_else(|| started["session"]["worktree_path"].as_str())
            .expect("worktree path");
        (id, PathBuf::from(worktree))
    }

    /// Recompute implicit leases from every worktree's diff, and classify
    /// overlapping pairs, the way any routine broker command does.
    fn refresh(&self) {
        self.run(&["broker", "status", "--json"]);
    }

    /// The hook's raw stdout for one event, asserting it never fails.
    fn hook(&self, worktree: &Path, event: &str, payload: &serde_json::Value) -> String {
        let mut child = self
            .command(worktree)
            .args(["hook", event, "--repo"])
            .arg(worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "the hook must never fail");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

/// The `additionalContext` of one envelope line; empty when the hook was
/// silent. Never a `permissionDecision`.
fn context(stdout: &str) -> String {
    if stdout.is_empty() {
        return String::new();
    }
    assert_eq!(stdout.lines().count(), 1, "one envelope line: {stdout}");
    let value: serde_json::Value = serde_json::from_str(stdout).unwrap();
    let inner = &value["hookSpecificOutput"];
    assert!(
        inner.get("permissionDecision").is_none(),
        "coordination must never block an edit: {stdout}"
    );
    inner["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn edit(path: &Path) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Edit",
        "tool_input": {"file_path": path.to_string_lossy()},
    })
}

fn change_lines(worktree: &Path, file: &str, replace: &[usize]) {
    let mut text: Vec<String> = lines(10).lines().map(str::to_string).collect();
    for &line in replace {
        text[line - 1] = format!("changed {line}");
    }
    std::fs::write(worktree.join(file), text.join("\n") + "\n").unwrap();
}

#[test]
fn an_edit_on_a_file_another_live_session_changes_is_announced_once_per_change() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Repair the shared parser", "pr-1122");
    let (me, my_tree) = fixture.start("Add the CSV exporter", "csv");
    change_lines(&other_tree, "shared.txt", &[3, 4]);
    fixture.refresh();

    let first = context(&fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt"))));
    assert!(
        first.contains(&format!("session {other} (pr-1122)")),
        "{first}"
    );
    assert!(first.contains("Repair the shared parser"), "{first}");
    assert!(first.contains("`shared.txt` (lines 3–4"), "{first}");
    assert!(
        first.contains(&format!(
            "aethyme broker advanced note send --session {me} --to-session {other}"
        )),
        "{first}"
    );

    let again = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt")));
    assert!(
        again.is_empty(),
        "an unchanged change is announced once: {again}"
    );

    change_lines(&other_tree, "shared.txt", &[3, 4, 8]);
    fixture.refresh();
    let moved = context(&fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt"))));
    assert!(
        moved.contains("lines 3–4, 8"),
        "a moved change is announced again: {moved}"
    );
}

/// The case the lease table alone misses: the other agent commits its change
/// and runs no broker command, so no lease refresh has recorded the file. The
/// edit-time note still names the session, because the hook reads the other
/// worktree's current state rather than the last refresh.
#[test]
fn a_change_committed_since_the_last_lease_refresh_is_announced() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Rewrite the shared header", "header");
    let (_me, my_tree) = fixture.start("Add a footer", "footer");
    fixture.refresh();
    // Asked once while nothing is changed, so the hook has remembered the
    // answer "not changed" for this file; the change below must override it.
    let before = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt")));
    assert!(before.is_empty(), "nothing is changing yet: {before}");

    change_lines(&other_tree, "shared.txt", &[2]);
    git(&other_tree, &["add", "shared.txt"]);
    git(&other_tree, &["commit", "-qm", "header"]);

    let text = context(&fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt"))));
    assert!(
        text.contains(&format!("session {other} (header)")),
        "{text}"
    );
    assert!(text.contains("`shared.txt` (line 2"), "{text}");
}

/// The other direction: a lease refresh recorded the change, then the other
/// agent reverted it. Its implicit lease still names the file until the next
/// refresh, but nobody is changing the file, so the edit is not interrupted.
#[test]
fn a_change_reverted_since_the_last_lease_refresh_is_not_announced() {
    let fixture = Fixture::new();
    let (_other, other_tree) = fixture.start("Try a parser tweak", "tweak");
    let (_me, my_tree) = fixture.start("Parser feature", "feature");
    change_lines(&other_tree, "shared.txt", &[5]);
    fixture.refresh();

    git(&other_tree, &["checkout", "--", "shared.txt"]);

    let text = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt")));
    assert!(text.is_empty(), "a reverted change is not reported: {text}");
}

#[test]
fn codex_apply_patch_edits_get_the_same_note() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Rework codex output", "codex-out");
    let (_me, my_tree) = fixture.start("Codex agent task", "codex-agent");
    change_lines(&other_tree, "codex.txt", &[5]);
    fixture.refresh();

    let patch = serde_json::json!({
        "tool_name": "apply_patch",
        "cwd": my_tree.to_string_lossy(),
        "tool_input": {"command": ["apply_patch", "*** Begin Patch\n*** Update File: codex.txt\n@@\n-line 5\n+other 5\n*** End Patch"]},
    });
    let text = context(&fixture.hook(&my_tree, "PreToolUse", &patch));
    assert!(
        text.contains(&format!("session {other} (codex-out)")),
        "{text}"
    );
    assert!(text.contains("`codex.txt` (line"), "{text}");
}

#[test]
fn finished_sessions_own_files_and_read_tools_stay_silent() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Short-lived task", "gone");
    let (_me, my_tree) = fixture.start("Long task", "stays");
    change_lines(&other_tree, "third.txt", &[2]);
    change_lines(&my_tree, "mine.txt", &[2]);
    fixture.refresh();

    let other_id = other.to_string();
    fixture.run(&["broker", "finish", "close", "--session", &other_id]);
    fixture.refresh();
    let finished = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("third.txt")));
    assert!(
        finished.is_empty(),
        "a finished session is not in the file: {finished}"
    );

    let own = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("mine.txt")));
    assert!(own.is_empty(), "a file only this session changes: {own}");

    let read = serde_json::json!({
        "tool_name": "Read",
        "tool_input": {"file_path": my_tree.join("third.txt").to_string_lossy()},
    });
    assert!(fixture.hook(&my_tree, "PreToolUse", &read).is_empty());
    assert!(
        fixture.hook(&my_tree, "PostToolUse", &read).is_empty(),
        "no note waiting"
    );
}

/// A stale session still holds its leases, but nobody is working in its
/// worktree: telling an agent about it would send it to coordinate with no
/// one.
#[test]
fn a_stale_session_still_holding_leases_is_not_reported() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Abandoned work", "abandoned");
    let (_me, my_tree) = fixture.start("Current work", "current");
    change_lines(&other_tree, "shared.txt", &[6]);
    fixture.refresh();

    let db = fixture.repo.join(".aethyme/broker.db");
    let status = Command::new("/usr/bin/sqlite3")
        .arg(&db)
        .arg(format!(
            "UPDATE sessions SET status = 'stale' WHERE id = {other}"
        ))
        .status()
        .expect("run sqlite3");
    assert!(status.success());

    let text = fixture.hook(&my_tree, "PreToolUse", &edit(&my_tree.join("shared.txt")));
    assert!(text.is_empty(), "a stale session is not reported: {text}");
}

#[test]
fn notes_reach_the_agent_after_its_next_tool_call_once() {
    let fixture = Fixture::new();
    let (sender, _sender_tree) = fixture.start("Sender task", "sender");
    let (me, my_tree) = fixture.start("Recipient task", "recipient");
    let sender_id = sender.to_string();
    let me_id = me.to_string();
    fixture.run(&[
        "broker",
        "advanced",
        "note",
        "send",
        "--session",
        &sender_id,
        "--to-session",
        &me_id,
        "--message",
        "I am rewriting parse(); please hold off on shared.txt",
    ]);

    let tool = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
    let delivered = context(&fixture.hook(&my_tree, "PostToolUse", &tool));
    assert!(
        delivered.contains(&format!(
            "Note from session {sender}: I am rewriting parse(); please hold off on shared.txt"
        )),
        "{delivered}"
    );
    assert!(
        delivered.contains(&format!(
            "reply: aethyme broker advanced note send --session {me} --to-session {sender}"
        )),
        "{delivered}"
    );

    assert!(
        fixture.hook(&my_tree, "PostToolUse", &tool).is_empty(),
        "a delivered note is not repeated"
    );
    assert!(
        context(&fixture.hook(&my_tree, "UserPromptSubmit", &serde_json::json!({}))).is_empty(),
        "nor at the next prompt"
    );
}

#[test]
fn session_start_names_live_sessions_changing_files_it_leases() {
    let fixture = Fixture::new();
    let (other, other_tree) = fixture.start("Parser rework", "parser");
    let (_me, my_tree) = fixture.start("Exporter", "exporter");
    change_lines(&other_tree, "shared.txt", &[2]);
    change_lines(&my_tree, "shared.txt", &[9]);
    fixture.refresh();

    let brief = context(&fixture.hook(&my_tree, "SessionStart", &serde_json::json!({})));
    assert!(
        brief.contains(&format!(
            "Overlap: Session {other} (parser) is changing 1 file you lease: shared.txt."
        )),
        "{brief}"
    );
}

/// Measured, not asserted against a budget: the hook runs on every edit, so
/// its cost belongs in the PR, but a wall-clock assertion would be flaky on a
/// loaded machine.
#[test]
fn pre_tool_use_latency_with_several_live_sessions() {
    let fixture = Fixture::new();
    for n in 0..4 {
        let (_id, tree) = fixture.start(&format!("peer {n}"), &format!("peer-{n}"));
        change_lines(&tree, "shared.txt", &[n + 1]);
    }
    let (_me, my_tree) = fixture.start("measured", "measured");
    let runs: u32 = 5;
    let mean = |event: &serde_json::Value| {
        let started = std::time::Instant::now();
        for _ in 0..runs {
            fixture.hook(&my_tree, "PreToolUse", event);
        }
        started.elapsed() / runs
    };
    // The same process spawn and broker open with no file to check: the
    // floor the coordination cost sits on, measured under the same load.
    let floor = mean(&serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}));
    // No refresh: every peer's change is read from its worktree, which is
    // the expensive case the first edit of a file pays.
    let event = edit(&my_tree.join("shared.txt"));
    let started = std::time::Instant::now();
    fixture.hook(&my_tree, "PreToolUse", &event);
    let cold = started.elapsed();
    let warm = mean(&event);
    eprintln!(
        "PreToolUse with 4 live peers on one file: no-target floor {floor:?}, first call \
         {cold:?}, then {warm:?} per call (all include process spawn)"
    );
}
