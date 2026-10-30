//! Change-based profile inference (#1216).
//!
//! `[[rules]]` map changed paths to a minimum profile. The session's diff
//! against its base fires rules; the inferred minimum is the highest-ranked
//! profile any fired rule names, or `default_profile` when none fires.
//!
//! Inference is **raise-only**: `env up` refuses a profile ranked below the
//! minimum and lists the rules and paths that fired. A higher profile is
//! allowed with a recorded reason. A wrong rule is fixed by editing
//! `environments.toml`, which is policy-classified and so reviewed; there is
//! deliberately no flag to go lower, because it would be the first thing an
//! agent reached for.
//!
//! Globs use `globset` with `literal_separator`, like gate triggers: `*`
//! stays within one path segment and `**` crosses directories, so
//! `src/**/*.rs` matches `src/a/b.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;

use super::compile::default_profile_name;
use super::spec::EnvironmentsFile;
use super::validate::effective_rank;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiredRule {
    pub rule: String,
    pub min_profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inference {
    /// The minimum profile for these paths.
    pub minimum: String,
    pub rank: u32,
    pub fired: Vec<FiredRule>,
    /// Changed paths no rule matched.
    pub unmatched: Vec<String>,
    pub changed: usize,
}

fn rank_of(file: &EnvironmentsFile, profile: &str) -> Option<u32> {
    file.profiles
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == profile)
        .map(|(index, p)| effective_rank(index, p))
}

pub fn profile_rank(file: &EnvironmentsFile, profile: &str) -> Option<u32> {
    rank_of(file, profile)
}

fn rule_set(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|error| format!("{pattern:?}: {error}"))?,
        );
    }
    builder.build().map_err(|error| error.to_string())
}

/// The minimum profile for `paths` under `file`'s rules.
pub fn infer(file: &EnvironmentsFile, paths: &[String]) -> Result<Inference, String> {
    let fallback = default_profile_name(file).ok_or("no profiles are declared")?;
    let mut minimum = (
        rank_of(file, fallback).ok_or("default profile is not declared")?,
        fallback.to_string(),
    );
    let mut fired = Vec::new();
    let mut matched = vec![false; paths.len()];
    for rule in &file.rules {
        let set = rule_set(&rule.paths)?;
        let hits: Vec<String> = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| set.is_match(path.as_str()))
            .map(|(index, path)| {
                matched[index] = true;
                path.clone()
            })
            .collect();
        if hits.is_empty() {
            continue;
        }
        let rank = rank_of(file, &rule.min_profile).ok_or_else(|| {
            format!(
                "rule {} names unknown profile {}",
                rule.name, rule.min_profile
            )
        })?;
        if rank > minimum.0 {
            minimum = (rank, rule.min_profile.clone());
        }
        fired.push(FiredRule {
            rule: rule.name.clone(),
            min_profile: rule.min_profile.clone(),
            reason: rule.reason.clone(),
            paths: hits,
        });
    }
    let unmatched = paths
        .iter()
        .zip(&matched)
        .filter(|(_, matched)| !**matched)
        .map(|(path, _)| path.clone())
        .collect();
    Ok(Inference {
        minimum: minimum.1,
        rank: minimum.0,
        fired,
        unmatched,
        changed: paths.len(),
    })
}

fn git(worktree: &Path, args: &[&str]) -> Result<String, String> {
    // The probed honest binary: a PATH wrapper may rewrite `git diff` (#176).
    let output = crate::git::git_command()
        .args(args)
        .current_dir(worktree)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Paths from `git diff --name-status -z -M` output: both sides of a rename
/// or copy, one path otherwise. `-z` keeps paths verbatim; without it Git
/// C-quotes non-ASCII and special characters and no rule would match them.
fn name_status_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = text.split('\0').filter(|f| !f.is_empty());
    while let Some(status) = fields.next() {
        let count = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        paths.extend(fields.by_ref().take(count).map(str::to_string));
    }
    paths
}

/// The base a session diff is measured from: the merge base with the local
/// integration branch, else with the remote default branch, else nothing.
pub fn diff_base(worktree: &Path) -> Option<String> {
    for candidate in [
        "aethyme/integration",
        "origin/HEAD",
        "origin/main",
        "origin/master",
    ] {
        if let Ok(base) = git(worktree, &["merge-base", "HEAD", candidate]) {
            let base = base.trim();
            if !base.is_empty() {
                return Some(base.to_string());
            }
        }
    }
    None
}

/// Every path the session changed: committed and uncommitted changes
/// against the base (both sides of renames) plus untracked files. Without a
/// base (no integration branch or remote default branch), only uncommitted
/// changes against `HEAD` count; the caller warns, since committed changes
/// are then invisible.
pub fn changed_paths(worktree: &Path) -> Result<(Option<String>, Vec<String>), String> {
    let base = diff_base(worktree);
    let against = match &base {
        Some(base) => Some(base.as_str()),
        None => git(worktree, &["rev-parse", "--verify", "-q", "HEAD"])
            .ok()
            .map(|_| "HEAD"),
    };
    let mut paths = match against {
        Some(against) => name_status_paths(&git(
            worktree,
            &["diff", "--name-status", "-z", "-M", against],
        )?),
        None => Vec::new(),
    };
    paths.extend(
        git(
            worktree,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?
        .split('\0')
        .filter(|l| !l.is_empty())
        .map(str::to_string),
    );
    paths.sort();
    paths.dedup();
    Ok((base, paths))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RangeReport {
    pub commits: usize,
    /// Commits per inferred minimum profile.
    pub profiles: BTreeMap<String, usize>,
    /// The rule that fired most often, per profile.
    pub top_rule: BTreeMap<String, String>,
}

/// Evaluate the rules for every commit in `range`, for tuning.
pub fn infer_range(
    worktree: &Path,
    file: &EnvironmentsFile,
    range: &str,
) -> Result<RangeReport, String> {
    let commits = git(worktree, &["rev-list", range])?;
    let mut profiles: BTreeMap<String, usize> = BTreeMap::new();
    let mut rule_counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut count = 0;
    for commit in commits.lines().filter(|l| !l.is_empty()) {
        let paths = name_status_paths(&git(
            worktree,
            &[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-status",
                "-z",
                "-r",
                "-M",
                // A merge is compared with its first parent, so it counts
                // what it brought in rather than nothing.
                "-m",
                "--first-parent",
                commit,
            ],
        )?);
        let inference = infer(file, &paths)?;
        *profiles.entry(inference.minimum.clone()).or_default() += 1;
        for fired in &inference.fired {
            *rule_counts
                .entry(fired.min_profile.clone())
                .or_default()
                .entry(fired.rule.clone())
                .or_default() += 1;
        }
        count += 1;
    }
    let top_rule = rule_counts
        .into_iter()
        .filter_map(|(profile, rules)| {
            rules
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .map(|(rule, _)| (profile, rule))
        })
        .collect();
    Ok(RangeReport {
        commits: count,
        profiles,
        top_rule,
    })
}

/// A session's live environment ranked below the profile its changes now
/// require (#1273), for `broker status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BelowMinimum {
    pub session_id: i64,
    pub env_id: String,
    pub profile: String,
    pub minimum: String,
    /// `rule -> min_profile (paths)` for each rule that fired.
    pub rules: Vec<String>,
}

/// Every live environment of `sessions` (id, worktree) ranked below the
/// minimum its worktree's changes infer. Only sessions with a live
/// environment are inferred, so the cost is one diff per such session.
/// Best effort: a worktree whose environments file is missing or invalid, or
/// whose diff cannot be read, is skipped (`env status` reports those).
pub fn below_minimum(
    store: &super::store::EnvironmentStore,
    sessions: &[(i64, std::path::PathBuf)],
) -> Vec<BelowMinimum> {
    let Ok(records) = store.list(None) else {
        return Vec::new();
    };
    let live: Vec<_> = records
        .into_iter()
        .filter(|r| r.phase.retains_worktree())
        .filter(|r| matches!(r.owner, super::compile::EnvironmentOwner::Worktree { .. }))
        .collect();
    let mut found = Vec::new();
    for (session_id, worktree) in sessions {
        let fingerprint = crate::console::worktree_fingerprint(worktree);
        let mine: Vec<_> = live
            .iter()
            .filter(|r| r.worktree_fingerprint == fingerprint)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let Ok(Some(loaded)) = super::load_repository_file(worktree) else {
            continue;
        };
        let Ok(file) = loaded.value else {
            continue;
        };
        let Ok((_, changed)) = changed_paths(worktree) else {
            continue;
        };
        let Ok(inference) = infer(&file, &changed) else {
            continue;
        };
        // Each worktree is judged by its best environment: a cheaper one kept
        // beside one that meets the minimum is not a problem to report.
        let best = mine
            .iter()
            .filter_map(|r| profile_rank(&file, &r.profile).map(|rank| (rank, *r)))
            .max_by_key(|(rank, _)| *rank);
        let Some((rank, record)) = best else {
            continue;
        };
        if rank < inference.rank {
            found.push(BelowMinimum {
                session_id: *session_id,
                env_id: record.env_id.clone(),
                profile: record.profile.clone(),
                minimum: inference.minimum.clone(),
                rules: inference
                    .fired
                    .iter()
                    .map(|f| format!("{} -> {} ({})", f.rule, f.min_profile, f.paths.join(", ")))
                    .collect(),
            });
        }
    }
    found
}
