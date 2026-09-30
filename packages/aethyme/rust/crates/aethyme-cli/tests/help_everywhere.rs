//! `--help` works on every command and subcommand, and has no side effects.
//!
//! Each invocation runs in a fresh repository with a throwaway `HOME`, and
//! every host-state, cache and worktree-root override points inside that
//! watched directory. It must exit 0, print its help on stdout, and leave no
//! file behind in the repository or anywhere under the watched directory. The motivating failure: `graph materialize --help` treated
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
    "deploy --generated-only",
    "deploy verify --generated-only",
    "ai-ready",
    "quality",
    "quality inspect",
    "autofix",
    // broker: the public verbs and their merged forms
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
    "broker push",
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

/// Every current advanced broker subcommand.
const BROKER_ADVANCED: &[&str] = &[
    "leases",
    "git",
    "gh",
    "operations",
    "exec",
    "ship",
    "review",
    "gates",
    "hooks",
    "trust",
    "agents",
    "handoff",
    "queue",
    "integration",
    "main",
    "representation",
    "checkpoint",
    "repair",
    "resources",
    "console",
    "advisories",
    "exposures",
    "note",
    "watch",
    "deliveries",
    "pr",
    "worktree-root",
    "report",
    "quality-report",
    "external-events",
    "events",
    "metrics",
    "worktrees",
    "scaffold",
    "quick-test",
    "verify-loop",
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
        .env("AETHYME_HOST_STATE_DIR", home.join("host-state"))
        .env("XDG_STATE_HOME", home.join("xdg-state"))
        .env("AETHYME_HOST_CACHE_DIR", home.join("host-cache"))
        .env("AETHYME_WORKTREE_ROOT", home.join("worktrees"))
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
    for verb in BROKER_ADVANCED {
        lines.push(format!("broker advanced {verb}"));
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
        "deploy --generated-only",
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
fn removed_spellings_return_migration_hints_without_side_effects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let git_before = files_under(&repo.join(".git"));

    for args in [
        &["readiness", "--help"][..],
        &["enhance", "deploy", "--help"][..],
        &["enhance", "verify", "--help"][..],
        &["broker", "readiness", "--help"][..],
        &["broker", "leases", "--help"][..],
        &["broker", "adopt", "--help"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("AETHYME_CACHE_DIR", home.join("cache"))
            .env("AETHYME_HOST_STATE_DIR", home.join("host-state"))
            .env("XDG_STATE_HOME", home.join("xdg-state"))
            .env("AETHYME_HOST_CACHE_DIR", home.join("host-cache"))
            .env("AETHYME_WORKTREE_ROOT", home.join("worktrees"))
            .env_remove("AETHYME_REPO")
            .stdin(Stdio::null())
            .output()
            .expect("run aethyme");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("removed in v0.8.8"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(files_under(&repo).is_empty());
    assert!(files_under(&home).is_empty());
    assert_eq!(files_under(&repo.join(".git")), git_before);
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
