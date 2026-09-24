//! `--help` works on every command and subcommand, and has no side effects.
//!
//! Each invocation runs in a fresh repository with a throwaway `HOME`. It must
//! exit 0, print its help on stdout, and leave no file behind in either
//! directory. The motivating failure: `graph materialize --help` treated
//! `--help` as noise, defaulted `--repo` to `.`, and built the graph store.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Every command line whose help must work, without the trailing `--help`.
const COMMANDS: &[&str] = &[
    // top level
    "explore",
    "verify-targets",
    "explore-summary",
    "graph",
    "graph status",
    "graph units",
    "graph materialize",
    "graph refresh",
    "graph refresh plan",
    "graph refresh execute",
    "graph refresh recover",
    "graph impact",
    "graph node",
    "graph children",
    "graph parents",
    "graph callers",
    "graph callees",
    "graph docs",
    "graph configs",
    "graph expand",
    "graph overview",
    "analyze",
    "analyze dead-code",
    "facts",
    "facts public-functions",
    "facts function-usage",
    "intents",
    "repo",
    "repo ingest",
    "repo inspect",
    "repo warm",
    "repo clear-cache",
    "repo engine-info",
    "repo deploy-skills",
    "repo compile-skills",
    "repo init-onboarding-overrides",
    "repo validate-onboarding-overrides",
    "repo init-agents-overrides",
    "repo validate-agents-overrides",
    "repo experience-telemetry",
    "repo experience-status",
    "repo commit-message-template",
    "repo lint-commit-message",
    "repo hook-envelope",
    "repo record-wrapper-invocation",
    "task",
    "task pack",
    "task context",
    "task anchors",
    "task scope",
    "task next",
    "task expand",
    "task explain",
    "query",
    "query symbol",
    "query deps",
    "query impact",
    "root",
    "root show",
    "root set",
    "hook",
    "hook SessionStart",
    "plugin",
    "plugin install",
    "plugin remove",
    "plugin status",
    "update",
    "update check",
    "update plan",
    "update execute",
    "upgrade",
    "upgrade plan",
    "upgrade apply",
    "upgrade recover",
    "certify",
    "init",
    "deploy",
    "deploy verify",
    "deploy bridge",
    "deploy plan",
    "deploy execute",
    "readiness",
    "ai-ready",
    "quality",
    "quality inspect",
    "autofix",
    "enhance",
    "enhance deploy",
    "enhance verify",
    // broker: the six public verbs and their merged forms
    "broker",
    "broker start",
    "broker status",
    "broker status readiness",
    "broker status readiness plan",
    "broker status readiness apply",
    "broker status readiness recover",
    "broker status doctor",
    "broker submit",
    "broker submit prepare",
    "broker submit promote",
    "broker submit promotion-record",
    "broker finish",
    "broker finish close",
    "broker finish cleanup",
    "broker unblock",
    "broker gc",
    "broker gc plan",
    "broker gc apply",
    "broker gc reclaim",
    "broker gc storage",
    "broker gc reap",
    "broker advanced",
];

/// Every internal broker subcommand. Each must answer help both under
/// `advanced` and under its old top-level spelling.
const BROKER_INTERNAL: &[&str] = &[
    "readiness",
    "worktree-root",
    "adopt",
    "start-agent",
    "report",
    "quality-report",
    "external-events",
    "reclaim",
    "deliveries",
    "review",
    "prepare",
    "console",
    "resources",
    "agents",
    "leases",
    "exec",
    "git",
    "gh",
    "advisories",
    "exposures",
    "note",
    "operations",
    "gates",
    "watch",
    "pr",
    "hooks",
    "repair",
    "representation",
    "main",
    "promotion-record",
    "checkpoint",
    "queue",
    "promote",
    "ship",
    "integration",
    "events",
    "metrics",
    "blockers",
    "doctor",
    "quick-test",
    "trust",
    "verify-loop",
    "e2e",
    "init",
    "certify",
    "scaffold",
    "handoff",
    "close",
    "worktrees",
    "storage",
    "cleanup",
    "check-contract",
];

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

fn check(repo: &Path, home: &Path, line: &str, flag: &str, problems: &mut Vec<String>) {
    let mut args: Vec<&str> = line.split_whitespace().collect();
    args.push(flag);
    let git_before = files_under(&repo.join(".git"));
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("AETHYME_CACHE_DIR", home.join("cache"))
        .env_remove("AETHYME_REPO")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme");
    let invocation = args.join(" ");
    if output.status.code() != Some(0) {
        problems.push(format!(
            "`aethyme {invocation}` exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        problems.push(format!("`aethyme {invocation}` printed no help on stdout"));
    }
    let written: Vec<PathBuf> = files_under(repo)
        .into_iter()
        .chain(files_under(home))
        .collect();
    if !written.is_empty() {
        problems.push(format!("`aethyme {invocation}` wrote {written:?}"));
        for path in written {
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
    }
    if files_under(&repo.join(".git")) != git_before {
        problems.push(format!("`aethyme {invocation}` changed .git"));
    }
}

#[test]
fn help_works_everywhere_without_side_effects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);

    let mut lines: Vec<String> = COMMANDS.iter().map(|line| (*line).to_string()).collect();
    for verb in BROKER_INTERNAL {
        lines.push(format!("broker advanced {verb}"));
        lines.push(format!("broker {verb}"));
    }

    let mut problems = Vec::new();
    check(&repo, &home, "", "--help", &mut problems);
    for line in &lines {
        check(&repo, &home, line, "--help", &mut problems);
    }
    // `-h` is the same request; spot-check it across the kinds of route.
    for line in [
        "graph materialize",
        "task",
        "enhance deploy",
        "broker",
        "broker gc reap",
        "update check",
        "deploy",
    ] {
        check(&repo, &home, line, "-h", &mut problems);
    }
    assert!(
        problems.is_empty(),
        "{} help problem(s):\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}

#[test]
fn help_after_the_command_separator_belongs_to_the_command() {
    // `broker exec -- cmd --help` must reach exec (which then refuses for a
    // missing session) rather than print broker help.
    let tmp = tempfile::tempdir().expect("tempdir");
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["broker", "advanced", "exec", "--", "ls", "--help"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme");
    assert_ne!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Usage"));
}
