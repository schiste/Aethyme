use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, BrokerOpenOptions};

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
    common::broker_cli_for_fixture(CLI, args, repo)
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

fn fixture() -> common::FixtureRepository {
    let tmp = common::FixtureRepository::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "README.md", ".gitignore"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

fn stored_session(fixture: &common::FixtureRepository, id: i64) -> aethyme_broker::Session {
    common::open_fixture_repository_broker(fixture)
        .unwrap()
        .store()
        .session(id)
        .unwrap()
}

fn stored_base_observation(
    fixture: &common::FixtureRepository,
    session_id: i64,
) -> serde_json::Value {
    let mut broker = common::open_fixture_repository_broker(fixture).unwrap();
    let database = broker.store().db_path().to_path_buf();
    drop(broker);
    let connection = rusqlite::Connection::open(database).unwrap();
    let key = format!("session_base_observation.{session_id}");
    let raw: String = connection
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn delete_stored_base_observation(fixture: &common::FixtureRepository, session_id: i64) {
    let mut broker = common::open_fixture_repository_broker(fixture).unwrap();
    let database = broker.store().db_path().to_path_buf();
    drop(broker);
    let connection = rusqlite::Connection::open(database).unwrap();
    let key = format!("session_base_observation.{session_id}");
    connection
        .execute("DELETE FROM meta WHERE key = ?1", [key])
        .unwrap();
}

fn overwrite_stored_base_observation(
    fixture: &common::FixtureRepository,
    session_id: i64,
    raw: &str,
) {
    let mut broker = common::open_fixture_repository_broker(fixture).unwrap();
    let database = broker.store().db_path().to_path_buf();
    drop(broker);
    let connection = rusqlite::Connection::open(database).unwrap();
    let key = format!("session_base_observation.{session_id}");
    let changed = connection
        .execute("UPDATE meta SET value = ?1 WHERE key = ?2", (raw, key))
        .unwrap();
    assert_eq!(
        changed, 1,
        "the malformed-observation control must replace an existing row"
    );
}

fn set_promote_branch(repo: &Path, branch: &str) {
    let aethyme = repo.join(".aethyme");
    std::fs::create_dir_all(&aethyme).unwrap();
    std::fs::write(
        aethyme.join("config.toml"),
        format!("[promote]\nbranch = \"{branch}\"\nmode = \"auto\"\n"),
    )
    .unwrap();
}

fn set_integration_at_head(repo: &Path) -> String {
    let head = git_output(repo, &["rev-parse", "HEAD"]);
    git(
        repo,
        &["update-ref", "refs/heads/aethyme/integration", &head],
    );
    head
}

fn advance_main(repo: &Path, filename: &str) -> String {
    std::fs::write(repo.join(filename), "main advanced\n").unwrap();
    git(repo, &["add", filename]);
    git(repo, &["commit", "-qm", "advance main"]);
    git_output(repo, &["rev-parse", "HEAD"])
}

#[test]
fn start_cli_returns_deterministic_planned_leases_and_status_exposes_them() {
    let tmp = fixture();
    let started = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "planned rewrite",
            "--path",
            "zeta.txt",
            "--path",
            "generated/",
            "--path",
            "zeta.txt",
            "--json",
        ],
    ));
    let value: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert!(value.get("scope_overlaps").is_none());
    assert!(value["scope_overlaps_warning"].is_string());
    assert_eq!(value["start_base"]["ref_name"], "refs/heads/main");
    assert_eq!(value["start_base"]["evidence"], "conventional_main");
    assert_eq!(
        value["planned_explicit_leases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lease| lease["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["generated/", "zeta.txt"]
    );

    let status = stdout(&run(tmp.path(), &["status", "--json"]));
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["leases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|lease| lease["kind"] == "explicit")
            .count(),
        2
    );

    let conflict = run(
        tmp.path(),
        &[
            "start",
            "--task",
            "conflicting rewrite",
            "--path",
            "generated/policy.md",
            "--json",
        ],
    );
    assert!(!conflict.status.success());
    assert!(
        json_error::message(&conflict).contains("planned lease"),
        "{}",
        json_error::message(&conflict)
    );
}

#[test]
fn start_cli_records_chau7_session_context_for_status_and_json() {
    let tmp = fixture();
    let started = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "review workspace",
            "--repo-name",
            "Aethyme",
            "--tab-name",
            "Fix auth",
            "--short-name",
            "Auth flow",
            "--ai-provider",
            "claude",
            "--json",
        ],
    ));
    let report: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(report["repository_name"], "Aethyme");
    assert_eq!(report["tab_name"], "Fix auth");
    assert_eq!(report["short_name"], "Auth flow");
    assert_eq!(report["ai_provider"], "claude");
    assert_eq!(report["tab_rename"]["status"], "pending");

    let status = stdout(&run(tmp.path(), &["status"]));
    assert!(status.contains("Aethyme / Fix auth / claude"), "{status}");
}

#[cfg(unix)]
#[test]
fn start_without_a_chau7_tab_skips_mcp_lookup() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = fixture();
    let bridge = tmp.path().join("record-mcp-lookup");
    let marker = tmp.path().join("mcp-was-called");
    std::fs::write(
        &bridge,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&bridge).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bridge, permissions).unwrap();

    let output = common::broker_cli_for_fixture(
        CLI,
        &["start", "--task", "without a tab", "--json"],
        tmp.path(),
    )
    .current_dir(tmp.path())
    .env("AETHYME_CHAU7_MCP_BRIDGE", &bridge)
    .output()
    .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON ({error}): stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report["tab_rename"]["status"], "not_applicable");
    assert!(
        !marker.exists(),
        "the Chau7 MCP bridge must not be launched"
    );
}

#[cfg(unix)]
#[test]
fn adopted_reused_session_without_a_chau7_tab_skips_mcp_lookup() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = fixture();
    set_integration_at_head(tmp.path());
    let created = stdout(&run_verbatim(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--allow-main-checkout",
            "--task",
            "first",
            "--json",
        ],
    ));
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    assert_eq!(created["tab_rename"]["status"], "not_applicable");

    let bridge = tmp.path().join("record-adopt-mcp-lookup");
    let marker = tmp.path().join("adopt-mcp-was-called");
    std::fs::write(
        &bridge,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&bridge).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bridge, permissions).unwrap();

    let output = common::broker_cli_for_fixture(
        CLI,
        &[
            "start",
            "--adopt",
            "--reuse",
            "--task",
            "follow-up",
            "--json",
        ],
        tmp.path(),
    )
    .current_dir(tmp.path())
    .env("AETHYME_CHAU7_MCP_BRIDGE", &bridge)
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let reused: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reused["outcome"], "reused");
    assert_eq!(reused["tab_rename"]["status"], "not_applicable");
    assert!(
        !marker.exists(),
        "the Chau7 MCP bridge must not be launched for an adopted session without a tab"
    );
}

#[cfg(unix)]
#[test]
fn mcp_timeout_keeps_start_successful_and_is_bounded() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let tmp = fixture();
    let bridge = tmp.path().join("stalling-mcp-bridge");
    std::fs::write(
        &bridge,
        "#!/bin/sh\nwhile IFS= read -r message; do :; done\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&bridge).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bridge, permissions).unwrap();

    let started_at = Instant::now();
    let output = common::broker_cli_for_fixture(
        CLI,
        &[
            "start",
            "--task",
            "tab lookup timeout",
            "--repo-name",
            "Aethyme",
            "--tab-name",
            "Named tab",
            "--json",
        ],
        tmp.path(),
    )
    .current_dir(tmp.path())
    .env("AETHYME_CHAU7_MCP_BRIDGE", &bridge)
    .output()
    .unwrap();
    let elapsed = started_at.elapsed();

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "bounded MCP timeout took {elapsed:?}"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tab_rename"]["status"], "pending");
    assert!(
        report["tab_rename"]["reason"]
            .as_str()
            .unwrap()
            .contains("timed out waiting for Chau7 MCP initialize"),
        "{}",
        report["tab_rename"]
    );
}

/// Run the CLI exactly as given: [`run`] adds a short name to every start.
fn run_verbatim(repo: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(CLI);
    command
        .args(args)
        .current_dir(repo)
        .env("AETHYME_CHAU7_MCP_BRIDGE", common::disabled_bridge_path());
    common::isolate_host_paths(&mut command, repo);
    command.output().unwrap()
}

/// `run_verbatim` without the fixture override: the production default.
fn run_enforced(repo: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(CLI);
    command
        .args(args)
        .current_dir(repo)
        .env("AETHYME_CHAU7_MCP_BRIDGE", common::disabled_bridge_path());
    common::isolate_host_paths(&mut command, repo);
    command.output().unwrap()
}

/// Callers written before short names existed keep working: the session is
/// named after its task.
#[test]
fn start_without_a_short_name_derives_one_from_the_task() {
    let tmp = fixture();
    let started = stdout(&run_verbatim(
        tmp.path(),
        &["start", "--task", "Fix auth token refresh", "--json"],
    ));
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["short_name"], "Fix auth token refresh");
}

#[test]
fn an_invalid_short_name_is_refused_before_start_creates_broker_state() {
    let tmp = fixture();
    let output = run_verbatim(
        tmp.path(),
        &["start", "--task", "labelled", "--short-name", "   "],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("short name must not be empty"), "{stderr}");
    assert!(
        !tmp.path().join(aethyme_broker::BROKER_DB_RELPATH).exists(),
        "short-name validation must precede broker database creation"
    );
}

/// `--reuse` without a name keeps the one the session already has rather
/// than renaming it after the follow-up task.
#[test]
fn reuse_without_a_short_name_keeps_the_stored_name() {
    let tmp = fixture();
    set_integration_at_head(tmp.path());
    stdout(&run_verbatim(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--allow-main-checkout",
            "--task",
            "first",
            "--short-name",
            "Chosen",
            "--json",
        ],
    ));
    let reused = stdout(&run_verbatim(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--task",
            "a different follow-up",
            "--json",
        ],
    ));
    let reused: serde_json::Value = serde_json::from_str(&reused).unwrap();
    assert_eq!(reused["short_name"], "Chosen");
}

#[test]
fn reuse_persists_a_new_short_name_but_keeps_tab_attribution() {
    let tmp = fixture();
    set_integration_at_head(tmp.path());
    let created = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--task",
            "first",
            "--tab-name",
            "Original tab",
            "--short-name",
            "Initial",
            "--json",
        ],
    ));
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    assert_eq!(created["short_name"], "Initial");
    assert_eq!(created["tab_name"], "Original tab");

    let reused = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--task",
            "follow-up",
            "--tab-name",
            "Current title",
            "--short-name",
            "Follow up",
            "--json",
        ],
    ));
    let reused: serde_json::Value = serde_json::from_str(&reused).unwrap();
    assert_eq!(reused["short_name"], "Follow up");
    assert_eq!(reused["tab_name"], "Original tab");
}

#[test]
fn start_selects_integration_or_default_branch_without_using_checkout_head() {
    for checkout in ["main", "feature", "detached"] {
        let tmp = fixture();
        let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
        if checkout != "main" {
            git(tmp.path(), &["switch", "-qc", "feature"]);
            std::fs::write(tmp.path().join("feature.txt"), checkout).unwrap();
            git(tmp.path(), &["add", "feature.txt"]);
            git(tmp.path(), &["commit", "-qm", "throwaway feature"]);
        }
        if checkout == "detached" {
            git(tmp.path(), &["checkout", "--detach", "HEAD"]);
        }

        let started = stdout(&run(tmp.path(), &["start", "--task", checkout, "--json"]));
        let value: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(value["start_base"]["ref_name"], "refs/heads/main");
        assert_eq!(value["start_base"]["commit"], main);
        assert_eq!(value["diff_base"], main);
    }

    let tmp = fixture();
    git(tmp.path(), &["switch", "-qc", "promoted"]);
    std::fs::write(tmp.path().join("promoted.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "promoted.txt"]);
    git(tmp.path(), &["commit", "-qm", "promoted work"]);
    let integration = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", &integration],
    );
    let started = stdout(&run(
        tmp.path(),
        &["start", "--task", "from integration", "--json"],
    ));
    let value: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(
        value["start_base"]["ref_name"],
        "refs/heads/aethyme/integration"
    );
    assert_eq!(value["start_base"]["commit"], integration);
    assert_eq!(value["start_base"]["evidence"], "integration_tip");
}

/// Point `main` at a tracking upstream without needing a real remote, so
/// `@{upstream}` resolves and the start base can be compared against it.
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

/// Put `aethyme/integration` `commits` ahead of `main`, the state a repository
/// is in whenever work has been promoted but not yet published.
fn integration_ahead_of_main(repo: &Path, commits: usize) -> String {
    git(repo, &["switch", "-qc", "promoted"]);
    for n in 0..commits {
        std::fs::write(repo.join(format!("promoted{n}.txt")), "promoted\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", &format!("promoted work {n}")]);
    }
    let integration = git_output(repo, &["rev-parse", "HEAD"]);
    git(
        repo,
        &["update-ref", "refs/heads/aethyme/integration", &integration],
    );
    git(repo, &["switch", "-q", "main"]);
    integration
}

/// A branch cut from integration inherits everything integration carries that
/// the default branch does not, and opening a pull request from it presents
/// those commits as the session's own (#283, #290). Being *ahead* is the
/// normal state, so it is the one that was never reported.
#[test]
fn start_reports_the_commits_its_base_carries_ahead_of_the_default_branch() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    integration_ahead_of_main(tmp.path(), 2);

    let started = stdout(&run(
        tmp.path(),
        &["start", "--task", "ahead of default", "--json"],
    ));
    let value: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(value["start_base"]["evidence"], "integration_tip");
    assert_eq!(value["start_base"]["ahead_default_commits"], 2);
    assert_eq!(value["start_base"]["behind_default_commits"], 0);

    let human = fixture();
    let main = git_output(human.path(), &["rev-parse", "HEAD"]);
    track_origin_main(human.path(), &main);
    integration_ahead_of_main(human.path(), 2);
    let rendered = stdout(&run(human.path(), &["start", "--task", "ahead of default"]));
    assert!(
        rendered.contains("note: this base is 2 commit(s) ahead of"),
        "{rendered}"
    );
    assert!(
        rendered.contains("carries them alongside your own work"),
        "{rendered}"
    );
}

/// A base level with the default branch inherits nothing, so the note would be
/// noise. Guards the threshold in the other direction.
#[test]
fn start_is_silent_when_its_base_carries_nothing_extra() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    git(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", &main],
    );

    let rendered = stdout(&run(tmp.path(), &["start", "--task", "level with default"]));
    assert!(!rendered.contains("commit(s) ahead of"), "{rendered}");
    assert!(!rendered.contains("commit(s) behind"), "{rendered}");
}

/// `start --cmd` selects a base exactly as `start` does and reported nothing
/// about it, which is the worse half of the gap: a detached agent has no one
/// reading its terminal, and it opens pull requests from that base (#290).
#[test]
fn start_with_cmd_reports_its_base_and_what_it_carries() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    integration_ahead_of_main(tmp.path(), 3);

    let rendered = stdout(&run(
        tmp.path(),
        &["start", "--cmd", "true", "--task", "detached"],
    ));
    assert!(
        rendered.contains("Start base: refs/heads/aethyme/integration"),
        "{rendered}"
    );
    assert!(
        rendered.contains("note: this base is 3 commit(s) ahead of"),
        "{rendered}"
    );
}

/// #290 phase 1.1: an explicit base is honored, recorded as its own evidence so
/// the choice is auditable, and still measured against the default branch --
/// naming a base does not stop its inherited commits landing in a pull request.
#[test]
fn start_cuts_from_an_explicit_base_when_one_is_given() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    let integration = integration_ahead_of_main(tmp.path(), 2);

    // Without --base the integration tip wins, and carries 2 inherited commits.
    let default_base = stdout(&run(tmp.path(), &["start", "--task", "implicit", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&default_base).unwrap();
    assert_eq!(value["start_base"]["commit"], integration);
    assert_eq!(value["start_base"]["ahead_default_commits"], 2);

    // Naming the default branch cuts from it instead, inheriting nothing.
    let chosen = fixture();
    let main = git_output(chosen.path(), &["rev-parse", "HEAD"]);
    track_origin_main(chosen.path(), &main);
    integration_ahead_of_main(chosen.path(), 2);
    let explicit = stdout(&run(
        chosen.path(),
        &[
            "start",
            "--task",
            "explicit",
            "--base",
            "refs/heads/main",
            "--json",
        ],
    ));
    let value: serde_json::Value = serde_json::from_str(&explicit).unwrap();
    assert_eq!(value["start_base"]["ref_name"], "refs/heads/main");
    assert_eq!(value["start_base"]["commit"], main);
    assert_eq!(value["start_base"]["evidence"], "explicit_base");
    assert_eq!(value["start_base"]["ahead_default_commits"], 0);
}

/// A base that does not resolve is refused rather than silently falling back to
/// inference, which would report a base the operator did not choose.
#[test]
fn start_refuses_a_base_that_does_not_resolve() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &["start", "--task", "bad base", "--base", "refs/heads/nope"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not resolve to a commit"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reuse_revalidates_and_fast_forwards_a_clean_conventional_base() {
    let tmp = fixture();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[worktree_freshness]\nmax_age_seconds = 86400\n",
    )
    .unwrap();
    let started = stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["start_base"]["evidence"], "conventional_main");
    let session = stored_session(&tmp, 1);
    let worktree = Path::new(&session.worktree_path);
    let original_head = git_output(worktree, &["rev-parse", "HEAD"]);
    assert_eq!(Some(original_head.as_str()), session.diff_base.as_deref());

    let current_main = advance_main(tmp.path(), "main-advance.txt");
    let worktree_arg = session.worktree_path.as_str();
    let reused = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "follow-up",
            "--json",
        ],
    ));
    let reused: serde_json::Value = serde_json::from_str(&reused).unwrap();

    assert_eq!(reused["outcome"], "reused");
    assert_eq!(reused["base_freshness"]["source"], "conventional_main");
    assert_eq!(reused["base_freshness"]["relation"], "fast_forwarded");
    assert_eq!(reused["base_freshness"]["max_age_seconds"], 86400);
    assert_eq!(
        reused["base_freshness"]["previous_observation_stale"], false,
        "a recent observation must never skip the fresh ancestry check"
    );
    assert!(reused["base_freshness"]["previous_observation_age_seconds"].is_u64());
    let observation = stored_base_observation(&tmp, session.id);
    assert_eq!(observation["target_commit"], current_main);
    assert!(observation["observed_at_ms"].is_i64());
    assert_eq!(reused["base_freshness"]["target_ref"], "refs/heads/main");
    assert_eq!(reused["base_freshness"]["target_commit"], current_main);
    assert_eq!(reused["diff_base"], current_main);
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), current_main);
    let updated = stored_session(&tmp, session.id);
    assert_eq!(updated.task.as_deref(), Some("follow-up"));
    assert_eq!(updated.diff_base.as_deref(), Some(current_main.as_str()));
}

#[test]
fn reuse_refuses_an_unconfirmed_integration_base_without_changing_the_session() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    set_integration_at_head(tmp.path());
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    let previous_observation = stored_base_observation(&tmp, session.id);
    let worktree = Path::new(&session.worktree_path);
    let before = git_output(worktree, &["rev-parse", "HEAD"]);
    let unavailable_remote = tmp.path().join("unavailable-remote");
    git(
        tmp.path(),
        &[
            "config",
            "remote.origin.url",
            unavailable_remote.to_str().unwrap(),
        ],
    );
    for sync in [false, true] {
        let mut args = vec![
            "start",
            "--reuse",
            session.worktree_path.as_str(),
            "--task",
            "offline follow-up",
            "--json",
        ];
        if sync {
            args.push("--sync-integration");
        }
        let output = run(tmp.path(), &args);
        assert!(!output.status.success());
        let error = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(error.contains("not freshly confirmed"), "{error}");
        assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), before);
        assert_eq!(stored_session(&tmp, 1).task.as_deref(), Some("initial"));
        assert_eq!(stored_base_observation(&tmp, 1), previous_observation);
    }
}

#[test]
fn reuse_refuses_a_stale_integration_even_with_unknown_provenance() {
    let tmp = fixture();
    let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    track_origin_main(tmp.path(), &main);
    set_integration_at_head(tmp.path());
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    delete_stored_base_observation(&tmp, session.id);
    let worktree = Path::new(&session.worktree_path);
    let before = git_output(worktree, &["rev-parse", "HEAD"]);
    advance_main(tmp.path(), "remote-advance.txt");
    for sync in [false, true] {
        let mut args = vec![
            "start",
            "--reuse",
            session.worktree_path.as_str(),
            "--task",
            "stale follow-up",
            "--json",
        ];
        if sync {
            args.push("--sync-integration");
        }
        let output = run(tmp.path(), &args);
        assert!(!output.status.success());
        let error = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(error.contains("freshly fetched default branch"), "{error}");
        assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), before);
        assert_eq!(stored_session(&tmp, 1).task.as_deref(), Some("initial"));
    }
}

#[test]
fn unavailable_remote_default_ref_is_not_treated_as_local_only() {
    let cases = [("origin", true), ("origin", false), ("upstream", false)];
    for (remote, symbolic_default) in cases {
        let tmp = fixture();
        let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
        track_origin_main(tmp.path(), &main);
        if remote != "origin" {
            git(tmp.path(), &["remote", "rename", "origin", remote]);
        }
        let tracking_ref = format!("refs/remotes/{remote}/main");
        if symbolic_default {
            git(
                tmp.path(),
                &["symbolic-ref", "refs/remotes/origin/HEAD", &tracking_ref],
            );
        }
        set_integration_at_head(tmp.path());
        stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
        let session = stored_session(&tmp, 1);
        let observation = stored_base_observation(&tmp, 1);
        let before = git_output(tmp.path(), &["worktree", "list", "--porcelain"]);
        git(tmp.path(), &["update-ref", "-d", &tracking_ref]);
        let output = run(tmp.path(), &["start", "--task", "must refuse", "--json"]);
        assert!(!output.status.success());
        assert_eq!(
            git_output(tmp.path(), &["worktree", "list", "--porcelain"]),
            before
        );
        for unknown_provenance in [false, true] {
            if unknown_provenance {
                delete_stored_base_observation(&tmp, 1);
            }
            let output = run(
                tmp.path(),
                &[
                    "start",
                    "--reuse",
                    session.worktree_path.as_str(),
                    "--task",
                    "must refuse",
                    "--json",
                ],
            );
            assert!(!output.status.success());
            assert_eq!(stored_session(&tmp, 1).task.as_deref(), Some("initial"));
            assert_eq!(
                git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
                main
            );
            if !unknown_provenance {
                assert_eq!(stored_base_observation(&tmp, 1), observation);
            }
        }
    }
}

#[test]
fn reuse_refuses_dirty_fast_forward_without_updating_task_or_head() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    let worktree = Path::new(&session.worktree_path);
    let original_head = git_output(worktree, &["rev-parse", "HEAD"]);
    let target = advance_main(tmp.path(), "main-advance.txt");
    std::fs::write(worktree.join("README.md"), "dirty session edit\n").unwrap();

    let worktree_arg = session.worktree_path.as_str();
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "must not be recorded",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains("dirty paths"),
        "{}",
        json_error::message(&refused)
    );
    let message = json_error::message(&refused);
    for detail in [
        format!("checkout_head={original_head}"),
        format!("recorded_base_sha={original_head}"),
        format!("target_sha={target}"),
        "target_ref=refs/heads/main".to_owned(),
        "ahead=0 behind=1".to_owned(),
        "prior_observation_age_seconds=".to_owned(),
        "max_age_seconds=900".to_owned(),
        "next: preserve local changes".to_owned(),
    ] {
        assert!(message.contains(&detail), "missing {detail}: {message}");
    }
    assert!(
        !message.contains("prior_observation_age_seconds=unknown"),
        "{message}"
    );
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "dirty session edit\n"
    );
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task.as_deref(), Some("initial"));
    assert_eq!(unchanged.diff_base, session.diff_base);
}

#[test]
fn reuse_retry_repairs_diff_base_after_fast_forward_persistence_failure() {
    let tmp = fixture();
    let started = stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    let session = stored_session(&tmp, started["id"].as_i64().unwrap());
    let original_diff_base = session.diff_base.clone();
    let original_adoption_base = session.adoption_base.clone();
    let worktree = Path::new(&session.worktree_path);
    let target = advance_main(tmp.path(), "main-advance.txt");

    let mut broker = common::open_fixture_repository_broker(&tmp).unwrap();
    let database = broker.store().db_path().to_path_buf();
    drop(broker);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_reuse_task_update
             BEFORE UPDATE OF task ON sessions
             WHEN NEW.task = 'retry after failed persistence'
             BEGIN
               SELECT RAISE(ABORT, 'fixture task persistence failure');
             END;",
        )
        .unwrap();

    let worktree_arg = session.worktree_path.as_str();
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "retry after failed persistence",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains(&format!("checkout was advanced to {target}")),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), target);
    let after_failure = stored_session(&tmp, session.id);
    assert_eq!(after_failure.task.as_deref(), Some("initial"));
    assert_eq!(after_failure.diff_base, original_diff_base);
    assert_eq!(after_failure.adoption_base, original_adoption_base);
    assert_eq!(
        stored_base_observation(&tmp, session.id)["target_commit"],
        original_diff_base.as_deref().unwrap()
    );

    connection
        .execute_batch("DROP TRIGGER fail_reuse_task_update;")
        .unwrap();
    let retried = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "retry succeeded",
            "--json",
        ],
    ));
    let retried: serde_json::Value = serde_json::from_str(&retried).unwrap();
    assert_eq!(retried["base_freshness"]["relation"], "current");
    assert_eq!(retried["diff_base"], target);
    let repaired = stored_session(&tmp, session.id);
    assert_eq!(repaired.task.as_deref(), Some("retry succeeded"));
    assert_eq!(repaired.diff_base.as_deref(), Some(target.as_str()));
    assert_eq!(repaired.adoption_base, original_adoption_base);
}

#[test]
fn reuse_keeps_an_explicit_base_pinned_when_its_branch_advances() {
    let tmp = fixture();
    let original_base = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "explicit base",
            "--base",
            "refs/heads/main",
            "--json",
        ],
    ));
    let session = stored_session(&tmp, 1);
    advance_main(tmp.path(), "main-advance.txt");

    let worktree_arg = session.worktree_path.as_str();
    let reused = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "keep pinned base",
            "--json",
        ],
    ));
    let reused: serde_json::Value = serde_json::from_str(&reused).unwrap();

    assert_eq!(reused["base_freshness"]["source"], "explicit_base");
    assert_eq!(reused["base_freshness"]["relation"], "current");
    assert_eq!(reused["base_freshness"]["target_commit"], original_base);
    assert_eq!(
        git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        original_base
    );
}

#[test]
fn reuse_refuses_changed_promote_policy_without_changing_checkout_or_task() {
    let tmp = fixture();
    let initial = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    let recorded_branch = "aethyme/integration-recorded";
    git(
        tmp.path(),
        &[
            "update-ref",
            &format!("refs/heads/{recorded_branch}"),
            &initial,
        ],
    );
    set_promote_branch(tmp.path(), recorded_branch);
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    assert_eq!(session.diff_base.as_deref(), Some(initial.as_str()));
    let original_head = git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]);

    set_promote_branch(tmp.path(), "aethyme/integration-new-policy");
    let worktree_arg = session.worktree_path.as_str();
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "new policy must not be recorded",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains("configured integration branch changed"),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(
        git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        original_head
    );
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task.as_deref(), Some("initial"));
    assert_eq!(unchanged.diff_base.as_deref(), session.diff_base.as_deref());
}

#[test]
fn reuse_refuses_a_missing_recorded_integration_ref_without_fallback() {
    let tmp = fixture();
    let initial = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    let recorded_branch = "aethyme/integration-recorded";
    let recorded_ref = format!("refs/heads/{recorded_branch}");
    git(tmp.path(), &["update-ref", &recorded_ref, &initial]);
    set_promote_branch(tmp.path(), recorded_branch);
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    let original_head = git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["update-ref", "-d", &recorded_ref]);

    let worktree_arg = session.worktree_path.as_str();
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "missing ref must not be recorded",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains(
            "recorded integration ref refs/heads/aethyme/integration-recorded is missing"
        ),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(
        git_output(Path::new(&session.worktree_path), &["rev-parse", "HEAD"]),
        original_head
    );
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task.as_deref(), Some("initial"));
    assert_eq!(unchanged.diff_base, session.diff_base);
}

#[test]
fn unknown_adopted_base_tracks_current_integration_and_requires_explicit_sync() {
    let tmp = fixture();
    let initial = set_integration_at_head(tmp.path());
    stdout(&run(
        tmp.path(),
        &["start", "--adopt", "--task", "initial", "--json"],
    ));
    let session = stored_session(&tmp, 1);

    let contained = stdout(&run(
        tmp.path(),
        &["start", "--reuse", "--task", "contained base", "--json"],
    ));
    let contained: serde_json::Value = serde_json::from_str(&contained).unwrap();
    assert_eq!(contained["base_freshness"]["target_commit"], initial);
    assert_eq!(contained["base_freshness"]["relation"], "current");
    assert!(contained["base_freshness"].get("source").is_none());

    let advanced_integration = integration_ahead_of_main(tmp.path(), 1);
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--task",
            "must require explicit sync",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused).contains("automatic movement is disabled"),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(git_output(tmp.path(), &["rev-parse", "HEAD"]), initial);
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task.as_deref(), Some("contained base"));

    let synchronized = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--sync-integration",
            "--task",
            "explicitly synchronized",
            "--json",
        ],
    ));
    let synchronized: serde_json::Value = serde_json::from_str(&synchronized).unwrap();
    assert_eq!(
        synchronized["integration_sync"]["outcome"],
        "fast_forwarded"
    );
    assert_eq!(
        git_output(tmp.path(), &["rev-parse", "HEAD"]),
        advanced_integration
    );
    assert_eq!(
        stored_session(&tmp, session.id).task.as_deref(),
        Some("explicitly synchronized")
    );
}

#[test]
fn unknown_reuse_base_refuses_verify_only_even_when_old_integration_matches_head() {
    let tmp = fixture();
    let original_head = set_integration_at_head(tmp.path());
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    let worktree = Path::new(&session.worktree_path);
    delete_stored_base_observation(&tmp, session.id);
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    let advanced_main = advance_main(tmp.path(), "new-default.txt");
    assert_ne!(advanced_main, original_head);
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), original_head);
    assert_eq!(
        git_output(tmp.path(), &["rev-parse", "aethyme/integration"]),
        original_head
    );

    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            &session.worktree_path,
            "--task",
            "must not be recorded",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        json_error::message(&refused)
            .contains("verify-only policy does not select the integration branch"),
        "{}",
        json_error::message(&refused)
    );
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), original_head);
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task, session.task);
    assert_eq!(unchanged.diff_base, session.diff_base);
    assert_eq!(unchanged.adoption_base, session.adoption_base);
}

#[test]
fn unknown_adopted_base_refuses_when_configured_integration_is_missing() {
    for malformed in [false, true] {
        let tmp = fixture();
        stdout(&run(
            tmp.path(),
            &["start", "--adopt", "--task", "initial", "--json"],
        ));
        let session = stored_session(&tmp, 1);
        let original_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
        if malformed {
            overwrite_stored_base_observation(&tmp, session.id, "not-json");
        } else {
            delete_stored_base_observation(&tmp, session.id);
        }

        let refused = run(
            tmp.path(),
            &[
                "start",
                "--reuse",
                "--task",
                "no inferred fallback",
                "--json",
            ],
        );
        assert!(!refused.status.success());
        assert!(
            json_error::message(&refused)
                .contains("configured integration ref refs/heads/aethyme/integration is missing"),
            "{}",
            json_error::message(&refused)
        );
        assert_eq!(
            git_output(tmp.path(), &["rev-parse", "HEAD"]),
            original_head
        );
        assert_eq!(
            stored_session(&tmp, session.id).task.as_deref(),
            Some("initial")
        );
    }
}

/// `start --adopt` registers an existing worktree, so there is no base to choose. The
/// refusal says so rather than giving the generic "valid only with" list.
#[test]
fn adopt_refuses_a_base_and_explains_why() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &["start", "--adopt", "--task", "x", "--base", "main"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not apply to broker adopt"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--base` is parsed for every subcommand, and only some can act on it.
/// Accepting it where it is ignored reports a base choice that was never made
/// (#290 phase 0.2). `start` and `start --cmd` now honor it (phase 1.1); every
/// other subcommand must refuse rather than drop it.
#[test]
fn subcommands_that_cannot_honor_a_base_refuse_it() {
    let tmp = fixture();

    // Only `scope` reads it within `advanced gates`.
    let output = run(
        tmp.path(),
        &["advanced", "gates", "draft", "--base", "main"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("`--base` is not valid for `broker gates draft`; it applies to:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And a subcommand with no notion of a base at all.
    let output = run(tmp.path(), &["status", "--base", "main"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("`--base` is not valid for `broker status`"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn start_refuses_ambiguous_or_missing_default_refs() {
    let ambiguous = fixture();
    git(ambiguous.path(), &["branch", "master", "main"]);
    let output = run(ambiguous.path(), &["start", "--task", "ambiguous default"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("both refs/heads/main and refs/heads/master exist"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let missing = fixture();
    git(missing.path(), &["branch", "-m", "feature-only"]);
    let output = run(missing.path(), &["start", "--task", "missing default"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no integration tip"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn start_refuses_a_pull_request_review_before_creating_a_session() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &[
            "start",
            "--task",
            "review workspace",
            "--pull-request",
            "42",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("integration-tip worktree"), "{stderr}");
    assert!(stderr.contains("pull-request review #42"), "{stderr}");
    assert!(
        common::open_fixture_repository_broker(&tmp)
            .unwrap()
            .store()
            .live_sessions()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn start_does_not_inspect_free_form_task_text_for_pull_request_reviews() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &["start", "--task", "fix the review gate so PR 754 passes"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn adopt_cli_distinguishes_created_and_reused_session_identities() {
    let tmp = fixture();
    set_integration_at_head(tmp.path());

    let created = stdout(&run(tmp.path(), &["start", "--adopt", "--task", "first"]));
    assert!(
        created.contains("Created session 1 on the existing worktree"),
        "{created}"
    );

    stdout(&run(tmp.path(), &["finish", "close", "--session", "1"]));
    let created_after_close = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--allow-main-checkout",
            "--task",
            "second",
        ],
    ));
    assert!(
        created_after_close.contains("Created session 2 on the existing worktree"),
        "{created_after_close}"
    );
    assert!(!created_after_close.contains("Reusing session"));

    let reused = stdout(&run(tmp.path(), &["start", "--reuse", "--task", "third"]));
    assert!(reused.contains("Reusing session 2"), "{reused}");
}

#[test]
fn reuse_cli_exposes_structured_integration_drift_and_safe_guidance() {
    let tmp = fixture();
    let started = stdout(&run(tmp.path(), &["start", "--task", "first", "--json"]));
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["start_base"]["evidence"], "conventional_main");
    let session = stored_session(&tmp, started["id"].as_i64().unwrap());
    let worktree = Path::new(&session.worktree_path);

    git(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("shared.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "shared.txt"]);
    git(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    git(tmp.path(), &["checkout", "-q", "main"]);
    let session_head = git_output(worktree, &["rev-parse", "HEAD"]);
    std::fs::write(worktree.join("shared.txt"), "dirty session edit\n").unwrap();
    let worktree_arg = session.worktree_path.as_str();

    let json = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            worktree_arg,
            "--task",
            "follow-up",
            "--json",
        ],
    ));
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["outcome"], "reused");
    assert_eq!(report["integration_drift"]["session_head"], session_head);
    assert_eq!(
        report["integration_drift"]["integration_head"],
        integration_head
    );
    assert_eq!(report["integration_drift"]["relation"], "behind");
    assert_eq!(report["integration_drift"]["ahead_commits"], 0);
    assert_eq!(report["integration_drift"]["behind_commits"], 1);
    assert_eq!(
        report["integration_drift"]["overlapping_changed_paths"],
        serde_json::json!(["shared.txt"])
    );
    assert!(report["integration_drift"]["warning"].is_string());
    assert_eq!(
        report["integration_drift"]["safe_next_action"],
        "aethyme broker advanced integration status"
    );

    let rendered = stdout(&run(
        tmp.path(),
        &["start", "--reuse", worktree_arg, "--task", "render drift"],
    ));
    assert!(rendered.contains("Integration drift: behind"), "{rendered}");
    assert!(rendered.contains("Overlapping changed paths:\n  shared.txt"));
    assert!(rendered.contains("Warning:"), "{rendered}");
    assert!(
        rendered.contains("Safe next action: aethyme broker advanced integration status"),
        "{rendered}"
    );
}

#[test]
fn reuse_behind_or_diverged_observes_drift_without_submission_patch_scans() {
    for diverged in [false, true] {
        let tmp = fixture();
        let started = stdout(&run(tmp.path(), &["start", "--task", "first", "--json"]));
        let started: serde_json::Value = serde_json::from_str(&started).unwrap();
        let session = stored_session(&tmp, started["id"].as_i64().unwrap());
        let worktree = Path::new(&session.worktree_path);
        std::fs::write(worktree.join("session.txt"), "owned session change\n").unwrap();
        git(worktree, &["add", "session.txt"]);
        git(worktree, &["commit", "-qm", "session-owned change"]);
        let session_head = git_output(worktree, &["rev-parse", "HEAD"]);

        let integration_base = if diverged {
            "main"
        } else {
            session_head.as_str()
        };
        git(
            tmp.path(),
            &["checkout", "-qb", "integration-work", integration_base],
        );
        std::fs::write(tmp.path().join("integration.txt"), "integration change\n").unwrap();
        git(tmp.path(), &["add", "integration.txt"]);
        git(tmp.path(), &["commit", "-qm", "integration change"]);
        git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
        git(tmp.path(), &["checkout", "-q", "main"]);

        let trace = tmp.path().join(".aethyme/reuse-trace.jsonl");
        let output = common::broker_cli_for_fixture(
            CLI,
            &[
                "start",
                "--reuse",
                session.worktree_path.as_str(),
                "--task",
                "inspect drift",
                "--json",
            ],
            tmp.path(),
        )
        .current_dir(tmp.path())
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
        let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(
            report["integration_drift"]["relation"],
            if diverged { "diverged" } else { "behind" }
        );
        assert!(report["integration_drift"]["warning"].is_string());
        assert_eq!(
            report["integration_drift"]["safe_next_action"],
            "aethyme broker advanced integration status"
        );
        assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), session_head);

        let events = std::fs::read_to_string(trace)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let commands = events
            .iter()
            .filter(|event| event["event"] == "start")
            .filter_map(|event| event["argv"].as_array())
            .collect::<Vec<_>>();
        assert!(
            commands
                .iter()
                .any(|argv| argv.get(1).is_some_and(|arg| arg == "rev-list")),
            "trace must observe the real drift Git queries"
        );
        assert!(
            !commands
                .iter()
                .any(|argv| argv.get(1).is_some_and(|arg| arg == "patch-id")),
            "drift guidance must not construct unused submission provenance: {commands:?}"
        );
    }
}

#[test]
fn adopt_cli_syncs_reuse_to_integration_and_exposes_the_exact_transition() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["start", "--adopt", "--task", "first"]));
    let session_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);

    git(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("integration.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "integration.txt"]);
    git(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    git(tmp.path(), &["checkout", "-q", "main"]);

    let json = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--sync-integration",
            "--task",
            "synchronized follow-up",
            "--json",
        ],
    ));
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["integration_sync"]["outcome"], "fast_forwarded");
    assert_eq!(report["integration_sync"]["before_head"], session_head);
    assert_eq!(report["integration_sync"]["after_head"], integration_head);
    assert_eq!(report["diff_base"], integration_head);
    assert_eq!(report["integration_drift"]["relation"], "current");
    assert_eq!(
        git_output(tmp.path(), &["rev-parse", "HEAD"]),
        integration_head
    );

    let rendered = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            "--sync-integration",
            "--task",
            "current",
        ],
    ));
    assert!(
        rendered.contains("Integration synchronization: already current"),
        "{rendered}"
    );
}

#[test]
fn adopt_cli_requires_reuse_for_integration_sync() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--short-name",
            "Existing worktree",
            "--sync-integration",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--sync-integration requires --reuse")
    );
}

/// Uncommitted work already in the checkout is not this session's, but a
/// repository pre-push gate validates the whole snapshot, so it fails the
/// session's first push for reasons the session cannot see (#131 finding 3).
#[test]
fn adopt_warns_about_uncommitted_paths_it_did_not_create() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);

    // Clean checkout: nothing to warn about.
    let clean = run(
        repo.path(),
        &["start", "--adopt", "--task", "clean checkout"],
    );
    let clean_out = String::from_utf8_lossy(&clean.stdout);
    assert!(
        !clean_out.contains("warning:"),
        "a clean checkout must not warn: {clean_out}"
    );
    run(repo.path(), &["finish", "close", "--session", "1"]);

    // Pre-existing unrelated work: the session must be told.
    std::fs::write(repo.path().join("unrelated.txt"), "not mine\n").unwrap();
    let dirty = run(
        repo.path(),
        &["start", "--adopt", "--task", "dirty checkout"],
    );
    let dirty_out = String::from_utf8_lossy(&dirty.stdout);
    assert!(
        dirty_out.contains("warning:") && dirty_out.contains("unrelated.txt"),
        "adopt must name the pre-existing uncommitted paths: {dirty_out}"
    );
    assert!(
        dirty_out.contains("pre-push"),
        "the warning must say why it matters: {dirty_out}"
    );
}

fn overlaps(repo: &Path) -> Vec<aethyme_broker::ScopeOverlap> {
    let host_state_dir = common::fixture_host_state_dir(repo);
    let preparation_cache_root = common::fixture_preparation_cache_root(repo);
    std::fs::create_dir_all(&preparation_cache_root).unwrap();
    let preparation_cache_root = preparation_cache_root.canonicalize().unwrap();
    let broker = Broker::open_with_options(
        repo,
        BrokerOpenOptions::default()
            .with_host_state_dir(host_state_dir.clone())
            .with_host_cache_dir(common::fixture_host_cache_dir(repo))
            .with_preparation_cache_root(preparation_cache_root)
            .with_host_operation_database(host_state_dir.join("host-operations.db"))
            .with_collaboration_state(host_state_dir.join("collaboration")),
    )
    .unwrap();
    // This observer is read-only. Let the durable repository binding choose
    // its repository-scoped root instead of pinning the host-state container.
    broker.scope_overlaps_snapshot().unwrap()
}

// #285: adopt pasted its capture call inside a string literal, so it recorded
// no scope and printed Rust source in its note. Start with `--json` skipped
// capture too. Both paths must persist the claim where overlap detection sees it.
#[test]
fn adopt_records_declared_scope_that_collides_with_a_json_start() {
    let tmp = fixture();
    stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "first",
            "--claim",
            "symbol:PaymentService=replace",
            "--json",
        ],
    ));
    let adopted = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--task",
            "second",
            "--claim",
            "symbol:PaymentService=replace",
        ],
    ));
    assert!(adopted.contains("Scope: 1 declared"), "{adopted}");
    assert!(
        !adopted.contains("&mut") && !adopted.contains(")?;"),
        "adopt must print prose, not source: {adopted}"
    );
    assert_eq!(overlaps(tmp.path()).len(), 1, "{adopted}");
}

#[test]
fn json_adopt_records_declared_scope_that_collides_with_a_start() {
    let tmp = fixture();
    stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "first",
            "--claim",
            "symbol:PaymentService=replace",
        ],
    ));
    let adopted = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--task",
            "second",
            "--claim",
            "symbol:PaymentService=replace",
            "--json",
        ],
    ));
    let adopted: serde_json::Value =
        serde_json::from_str(&adopted).expect("adopt --json stays pure JSON");
    let session_id = adopted["id"].as_i64().unwrap();
    let reported = adopted["scope_overlaps"].as_array().unwrap();
    assert_eq!(reported.len(), 1, "{adopted}");
    assert!(
        reported[0]["session_a"] == session_id || reported[0]["session_b"] == session_id,
        "{adopted}"
    );
    assert_eq!(overlaps(tmp.path()).len(), 1);
}

#[test]
fn json_start_reports_only_overlaps_involving_the_new_session() {
    let tmp = fixture();

    let first = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "first",
            "--claim",
            "symbol:PaymentService=extend",
            "--json",
        ],
    ));
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    let first_id = first["id"].as_i64().unwrap();
    assert_eq!(first["scope_overlaps"], serde_json::json!([]));

    let second = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "second",
            "--claim",
            "symbol:PaymentService=extend",
            "--json",
        ],
    ));
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let second_id = second["id"].as_i64().unwrap();
    let reported = second["scope_overlaps"].as_array().unwrap();
    assert_eq!(reported.len(), 1, "{second}");
    assert_eq!(reported[0]["value"], "PaymentService");
    assert!(
        (reported[0]["session_a"] == first_id && reported[0]["session_b"] == second_id)
            || (reported[0]["session_a"] == second_id && reported[0]["session_b"] == first_id),
        "{second}"
    );

    let third = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "third",
            "--claim",
            "symbol:SubscriptionService=extend",
            "--json",
        ],
    ));
    let third: serde_json::Value = serde_json::from_str(&third).unwrap();
    assert_eq!(
        third["scope_overlaps"],
        serde_json::json!([]),
        "the JSON response must exclude an overlap between older sessions"
    );
    assert_eq!(overlaps(tmp.path()).len(), 1);
}

#[test]
fn subcommands_that_cannot_honor_a_claim_refuse_it() {
    let tmp = fixture();
    for args in [
        &[
            "start", "--cmd", "true", "--task", "t", "--claim", "symbol:A",
        ][..],
        &["submit", "--session", "1", "--claim", "symbol:A"][..],
    ] {
        let output = run(tmp.path(), args);
        assert!(!output.status.success(), "{args:?} must refuse --claim");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("`--claim` is not valid for")
                && stderr.contains("it applies to: start, adopt"),
            "{args:?}: {stderr}"
        );
    }
}

const MAIN_CHECKOUT_REFUSED: &str = "refusing to register a session on the main checkout";

fn assert_main_checkout_refused(output: &Output, expect_json: bool) {
    assert!(
        !output.status.success(),
        "adopt unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    if expect_json {
        let error = json_error::message(output);
        assert!(error.contains(MAIN_CHECKOUT_REFUSED), "{error}");
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(MAIN_CHECKOUT_REFUSED), "{error}");
    }
}

/// #284 proposal 9: a new session on the repository's main checkout is
/// refused, however the path is spelled; work there would land on the
/// default branch before any gate runs.
#[test]
fn a_new_session_on_the_main_checkout_is_refused_however_it_is_spelled() {
    let tmp = fixture();
    let root = tmp.path();
    assert_main_checkout_refused(
        &run_enforced(root, &["start", "--adopt", "--task", "t"]),
        false,
    );

    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("link-to-main");
    std::os::unix::fs::symlink(root, &link).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let trailing = format!("{}/", root.display());
    let dotted = root.join("sub").join("..");
    let spellings = vec![
        link.display().to_string(),
        trailing,
        dotted.display().to_string(),
        // APFS is case-insensitive by default: another case names the same
        // tree. Elsewhere this path does not exist and the loop skips it.
        root.display().to_string().to_uppercase(),
    ];
    for spelling in spellings {
        if !Path::new(&spelling).exists() {
            continue;
        }
        let output = run_enforced(
            &root.join("sub"),
            &[
                "start",
                "--adopt",
                &spelling,
                "--task",
                "t",
                "--short-name",
                "s",
            ],
        );
        assert_main_checkout_refused(&output, false);
    }
    assert!(
        common::open_fixture_broker(root, root.parent().unwrap_or(root))
            .unwrap()
            .store()
            .live_sessions()
            .unwrap()
            .is_empty(),
        "a refused adopt registers nothing"
    );
}

/// The operator override registers it, and a session that already lives on
/// the main checkout can still be re-entered without the flag.
#[test]
fn the_main_checkout_needs_an_explicit_override_and_its_session_stays_reusable() {
    let tmp = fixture();
    set_integration_at_head(tmp.path());
    let created = stdout(&run_enforced(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--allow-main-checkout",
            "--task",
            "operator-approved",
            "--json",
        ],
    ));
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    assert_eq!(created["outcome"], "created");
    let reused = stdout(&run_verbatim(
        tmp.path(),
        &[
            "start",
            "--adopt",
            "--reuse",
            "--task",
            "follow-up",
            "--json",
        ],
    ));
    let reused: serde_json::Value = serde_json::from_str(&reused).unwrap();
    assert_eq!(reused["outcome"], "reused");
}

#[test]
fn the_main_checkout_environment_variable_is_not_an_operator_override() {
    let tmp = fixture();
    let mut command = Command::new(CLI);
    command
        .args([
            "start",
            "--adopt",
            "--task",
            "environment override must not work",
            "--short-name",
            "environment-override",
            "--json",
        ])
        .current_dir(tmp.path())
        .env("AETHYME_CHAU7_MCP_BRIDGE", common::disabled_bridge_path())
        .env("AETHYME_ALLOW_MAIN_CHECKOUT", "1");
    common::isolate_host_paths(&mut command, tmp.path());
    let output = command.output().unwrap();
    assert_main_checkout_refused(&output, true);
}

/// A linked worktree is not the main checkout: adopting it still works.
#[test]
fn a_linked_worktree_is_still_adoptable() {
    let tmp = fixture();
    let linked = tempfile::tempdir().unwrap();
    let worktree = linked.path().join("linked");
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    let created = stdout(&run_enforced(
        &worktree,
        &["start", "--adopt", "--task", "linked work", "--json"],
    ));
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    assert_eq!(created["outcome"], "created");
}

#[path = "common/json_error.rs"]
mod json_error;

#[test]
fn divergent_reuse_reports_exact_base_target_counts_and_age_without_moving_work() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["start", "--task", "initial", "--json"]));
    let session = stored_session(&tmp, 1);
    let worktree = Path::new(&session.worktree_path);
    let base = git_output(worktree, &["rev-parse", "HEAD"]);
    std::fs::write(worktree.join("session-only.txt"), "owned source\n").unwrap();
    git(worktree, &["add", "session-only.txt"]);
    git(worktree, &["commit", "-qm", "test: session diverges"]);
    let head = git_output(worktree, &["rev-parse", "HEAD"]);
    let target = advance_main(tmp.path(), "main-advance.txt");
    let refused = run(
        tmp.path(),
        &[
            "start",
            "--reuse",
            &session.worktree_path,
            "--task",
            "must not be recorded",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    let message = json_error::message(&refused);
    for detail in [
        "have diverged".to_owned(),
        format!("checkout_head={head}"),
        format!("recorded_base_sha={base}"),
        format!("target_sha={target}"),
        "ahead=1 behind=1".to_owned(),
        "prior_observation_age_seconds=".to_owned(),
    ] {
        assert!(message.contains(&detail), "missing {detail}: {message}");
    }
    assert_eq!(git_output(worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read_to_string(worktree.join("session-only.txt")).unwrap(),
        "owned source\n"
    );
    let unchanged = stored_session(&tmp, session.id);
    assert_eq!(unchanged.task, session.task);
    assert_eq!(unchanged.diff_base, session.diff_base);
}
