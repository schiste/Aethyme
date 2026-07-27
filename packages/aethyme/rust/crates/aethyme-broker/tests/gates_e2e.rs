//! End-to-end gate runner tests (issues #15-#18): real repos, real gate
//! processes, tree-hash caching across sessions, cheap-first ordering
//! with fail-fast, and cancellation of superseded runs.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use aethyme_broker::{Broker, GateProgressSink, GateStatus};

#[derive(Default)]
struct CapturedProgress {
    lines: Mutex<Vec<String>>,
}

impl CapturedProgress {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

impl GateProgressSink for CapturedProgress {
    fn report(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_string());
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this test restores the variable via Drop. Other tests do
        // not depend on this value, and a shorter heartbeat is harmless.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restoring process environment at test teardown; see set().
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.py"), "x = 1\n").unwrap();
    std::fs::write(root.join("docs.md"), "docs\n").unwrap();
    // Gate outputs must be gitignored: the working-tree hash respects
    // .gitignore, so ignored artifacts don't bust the result cache. The
    // .aethyme runtime-state lines mirror the scaffold's gitignore block —
    // without them a run against the MAIN checkout (gates run --all) would
    // bust its own cache by writing broker.db/logs between runs.
    std::fs::write(
        root.join(".gitignore"),
        "gate-markers.txt\nslow-finished.txt\n\
         .aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn write_gates(root: &Path, body: &str) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(root.join(".aethyme/gates.toml"), body).unwrap();
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

#[test]
fn selection_run_cache_and_fail_fast() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cheap-marker"
command = "echo cheap-ran >> gate-markers.txt"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "expensive-fail"
command = "echo expensive-ran >> gate-markers.txt; exit 3"
cost = 2
triggers = ["**/*.py"]

[[gate]]
name = "never-reached"
command = "echo never >> gate-markers.txt"
cost = 3
triggers = ["**/*.py"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Docs-only session: zero gates selected (#16 acceptance).
    let wt_docs = add_worktree(tmp.path(), "docs");
    let docs = broker.adopt(&wt_docs, None).unwrap();
    std::fs::write(wt_docs.join("docs.md"), "changed docs\n").unwrap();
    assert!(broker.affected_gates(docs.id).unwrap().is_empty());
    assert!(broker.run_gates(docs.id).unwrap().is_empty());

    // Python session: cheap-first, fail-fast at the failing gate (#17).
    let wt_py = add_worktree(tmp.path(), "py");
    let py = broker.adopt(&wt_py, None).unwrap();
    std::fs::write(wt_py.join("src/app.py"), "x = 2\n").unwrap();

    let affected = broker.affected_gates(py.id).unwrap();
    assert_eq!(affected.len(), 3);
    assert_eq!(affected[0].1.as_deref(), Some("src/app.py"), "--why works");

    let outcomes = broker.run_gates(py.id).unwrap();
    assert_eq!(outcomes.len(), 2, "fail-fast stops before never-reached");
    assert_eq!(outcomes[0].gate, "cheap-marker");
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    assert!(!outcomes[0].cached);
    assert_eq!(outcomes[1].gate, "expensive-fail");
    assert_eq!(outcomes[1].status, GateStatus::Fail);
    assert_eq!(outcomes[1].exit_code, Some(3));
    let markers = std::fs::read_to_string(wt_py.join("gate-markers.txt")).unwrap();
    assert!(!markers.contains("never"));

    // Same tree, same session, run again: pure cache, no re-execution.
    // (gate-markers.txt is gitignored, so writing it did not change the
    // working-tree hash — the real-world contract for gate outputs.)
    let outcomes = broker.run_gates(py.id).unwrap();
    assert!(outcomes.iter().all(|o| o.cached), "all cache hits");
    let markers_after = std::fs::read_to_string(wt_py.join("gate-markers.txt")).unwrap();
    assert_eq!(markers, markers_after, "cached rerun executed nothing");

    // Cross-agent dedup (#17): a second worktree with IDENTICAL content
    // hashes to the same tree → cache hits without running.
    let wt_py2 = add_worktree(tmp.path(), "py2");
    let py2 = broker.adopt(&wt_py2, None).unwrap();
    std::fs::write(wt_py2.join("src/app.py"), "x = 2\n").unwrap();
    let outcomes = broker.run_gates(py2.id).unwrap();
    assert!(
        outcomes.iter().all(|o| o.cached),
        "identical tree in another session reuses results: {outcomes:?}"
    );
}

#[test]
fn cargo_target_dir_infra_errors_are_not_cached_failures() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cargo-test"
command = "echo cargo-run >> gate-markers.txt; printf '%s\n' 'error: extern location for aethyme_broker does not exist: target/debug/deps/libaethyme_broker.rlib' >&2; exit 101 # cargo test"
triggers = ["**/*.py"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "cargo-infra");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 4\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].gate, "cargo-test");
    assert_eq!(outcomes[0].status, GateStatus::Error);
    assert_eq!(outcomes[0].exit_code, Some(101));
    assert!(!outcomes[0].cached);

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].status,
        GateStatus::Error,
        "the same infra-shaped cargo failure is still an error"
    );
    assert!(
        !outcomes[0].cached,
        "error rows must never satisfy the tree-hash gate cache"
    );

    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers.lines().count(),
        2,
        "a second run re-executes instead of reusing a poisoned fail cache"
    );

    let tree = aethyme_broker::GitRepo::discover(&wt)
        .unwrap()
        .working_tree_hash()
        .unwrap();
    assert!(
        broker
            .store()
            .cached_gate_result("cargo-test", &tree)
            .unwrap()
            .is_none(),
        "cargo target-dir infrastructure errors are non-conclusive"
    );
}

#[test]
fn slow_gate_emits_heartbeat_progress() {
    let _env = EnvGuard::set("AETHYME_GATE_HEARTBEAT_SECS", "1");
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "heartbeat"
command = "sleep 2"
triggers = ["**/*.py"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "heartbeat");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 3\n").unwrap();

    let progress = CapturedProgress::default();
    let outcomes = broker
        .run_gates_with_progress(session.id, &progress)
        .unwrap();

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    let lines = progress.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("gate heartbeat running... ") && line.ends_with('s')),
        "expected heartbeat progress line, got {lines:?}"
    );
}

#[test]
fn resubmit_with_new_tree_cancels_obsolete_slow_run() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "slow"
command = "sleep 30; echo done >> slow-finished.txt"
triggers = ["**/*.py"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "slow");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "v1\n").unwrap();

    // Launch the slow gate run on a second Broker handle (separate SQLite
    // connection — cancellation must work across connections/processes).
    let db_dir = tmp.path().to_path_buf();
    let session_id = session.id;
    let runner = std::thread::spawn(move || {
        let mut broker = Broker::open(&db_dir).unwrap();
        broker.run_gates(session_id)
    });

    // Give the gate time to actually start.
    std::thread::sleep(std::time::Duration::from_millis(800));

    // The agent edits again → new tree → the obsolete run is cancelled
    // (run_gates does this automatically; tested here in isolation so the
    // test doesn't have to sit through the NEW slow gate).
    std::fs::write(wt.join("src/app.py"), "v2\n").unwrap();
    let cancelled = broker.cancel_obsolete_gate_runs(session.id).unwrap();
    assert_eq!(cancelled, vec!["slow".to_string()]);

    let old = runner.join().unwrap().unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(
        old[0].status,
        GateStatus::Cancelled,
        "a killed run is not a verdict on the code — it must never record \
         a conclusive fail: {old:?}"
    );
    assert!(
        !wt.join("slow-finished.txt").exists(),
        "the superseded slow gate never completed"
    );

    // A cancelled row exists for the old tree.
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events.iter().any(|e| e.kind == "gate.cancelled"),
        "cancellation recorded"
    );

    // The poisoning scenario: the agent reverts to the v1 content, so the
    // old tree hash recurs. The killed run's row must NOT satisfy the
    // cache — a cached fail here would reject a perfectly good
    // submission without ever running the gate.
    std::fs::write(wt.join("src/app.py"), "v1\n").unwrap();
    let v1_tree = aethyme_broker::GitRepo::discover(&wt)
        .unwrap()
        .working_tree_hash()
        .unwrap();
    assert!(
        broker
            .store()
            .cached_gate_result("slow", &v1_tree)
            .unwrap()
            .is_none(),
        "killed run must not leave a conclusive cached result for its tree"
    );
}

#[test]
fn run_all_runs_every_gate_in_cost_order_ignoring_diff_selection() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    // Triggers deliberately match NOTHING in the repo: diff selection
    // would pick zero of these gates, --all must still run them all.
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "expensive"
command = "echo expensive >> gate-markers.txt"
cost = 3
triggers = ["**/*.nomatch"]

[[gate]]
name = "cheap"
command = "echo cheap >> gate-markers.txt"
cost = 1
triggers = ["**/*.nomatch"]

[[gate]]
name = "mid"
command = "echo mid >> gate-markers.txt"
cost = 2
triggers = ["**/*.nomatch"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();

    // CI shape: run against the main checkout itself — clean tree, no
    // worktree, no session ever adopted.
    let progress = CapturedProgress::default();
    let outcomes = broker
        .run_all_gates_with_progress(tmp.path(), &progress)
        .unwrap();
    assert_eq!(
        outcomes.iter().map(|o| o.gate.as_str()).collect::<Vec<_>>(),
        vec!["cheap", "mid", "expensive"],
        "every gate runs, cost order, despite zero trigger matches"
    );
    assert!(outcomes.iter().all(|o| o.status == GateStatus::Pass));
    assert!(outcomes.iter().all(|o| !o.cached), "first run executes");
    let markers = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert_eq!(markers, "cheap\nmid\nexpensive\n");

    // The existing streaming surface is reused: started/finished lines.
    let lines = progress.lines();
    assert!(
        lines.iter().any(|l| l.starts_with("gate cheap started")),
        "streaming progress present: {lines:?}"
    );

    // Same tree again: the tree-hash result cache answers, nothing runs.
    let outcomes = broker.run_all_gates(tmp.path()).unwrap();
    assert!(
        outcomes.iter().all(|o| o.cached),
        "second run is pure cache"
    );
    let markers_after = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert_eq!(markers, markers_after, "cached rerun executed nothing");
}

#[test]
fn run_all_fails_fast_on_first_failing_gate() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cheap-pass"
command = "echo cheap >> gate-markers.txt"
cost = 1

[[gate]]
name = "mid-fail"
command = "echo failing >> gate-markers.txt; exit 7"
cost = 2

[[gate]]
name = "never-reached"
command = "echo never >> gate-markers.txt"
cost = 3
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let outcomes = broker.run_all_gates(tmp.path()).unwrap();
    assert_eq!(outcomes.len(), 2, "fail-fast stops before never-reached");
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    assert_eq!(outcomes[1].gate, "mid-fail");
    assert_eq!(outcomes[1].status, GateStatus::Fail);
    assert_eq!(outcomes[1].exit_code, Some(7));
    let markers = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert!(!markers.contains("never"));
}
