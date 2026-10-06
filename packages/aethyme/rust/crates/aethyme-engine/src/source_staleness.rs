//! Whether the checkout a read command answers from is behind the code that
//! ships (issue #232).
//!
//! The primary checkout tracks `main`, and promoted work reaches
//! `aethyme/integration` before it reaches `main`, so the primary checkout is
//! never the newest source even when it is perfectly synced. An agent that
//! researches there reports already-fixed work as missing. Nothing it does is
//! a write the broker could refuse, so the read surface itself has to say how
//! far behind the answered tree is.
//!
//! Detection is local and cheap: it compares `HEAD` with refs that already
//! exist in the repository (`aethyme/integration` and the fetched upstream
//! default branch). It never fetches, so a reference that is itself stale
//! understates the gap rather than slowing the command down.

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use serde_json::{Value, json};

/// Local refs that can hold newer source than the checkout. On a tie the
/// first listed names the gap, so `origin/main` is preferred to the
/// equivalent `origin/HEAD`.
const REFERENCE_CANDIDATES: &[&str] = &[
    "refs/heads/aethyme/integration",
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/remotes/origin/HEAD",
];

/// The distance between the answered checkout and the freshest local ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Staleness {
    /// Commit the answer was computed from.
    pub head: String,
    /// Short name of the ref the checkout is behind, e.g. `aethyme/integration`.
    pub reference: String,
    /// Commits reachable from `reference` but not from `head`.
    pub behind: u64,
    /// Whether the checkout is a linked (session) worktree rather than the
    /// primary checkout; it decides which remedy is offered.
    pub linked_worktree: bool,
    /// The command that gets a fresh tree to read from.
    pub suggestion: String,
}

impl Staleness {
    /// One stderr line for agents and operators.
    pub fn warning_line(&self) -> String {
        format!(
            "warning: this checkout is {} commit(s) behind {}; answers may report already-fixed code as missing. {}",
            self.behind, self.reference, self.suggestion
        )
    }

    /// Mark an answer document as computed from a stale tree: add
    /// `source_staleness` and withdraw `safe_to_use_as_answer`, at the top
    /// level and inside `trust_policy`, since an answer from an old tree is
    /// not an answer about the current code.
    pub fn apply(&self, answer: &mut Value) {
        let Some(object) = answer.as_object_mut() else {
            return;
        };
        object.insert(
            "source_staleness".to_string(),
            json!({
                "head": self.head,
                "reference": self.reference,
                "behind": self.behind,
                "linked_worktree": self.linked_worktree,
                "suggestion": self.suggestion,
            }),
        );
        object.insert("safe_to_use_as_answer".to_string(), Value::Bool(false));
        if let Some(trust) = object
            .get_mut("trust_policy")
            .and_then(Value::as_object_mut)
        {
            trust.insert("safe_to_use_as_answer".to_string(), Value::Bool(false));
        }
    }
}

/// How far `repo`'s checkout is behind the freshest local reference, or
/// `None` when it is current, is not a Git checkout, or has no reference to
/// compare with. Every Git failure answers `None`: the check must never turn
/// a read into an error.
pub fn detect(repo: &Path) -> Option<Staleness> {
    let identity = git(
        repo,
        &["rev-parse", "HEAD", "--git-dir", "--git-common-dir"],
    )?;
    let mut lines = identity.lines();
    let head = lines.next()?.trim().to_string();
    let git_dir = lines.next()?.trim().to_string();
    let common_dir = lines.next()?.trim().to_string();
    let linked_worktree = canonical(repo, &git_dir) != canonical(repo, &common_dir);

    let mut list_refs = vec!["for-each-ref", "--format=%(refname)"];
    list_refs.extend(REFERENCE_CANDIDATES);
    let present = git(repo, &list_refs)?;
    let mut worst: Option<(String, u64)> = None;
    for reference in REFERENCE_CANDIDATES
        .iter()
        .filter(|candidate| present.lines().any(|line| line.trim() == **candidate))
    {
        let range = format!("{head}..{reference}");
        let Some(count) = git(repo, &["rev-list", "--count", &range])
            .and_then(|text| text.trim().parse::<u64>().ok())
        else {
            continue;
        };
        if count > worst.as_ref().map_or(0, |(_, behind)| *behind) {
            worst = Some((short_name(reference), count));
        }
    }
    let (reference, behind) = worst?;
    let suggestion = if linked_worktree {
        format!(
            "Merge {} into this session (`aethyme broker sync --session <id>`) or read the fresh file with `git show {}:<path>`.",
            shell_quote(&reference),
            shell_quote(&reference)
        )
    } else {
        format!(
            "Research in a broker session worktree (`aethyme broker start --task \"<task>\"`), which is cut from the integration tip, or read the fresh file with `git show {}:<path>`.",
            shell_quote(&reference)
        )
    };
    Some(Staleness {
        head,
        reference,
        behind,
        linked_worktree,
        suggestion,
    })
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn canonical(repo: &Path, dir: &str) -> std::path::PathBuf {
    let path = Path::new(dir);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo.join(path)
    };
    path.canonicalize().unwrap_or(path)
}

fn short_name(reference: &str) -> String {
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/"))
        .unwrap_or(reference)
        .to_string()
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'@')
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stale() -> Staleness {
        Staleness {
            head: "a".repeat(40),
            reference: "aethyme/integration".to_string(),
            behind: 3,
            linked_worktree: false,
            suggestion: "read elsewhere".to_string(),
        }
    }

    // Through the CLI a graph-free answer is never safe, so only this pins
    // that a stale tree withdraws an answer a graph would have marked safe.
    #[test]
    fn a_stale_tree_withdraws_an_answer_marked_safe() {
        let mut answer = json!({
            "safe_to_use_as_answer": true,
            "trust_policy": {"safe_to_use_as_answer": true, "trust_policy": "answer_candidate"},
        });
        stale().apply(&mut answer);
        assert_eq!(answer["safe_to_use_as_answer"], false);
        assert_eq!(answer["trust_policy"]["safe_to_use_as_answer"], false);
        assert_eq!(answer["source_staleness"]["behind"], 3);
        assert_eq!(
            answer["source_staleness"]["reference"],
            "aethyme/integration"
        );
    }

    #[test]
    fn suggestions_quote_reference_names() {
        assert_eq!(shell_quote("origin/main"), "origin/main");
        assert_eq!(shell_quote("it's $(x)"), "'it'\\''s $(x)'");
    }
}
