//! #1238 end to end: an outcome report under the default `deny` policy
//! writes nothing to GitHub, still tells the live owner, and `doctor` shows
//! the refusal.

mod common;

use std::path::Path;
use std::process::Command;

use aethyme_broker::Broker;
use common::{FixtureRepository, open_fixture_repository_broker};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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

fn child_command(broker: &Broker, cwd: &Path, args: &[&str]) -> Command {
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(cwd);
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    command
}

const CONFIG: &str = r#"
[[automation.outcomes]]
source = "prebuild:db"
notify_owners = true
paths = ["db/**"]
"#;

#[test]
fn a_denied_outcome_stays_local_tells_the_owner_and_shows_in_doctor() {
    let fixture = FixtureRepository::new();
    let repo = fixture.path().to_path_buf();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/config.toml\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(repo.join(".aethyme/config.toml"), CONFIG).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/n.git"],
    );
    let base = git(&repo, &["rev-parse", "HEAD"]);

    let worktree = repo.join(".aethyme/worktrees/owner");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/owner",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::create_dir_all(worktree.join("db")).unwrap();
    std::fs::write(worktree.join("db/001.sql"), "create table t ();\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "add migration"]);
    git(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "agent/owner",
            "-m",
            "Merge pull request #5 from o/agent/owner",
        ],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let owner = broker.adopt(&worktree, Some("owner")).unwrap();
    let range = format!("{base}..main");
    let mut report = child_command(
        &broker,
        &repo,
        &[
            "advanced",
            "outcomes",
            "report",
            "--source",
            "prebuild:db",
            "--failed",
            "--title",
            "db template prebuild failed",
            "--commits",
            &range,
            "--json",
        ],
    );
    let mut unknown = child_command(
        &broker,
        &repo,
        &[
            "advanced",
            "outcomes",
            "report",
            "--source",
            "gate:unit",
            "--recovered",
        ],
    );
    let mut doctor = child_command(&broker, &repo, &["status", "doctor", "--json"]);
    let mut notes = child_command(
        &broker,
        &worktree,
        &[
            "advanced",
            "note",
            "list",
            "--session",
            &owner.id.to_string(),
            "--json",
        ],
    );
    drop(broker);

    let output = report.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], "aethyme.outcome-report/v1");
    assert_eq!(report["fingerprint"], "outcome:prebuild:db");
    let actions: Vec<(String, String)> = report["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["kind"].as_str().unwrap().into(),
                a["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            ("owners".to_string(), "done".to_string()),
            ("issue".into(), "refused".into()),
            ("note".into(), "done".into()),
        ],
        "{report}"
    );
    assert_eq!(report["actions"][1]["code"], "automation_issues_denied");
    assert_eq!(report["owners"][0]["session_id"], owner.id);

    let unknown = unknown.output().unwrap();
    assert_eq!(unknown.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no_outcome_rule"));

    let doctor = doctor.output().unwrap();
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let refusals = doctor["automation_issue_refusals"]
        .as_array()
        .unwrap_or_else(|| panic!("{doctor}"));
    assert_eq!(refusals.len(), 1, "{doctor}");
    assert_eq!(refusals[0]["fingerprint"], "outcome:prebuild:db");
    assert_eq!(refusals[0]["code"], "automation_issues_denied");

    let notes: serde_json::Value = serde_json::from_slice(&notes.output().unwrap().stdout).unwrap();
    assert_eq!(
        notes["automation_notes"][0]["message"],
        "db template prebuild failed — no issue filed (automation_issues_denied)"
    );
    assert_eq!(
        notes["automation_notes"][0]["sender"],
        "automation:prebuild.db"
    );
}
