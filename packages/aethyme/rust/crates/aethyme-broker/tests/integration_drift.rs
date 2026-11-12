//! Integration drift is reported and repaired by its actual shape (#290 phase 3).
//!
//! Two states were collapsed into one refusal: integration *diverged* from the
//! tracked upstream, which needs a reviewed reconciliation, and integration
//! merely *behind* it, which is a fast-forward that discards nothing. Treating
//! the second as the first meant every pull-request merge left the gap in
//! place, and every session started afterwards inherited it.
//!
//! Local promotion was removed; these cases cover the integration ref a
//! repository may still carry from before, and the read-only doctor report.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{AutomaticIntegrationCleanupState, Broker, StatusAdviceSeverity};

#[path = "common/isolation.rs"]
mod isolation;

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

/// A repository whose `main` tracks `origin/main`, with integration pinned at
/// the commit `main` started from, then `origin/main` advanced by `ahead`
/// commits. Integration is therefore a strict ancestor of upstream.
fn fixture(ahead: usize) -> (isolation::FixtureRepo, Broker) {
    let tmp = isolation::FixtureRepo::new();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let base = git(root, &["rev-parse", "HEAD"]);

    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &base],
    );
    // The upstream commits must NOT be on local `main`. `integration_head`
    // already fast-forwards integration onto the main checkout's HEAD when it
    // is an ancestor of it (#40), so advancing local main here would mask the
    // state under test -- integration behind the *tracked upstream* while the
    // local branch is behind it too, which is what a pull-request merge leaves
    // behind and what #290 phase 3.1 is about.
    git(root, &["switch", "-qc", "upstream-side"]);
    for n in 0..ahead {
        std::fs::write(root.join(format!("upstream{n}.txt")), "upstream\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", &format!("upstream {n}")]);
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["switch", "-q", "main"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    git(root, &["config", "remote.origin.url", "."]);
    git(
        root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(root, &["config", "branch.main.remote", "origin"]);
    git(root, &["config", "branch.main.merge", "refs/heads/main"]);

    let broker = isolation::open(root);
    (tmp, broker)
}

/// Phase 3.1. Nothing was promoted, so the reconciliation plan is empty — the
/// case the previous guard required to be non-empty and therefore refused.
#[test]
fn automatic_cleanup_fast_forwards_when_integration_carries_nothing_of_its_own() {
    let (tmp, mut broker) = fixture(3);
    let upstream = git(tmp.path(), &["rev-parse", "refs/remotes/origin/main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();

    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Cleaned,
        "{}",
        report.explanation
    );
    assert_eq!(report.new_integration, upstream);
    assert!(
        report.explanation.contains("fast-forwarded"),
        "{}",
        report.explanation
    );
    assert_eq!(
        git(tmp.path(), &["rev-parse", "refs/heads/aethyme/integration"]),
        upstream
    );
}

/// The guard that matters: integration holding work upstream lacks is
/// divergence, and must still refuse rather than fast-forward over it.
#[test]
fn divergence_is_still_refused() {
    let (tmp, mut broker) = fixture(2);
    let root = tmp.path();

    // Put a commit on integration that upstream does not have.
    let integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    git(root, &["switch", "-qc", "tmp-integration", &integration]);
    std::fs::write(root.join("only-on-integration.txt"), "mine\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "unique to integration"]);
    let diverged = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &diverged],
    );
    git(root, &["switch", "-q", "main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();
    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Deferred,
        "{}",
        report.explanation
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        diverged,
        "a diverged integration ref must not move"
    );
}

/// Put `count` commits on integration that `origin/main` lacks, the shape a
/// repository leaves behind when it promoted once and later switched to
/// verify-only (the SP42 case: six-week-old drafts superseded by merged PRs).
fn add_leftover_integration_work(root: &Path, count: usize) -> String {
    let integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    git(root, &["switch", "-qc", "leftover", &integration]);
    for n in 0..count {
        std::fs::write(root.join(format!("leftover{n}.txt")), "draft\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", &format!("leftover {n}")]);
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &head],
    );
    git(root, &["switch", "-q", "main"]);
    head
}

fn set_promote_mode(root: &Path, mode: &str) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        format!("[promote]\nmode = \"{mode}\"\n"),
    )
    .unwrap();
}

/// A verify-only repository does not use integration, so the routine status
/// every session runs first no longer asks agents to reconcile leftover work
/// on it (it used to, with an `integration reconcile` command); it compares
/// against the fetched default branch, and `doctor` still reports the
/// leftover commits.
#[test]
fn verify_only_status_does_not_report_leftover_integration_work() {
    let (tmp, _broker) = fixture(1);
    let root = tmp.path();
    let leftover = add_leftover_integration_work(root, 2);
    set_promote_mode(root, "verify-only");
    let mut broker = isolation::open(root);

    let status = broker.status_current(0).unwrap();

    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.id == "integration.leftover-work"),
        "{:?}",
        status.advice
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        leftover,
        "the leftover work is left where it is"
    );
}

/// Integration behind the published branch carries nothing of its own; it is
/// the normal verify-only state and must stay silent.
#[test]
fn verify_only_status_is_silent_when_integration_holds_nothing_of_its_own() {
    let (tmp, _broker) = fixture(2);
    set_promote_mode(tmp.path(), "verify-only");
    let mut broker = isolation::open(tmp.path());

    let status = broker.status_current(0).unwrap();
    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.id == "integration.leftover-work"),
        "{:?}",
        status.advice
    );
}

fn doctor_cli(root: &Path, args: &[&str]) -> String {
    let mut arguments = vec!["status", "doctor"];
    arguments.extend_from_slice(args);
    let output = isolation::command(env!("CARGO_BIN_EXE_broker-cli-shim"), root, &arguments)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository health check must find the leftover work `status` reports,
/// with the same dry-run next action, and leave every ref where it was.
#[test]
fn verify_only_doctor_reports_leftover_integration_work() {
    let (tmp, mut setup_broker) = fixture(1);
    let root = tmp.path();
    // A fresh isolated host has no worktree-history registry. Establish it
    // through the real lifecycle so doctor can complete its orphan inventory;
    // missing history must continue to report incomplete health in production.
    let fixture_upstream = git(root, &["rev-parse", "refs/remotes/origin/main"]);
    let fixture_integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    let remote_owner = tempfile::tempdir().unwrap();
    let remote = remote_owner.path().join("origin.git");
    git(
        root,
        &["clone", "--bare", "-q", ".", remote.to_str().unwrap()],
    );
    git(
        &remote,
        &["update-ref", "refs/heads/main", &fixture_upstream],
    );
    git(
        root,
        &["remote", "set-url", "origin", remote.to_str().unwrap()],
    );
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let seed = setup_broker
        .start_worktree(
            "initialize fixture worktree history",
            Some("Fixture <fixture@example.invalid>"),
        )
        .unwrap();
    let finished = setup_broker.finish(seed.id).unwrap();
    assert!(finished.closed && finished.cleanup_safe, "{finished:?}");
    assert!(finished.dirty_paths.is_empty(), "{finished:?}");
    assert_eq!(finished.unsubmitted_commits, 0, "{finished:?}");
    assert!(!finished.pending_work.present, "{finished:?}");
    assert!(finished.cleanup.completed, "{finished:?}");
    assert!(finished.cleanup.worktree_removed, "{finished:?}");
    assert!(finished.cleanup.branch_removed, "{finished:?}");
    assert!(!std::path::Path::new(&seed.worktree_path).exists());
    // Restore the scenario's integration tip after lifecycle initialization,
    // before constructing leftover commits and taking read-only snapshots.
    git(
        root,
        &["update-ref", "refs/remotes/origin/main", &fixture_upstream],
    );
    git(
        root,
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            &fixture_integration,
        ],
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/main"]),
        fixture_upstream
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        fixture_integration
    );
    drop(setup_broker);
    let leftover = add_leftover_integration_work(root, 2);
    set_promote_mode(root, "verify-only");
    let upstream = git(root, &["rev-parse", "refs/remotes/origin/main"]);
    let mut broker = isolation::open(root);

    let report = broker.doctor().unwrap();
    let advice = report
        .leftover_integration_work
        .as_ref()
        .expect("doctor must report leftover integration work");
    assert_eq!(advice.id, "integration.leftover-work");
    assert_eq!(advice.severity, StatusAdviceSeverity::Warning);
    assert!(
        advice.summary.contains("2 commits origin/main lacks"),
        "{}",
        advice.summary
    );
    let command = "aethyme broker advanced integration reconcile --upstream origin/main --dry-run";
    assert_eq!(advice.commands, vec![command]);
    assert!(
        report.healthy(),
        "leftover work is reported, not unhealthy: {report:?}"
    );
    drop(broker);

    let json: serde_json::Value =
        serde_json::from_str(&doctor_cli(root, &["--json"])).expect("doctor --json");
    assert_eq!(
        json["leftover_integration_work"]["commands"][0], command,
        "{json}"
    );
    let text = doctor_cli(root, &[]);
    assert!(
        text.contains("integration leftover work: integration carries 2 commits"),
        "{text}"
    );
    assert!(text.contains(&format!("  run: {command}")), "{text}");

    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        leftover,
        "doctor must not move integration"
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/main"]),
        upstream,
        "doctor must not move the published branch"
    );
}

/// Integration behind the published branch holds nothing of its own, and the
/// JSON key is omitted rather than reported empty.
#[test]
fn verify_only_doctor_is_silent_when_integration_holds_nothing_of_its_own() {
    let (tmp, _broker) = fixture(2);
    set_promote_mode(tmp.path(), "verify-only");
    let mut broker = isolation::open(tmp.path());

    assert!(broker.doctor().unwrap().leftover_integration_work.is_none());
    drop(broker);
    let json: serde_json::Value =
        serde_json::from_str(&doctor_cli(tmp.path(), &["--json"])).expect("doctor --json");
    assert!(json.get("leftover_integration_work").is_none(), "{json}");
}

/// Trace real doctor phases without a wall-clock race or a deliberately slow Git.
#[test]
fn doctor_inspects_version_before_retention_inventory() {
    const CHILD: &str = "AETHYME_DOCTOR_VERSION_ORDER_CHILD";
    const NAME: &str = "doctor_inspects_version_before_retention_inventory";
    if std::env::var_os(CHILD).is_some() {
        let (_owner, mut broker) = fixture(0);
        let session = broker.start_worktree("doctor ordering", None).unwrap();
        broker.close(session.id).unwrap();
        let trace = std::path::PathBuf::from(
            std::env::var_os("GIT_TRACE2_EVENT").expect("parent supplies private trace"),
        );
        std::fs::write(&trace, "").unwrap();
        let report = broker.doctor().unwrap();
        assert_eq!(report.integrity, "ok");
        let events = std::fs::read_to_string(&trace).unwrap();
        let starts = events
            .lines()
            .filter_map(|line| {
                let event: serde_json::Value =
                    serde_json::from_str(line).expect("valid Git Trace2 JSON");
                if event["event"] != "start" {
                    return None;
                }
                Some(event["argv"].as_array().expect("start has argv").clone())
            })
            .collect::<Vec<_>>();
        let config_tree_reads = starts
            .iter()
            .filter(|argv| {
                argv.iter().any(|arg| arg == "ls-tree")
                    && argv
                        .iter()
                        .any(|arg| arg.as_str().is_some_and(|arg| arg.ends_with("config.toml")))
            })
            .collect::<Vec<_>>();
        assert!(
            !config_tree_reads.is_empty(),
            "doctor must inspect committed configuration before reporting version: {starts:?}"
        );
        for argv in &config_tree_reads {
            for extension in ["toml", "yaml", "yml"] {
                let expected = format!(":(literal).aethyme/config.{extension}");
                assert!(
                    argv.iter()
                        .any(|arg| arg.as_str() == Some(expected.as_str())),
                    "each checked tree read must batch the {expected} candidate: {starts:?}"
                );
            }
        }
        let version = starts
            .iter()
            .position(|argv| argv.iter().any(|arg| arg == "describe"))
            .expect("version inspection describes integration");
        let inventory = starts
            .iter()
            .position(|argv| {
                argv.iter().any(|arg| arg == "for-each-ref")
                    && argv.iter().any(|arg| arg == "refs/heads/")
            })
            .expect("retention inventories local branch tips");
        assert!(
            version < inventory,
            "doctor must inspect version before retention inventory can spend its Git deadline; starts={starts:?}"
        );
        return;
    }
    let owner = tempfile::tempdir().unwrap();
    let trace = owner
        .path()
        .canonicalize()
        .unwrap()
        .join("doctor-trace.jsonl");
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, "1")
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "doctor phase child failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "child must execute exactly the selected regression"
    );
}
