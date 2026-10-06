//! In a verify-only repository `aethyme/integration` is a disposable
//! verification base (#352): nothing lands through it, so whenever it carries
//! nothing the fetched default branch lacks and has fallen behind it, `start`,
//! `status`, `submit` and `sync` advance it, and record the move as
//! `broker.integration.refreshed`. An integration holding work of its own is
//! never moved, and promoting repositories keep today's behaviour.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, IntegrationRefreshTrigger};

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

    fn broker(&self) -> Broker {
        Broker::open(&self.root).unwrap()
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
fn start_refreshes_integration_and_cuts_from_the_fetched_default_branch() {
    let fixture = Fixture::new("verify-only", 2);
    let upstream = fixture.upstream_head();
    let mut broker = fixture.broker();

    let session = broker.start_worktree("verify base start", None).unwrap();

    assert_eq!(fixture.integration(), upstream);
    assert_eq!(
        git(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        upstream,
        "a verify-only session starts from the fetched default branch"
    );
    assert_eq!(refresh_triggers(&mut broker), ["start"]);
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
