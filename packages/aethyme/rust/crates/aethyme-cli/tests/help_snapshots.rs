//! Snapshot of the whole `--help` surface: every top-level command and every
//! broker subcommand, run in a disposable repository.
//!
//! The point is review: a change to what the CLI tells a user shows up as a
//! diff under `tests/snapshots/help/`. Output identical to an earlier entry is
//! stored as a `same-as:` reference so a shared usage dump is recorded once.

mod support;

use std::path::Path;
use std::process::{Command, Stdio};

use support::snapshots::assert_snapshots;

const TOP_LEVEL: &[&str] = &[
    "explore",
    "verify-targets",
    "explore-summary",
    "graph",
    "analyze",
    "facts",
    "intents",
    "repo",
    "task",
    "query",
    "root",
    "hook",
    "plugin",
    "broker",
    "update",
    "upgrade",
    "certify",
    "init",
    "deploy",
    "readiness",
    "ai-ready",
    "quality",
    "autofix",
    "enhance",
];

const BROKER: &[&str] = &[
    "readiness",
    "worktree-root",
    "adopt",
    "start",
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
    "submit",
    "repair",
    "representation",
    "main",
    "promotion-record",
    "checkpoint",
    "queue",
    "promote",
    "ship",
    "integration",
    "status",
    "events",
    "metrics",
    "blockers",
    "unblock",
    "doctor",
    "quick-test",
    "trust",
    "verify-loop",
    "e2e",
    "init",
    "certify",
    "scaffold",
    "handoff",
    "finish",
    "close",
    "gc",
    "worktrees",
    "storage",
    "cleanup",
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

fn help(repo: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("AETHYME_REPO")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme");
    format!(
        "exit: {}\n--- stdout\n{}--- stderr\n{}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn slug(args: &[&str]) -> String {
    let joined = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .copied()
        .collect::<Vec<_>>()
        .join("-");
    if joined.is_empty() {
        "aethyme".into()
    } else {
        joined
    }
}

#[test]
fn help_surface_matches_snapshots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);

    let mut invocations: Vec<Vec<&str>> = vec![vec!["--help"]];
    for command in TOP_LEVEL {
        invocations.push(vec![command, "--help"]);
    }
    invocations.push(vec!["broker"]);
    for subcommand in BROKER {
        invocations.push(vec!["broker", subcommand, "--help"]);
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    for args in &invocations {
        let body = help(&repo, &home, args);
        let name = if args.len() == 1 && args[0] == "broker" {
            "broker-no-args".to_string()
        } else {
            slug(args)
        };
        let stored = match entries.iter().find(|(_, earlier)| *earlier == body) {
            Some((earlier, _)) => format!("same-as: {earlier}\n"),
            None => body,
        };
        entries.push((name, stored));
    }
    // Keep only full bodies searchable for later dedup lookups: a `same-as`
    // body never equals real output, so the lookup above stays correct.
    assert_snapshots("help", &entries);
}
