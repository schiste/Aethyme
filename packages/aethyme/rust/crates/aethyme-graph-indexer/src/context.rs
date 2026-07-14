//! `IndexerContext`: the values that every indexing operation needs
//! to know, independent of which file is being processed.

use std::path::PathBuf;

/// Repo-wide state passed to every indexer call.
///
/// `repo_name` is the logical namespace identifier used as the
/// `<repo>` component of every NodeId (per
/// `aethyme_graph_schema::NodeId::new`). It must satisfy NodeId's
/// validation (non-empty, no `:`); the indexer enforces this at
/// context construction.
///
/// `repo_root` is the absolute filesystem path. All source paths
/// returned by indexers are RELATIVE to this root, matching the
/// `source_path` argument expected by the storage layer's
/// `fragment_path` helper.
///
/// `engine_version` is the pinned engine version (the same string
/// written to `.aethyme/engine-version` by
/// `aethyme_graph_storage::bootstrap_repo`). Stored on the context
/// so produced fragments can be cross-checked against the running
/// engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerContext {
    repo_name: Box<str>,
    repo_root: PathBuf,
    engine_version: Box<str>,
}

impl IndexerContext {
    pub fn new(
        repo_name: &str,
        repo_root: impl Into<PathBuf>,
        engine_version: &str,
    ) -> Result<Self, IndexerContextError> {
        if repo_name.is_empty() {
            return Err(IndexerContextError::EmptyRepoName);
        }
        if repo_name.contains(':') {
            return Err(IndexerContextError::RepoNameContainsColon {
                given: repo_name.into(),
            });
        }
        if engine_version.is_empty() {
            return Err(IndexerContextError::EmptyEngineVersion);
        }
        let repo_root = repo_root.into();
        if !repo_root.is_absolute() {
            return Err(IndexerContextError::RelativeRepoRoot { given: repo_root });
        }
        Ok(IndexerContext {
            repo_name: repo_name.into(),
            repo_root,
            engine_version: engine_version.into(),
        })
    }

    pub fn repo_name(&self) -> &str {
        &self.repo_name
    }
    pub fn repo_root(&self) -> &std::path::Path {
        &self.repo_root
    }
    pub fn engine_version(&self) -> &str {
        &self.engine_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexerContextError {
    EmptyRepoName,
    RepoNameContainsColon { given: Box<str> },
    EmptyEngineVersion,
    RelativeRepoRoot { given: PathBuf },
}

impl std::fmt::Display for IndexerContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyRepoName => f.write_str("IndexerContext: repo_name must not be empty"),
            Self::RepoNameContainsColon { given } => write!(
                f,
                "IndexerContext: repo_name {given:?} contains ':', \
                 which is forbidden by NodeId's identifier format"
            ),
            Self::EmptyEngineVersion => {
                f.write_str("IndexerContext: engine_version must not be empty")
            }
            Self::RelativeRepoRoot { given } => write!(
                f,
                "IndexerContext: repo_root {given:?} is relative; \
                 absolute path required so source_path arithmetic is \
                 unambiguous"
            ),
        }
    }
}

impl std::error::Error for IndexerContextError {}
