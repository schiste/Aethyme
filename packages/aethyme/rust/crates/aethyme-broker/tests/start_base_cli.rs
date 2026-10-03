//! Where `broker start` cuts a session from when integration and the fetched
//! default branch disagree.
//!
//! Choosing integration whenever it existed cut sessions 2,101 commits behind
//! upstream in one repository, where integration had stopped moving three days
//! earlier. These tests pin the rules that replaced that: integration is the
//! base only when promotion is on and it contains the fetched default branch.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) {
    git_output(repo, args);
}

fn git_output(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "README.md", ".gitignore"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

fn commit_file(repo: &Path, name: &str, message: &str) -> String {
    std::fs::write(repo.join(name), format!("{message}\n")).unwrap();
    git(repo, &["add", "-f", name]);
    git(repo, &["commit", "-qm", message]);
    git_output(repo, &["rev-parse", "HEAD"])
}

/// Point `origin/main` at `commit` and make `main` track it, without a real
/// remote, so the fetched default branch resolves.
fn track_origin_main(repo: &Path, commit: &str) {
    git(repo, &["update-ref", "refs/remotes/origin/main", commit]);
    git(repo, &["config", "remote.origin.url", "."]);
    git(
        repo,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(repo, &["config", "branch.main.remote", "origin"]);
    git(repo, &["config", "branch.main.merge", "refs/heads/main"]);
}

/// Promote `commits` commits on top of `from` into `aethyme/integration`.
fn promote_onto(repo: &Path, from: &str, commits: usize) -> String {
    git(repo, &["switch", "-qc", "promoted", from]);
    for n in 0..commits {
        commit_file(repo, &format!("promoted{n}.txt"), &format!("promoted {n}"));
    }
    let integration = git_output(repo, &["rev-parse", "HEAD"]);
    git(
        repo,
        &["update-ref", "refs/heads/aethyme/integration", &integration],
    );
    git(repo, &["switch", "-q", "main"]);
    integration
}

fn write_promote_mode(repo: &Path, mode: &str) {
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        format!("schema = 1\n\n[promote]\nmode = \"{mode}\"\n"),
    )
    .unwrap();
}

/// Integration carrying 2 promotions, then `main` moving on by one commit
/// that integration never saw: the frozen-integration shape.
fn integration_behind_upstream(repo: &Path) -> (String, String) {
    let base = git_output(repo, &["rev-parse", "HEAD"]);
    let integration = promote_onto(repo, &base, 2);
    let upstream = commit_file(repo, "upstream.txt", "merged upstream");
    track_origin_main(repo, &upstream);
    (integration, upstream)
}

/// Verify-only never promotes, so integration is never a current base -- even
/// when it contains upstream and carries promotions.
#[test]
fn verify_only_starts_from_the_fetched_default_branch() {
    let tmp = fixture();
    let upstream = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &upstream);
    let integration = promote_onto(tmp.path(), &upstream, 2);
    // A local, never-committed policy: the fallback when the default branch
    // commits none.
    write_promote_mode(tmp.path(), "verify-only");

    let value = json(&run(
        tmp.path(),
        &["start", "--task", "verify only", "--json"],
    ));
    let base = &value["start_base"];
    assert_eq!(base["commit"], upstream, "{value:#}");
    assert_eq!(base["ref_name"], "refs/remotes/origin/main");
    assert_eq!(base["evidence"], "fetched_default_branch");
    assert_eq!(base["bypassed_integration"]["reason"], "verify_only");
    assert_eq!(base["bypassed_integration"]["commit"], integration);
    assert_eq!(base["bypassed_integration"]["ahead_default_commits"], 2);
    assert!(base["bypassed_integration"]["recovery_command"].is_null());
}

/// The Mockup shape: integration stopped moving while upstream advanced. The
/// session starts from upstream, the JSON names what it skipped and how to
/// recover, and `status` keeps saying so.
#[test]
fn a_promoting_repository_skips_an_integration_that_fell_behind() {
    let tmp = fixture();
    let (integration, upstream) = integration_behind_upstream(tmp.path());

    let value = json(&run(tmp.path(), &["start", "--task", "behind", "--json"]));
    let base = &value["start_base"];
    assert_eq!(base["commit"], upstream, "{value:#}");
    assert_eq!(base["evidence"], "fetched_default_branch");
    let bypassed = &base["bypassed_integration"];
    assert_eq!(bypassed["reason"], "behind_upstream");
    assert_eq!(bypassed["commit"], integration);
    assert_eq!(bypassed["behind_default_commits"], 1);
    assert_eq!(bypassed["ahead_default_commits"], 2);
    assert_eq!(
        bypassed["recovery_command"],
        "aethyme broker advanced integration reconcile --upstream origin/main"
    );

    let human = stdout(&run(tmp.path(), &["start", "--task", "behind again"]));
    assert!(
        human.contains(
            "is 1 commit(s) behind and 2 ahead of origin/main; started from origin/main instead"
        ),
        "{human}"
    );
    assert!(
        human.contains("integration reconcile --upstream origin/main"),
        "{human}"
    );

    let status = json(&run(tmp.path(), &["status", "--refresh", "--json"]));
    let advice = status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "integration.behind-upstream")
        .unwrap_or_else(|| panic!("no integration.behind-upstream advice: {status:#}"));
    assert_eq!(advice["severity"], "warning");
}

/// Integration that contains upstream is still the base in a promoting
/// repository: sessions build on verified promotions, as before.
#[test]
fn a_current_integration_is_still_the_base() {
    let tmp = fixture();
    let upstream = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &upstream);
    let integration = promote_onto(tmp.path(), &upstream, 2);

    let value = json(&run(tmp.path(), &["start", "--task", "current", "--json"]));
    let base = &value["start_base"];
    assert_eq!(base["commit"], integration, "{value:#}");
    assert_eq!(base["evidence"], "integration_tip");
    assert!(base["bypassed_integration"].is_null());

    let status = json(&run(tmp.path(), &["status", "--refresh", "--json"]));
    assert!(
        !status["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "integration.behind-upstream"),
        "{status:#}"
    );
}

/// Without integration, a local `main` that has not pulled is not the base;
/// the fetched default branch is.
#[test]
fn without_integration_the_fetched_default_beats_a_stale_local_main() {
    let tmp = fixture();
    let local = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["switch", "-qc", "elsewhere"]);
    let upstream = commit_file(tmp.path(), "upstream.txt", "merged upstream");
    git(tmp.path(), &["switch", "-q", "main"]);
    track_origin_main(tmp.path(), &upstream);
    assert_ne!(local, upstream);

    let value = json(&run(
        tmp.path(),
        &["start", "--task", "stale main", "--json"],
    ));
    assert_eq!(value["start_base"]["commit"], upstream, "{value:#}");
    assert_eq!(value["start_base"]["evidence"], "fetched_default_branch");
}

/// The committed policy on the default branch outranks the working-tree file,
/// the same trust rule the push lane uses.
#[test]
fn promote_mode_is_read_from_the_default_branch_before_the_working_tree() {
    let tmp = fixture();
    write_promote_mode(tmp.path(), "verify-only");
    git(tmp.path(), &["add", "-f", ".aethyme/config.toml"]);
    git(tmp.path(), &["commit", "-qm", "verify-only policy"]);
    let upstream = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &upstream);
    promote_onto(tmp.path(), &upstream, 1);
    // The working tree says auto; the default branch says verify-only.
    write_promote_mode(tmp.path(), "auto");

    let value = json(&run(
        tmp.path(),
        &["start", "--task", "committed wins", "--json"],
    ));
    let base = &value["start_base"];
    assert_eq!(base["commit"], upstream, "{value:#}");
    assert_eq!(base["bypassed_integration"]["reason"], "verify_only");
}

/// A session cut from upstream while integration lagged must not count
/// everything upstream merged since integration last moved as its own
/// changes: those would become leases and overlaps it never touched.
#[test]
fn a_session_started_from_upstream_leases_only_its_own_changes() {
    let tmp = fixture();
    integration_behind_upstream(tmp.path());

    let value = json(&run(
        tmp.path(),
        &["start", "--task", "own changes", "--json"],
    ));
    let worktree = value["worktree_path"].as_str().unwrap().to_string();
    commit_file(Path::new(&worktree), "mine.txt", "my change");

    let status = json(&run(tmp.path(), &["status", "--refresh", "--json"]));
    let paths: Vec<&str> = status["leases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|lease| lease["kind"] == "implicit")
        .map(|lease| lease["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["mine.txt"], "{status:#}");
}
