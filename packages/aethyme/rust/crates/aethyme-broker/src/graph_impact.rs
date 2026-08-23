//! Advisory graph-impact provider boundary.
//!
//! The broker owns the safety contract around semantic gate advice, but not
//! the graph engine or its storage. Providers therefore return degraded
//! outcomes as data instead of errors: a cold, stale, or broken graph must not
//! prevent path-selected gates from being reported or run.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Maximum number of provider-ranked impact paths admitted to one report.
pub const GRAPH_IMPACT_RESULT_LIMIT: usize = 64;

/// Inputs available to an advisory graph-impact provider.
#[derive(Debug, Clone, Copy)]
pub struct GraphImpactQuery<'a> {
    pub repo_root: &'a Path,
    pub changed_files: &'a [String],
    pub max_results: usize,
}

/// Availability/result state returned by a graph-impact provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphImpactStatus {
    Ready,
    GraphMissing,
    GraphStale,
    ProviderError,
}

impl GraphImpactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::GraphMissing => "graph_missing",
            Self::GraphStale => "graph_stale",
            Self::ProviderError => "provider_error",
        }
    }
}

/// Provider output before the broker derives advisory gate suggestions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphImpactLookup {
    pub status: GraphImpactStatus,
    pub impacted_paths: Vec<String>,
    pub truncated: bool,
    pub explanation: String,
}

impl GraphImpactLookup {
    pub fn ready(
        impacted_paths: Vec<String>,
        truncated: bool,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            status: GraphImpactStatus::Ready,
            impacted_paths,
            truncated,
            explanation: explanation.into(),
        }
    }

    pub fn graph_missing(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::GraphMissing, explanation)
    }

    pub fn graph_stale(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::GraphStale, explanation)
    }

    pub fn provider_error(explanation: impl Into<String>) -> Self {
        Self::degraded(GraphImpactStatus::ProviderError, explanation)
    }

    fn degraded(status: GraphImpactStatus, explanation: impl Into<String>) -> Self {
        Self {
            status,
            impacted_paths: Vec::new(),
            truncated: false,
            explanation: explanation.into(),
        }
    }

    /// Enforce the broker-side result contract even when a provider returns
    /// duplicates, unsafe paths, or more entries than requested. First-seen
    /// order is provider ranking and is preserved.
    pub(crate) fn bounded(mut self, limit: usize) -> Self {
        if self.status != GraphImpactStatus::Ready {
            self.impacted_paths.clear();
            self.truncated = false;
            return self;
        }

        let original_len = self.impacted_paths.len();
        let mut seen = HashSet::new();
        self.impacted_paths
            .retain(|path| is_safe_repo_relative(path) && seen.insert(path.clone()));
        if self.impacted_paths.len() > limit {
            self.impacted_paths.truncate(limit);
        }
        self.truncated |= original_len > self.impacted_paths.len();
        self
    }
}

/// Read-only source of semantic impact paths. Implementations must not mutate
/// graph or broker state and must translate expected graph failures into a
/// [`GraphImpactLookup`] outcome.
pub trait GraphImpactProvider: Send + Sync {
    fn name(&self) -> &str;
    fn lookup(&self, query: &GraphImpactQuery<'_>) -> GraphImpactLookup;
}

/// Default availability provider used until the engine adapter supplies
/// caller-edge paths. It identifies cold/stale stores without opening or
/// mutating the engine database.
#[derive(Debug, Default)]
pub struct GraphStoreImpactProvider;

impl GraphImpactProvider for GraphStoreImpactProvider {
    fn name(&self) -> &str {
        "graph_store_probe"
    }

    fn lookup(&self, query: &GraphImpactQuery<'_>) -> GraphImpactLookup {
        let store_path = query.repo_root.join(".aethyme/graph_store.redb");
        let fragments_path = query.repo_root.join(".aethyme/graph");
        if !store_path.is_file() {
            return GraphImpactLookup::graph_missing(
                "no .aethyme/graph_store.redb found; semantic suggestions are unavailable",
            );
        }

        let store_modified = match modified(&store_path) {
            Ok(modified) => modified,
            Err(error) => {
                return GraphImpactLookup::provider_error(format!(
                    "could not inspect .aethyme/graph_store.redb freshness: {error}"
                ));
            }
        };
        let newest_fragment = match newest_modified(&fragments_path) {
            Ok(modified) => modified,
            Err(error) => {
                return GraphImpactLookup::provider_error(format!(
                    "could not inspect .aethyme/graph fragments: {error}"
                ));
            }
        };
        if newest_fragment.is_some_and(|fragment| fragment > store_modified) {
            return GraphImpactLookup::graph_stale(
                ".aethyme/graph contains fragments newer than graph_store.redb; rebuild the graph before using semantic suggestions",
            );
        }

        GraphImpactLookup::ready(
            Vec::new(),
            false,
            "graph store is present and not older than its fragments; the availability provider returned no additional impact paths",
        )
    }
}

fn modified(path: &Path) -> std::io::Result<SystemTime> {
    std::fs::metadata(path)?.modified()
}

fn newest_modified(root: &Path) -> std::io::Result<Option<SystemTime>> {
    if !root.exists() {
        return Ok(None);
    }

    let mut newest = Some(modified(root)?);
    let mut stack = vec![PathBuf::from(root)];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                newest = newest.max(Some(modified(&path)?));
            }
        }
    }
    Ok(newest)
}

fn is_safe_repo_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(root: &Path) -> GraphImpactQuery<'_> {
        GraphImpactQuery {
            repo_root: root,
            changed_files: &[],
            max_results: GRAPH_IMPACT_RESULT_LIMIT,
        }
    }

    #[test]
    fn bounded_results_preserve_first_seen_order_and_mark_omissions() {
        let lookup = GraphImpactLookup::ready(
            vec![
                "src/first.rs".into(),
                "../outside.rs".into(),
                "src/first.rs".into(),
                "src/second.rs".into(),
                "src/third.rs".into(),
            ],
            false,
            "fixture",
        )
        .bounded(2);

        assert_eq!(
            lookup.impacted_paths,
            vec!["src/first.rs".to_string(), "src/second.rs".to_string()]
        );
        assert!(lookup.truncated);
    }

    #[test]
    fn degraded_results_never_carry_provider_paths() {
        let mut lookup = GraphImpactLookup::graph_stale("stale fixture");
        lookup.impacted_paths.push("src/unsafe.rs".into());
        lookup.truncated = true;

        let bounded = lookup.bounded(GRAPH_IMPACT_RESULT_LIMIT);
        assert!(bounded.impacted_paths.is_empty());
        assert!(!bounded.truncated);
    }

    #[test]
    fn default_provider_distinguishes_missing_ready_and_stale_graphs() {
        let missing = tempfile::tempdir().unwrap();
        let provider = GraphStoreImpactProvider;
        assert_eq!(
            provider.lookup(&query(missing.path())).status,
            GraphImpactStatus::GraphMissing
        );

        let ready = tempfile::tempdir().unwrap();
        let ready_aethyme = ready.path().join(".aethyme");
        std::fs::create_dir_all(&ready_aethyme).unwrap();
        std::fs::write(ready_aethyme.join("graph_store.redb"), "fixture").unwrap();
        assert_eq!(
            provider.lookup(&query(ready.path())).status,
            GraphImpactStatus::Ready
        );

        let stale = tempfile::tempdir().unwrap();
        let stale_aethyme = stale.path().join(".aethyme");
        std::fs::create_dir_all(stale_aethyme.join("graph")).unwrap();
        std::fs::write(stale_aethyme.join("graph_store.redb"), "fixture").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(stale_aethyme.join("graph/newer.bin"), "fixture").unwrap();
        assert_eq!(
            provider.lookup(&query(stale.path())).status,
            GraphImpactStatus::GraphStale
        );
    }
}
