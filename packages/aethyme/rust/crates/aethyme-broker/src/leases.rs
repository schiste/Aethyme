//! Lease refresh, overlap detection, and ignore rules (Phase 3).
//!
//! Leases are advisory in v0: overlaps warn, never block (an advanced
//! lease policy engine is an explicit non-goal). Implicit leases are
//! derived from each session's diff against its recorded base; explicit
//! leases are user claims, optionally with TTL. A lease path ending in
//! `/` is a directory claim and overlaps everything under it.

use std::collections::HashSet;
use std::path::Path;

use crate::types::Lease;

/// Files whose concurrent modification is expected and harmless —
/// lockfiles regenerate deterministically, so two sessions touching only
/// a lockfile must not warn (issue #14). Mirrors the autofixer safety
/// list (`src/autofixers/safety.py:LOCK_FILES`) plus broker/engine
/// artifacts.
const DEFAULT_IGNORED_FILENAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Gemfile.lock",
    "Pipfile.lock",
    "poetry.lock",
    "composer.lock",
    "Cargo.lock",
    "go.sum",
    "uv.lock",
];

/// Directory prefixes never leased (generated/derived trees plus the
/// broker's own state directory).
const DEFAULT_IGNORED_PREFIXES: &[&str] = &[".aethyme/", "node_modules/", "dist/", "build/"];

/// Ignore rules applied before paths become implicit leases. Extra
/// entries come from `.aethyme/config.toml` (`[leases] ignore = [...]`,
/// entries ending in `/` are prefixes).
#[derive(Debug, Clone, Default)]
pub struct LeaseIgnoreRules {
    extra_filenames: Vec<String>,
    extra_prefixes: Vec<String>,
}

impl LeaseIgnoreRules {
    /// Load extra rules from `<main_root>/.aethyme/config.toml`. Missing
    /// or unparseable config degrades to the defaults — ignore rules must
    /// never make the broker unusable.
    pub fn load(main_root: &Path) -> Self {
        let mut rules = Self::default();
        let path = main_root.join(".aethyme/config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return rules;
        };
        let Ok(value) = text.parse::<toml::Value>() else {
            return rules;
        };
        if let Some(entries) = value
            .get("leases")
            .and_then(|leases| leases.get("ignore"))
            .and_then(|ignore| ignore.as_array())
        {
            for entry in entries.iter().filter_map(|entry| entry.as_str()) {
                if let Some(prefix) = entry.strip_suffix('/') {
                    rules.extra_prefixes.push(format!("{prefix}/"));
                } else {
                    rules.extra_filenames.push(entry.to_string());
                }
            }
        }
        rules
    }

    pub fn is_ignored(&self, repo_relative: &str) -> bool {
        let filename = repo_relative.rsplit('/').next().unwrap_or(repo_relative);
        if DEFAULT_IGNORED_FILENAMES.contains(&filename)
            || self.extra_filenames.iter().any(|f| f == filename)
        {
            return true;
        }
        DEFAULT_IGNORED_PREFIXES
            .iter()
            .any(|prefix| repo_relative.starts_with(prefix))
            || self
                .extra_prefixes
                .iter()
                .any(|prefix| repo_relative.starts_with(prefix.as_str()))
    }
}

/// One detected overlap: two live sessions holding leases on the same
/// path or on nesting paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Overlap {
    /// Lower session id first — the pair is unordered.
    pub session_a: i64,
    pub session_b: i64,
    /// The narrower of the two overlapping paths.
    pub path: String,
}

/// Whether two lease-style paths overlap. Shared with the post-commit
/// conflict radar, which compares commit paths against lease paths.
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Directory claims end in '/'; a claim covers everything under it.
    (a.ends_with('/') && b.starts_with(a)) || (b.ends_with('/') && a.starts_with(b))
}

/// Pairwise overlap detection over active leases. O(n²) over leases held
/// by *different* sessions — fine at the 15-session design ceiling.
pub fn detect_overlaps(leases: &[Lease]) -> Vec<Overlap> {
    let mut seen = HashSet::new();
    let mut overlaps = Vec::new();
    for (i, a) in leases.iter().enumerate() {
        for b in &leases[i + 1..] {
            if a.session_id == b.session_id || !paths_overlap(&a.path, &b.path) {
                continue;
            }
            let (low, high) = if a.session_id < b.session_id {
                (a.session_id, b.session_id)
            } else {
                (b.session_id, a.session_id)
            };
            // Report the narrower path (the file, not the dir claim).
            let path = if a.path.len() >= b.path.len() {
                a.path.clone()
            } else {
                b.path.clone()
            };
            if seen.insert((low, high, path.clone())) {
                overlaps.push(Overlap {
                    session_a: low,
                    session_b: high,
                    path,
                });
            }
        }
    }
    overlaps.sort();
    overlaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LeaseKind;

    fn lease(session_id: i64, path: &str) -> Lease {
        Lease {
            id: 0,
            session_id,
            path: path.into(),
            kind: LeaseKind::Implicit,
            created_at: 0,
            expires_at: None,
            released_at: None,
        }
    }

    #[test]
    fn ignore_rules_cover_lockfiles_and_prefixes() {
        let rules = LeaseIgnoreRules::default();
        assert!(rules.is_ignored("pnpm-lock.yaml"));
        assert!(rules.is_ignored("packages/app/Cargo.lock"));
        assert!(rules.is_ignored(".aethyme/broker.db"));
        assert!(rules.is_ignored("node_modules/x/index.js"));
        assert!(!rules.is_ignored("src/auth.py"));
        assert!(!rules.is_ignored("locksmith.rs"));
    }

    #[test]
    fn overlap_matrix() {
        // Same file across sessions → overlap.
        let overlaps = detect_overlaps(&[lease(1, "src/auth.py"), lease(2, "src/auth.py")]);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].path, "src/auth.py");

        // Dir claim vs file under it (both directions).
        let overlaps = detect_overlaps(&[lease(1, "src/auth/"), lease(2, "src/auth/token.py")]);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].path, "src/auth/token.py");

        // Same session never overlaps itself; disjoint paths never overlap.
        assert!(detect_overlaps(&[lease(1, "a.py"), lease(1, "a.py")]).is_empty());
        assert!(detect_overlaps(&[lease(1, "src/a.py"), lease(2, "src/b.py")]).is_empty());
        // Prefix without dir marker is NOT an overlap (src vs src2 hazard).
        assert!(detect_overlaps(&[lease(1, "src"), lease(2, "src2/b.py")]).is_empty());

        // Duplicate pairs collapse.
        let overlaps = detect_overlaps(&[
            lease(1, "src/auth.py"),
            lease(2, "src/auth.py"),
            lease(2, "src/auth.py"),
        ]);
        assert_eq!(overlaps.len(), 1);
    }
}
