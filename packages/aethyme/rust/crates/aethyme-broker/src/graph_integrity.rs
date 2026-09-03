//! Repository-owned policy for committed graph artifacts.
//!
//! Semantic graph impact remains advisory. This policy models a different
//! contract: whether committed `.aethyme/graph/**` fragments are an
//! authoritative generated artifact that must match the exact tree before it
//! can be promoted.

use std::path::{Path, PathBuf};

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
use aethyme_graph_storage::{bootstrap_repo, read_engine_version};
use sha2::{Digest, Sha256};

pub const GRAPH_CONFIG_RELPATH: &str = ".aethyme/config.toml";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphAuthority {
    #[default]
    Disabled,
    CommittedFragments,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GraphIntegrityPolicy {
    pub authority: GraphAuthority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl GraphIntegrityPolicy {
    pub fn load(repo_root: &Path) -> Result<Self, GraphIntegrityPolicyError> {
        let path = repo_root.join(GRAPH_CONFIG_RELPATH);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(GraphIntegrityPolicyError::Read {
                    path,
                    message: error.to_string(),
                });
            }
        };
        Self::parse_at(&text, path)
    }

    /// Parse policy bytes obtained from an exact Git tree without consulting
    /// the invoking worktree.
    pub fn parse(text: &str) -> Result<Self, GraphIntegrityPolicyError> {
        Self::parse_at(text, PathBuf::from(GRAPH_CONFIG_RELPATH))
    }

    pub(crate) fn load_at_commit(
        repository: &crate::GitRepo,
        commit: &str,
    ) -> Result<Self, crate::BrokerOpError> {
        match repository.file_at_commit(commit, GRAPH_CONFIG_RELPATH)? {
            Some(text) => Ok(Self::parse(&text)?),
            None => Ok(Self::default()),
        }
    }

    fn parse_at(text: &str, path: PathBuf) -> Result<Self, GraphIntegrityPolicyError> {
        let value =
            text.parse::<toml::Value>()
                .map_err(|error| GraphIntegrityPolicyError::Parse {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
        let Some(graph) = value.get("graph") else {
            return Ok(Self::default());
        };
        let Some(table) = graph.as_table() else {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "[graph] must be a TOML table".into(),
            });
        };
        let Some(authority) = table.get("authority") else {
            return Ok(Self::default());
        };
        let Some(authority) = authority.as_str() else {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "graph.authority must be a string".into(),
            });
        };
        let authority = match authority {
            "disabled" => GraphAuthority::Disabled,
            "committed_fragments" => GraphAuthority::CommittedFragments,
            other => {
                return Err(GraphIntegrityPolicyError::Invalid {
                    path,
                    message: format!(
                        "unsupported graph.authority {other:?}; expected disabled or committed_fragments"
                    ),
                });
            }
        };
        let repository = table
            .get("repository")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| GraphIntegrityPolicyError::Invalid {
                        path: path.clone(),
                        message: "graph.repository must be a string".into(),
                    })
                    .and_then(|value| {
                        if value.is_empty() || value.contains(':') {
                            Err(GraphIntegrityPolicyError::Invalid {
                                path: path.clone(),
                                message: "graph.repository must be non-empty and contain no ':'"
                                    .into(),
                            })
                        } else {
                            Ok(value.to_string())
                        }
                    })
            })
            .transpose()?;
        if authority == GraphAuthority::CommittedFragments && repository.is_none() {
            return Err(GraphIntegrityPolicyError::Invalid {
                path,
                message: "graph.repository is required for committed_fragments authority".into(),
            });
        }
        Ok(Self {
            authority,
            repository,
        })
    }

    pub fn enforces_committed_fragments(&self) -> bool {
        self.authority == GraphAuthority::CommittedFragments
    }

    /// Stable cache/provenance identity for policy plus checker implementation.
    pub fn digest(&self) -> String {
        let authority = match self.authority {
            GraphAuthority::Disabled => "disabled",
            GraphAuthority::CommittedFragments => "committed_fragments",
        };
        let repository = self.repository.as_deref().unwrap_or("");
        format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "graph-integrity-v1\nauthority={authority}\nrepository={repository}\nengine={}\n",
                    env!("CARGO_PKG_VERSION")
                )
                .as_bytes()
            )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphIntegrityStatus {
    Disabled,
    Passed,
    Stale,
    Incompatible,
    Error,
}

impl GraphIntegrityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Passed => "passed",
            Self::Stale => "stale",
            Self::Incompatible => "incompatible",
            Self::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "disabled" => Some(Self::Disabled),
            "passed" => Some(Self::Passed),
            "stale" => Some(Self::Stale),
            "incompatible" => Some(Self::Incompatible),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphIntegrityOutcome {
    pub status: GraphIntegrityStatus,
    pub enforced: bool,
    pub tree_hash: String,
    pub policy_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    pub changed_paths: Vec<String>,
    pub reason: String,
}

impl GraphIntegrityOutcome {
    pub fn allows_promotion(&self) -> bool {
        matches!(
            self.status,
            GraphIntegrityStatus::Disabled | GraphIntegrityStatus::Passed
        )
    }
}

/// Rebuild authoritative fragments inside a disposable exact-tree checkout.
///
/// The caller must discard the checkout after this returns. Rebuilding in the
/// disposable submission slot lets the verifier compare canonical bytes
/// without modifying the session worktree or silently adding generated files
/// to the patch being reviewed.
pub(crate) fn verify_disposable_checkout(
    checkout: &crate::GitRepo,
    policy: &GraphIntegrityPolicy,
) -> GraphIntegrityOutcome {
    let policy_digest = policy.digest();
    let tree_hash = match checkout.working_tree_hash() {
        Ok(tree) => tree,
        Err(error) => {
            return GraphIntegrityOutcome {
                status: GraphIntegrityStatus::Error,
                enforced: policy.enforces_committed_fragments(),
                tree_hash: String::new(),
                policy_digest,
                engine_version: None,
                changed_paths: Vec::new(),
                reason: format!("cannot hash the exact verification tree: {error}"),
            };
        }
    };
    if !policy.enforces_committed_fragments() {
        return GraphIntegrityOutcome {
            status: GraphIntegrityStatus::Disabled,
            enforced: false,
            tree_hash,
            policy_digest,
            engine_version: None,
            changed_paths: Vec::new(),
            reason: "repository does not declare committed graph fragments authoritative".into(),
        };
    }

    let pinned_version = match read_engine_version(checkout.root()) {
        Ok(version) => version,
        Err(error) => {
            return GraphIntegrityOutcome {
                status: GraphIntegrityStatus::Incompatible,
                enforced: true,
                tree_hash,
                policy_digest,
                engine_version: None,
                changed_paths: Vec::new(),
                reason: format!(
                    "cannot read .aethyme/engine-version: {error}; run `aethyme graph refresh plan --repo .`"
                ),
            };
        }
    };
    let running_version = env!("CARGO_PKG_VERSION");
    if pinned_version != running_version {
        return GraphIntegrityOutcome {
            status: GraphIntegrityStatus::Incompatible,
            enforced: true,
            tree_hash,
            policy_digest,
            engine_version: Some(pinned_version.clone()),
            changed_paths: Vec::new(),
            reason: format!(
                "graph fragments are pinned to Aethyme {pinned_version}, but this verifier is {running_version}; run `aethyme graph refresh plan --repo .` with a compatible release"
            ),
        };
    }

    let graph_dir = checkout.root().join(".aethyme/graph");
    if let Err(error) = std::fs::remove_dir_all(&graph_dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return graph_error(
            tree_hash,
            policy_digest,
            Some(pinned_version),
            format!("cannot reset disposable graph directory: {error}"),
        );
    }
    if let Err(error) = bootstrap_repo(checkout.root(), running_version) {
        return graph_error(
            tree_hash,
            policy_digest,
            Some(pinned_version),
            format!("cannot bootstrap disposable graph output: {error}"),
        );
    }
    let repository = policy
        .repository
        .as_deref()
        .expect("committed fragment policy validates repository identity");
    let context =
        match IndexerContext::new(repository, checkout.root().to_path_buf(), running_version) {
            Ok(context) => context,
            Err(error) => {
                return graph_error(
                    tree_hash,
                    policy_digest,
                    Some(pinned_version),
                    format!("cannot initialize graph verifier: {error}"),
                );
            }
        };
    if let Err(error) = index_repo_to_disk(&context, &WalkOptions::default()) {
        return graph_error(
            tree_hash,
            policy_digest,
            Some(pinned_version),
            format!("cannot regenerate graph fragments: {error}"),
        );
    }
    if let Err(error) = link_repo(&context) {
        return graph_error(
            tree_hash,
            policy_digest,
            Some(pinned_version),
            format!("cannot link regenerated graph fragments: {error}"),
        );
    }

    let regenerated_tree = match checkout.working_tree_hash() {
        Ok(tree) => tree,
        Err(error) => {
            return graph_error(
                tree_hash,
                policy_digest,
                Some(pinned_version),
                format!("cannot hash regenerated graph fragments: {error}"),
            );
        }
    };
    let mut changed_paths = checkout
        .dirty_paths()
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path == ".aethyme/engine-version" || path.starts_with(".aethyme/graph/"))
        .collect::<Vec<_>>();
    changed_paths.sort();
    if regenerated_tree == tree_hash {
        GraphIntegrityOutcome {
            status: GraphIntegrityStatus::Passed,
            enforced: true,
            tree_hash,
            policy_digest,
            engine_version: Some(pinned_version),
            changed_paths,
            reason: "committed graph fragments match the exact verification tree".into(),
        }
    } else {
        GraphIntegrityOutcome {
            status: GraphIntegrityStatus::Stale,
            enforced: true,
            tree_hash,
            policy_digest,
            engine_version: Some(pinned_version),
            changed_paths,
            reason: "committed graph fragments are stale; run `aethyme graph refresh plan --repo .` and review the generated diff".into(),
        }
    }
}

fn graph_error(
    tree_hash: String,
    policy_digest: String,
    engine_version: Option<String>,
    reason: String,
) -> GraphIntegrityOutcome {
    GraphIntegrityOutcome {
        status: GraphIntegrityStatus::Error,
        enforced: true,
        tree_hash,
        policy_digest,
        engine_version,
        changed_paths: Vec::new(),
        reason,
    }
}

/// Verify the caller's exact working state without mutating its worktree or
/// index. A synthetic commit materializes staged, unstaged, and untracked
/// content captured by [`crate::GitRepo::working_tree_hash`].
pub(crate) fn verify_checkout_without_mutation(
    main_root: &Path,
    checkout: &crate::GitRepo,
    policy: &GraphIntegrityPolicy,
) -> Result<GraphIntegrityOutcome, crate::BrokerOpError> {
    if !policy.enforces_committed_fragments() {
        return Ok(verify_disposable_checkout(checkout, policy));
    }
    let tree = checkout.working_tree_hash()?;
    let head = checkout.head_commit()?;
    let commit = checkout.commit_tree(
        &tree,
        &[&head],
        "broker: materialize exact graph-integrity verification tree",
    )?;
    let mut slot =
        crate::verification::ExactTreeVerificationSlot::acquire(main_root, "graph-integrity")?;
    let disposable = slot.materialize(checkout, &commit)?;
    let outcome = verify_disposable_checkout(&disposable, policy);
    slot.cleanup();
    Ok(outcome)
}

#[derive(Debug, thiserror::Error)]
#[error(
    "graph integrity {status:?} for tree {tree_hash} under policy {policy_digest}: {reason}; changed paths: {changed_paths:?}"
)]
pub struct GraphIntegrityRejection {
    pub status: GraphIntegrityStatus,
    pub tree_hash: String,
    pub policy_digest: String,
    pub changed_paths: Vec<String>,
    pub reason: String,
}

impl From<GraphIntegrityOutcome> for GraphIntegrityRejection {
    fn from(outcome: GraphIntegrityOutcome) -> Self {
        Self {
            status: outcome.status,
            tree_hash: outcome.tree_hash,
            policy_digest: outcome.policy_digest,
            changed_paths: outcome.changed_paths,
            reason: outcome.reason,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphIntegrityPolicyError {
    #[error("cannot read graph integrity policy at {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("cannot parse graph integrity policy at {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid graph integrity policy at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn write_config(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(root.join(GRAPH_CONFIG_RELPATH), body).unwrap();
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn graph_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.name", "Graph Test"]);
        git(repo.path(), &["config", "user.email", "graph@example.test"]);
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        write_config(
            repo.path(),
            "[graph]\nauthority = 'committed_fragments'\nrepository = 'fixture'\n",
        );
        bootstrap_repo(repo.path(), env!("CARGO_PKG_VERSION")).unwrap();
        let context = IndexerContext::new(
            "fixture",
            repo.path().to_path_buf(),
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();
        index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
        link_repo(&context).unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "fixture"]);
        repo
    }

    #[test]
    fn missing_configuration_disables_graph_authority() {
        let repo = tempfile::tempdir().unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        assert_eq!(policy.authority, GraphAuthority::Disabled);
        assert!(!policy.enforces_committed_fragments());
    }

    #[test]
    fn unrelated_configuration_preserves_the_disabled_default() {
        let repo = tempfile::tempdir().unwrap();
        write_config(repo.path(), "[promote]\nmode = 'auto'\n");
        assert_eq!(
            GraphIntegrityPolicy::load(repo.path()).unwrap(),
            GraphIntegrityPolicy::default()
        );
    }

    #[test]
    fn committed_fragment_authority_is_explicit() {
        let repo = tempfile::tempdir().unwrap();
        write_config(
            repo.path(),
            "[graph]\nauthority = 'committed_fragments'\nrepository = 'example'\n",
        );
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        assert!(policy.enforces_committed_fragments());
        assert_eq!(policy.repository.as_deref(), Some("example"));
    }

    #[test]
    fn invalid_authority_refuses_instead_of_guessing() {
        let repo = tempfile::tempdir().unwrap();
        write_config(repo.path(), "[graph]\nauthority = 'magic'\n");
        let error = GraphIntegrityPolicy::load(repo.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected disabled or committed_fragments")
        );
    }

    #[test]
    fn non_string_authority_refuses() {
        let repo = tempfile::tempdir().unwrap();
        write_config(repo.path(), "[graph]\nauthority = true\n");
        let error = GraphIntegrityPolicy::load(repo.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("graph.authority must be a string")
        );
    }

    #[test]
    fn committed_authority_requires_an_explicit_repository_namespace() {
        let repo = tempfile::tempdir().unwrap();
        write_config(repo.path(), "[graph]\nauthority = 'committed_fragments'\n");
        let error = GraphIntegrityPolicy::load(repo.path()).unwrap_err();
        assert!(error.to_string().contains("graph.repository is required"));
    }

    #[test]
    fn invalid_repository_namespace_refuses() {
        let repo = tempfile::tempdir().unwrap();
        write_config(
            repo.path(),
            "[graph]\nauthority = 'committed_fragments'\nrepository = 'owner:repo'\n",
        );
        let error = GraphIntegrityPolicy::load(repo.path()).unwrap_err();
        assert!(error.to_string().contains("contain no ':'"));
    }

    #[test]
    fn fresh_committed_fragments_match_the_exact_tree() {
        let repo = graph_repo();
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let outcome = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(outcome.status, GraphIntegrityStatus::Passed);
        assert!(outcome.allows_promotion());
        assert!(outcome.changed_paths.is_empty());
    }

    #[test]
    fn stale_source_change_is_detected_by_regenerated_fragment_bytes() {
        let repo = graph_repo();
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 43 }\n",
        )
        .unwrap();
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-m", "change source only"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let outcome = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(outcome.status, GraphIntegrityStatus::Stale);
        assert!(!outcome.allows_promotion());
        assert!(
            outcome
                .changed_paths
                .iter()
                .any(|path| path == ".aethyme/graph/src/lib.rs.bin")
        );
    }

    #[test]
    fn corrupted_and_deleted_fragments_are_stale_and_deterministic() {
        let repo = graph_repo();
        let fragment = repo.path().join(".aethyme/graph/src/lib.rs.bin");
        std::fs::write(&fragment, b"corrupted graph bytes").unwrap();
        git(repo.path(), &["add", ".aethyme/graph"]);
        git(repo.path(), &["commit", "-m", "corrupt graph fragment"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let corrupted = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(corrupted.status, GraphIntegrityStatus::Stale);
        assert_eq!(
            corrupted.changed_paths,
            vec![".aethyme/graph/src/lib.rs.bin"]
        );

        git(repo.path(), &["checkout", "HEAD^", "--", ".aethyme/graph"]);
        std::fs::remove_file(&fragment).unwrap();
        git(repo.path(), &["add", ".aethyme/graph"]);
        git(repo.path(), &["commit", "-m", "delete graph fragment"]);
        let deleted = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(deleted.status, GraphIntegrityStatus::Stale);
        assert_eq!(deleted.changed_paths, corrupted.changed_paths);
        assert_eq!(deleted.policy_digest, corrupted.policy_digest);
    }

    #[test]
    fn source_rename_is_stale_until_fragments_are_refreshed() {
        let repo = graph_repo();
        git(repo.path(), &["mv", "src/lib.rs", "src/renamed.rs"]);
        git(
            repo.path(),
            &["commit", "-m", "rename source without graph"],
        );
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let stale = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(stale.status, GraphIntegrityStatus::Stale);
        assert_eq!(
            stale.changed_paths,
            vec![
                ".aethyme/graph/_index/src.lib.ndjson",
                ".aethyme/graph/_index/src.renamed.ndjson",
                ".aethyme/graph/src/lib.rs.bin",
                ".aethyme/graph/src/renamed.rs.bin"
            ]
        );

        let context = IndexerContext::new(
            "fixture",
            repo.path().to_path_buf(),
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();
        std::fs::remove_dir_all(repo.path().join(".aethyme/graph")).unwrap();
        bootstrap_repo(repo.path(), env!("CARGO_PKG_VERSION")).unwrap();
        index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
        link_repo(&context).unwrap();
        assert_eq!(
            verify_disposable_checkout(&checkout, &policy).status,
            GraphIntegrityStatus::Passed
        );
    }

    #[test]
    fn binary_only_and_empty_changes_leave_fresh_fragments_valid() {
        let repo = graph_repo();
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let unchanged = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(unchanged.status, GraphIntegrityStatus::Passed);

        std::fs::write(repo.path().join("asset.bin"), [0_u8, 159, 146, 150]).unwrap();
        let binary_only = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(binary_only.status, GraphIntegrityStatus::Passed);
        assert!(binary_only.changed_paths.is_empty());
        assert_ne!(binary_only.tree_hash, unchanged.tree_hash);
        assert_eq!(binary_only.policy_digest, unchanged.policy_digest);
    }

    #[test]
    fn mismatched_engine_pin_refuses_without_rewriting_fragments() {
        let repo = graph_repo();
        std::fs::write(repo.path().join(".aethyme/engine-version"), "0.0.1\n").unwrap();
        git(repo.path(), &["add", ".aethyme/engine-version"]);
        git(repo.path(), &["commit", "-m", "old graph pin"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let before = checkout.working_tree_hash().unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let outcome = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(outcome.status, GraphIntegrityStatus::Incompatible);
        assert!(!outcome.allows_promotion());
        assert_eq!(checkout.working_tree_hash().unwrap(), before);
    }
}
