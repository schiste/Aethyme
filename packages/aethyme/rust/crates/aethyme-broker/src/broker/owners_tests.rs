//! #1238 owner resolution: landed commits back to the sessions behind them.

use std::path::Path;
use std::process::Command;

use super::{landing_reference, split_range};
use crate::broker::Broker;
use crate::types::{NewSession, SessionOrigin, SessionStatus};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git fixture command");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_file(repo: &Path, path: &str, message: &str) -> String {
    let file = repo.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, message).unwrap();
    git(repo, &["add", path]);
    git(repo, &["commit", "-qm", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// Land `branch` on main the way GitHub's merge button does.
fn merge_pull_request(repo: &Path, number: i64, branch: &str) -> String {
    git(repo, &["checkout", "-q", "main"]);
    git(
        repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            branch,
            "-m",
            &format!("Merge pull request #{number} from o/{branch}"),
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

fn register(broker: &mut Broker, branch: &str) -> i64 {
    broker
        .store
        .register_session(&NewSession {
            worktree_path: format!("/nonexistent/{branch}"),
            branch: branch.into(),
            origin: SessionOrigin::Adopted,
            task: Some(format!("work on {branch}")),
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap()
        .id
}

struct Fixture {
    _temp: tempfile::TempDir,
    broker: Broker,
    base: String,
    migration_via_merge: String,
    source_via_merge: String,
    migration_via_squash: String,
    migration_direct: String,
    live: i64,
    closed: i64,
    squashed: i64,
}

/// Main gains, after `base`:
/// - PR #11 (branch `agent/a`, live session) touching `db/migrations/`;
/// - PR #12 (branch `agent/b`, closed session) touching `src/` only;
/// - a squash commit `(#13)` touching `db/migrations/`, linked to a session
///   only through the recorded pull request link;
/// - a direct commit touching `db/migrations/` that no session produced.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "initial"]);
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/n.git"],
    );
    let base = git(&repo, &["rev-parse", "HEAD"]);

    git(&repo, &["checkout", "-q", "-b", "agent/a"]);
    let migration_via_merge = commit_file(&repo, "db/migrations/001.sql", "add table");
    merge_pull_request(&repo, 11, "agent/a");

    git(&repo, &["checkout", "-q", "-b", "agent/b"]);
    let source_via_merge = commit_file(&repo, "src/lib.rs", "touch source");
    merge_pull_request(&repo, 12, "agent/b");

    let migration_via_squash = commit_file(&repo, "db/migrations/002.sql", "alter table (#13)");
    let migration_direct = commit_file(&repo, "db/migrations/003.sql", "hotfix");

    let host_state = temp.path().join("host-state");
    let options = crate::BrokerOpenOptions::default()
        .with_host_state_dir(&host_state)
        .with_host_cache_dir(temp.path().join("host-cache"))
        .with_worktree_root(temp.path().join("worktrees"))
        .with_collaboration_state(host_state.clone())
        .with_host_operation_database(host_state.join("host-operations.db"));
    let mut broker = Broker::open_with_options(&repo, options).unwrap();
    let live = register(&mut broker, "agent/a");
    let closed = register(&mut broker, "agent/b");
    broker
        .store
        .set_session_status(closed, SessionStatus::Closed, None)
        .unwrap();
    let squashed = register(&mut broker, "agent/squashed");
    broker
        .store
        .connection_mut()
        .execute(
            "INSERT INTO pull_request_session_links
                 (repository, pr_number, session_id, linked_at, link_source)
             VALUES ('o/n', 13, ?1, 0, 'watch')",
            [squashed],
        )
        .unwrap();
    Fixture {
        _temp: temp,
        broker,
        base,
        migration_via_merge,
        source_via_merge,
        migration_via_squash,
        migration_direct,
        live,
        closed,
        squashed,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// #1238 acceptance: narrowing to the migration paths names the session
/// that merged the migration (through its merge subject's branch) and the
/// squash's session (through its pull request link), says which are still
/// alive, and lists the commit no session produced.
#[test]
fn paths_narrow_owners_to_the_sessions_that_touched_them() {
    let f = fixture();
    let report = f
        .broker
        .owners(
            &format!("{}..main", f.base),
            &["db/migrations/**".to_string()],
            now(),
        )
        .unwrap();
    assert_eq!(report.repository.as_deref(), Some("o/n"));
    let shas: Vec<&str> = report.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(
        shas,
        vec![
            f.migration_via_merge.as_str(),
            f.migration_via_squash.as_str(),
            f.migration_direct.as_str()
        ]
    );
    let merged = &report.commits[0];
    assert_eq!(
        (merged.pull_request, merged.branch.as_deref()),
        (Some(11), Some("agent/a"))
    );
    assert_ne!(
        merged.landing, merged.sha,
        "a merged commit lands by its merge"
    );
    assert_eq!(report.commits[1].landing, f.migration_via_squash);

    let owners: Vec<(i64, bool, Option<i64>)> = report
        .owners
        .iter()
        .map(|o| (o.session_id, o.live, o.pull_request))
        .collect();
    assert_eq!(
        owners,
        vec![(f.live, true, Some(11)), (f.squashed, true, Some(13))]
    );
    assert_eq!(
        report.owners[0].commits,
        vec![f.migration_via_merge.clone()]
    );
    assert_eq!(report.unowned, vec![f.migration_direct.clone()]);
    assert!(!report.truncated);
}

/// Without paths every commit counts, and a finished session still owns its
/// work but is not live.
#[test]
fn a_finished_session_owns_its_commits_but_is_not_live() {
    let f = fixture();
    let report = f
        .broker
        .owners(&format!("{}..main", f.base), &[], now())
        .unwrap();
    let closed = report
        .owners
        .iter()
        .find(|o| o.session_id == f.closed)
        .expect("the closed session owns PR #12's commit");
    assert!(!closed.live);
    assert_eq!(closed.status, SessionStatus::Closed);
    // Without paths the merge commit is in range too, and lands itself.
    let merge = f
        .broker
        .repo
        .landing_commit(&f.source_via_merge, "main")
        .unwrap();
    assert_eq!(closed.commits, vec![f.source_via_merge.clone(), merge]);
}

/// A single commit reads as the commits it brought in: a merge names its
/// pull request's work and nothing before it.
#[test]
fn a_single_merge_commit_resolves_what_it_landed() {
    let f = fixture();
    let merge = f
        .broker
        .repo
        .landing_commit(&f.source_via_merge, "main")
        .unwrap();
    let report = f.broker.owners(&merge, &[], now()).unwrap();
    let shas: Vec<&str> = report.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, vec![f.source_via_merge.as_str(), merge.as_str()]);
    assert!(report.owners.iter().all(|o| o.session_id == f.closed));
}

#[test]
fn malformed_requests_are_refused() {
    let f = fixture();
    for (range, paths) in [
        ("main...HEAD", vec![]),
        ("..main", vec![]),
        ("--all..main", vec![]),
        ("no-such-ref..main", vec![]),
        ("main", vec!["/etc/**".to_string()]),
    ] {
        let error = f.broker.owners(range, &paths, now()).unwrap_err();
        assert!(
            matches!(error, crate::BrokerOpError::InvalidOwnersRequest { .. }),
            "{range} {paths:?}: {error}"
        );
    }
}

#[test]
fn landing_subjects_name_pull_requests_and_branches() {
    assert_eq!(
        landing_reference("Merge pull request #1341 from schiste/agent/x-y"),
        (Some(1341), Some("agent/x-y".to_string()))
    );
    assert_eq!(
        landing_reference("feat(broker): thing (#77)"),
        (Some(77), None)
    );
    assert_eq!(
        landing_reference("Merge branch 'main' into x"),
        (None, None)
    );
    assert_eq!(landing_reference("fix: (#not-a-number)"), (None, None));
    assert_eq!(landing_reference("Merge pull request #9"), (Some(9), None));
}

#[test]
fn ranges_split_into_endpoints() {
    assert_eq!(split_range("a..b").unwrap(), ("a".into(), "b".into()));
    assert_eq!(split_range("a..").unwrap(), ("a".into(), "HEAD".into()));
    assert_eq!(split_range("abc").unwrap(), ("abc^".into(), "abc".into()));
    assert!(split_range("").is_err());
    assert!(split_range("a...b").is_err());
    assert!(split_range("a..-x").is_err());
}
