//! "Land the shared change first": `broker status` advice for two live
//! sessions that changed the same lines, or declared conflicting intents on
//! the same symbol, in a repository that delivers through pull requests.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, ScopeKind, ScopeOperation};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const PUSH_LANE: &str = "schema = 1\n[delivery]\npush_session_branches = true\n";
const ADVICE: &str = "coordination.land-shared-edit-first";

fn git_at(repo: &Path, args: &[&str], date: Option<&str>) -> String {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(repo: &Path, args: &[&str]) -> String {
    git_at(repo, args, None)
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

/// A repository with a bare `origin`, a twelve-line `lines.txt` on the
/// default branch, and (when `config` is non-empty) that config committed as
/// `.aethyme/config.toml` -- the only copy the push policy trusts.
fn fixture(config: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    let lines: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    std::fs::write(repo.join("lines.txt"), lines).unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    if !config.is_empty() {
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/config.toml"), config).unwrap();
        git(&repo, &["add", "-f", ".aethyme/config.toml"]);
        git(&repo, &["commit", "-qm", "policy"]);
    }
    let origin = tmp.path().join("origin.git");
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);
    Fixture { _tmp: tmp, repo }
}

fn start(repo: &Path, task: &str) -> (i64, PathBuf) {
    let mut broker = Broker::open(repo).unwrap();
    let session = broker.start_worktree(task, None).unwrap();
    (session.id, PathBuf::from(session.worktree_path))
}

/// Rewrite the given 1-based lines of `lines.txt` and commit at `date`.
fn edit_lines(worktree: &Path, lines: &[usize], tag: &str, date: &str) {
    let path = worktree.join("lines.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    let edited: String = text
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if lines.contains(&(index + 1)) {
                format!("{line} ({tag})\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    std::fs::write(&path, edited).unwrap();
    git(worktree, &["add", "lines.txt"]);
    git_at(worktree, &["commit", "-qm", tag], Some(date));
}

fn advice(repo: &Path) -> Vec<serde_json::Value> {
    let output = run(repo, &["status", "--refresh", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["id"] == ADVICE)
        .cloned()
        .collect()
}

fn commands(row: &serde_json::Value) -> Vec<String> {
    row["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|command| command.as_str().unwrap().to_string())
        .collect()
}

/// The later session committed the shared lines first, so it -- not the
/// lower id -- is told to land them, and both get their rebase command.
#[test]
fn overlapping_committed_hunks_name_the_earlier_committer_and_the_commands() {
    let fixture = fixture(PUSH_LANE);
    let (_, first_worktree) = start(&fixture.repo, "first");
    let (second, second_worktree) = start(&fixture.repo, "second");
    edit_lines(&second_worktree, &[5], "second", "2026-01-01T10:00:00Z");
    edit_lines(&first_worktree, &[5, 6], "first", "2026-01-01T11:00:00Z");

    let rows = advice(&fixture.repo);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    assert_eq!(row["session_id"], second, "{row:#}");
    assert_eq!(row["severity"], "warning");
    let summary = row["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(&format!(
            "Land the shared change first: session {second} commits only the shared edit to lines.txt"
        )),
        "{summary}"
    );
    assert!(summary.contains("origin/main"), "{summary}");
    let evidence = row["evidence"].to_string();
    assert!(
        evidence.contains(&format!(
            "session {second} committed the shared change first"
        )),
        "{evidence}"
    );
    let commands = commands(row);
    assert_eq!(
        commands[0],
        format!("aethyme broker push --session {second} --pr")
    );
    for worktree in [&second_worktree, &first_worktree] {
        let expected = format!("git -C '{}' fetch origin", worktree.display());
        assert!(
            commands
                .iter()
                .any(|command| command.starts_with(&expected)
                    && command.ends_with("rebase origin/main")),
            "missing rebase for {}: {commands:#?}",
            worktree.display()
        );
    }
}

/// Git merges changes separated by an untouched line; so does the advice.
#[test]
fn disjoint_hunks_in_one_file_get_no_advice() {
    let fixture = fixture(PUSH_LANE);
    let (_, first_worktree) = start(&fixture.repo, "first");
    let (_, second_worktree) = start(&fixture.repo, "second");
    edit_lines(&first_worktree, &[2], "first", "2026-01-01T10:00:00Z");
    edit_lines(&second_worktree, &[10], "second", "2026-01-01T11:00:00Z");

    assert!(advice(&fixture.repo).is_empty());
}

/// Without the push lane, landing a PR first is not how work arrives.
#[test]
fn without_the_push_policy_there_is_no_advice() {
    let fixture = fixture("");
    let (_, first_worktree) = start(&fixture.repo, "first");
    let (_, second_worktree) = start(&fixture.repo, "second");
    edit_lines(&first_worktree, &[5], "first", "2026-01-01T10:00:00Z");
    edit_lines(&second_worktree, &[5], "second", "2026-01-01T11:00:00Z");

    assert!(advice(&fixture.repo).is_empty());
}

fn declare(repo: &Path, session: i64, symbol: &str, operation: ScopeOperation) {
    let mut broker = Broker::open(repo).unwrap();
    broker
        .capture_session_scopes(
            session,
            &[(ScopeKind::Symbol, symbol.to_string(), operation)],
            None,
        )
        .unwrap();
}

/// A rewrite under an extension is a high-severity collision before any line
/// is written; the rewriting session lands first.
#[test]
fn a_high_scope_collision_advises_the_rewriting_session_to_land_first() {
    let fixture = fixture(PUSH_LANE);
    let (first, _) = start(&fixture.repo, "first");
    let (second, _) = start(&fixture.repo, "second");
    declare(
        &fixture.repo,
        first,
        "PaymentService",
        ScopeOperation::Extend,
    );
    declare(
        &fixture.repo,
        second,
        "PaymentService",
        ScopeOperation::Replace,
    );

    let rows = advice(&fixture.repo);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["session_id"], second, "{:#}", rows[0]);
    assert!(
        rows[0]["summary"]
            .as_str()
            .unwrap()
            .contains("symbol PaymentService"),
        "{:#}",
        rows[0]
    );
}

/// Two extensions of one symbol are ordinary work, not a shared edit.
#[test]
fn a_low_scope_collision_gets_no_advice() {
    let fixture = fixture(PUSH_LANE);
    let (first, _) = start(&fixture.repo, "first");
    let (second, _) = start(&fixture.repo, "second");
    declare(
        &fixture.repo,
        first,
        "PaymentService",
        ScopeOperation::Extend,
    );
    declare(
        &fixture.repo,
        second,
        "PaymentService",
        ScopeOperation::Extend,
    );

    assert!(advice(&fixture.repo).is_empty());
}
