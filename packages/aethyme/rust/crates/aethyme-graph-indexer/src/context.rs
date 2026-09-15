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
    source_revision: Option<Box<str>>,
    source_tree_digest: Option<Box<str>>,
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
            source_revision: None,
            source_tree_digest: None,
        })
    }

    /// Bind indexing output to one exact source revision.  The plain
    /// constructor remains useful for library callers that are indexing an
    /// uncommitted filesystem snapshot; authoritative graph refreshes should
    /// use this method before writing coverage artifacts.
    pub fn with_source_revision(
        mut self,
        source_revision: &str,
    ) -> Result<Self, IndexerContextError> {
        if source_revision.is_empty() {
            return Err(IndexerContextError::EmptySourceRevision);
        }
        self.source_revision = Some(source_revision.into());
        Ok(self)
    }

    pub fn with_source_tree_digest(
        mut self,
        source_tree_digest: &str,
    ) -> Result<Self, IndexerContextError> {
        if source_tree_digest.is_empty() {
            return Err(IndexerContextError::EmptySourceTreeDigest);
        }
        self.source_tree_digest = Some(source_tree_digest.into());
        Ok(self)
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

    pub fn source_revision(&self) -> Option<&str> {
        self.source_revision.as_deref()
    }

    pub fn source_tree_digest(&self) -> Option<&str> {
        self.source_tree_digest.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexerContextError {
    EmptyRepoName,
    RepoNameContainsColon { given: Box<str> },
    EmptyEngineVersion,
    EmptySourceRevision,
    EmptySourceTreeDigest,
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
            Self::EmptySourceRevision => {
                f.write_str("IndexerContext: source_revision must not be empty")
            }
            Self::EmptySourceTreeDigest => {
                f.write_str("IndexerContext: source_tree_digest must not be empty")
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
