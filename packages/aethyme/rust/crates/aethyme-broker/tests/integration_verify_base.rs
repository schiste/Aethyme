//! In a verify-only repository `aethyme/integration` is a disposable
//! verification base (#352): nothing lands through it, so whenever it carries
//! nothing the fetched default branch lacks and has fallen behind it,
//! `status`, `submit` and `sync` advance it, and record the move as
//! `broker.integration.refreshed`. An integration holding work of its own is
//! never moved, and promoting repositories keep today's behaviour. Startup
//! selects the fresh default without reconciling disposable integration.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, IntegrationRefreshTrigger};

mod common;

const INTEGRATION: &str = "refs/heads/aethyme/integration";

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

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    /// A clone of the remote, standing in for whoever merges pull requests.
    upstream: PathBuf,
}

impl Fixture {
    /// A repository whose `main` tracks a real `origin/main` that is `ahead`
    /// commits past the local checkout, with integration still at the commit
    /// both started from: what a pull request merged on the provider leaves.
    fn new(mode: &str, ahead: usize) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "init"]);
        let base = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["update-ref", INTEGRATION, &base]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n"),
        )
        .unwrap();

        git(
            tmp.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&root, &["push", "-q", "origin", "main"]);
        git(&root, &["branch", "--set-upstream-to=origin/main", "main"]);
        let upstream = tmp.path().join("upstream");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                upstream.to_str().unwrap(),
            ],
        );
        let fixture = Self {
            _tmp: tmp,
            root,
            upstream,
        };
        fixture.merge_upstream(ahead);
        fixture
    }

    /// Land `count` commits on the remote's `main` and fetch them, leaving
    /// the local checkout where it was.
    fn merge_upstream(&self, count: usize) -> String {
        for _ in 0..count {
            let n = git(&self.upstream, &["rev-list", "--count", "HEAD"]);
            std::fs::write(self.upstream.join(format!("merged{n}.txt")), "merged\n").unwrap();
            git(&self.upstream, &["add", "-A"]);
            git(
                &self.upstream,
                &["commit", "-qm", &format!("merged pull request {n}")],
            );
        }
        git(&self.upstream, &["push", "-q", "origin", "main"]);
        git(&self.root, &["fetch", "-q", "origin"]);
        self.upstream_head()
    }

    fn upstream_head(&self) -> String {
        git(&self.root, &["rev-parse", "refs/remotes/origin/main"])
    }

    fn integration(&self) -> String {
        git(&self.root, &["rev-parse", INTEGRATION])
    }

    fn broker(&self) -> common::FixtureBroker {
        common::open_fixture_broker(&self.root, self._tmp.path()).unwrap()
    }
}

fn refresh_triggers(broker: &mut Broker) -> Vec<String> {
    broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "broker.integration.refreshed")
        .map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(event.payload_json.as_deref().unwrap_or("{}")).unwrap();
            payload["trigger"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

#[test]
fn status_advances_a_verify_only_integration_that_is_strictly_behind() {
    let fixture = Fixture::new("verify-only", 3);
    let upstream = fixture.upstream_head();
    let mut broker = fixture.broker();

    let status = broker.status_current(0).unwrap();

    assert_eq!(fixture.integration(), upstream);
    let row = status
        .advice
        .iter()
        .find(|row| row.id == "integration.refreshed")
        .expect("the refresh is reported in status");
    assert!(row.summary.contains("fast-forwarded"), "{}", row.summary);
    assert_eq!(refresh_triggers(&mut broker), ["status"]);

    // Already current: nothing to do, nothing recorded twice.
    let again = broker.status_current(0).unwrap();
    assert!(
        !again
            .advice
            .iter()
            .any(|row| row.id == "integration.refreshed")
    );
    assert_eq!(refresh_triggers(&mut broker), ["status"]);
}

#[test]
fn a_promoting_repository_keeps_its_integration_and_the_notice() {
    let fixture = Fixture::new("auto", 2);
    let before = fixture.integration();
    let mut broker = fixture.broker();

    // The refreshing view, the only one that assesses drift and names it.
    let status = broker.status(0).unwrap();

    assert_eq!(fixture.integration(), before, "promote mode is unchanged");
    assert!(
        status
            .advice
            .iter()
            .any(|row| row.id == "integration.fast-forward-available"),
        "{:?}",
        status.advice
    );
    assert!(refresh_triggers(&mut broker).is_empty());
}

#[test]
fn an_integration_holding_work_of_its_own_is_never_reset() {
    let fixture = Fixture::new("verify-only", 2);
    let root = &fixture.root;
    git(
        root,
        &["switch", "-qc", "on-integration", &fixture.integration()],
    );
    std::fs::write(root.join("only-on-integration.txt"), "mine\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "unique to integration"]);
    let diverged = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", INTEGRATION, &diverged]);
    git(root, &["switch", "-q", "main"]);
    let mut broker = fixture.broker();

    let status = broker.status_current(0).unwrap();
    for trigger in [
        IntegrationRefreshTrigger::Start,
        IntegrationRefreshTrigger::Submit,
        IntegrationRefreshTrigger::Sync,
    ] {
        assert_eq!(broker.refresh_disposable_integration(trigger), None);
    }

    assert_eq!(
        fixture.integration(),
        diverged,
        "unique work is never moved"
    );
    assert!(
        status
            .advice
            .iter()
            .any(|row| row.id == "integration.leftover-work"),
        "the reviewed path is still named: {:?}",
        status.advice
    );
    assert!(refresh_triggers(&mut broker).is_empty());
}

#[test]
fn start_preserves_integration_and_cuts_from_the_fetched_default_branch() {
    let fixture = Fixture::new("verify-only", 2);
    let upstream = fixture.upstream_head();
    let integration = fixture.integration();
    assert_ne!(integration, upstream);
    let mut broker = fixture.broker();

    let session = broker.start_worktree("verify base start", None).unwrap();

    assert_eq!(
        fixture.integration(),
        integration,
        "start must not reconcile integration"
    );
    assert_eq!(
        git(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        upstream,
        "a verify-only session starts from the fetched default branch"
    );
    assert!(refresh_triggers(&mut broker).is_empty());
}

#[test]
fn start_does_not_inspect_files_in_a_thousand_landed_promotions() {
    use std::io::Write;
    use std::process::Stdio;

    let fixture = Fixture::new("verify-only", 0);
    let base = fixture.integration();
    let mut broker = fixture.broker();
    let owner = broker
        .start_worktree("historical promotion owner", None)
        .unwrap();
    // Import real commits in one Git process: fixture construction should not
    // reproduce the per-entry process overhead this regression guards against.
    let mut history = Vec::new();
    for n in 0..1024 {
        let message = format!("historical promotion {n}\n");
        write!(
            history,
            "commit refs/heads/fixture-history\ncommitter t <t@t> 1700000000 +0000\ndata {}\n{}",
            message.len(),
            message
        )
        .unwrap();
        if n == 0 {
            writeln!(history, "from {base}").unwrap();
        }
        writeln!(history).unwrap();
    }
    let mut importer = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&fixture.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    importer.stdin.take().unwrap().write_all(&history).unwrap();
    let imported = importer.wait_with_output().unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let commits = git(
        &fixture.root,
        &["rev-list", "--reverse", &format!("{base}..fixture-history")],
    );
    let commits: Vec<_> = commits.lines().collect();
    assert_eq!(commits.len(), 1024);
    let integration = *commits.last().unwrap();
    git(&fixture.root, &["update-ref", INTEGRATION, integration]);
    git(
        &fixture.root,
        &["push", "-q", "origin", "fixture-history:main"],
    );
    git(&fixture.upstream, &["pull", "-q", "--ff-only"]);
    let upstream = fixture.merge_upstream(1);

    // Keep the owner checkout and its accepted history consistent too.
    git(
        Path::new(&owner.worktree_path),
        &["reset", "--hard", integration],
    );
    let tree = git(&fixture.root, &["rev-parse", "fixture-history^{tree}"]);
    let mut parent = base;
    for commit in &commits {
        let entry = broker.store().submit(owner.id, commit, &parent).unwrap();
        broker
            .store()
            .set_merge_status(
                entry.id,
                aethyme_broker::MergeStatus::Verified,
                Some(&tree),
                None,
            )
            .unwrap();
        broker
            .store()
            .record_merge_promotion(
                entry.id,
                commit,
                INTEGRATION,
                &[],
                &serde_json::json!({"commit": commit}).to_string(),
            )
            .unwrap();
        parent = (*commit).to_string();
    }
    let before = broker.store().merge_queue().unwrap();
    assert_eq!(before.len(), 1024);
    let trace = fixture._tmp.path().join("start-trace.json");
    let mut command = common::broker_cli_for_fixture(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["start", "--task", "start after landed history", "--json"],
        &fixture.root,
    );
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(&fixture.root)
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(session["start_base"]["commit"], upstream);
    assert_eq!(
        fixture.integration(),
        integration,
        "allocation must not reconcile history"
    );
    let after = broker.store().merge_queue().unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap(),
        "startup must preserve historical queue authority"
    );
    let events = std::fs::read_to_string(trace).unwrap();
    let starts: Vec<serde_json::Value> = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"] == "start")
        .collect();
    assert!(!starts.is_empty(), "Git tracing must be active");
    for event in starts {
        let args = event["argv"].as_array().unwrap();
        let reads_promotion = args.iter().any(|arg| arg == "diff")
            && args
                .iter()
                .any(|arg| arg.as_str().is_some_and(|value| commits.contains(&value)));
        assert!(
            !reads_promotion,
            "startup read historical promotion paths: {event}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), Some("diff-tree" | "patch-id"))),
            "startup inspected historical promotion contents: {event}"
        );
    }
    assert!(refresh_triggers(&mut broker).is_empty());
}

#[cfg(unix)]
#[test]
fn start_completes_while_an_unrelated_coordinated_commit_waits_in_its_hook() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let fixture = Fixture::new("verify-only", 0);
    let mut broker = fixture.broker();
    let writer = broker.start_worktree("commit hook owner", None).unwrap();
    let hooks = fixture._tmp.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let ready = hooks.join("ready");
    let release = hooks.join("release");
    let hook = hooks.join("pre-commit");
    std::fs::write(
        &hook,
        r#"#!/bin/sh
hook_root=$(dirname "$0")
: > "$hook_root/ready"
n=0
while [ ! -f "$hook_root/release" ]; do
    n=$((n+1))
    [ "$n" -lt 600 ] || exit 1
    sleep 0.1
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    git(&fixture.root, &["config", "user.name", "t"]);
    git(&fixture.root, &["config", "user.email", "t@t"]);

    // Always release the hook on assertion failure. Its own bounded wait also
    // prevents a failed test from leaving an unbounded provider child behind.
    struct ReleaseHook(PathBuf);
    impl Drop for ReleaseHook {
        fn drop(&mut self) {
            std::fs::write(&self.0, b"release").expect("release fixture commit hook");
        }
    }
    let release_hook = ReleaseHook(release.clone());
    let root = fixture.root.clone();
    let owner = fixture._tmp.path().to_path_buf();
    let commit = std::thread::spawn(move || {
        let mut broker = common::open_fixture_broker(&root, &owner).unwrap();
        broker.run_coordinated_operation(aethyme_broker::CoordinatedCommand {
            session_id: writer.id,
            provider: aethyme_broker::OperationProvider::Git,
            repository: None,
            resolved_target: None,
            scope: None,
            declared_effect: None,
            destructive_confirmed: false,
            cross_session: None,
            ref_write_acknowledged: false,
            authorization_reason: Some("startup concurrency regression".into()),
            args: vec![
                "commit".into(),
                "--allow-empty".into(),
                "-m".into(),
                "hook-held commit".into(),
            ],
        })
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() || commit.is_finished() {
        drop(release_hook);
        let outcome = commit.join().unwrap();
        panic!("coordinated commit did not stay in its hook: {outcome:?}");
    }

    let (sender, receiver) = mpsc::channel();
    let start = std::thread::spawn(move || {
        sender
            .send(broker.start_worktree("scout while commit hook waits", None))
            .unwrap();
    });
    let observed = receiver.recv_timeout(Duration::from_secs(20));
    let completed_before_release = observed.is_ok();
    drop(release_hook);
    start.join().unwrap();
    assert!(
        commit.join().unwrap().unwrap().ok(),
        "coordinated commit failed"
    );
    assert!(
        completed_before_release,
        "native startup waited behind an unrelated commit hook"
    );
    let session = observed.unwrap().unwrap();
    assert!(Path::new(&session.worktree_path).is_dir());
    assert_eq!(
        git(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        fixture.upstream_head()
    );
}

#[test]
fn submit_refreshes_integration_before_planning() {
    let fixture = Fixture::new("verify-only", 1);
    let mut broker = fixture.broker();
    let session = broker.start_worktree("verify base submit", None).unwrap();
    let upstream = fixture.merge_upstream(2);
    assert_ne!(
        fixture.integration(),
        upstream,
        "a merge left it behind again"
    );

    // Whatever the gates conclude, the refresh happens before planning.
    let _outcome = broker.submit(session.id);

    assert_eq!(fixture.integration(), upstream);
    assert_eq!(
        refresh_triggers(&mut broker).last().map(String::as_str),
        Some("submit")
    );
}

#[test]
fn sync_refreshes_integration_after_its_fetch() {
    let fixture = Fixture::new("verify-only", 1);
    let mut broker = fixture.broker();
    let session = broker.start_worktree("verify base sync", None).unwrap();
    let upstream = fixture.merge_upstream(1);
    assert_ne!(fixture.integration(), upstream);

    let _report = broker.sync_session(session.id);

    assert_eq!(fixture.integration(), upstream);
    assert_eq!(
        refresh_triggers(&mut broker).last().map(String::as_str),
        Some("sync")
    );
}

/// Proposal G: a session whose work merged upstream has nothing pending,
/// whatever integration's state, so `finish` never calls main's commits the
/// session's unsubmitted work.
#[test]
fn finish_counts_pending_work_against_upstream_not_a_stale_integration() {
    let fixture = Fixture::new("verify-only", 1);
    let mut broker = fixture.broker();
    let session = broker.start_worktree("verify base finish", None).unwrap();
    let worktree = Path::new(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "session work\n").unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-qm", "session work"]);
    let head = git(worktree, &["rev-parse", "HEAD"]);
    // The pull request merges as a fast-forward, then main moves further.
    git(
        &fixture.upstream,
        &["fetch", "-q", worktree.to_str().unwrap(), &head],
    );
    git(&fixture.upstream, &["merge", "-q", "--ff-only", &head]);
    let upstream = fixture.merge_upstream(3);
    // Integration is pinned behind on purpose: this is the counting rule,
    // not the refresh.
    let stale = fixture.integration();
    assert_ne!(stale, upstream);

    let report = broker.finish(session.id).unwrap();

    assert_eq!(report.unsubmitted_commits, 0, "{report:?}");
}
