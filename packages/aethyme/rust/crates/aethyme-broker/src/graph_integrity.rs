//! Repository-owned policy for committed graph artifacts.
//!
//! This policy models whether committed `.aethyme/graph/**` fragments are an
//! authoritative generated artifact matching the exact tree. Its verdict is
//! advice on every landing lane (#280, #292): a stale or unverifiable graph is
//! recorded and reported, and never refuses a submission, a gate run or a
//! brokered pull-request merge. Blocking coupled every landing to an optional
//! subsystem, and only one of the two lanes enforced it.

use std::path::Path;

use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
#[cfg(test)]
use aethyme_graph_storage::GraphAuthority;
use aethyme_graph_storage::{
    GRAPH_CONFIG_RELPATH, GraphIntegrityPolicy, bootstrap_repo, committed_source_tree_digest,
    read_coverage, read_engine_version, write_graph_authority_manifest,
};

pub(crate) fn load_graph_policy_at_commit(
    repository: &crate::GitRepo,
    commit: &str,
) -> Result<GraphIntegrityPolicy, crate::BrokerOpError> {
    match repository.file_at_commit(commit, GRAPH_CONFIG_RELPATH)? {
        Some(text) => Ok(GraphIntegrityPolicy::parse(&text)?),
        None => Ok(GraphIntegrityPolicy::default()),
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

/// What a graph-integrity outcome tells a reader, independent of how it was
/// reached. Only a verified match is `Fresh`; a check that could not reach a
/// verdict is `Unknown`, never `Fresh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphIntegrityVerdict {
    Disabled,
    Fresh,
    Stale,
    Unknown,
}

impl GraphIntegrityVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

impl GraphIntegrityStatus {
    pub fn verdict(self) -> GraphIntegrityVerdict {
        match self {
            Self::Disabled => GraphIntegrityVerdict::Disabled,
            Self::Passed => GraphIntegrityVerdict::Fresh,
            Self::Stale => GraphIntegrityVerdict::Stale,
            Self::Incompatible | Self::Error => GraphIntegrityVerdict::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphIntegrityOutcome {
    pub status: GraphIntegrityStatus,
    pub enforced: bool,
    pub tree_hash: String,
    pub policy_digest: String,
    pub engine_version: Option<String>,
    pub changed_paths: Vec<String>,
    pub reason: String,
}

impl GraphIntegrityOutcome {
    pub fn verdict(&self) -> GraphIntegrityVerdict {
        self.status.verdict()
    }

    /// The advice a stale or unverifiable graph earns, or `None` when there is
    /// nothing to say. It never refuses anything (#280).
    pub fn advice(&self) -> Option<String> {
        match self.verdict() {
            GraphIntegrityVerdict::Disabled | GraphIntegrityVerdict::Fresh => None,
            GraphIntegrityVerdict::Stale => Some(format!(
                "committed graph fragments are stale for tree {}; this is advice and blocks \
                 nothing. Refresh them with `aethyme graph refresh plan --repo .` and commit \
                 the generated diff",
                short_tree(&self.tree_hash)
            )),
            GraphIntegrityVerdict::Unknown => Some(format!(
                "graph integrity could not be verified for tree {} ({}); treat the committed \
                 graph as unverified. This is advice and blocks nothing",
                short_tree(&self.tree_hash),
                self.reason
            )),
        }
    }
}

fn short_tree(tree: &str) -> &str {
    if tree.is_empty() {
        "<unknown>"
    } else {
        tree.get(..12).unwrap_or(tree)
    }
}

impl serde::Serialize for GraphIntegrityOutcome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let advice = self.advice();
        let fields = 7 + usize::from(self.engine_version.is_some()) + usize::from(advice.is_some());
        let mut state = serializer.serialize_struct("GraphIntegrityOutcome", fields)?;
        state.serialize_field("status", &self.status)?;
        state.serialize_field("verdict", &self.verdict())?;
        state.serialize_field("enforced", &self.enforced)?;
        state.serialize_field("tree_hash", &self.tree_hash)?;
        state.serialize_field("policy_digest", &self.policy_digest)?;
        if let Some(engine_version) = &self.engine_version {
            state.serialize_field("engine_version", engine_version)?;
        }
        state.serialize_field("changed_paths", &self.changed_paths)?;
        state.serialize_field("reason", &self.reason)?;
        if let Some(advice) = &advice {
            state.serialize_field("advice", advice)?;
        }
        state.end()
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
    // The pin is no longer a gate. It was a fast path in front of the real
    // check below -- regenerate the fragments and compare the resulting tree --
    // and it answered a different question than the one it was asked.
    // `CARGO_PKG_VERSION` collapses distinct builds onto one string, so a
    // release `0.7.23` and a worktree build describing itself as `0.7.23` pass
    // the pin while producing different bytes, and two builds that would emit
    // identical fragments fail it whenever their versions differ. It refused on
    // an identity that is neither recorded nor stable: the verifying binary is
    // whichever `aethyme` happens to be installed, which on a multi-agent host
    // any session replaces at will (#254).
    //
    // The regeneration below detects drift directly and needs no version to do
    // it, so the pin is kept as provenance and not consulted for a verdict.
    let running_version = env!("CARGO_PKG_VERSION");

    let head_revision = match checkout.head_commit() {
        Ok(revision) => revision,
        Err(error) => {
            return graph_error(
                tree_hash,
                policy_digest,
                Some(pinned_version),
                format!("cannot resolve the exact verification revision: {error}"),
            );
        }
    };
    let source_revision = read_coverage(checkout.root())
        .ok()
        .and_then(|coverage| coverage.source_revision)
        .unwrap_or_else(|| head_revision.clone());
    let source_tree_digest = match committed_source_tree_digest(checkout.root(), &head_revision) {
        Ok(digest) => digest,
        Err(error) => {
            return graph_error(
                tree_hash,
                policy_digest,
                Some(pinned_version),
                format!("cannot resolve the exact source tree: {error}"),
            );
        }
    };

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
        match IndexerContext::new(repository, checkout.root().to_path_buf(), running_version)
            .and_then(|context| context.with_source_revision(&source_revision))
            .and_then(|context| context.with_source_tree_digest(&source_tree_digest))
        {
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
    if let Err(error) =
        write_graph_authority_manifest(checkout.root(), "HEAD", repository, running_version)
    {
        return graph_error(
            tree_hash,
            policy_digest,
            Some(pinned_version),
            format!("cannot write graph authority manifest: {error}"),
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
        "chore(broker): materialize exact graph-integrity verification tree",
        &crate::attribution::Attribution::broker_only(),
    )?;
    let mut slot =
        crate::verification::ExactTreeVerificationSlot::acquire(main_root, "graph-integrity")?;
    let disposable = slot.materialize(checkout, &commit)?;
    let outcome = verify_disposable_checkout(&disposable, policy);
    slot.cleanup();
    Ok(outcome)
}

/// Check one local commit's tree for the brokered pull-request lane (#292).
///
/// Infallible on purpose: the verdict is advice, so every failure to reach
/// one -- an unreadable policy, a head not fetched locally, a slot or
/// checkout error -- becomes an `Error` outcome, reported as `unknown`, and
/// never an error that could stop the merge. `None` means the repository does
/// not declare committed graph fragments authoritative.
pub(crate) fn verify_commit_for_advice(
    main_root: &Path,
    checkout: &crate::GitRepo,
    commit: &str,
) -> Option<GraphIntegrityOutcome> {
    let unknown = |policy_digest: String, reason: String| {
        Some(graph_error(String::new(), policy_digest, None, reason))
    };
    let policy = match GraphIntegrityPolicy::load(checkout.root()) {
        Ok(policy) => policy,
        Err(error) => {
            return unknown(
                String::new(),
                format!("cannot read the graph policy: {error}"),
            );
        }
    };
    if !policy.enforces_committed_fragments() {
        return None;
    }
    let policy_digest = policy.digest();
    if checkout
        .resolve_ref(&format!("{commit}^{{commit}}"))
        .is_none()
    {
        return unknown(
            policy_digest,
            format!("commit {commit} is not available locally; fetch it to verify its graph"),
        );
    }
    let mut slot =
        match crate::verification::ExactTreeVerificationSlot::acquire(main_root, "graph-integrity")
        {
            Ok(slot) => slot,
            Err(error) => {
                return unknown(
                    policy_digest,
                    format!("cannot acquire a verification slot: {error}"),
                );
            }
        };
    let outcome = match slot.materialize(checkout, commit) {
        Ok(disposable) => verify_disposable_checkout(&disposable, &policy),
        Err(error) => graph_error(
            String::new(),
            policy_digest,
            None,
            format!("cannot materialize commit {commit}: {error}"),
        ),
    };
    slot.cleanup();
    Some(outcome)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn outcome(status: GraphIntegrityStatus) -> GraphIntegrityOutcome {
        GraphIntegrityOutcome {
            status,
            enforced: true,
            tree_hash: "0123456789abcdef0123456789abcdef01234567".into(),
            policy_digest: "digest".into(),
            engine_version: None,
            changed_paths: Vec::new(),
            reason: "reason".into(),
        }
    }

    /// #280: only a verified match is fresh. A check that could not reach a
    /// verdict reads as unknown, never as fresh, and both it and a stale
    /// verdict carry advice; the JSON carries `verdict` and `advice`.
    #[test]
    fn verdicts_map_to_advice_and_unknown_is_never_fresh() {
        let cases = [
            (
                GraphIntegrityStatus::Disabled,
                GraphIntegrityVerdict::Disabled,
                false,
            ),
            (
                GraphIntegrityStatus::Passed,
                GraphIntegrityVerdict::Fresh,
                false,
            ),
            (
                GraphIntegrityStatus::Stale,
                GraphIntegrityVerdict::Stale,
                true,
            ),
            (
                GraphIntegrityStatus::Incompatible,
                GraphIntegrityVerdict::Unknown,
                true,
            ),
            (
                GraphIntegrityStatus::Error,
                GraphIntegrityVerdict::Unknown,
                true,
            ),
        ];
        for (status, verdict, advised) in cases {
            let outcome = outcome(status);
            assert_eq!(outcome.verdict(), verdict, "{status:?}");
            assert_eq!(outcome.advice().is_some(), advised, "{status:?}");
            let json = serde_json::to_value(&outcome).unwrap();
            assert_eq!(json["verdict"], verdict.as_str());
            assert_eq!(json["status"], status.as_str());
            assert_eq!(json.get("advice").is_some(), advised, "{json}");
        }
    }

    /// The pull-request lane's check never fails: a head that is not
    /// available locally is an `unknown` verdict, not an error that could
    /// stop the merge (#292).
    #[test]
    fn an_unavailable_pull_request_head_is_unknown_not_an_error() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        write_config(
            repo.path(),
            "[graph]\nauthority = 'committed_fragments'\nrepository = 'fixture'\n",
        );
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "init"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let outcome = verify_commit_for_advice(
            repo.path(),
            &checkout,
            "1111111111111111111111111111111111111111",
        )
        .expect("an authoritative policy always yields an outcome");
        assert_eq!(outcome.verdict(), GraphIntegrityVerdict::Unknown);
        assert!(
            outcome.reason.contains("not available locally"),
            "{}",
            outcome.reason
        );
    }

    #[test]
    fn the_pull_request_lane_says_nothing_without_an_authoritative_policy() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "init"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let head = checkout.head_commit().unwrap();
        assert!(verify_commit_for_advice(repo.path(), &checkout, &head).is_none());
    }

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

    fn indexed_context(root: &Path) -> IndexerContext {
        let checkout = crate::GitRepo::discover(root).unwrap();
        let source_revision = checkout.head_commit().unwrap();
        let source_tree_digest = committed_source_tree_digest(root, &source_revision).unwrap();
        IndexerContext::new("fixture", root.to_path_buf(), env!("CARGO_PKG_VERSION"))
            .unwrap()
            .with_source_revision(&source_revision)
            .unwrap()
            .with_source_tree_digest(&source_tree_digest)
            .unwrap()
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
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "fixture source"]);
        let context = indexed_context(repo.path());
        index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
        link_repo(&context).unwrap();
        write_graph_authority_manifest(repo.path(), "HEAD", "fixture", env!("CARGO_PKG_VERSION"))
            .unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "fixture graph"]);
        repo
    }

    #[test]
    fn affected_gate_selection_does_not_wait_for_graph_integrity_slot() {
        let repo = graph_repo();
        std::fs::write(
            repo.path().join(".aethyme/gates.toml"),
            "[[gate]]\nname = 'source-check'\ncommand = 'true'\ntriggers = ['src/**']\n",
        )
        .unwrap();
        git(repo.path(), &["add", ".aethyme/gates.toml"]);
        git(repo.path(), &["commit", "-m", "configure source gate"]);

        let mut setup = crate::Broker::open(repo.path()).unwrap();
        let session = setup.start_worktree("inspection", None).unwrap();
        drop(setup);
        std::fs::write(
            Path::new(&session.worktree_path).join("src/lib.rs"),
            "pub fn answer() -> u8 { 43 }\n",
        )
        .unwrap();

        let held =
            crate::verification::ExactTreeVerificationSlot::acquire(repo.path(), "graph-integrity")
                .unwrap();
        let root = repo.path().to_path_buf();
        let session_id = session.id;
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut broker = crate::Broker::open(&root).unwrap();
            sender.send(broker.affected_gates(session_id)).unwrap();
        });

        let selected = receiver.recv_timeout(std::time::Duration::from_secs(30));
        drop(held);
        let selected = selected
            .expect("affected-gate inspection waited on graph-integrity verification")
            .unwrap();
        worker.join().unwrap();

        assert_eq!(
            selected,
            vec![("source-check".to_string(), Some("src/lib.rs".to_string()))]
        );
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
        assert_eq!(outcome.verdict(), GraphIntegrityVerdict::Fresh);
        assert!(outcome.advice().is_none());
        assert!(outcome.changed_paths.is_empty());
    }

    /// The pull-request lane checks a commit, not a worktree: a fresh head
    /// reads as fresh, and a stale one as stale with advice (#292).
    #[test]
    fn the_pull_request_lane_checks_the_head_commit() {
        let repo = graph_repo();
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let fresh = checkout.head_commit().unwrap();
        let outcome = verify_commit_for_advice(repo.path(), &checkout, &fresh).unwrap();
        assert_eq!(
            outcome.verdict(),
            GraphIntegrityVerdict::Fresh,
            "{outcome:?}"
        );

        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 44 }\n",
        )
        .unwrap();
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-m", "change source only"]);
        let stale = checkout.head_commit().unwrap();
        let outcome = verify_commit_for_advice(repo.path(), &checkout, &stale).unwrap();
        assert_eq!(
            outcome.verdict(),
            GraphIntegrityVerdict::Stale,
            "{outcome:?}"
        );
        assert!(outcome.advice().is_some());
        assert_eq!(
            checkout.head_commit().unwrap(),
            stale,
            "the check never moves the caller's checkout"
        );
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
        assert_eq!(outcome.verdict(), GraphIntegrityVerdict::Stale);
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
                ".aethyme/graph/coverage.json",
                ".aethyme/graph/manifest.json",
                ".aethyme/graph/src/lib.rs.bin",
                ".aethyme/graph/src/renamed.rs.bin",
                ".aethyme/graph/units.ndjson"
            ]
        );

        let context = indexed_context(repo.path());
        std::fs::remove_dir_all(repo.path().join(".aethyme/graph")).unwrap();
        bootstrap_repo(repo.path(), env!("CARGO_PKG_VERSION")).unwrap();
        index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
        link_repo(&context).unwrap();
        write_graph_authority_manifest(repo.path(), "HEAD", "fixture", env!("CARGO_PKG_VERSION"))
            .unwrap();
        assert_eq!(
            verify_disposable_checkout(&checkout, &policy).status,
            GraphIntegrityStatus::Passed
        );
    }

    #[test]
    fn binary_only_changes_are_visible_in_coverage_artifacts() {
        let repo = graph_repo();
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let unchanged = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(unchanged.status, GraphIntegrityStatus::Passed);

        std::fs::write(repo.path().join("asset.bin"), [0_u8, 159, 146, 150]).unwrap();
        let binary_only = verify_disposable_checkout(&checkout, &policy);
        assert_eq!(binary_only.status, GraphIntegrityStatus::Stale);
        assert_eq!(
            binary_only.changed_paths,
            vec![
                ".aethyme/graph/coverage.json",
                ".aethyme/graph/manifest.json"
            ]
        );
        assert_ne!(binary_only.tree_hash, unchanged.tree_hash);
        assert_eq!(binary_only.policy_digest, unchanged.policy_digest);
    }

    /// A pin that disagrees with the running version is not by itself a
    /// verdict.
    ///
    /// It used to refuse outright, which meant a repository could be blocked by
    /// which binary happened to be installed rather than by anything committed.
    /// Fragments that still regenerate to the verification tree are current, so
    /// the pin's disagreement must not override that evidence.
    #[test]
    fn a_stale_engine_pin_alone_does_not_refuse() {
        let repo = graph_repo();
        std::fs::write(repo.path().join(".aethyme/engine-version"), "0.0.1\n").unwrap();
        git(repo.path(), &["add", ".aethyme/engine-version"]);
        git(repo.path(), &["commit", "-m", "old graph pin"]);
        let checkout = crate::GitRepo::discover(repo.path()).unwrap();
        let policy = GraphIntegrityPolicy::load(repo.path()).unwrap();
        let outcome = verify_disposable_checkout(&checkout, &policy);
        assert_ne!(
            outcome.status,
            GraphIntegrityStatus::Incompatible,
            "a version string must not decide this on its own"
        );
    }
}
