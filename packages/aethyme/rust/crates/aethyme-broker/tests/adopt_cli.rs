use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;

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

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "README.md", ".gitignore"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
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
        String::from_utf8_lossy(&conflict.stderr).contains("planned lease"),
        "{}",
        String::from_utf8_lossy(&conflict.stderr)
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

    let output = common::broker_cli(CLI, &["start", "--task", "without a tab", "--json"])
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

    let output = common::broker_cli(
        CLI,
        &[
            "start",
            "--adopt",
            "--reuse",
            "--task",
            "follow-up",
            "--json",
        ],
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
    let output = common::broker_cli(
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
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_CHAU7_MCP_BRIDGE", common::disabled_bridge_path())
        .output()
        .unwrap()
}

/// `run_verbatim` without the fixture override: the production default.
fn run_enforced(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_CHAU7_MCP_BRIDGE", common::disabled_bridge_path())
        .output()
        .unwrap()
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
        Broker::open(tmp.path())
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
fn adopt_cli_exposes_structured_reuse_drift_and_safe_guidance() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["start", "--adopt", "--task", "first"]));

    git(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("shared.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "shared.txt"]);
    git(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    git(tmp.path(), &["checkout", "-q", "main"]);
    let session_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(tmp.path().join("shared.txt"), "dirty session edit\n").unwrap();

    let json = stdout(&run(
        tmp.path(),
        &["start", "--reuse", "--task", "follow-up", "--json"],
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
        &["start", "--reuse", "--task", "render drift"],
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
    Broker::open(repo)
        .unwrap()
        .scope_overlaps_snapshot()
        .unwrap()
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
    serde_json::from_str::<serde_json::Value>(&adopted).expect("adopt --json stays pure JSON");
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

fn assert_main_checkout_refused(output: &Output) {
    assert!(
        !output.status.success(),
        "adopt unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(MAIN_CHECKOUT_REFUSED), "{stderr}");
}

/// #284 proposal 9: a new session on the repository's main checkout is
/// refused, however the path is spelled; work there would land on the
/// default branch before any gate runs.
#[test]
fn a_new_session_on_the_main_checkout_is_refused_however_it_is_spelled() {
    let tmp = fixture();
    let root = tmp.path();
    assert_main_checkout_refused(&run_enforced(root, &["start", "--adopt", "--task", "t"]));

    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("link-to-main");
    std::os::unix::fs::symlink(root, &link).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let trailing = format!("{}/", root.display());
    let dotted = root.join("sub").join("..");
    let mut spellings = vec![
        link.display().to_string(),
        trailing,
        dotted.display().to_string(),
    ];
    // APFS is case-insensitive by default: another case names the same tree.
    #[cfg(target_os = "macos")]
    spellings.push(root.display().to_string().to_uppercase());
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
        assert_main_checkout_refused(&output);
    }
    assert!(
        Broker::open(root)
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
    let output = Command::new(CLI)
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
        .env("AETHYME_ALLOW_MAIN_CHECKOUT", "1")
        .output()
        .unwrap();
    assert_main_checkout_refused(&output);
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
