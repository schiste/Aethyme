//! Another session's change that could affect a file without touching it.
//!
//! Same-file overlaps carry their diff (`overlap_diff`). This looks one step
//! further: the graph says a file this session is editing calls or imports a file
//! another live session is changing, so that change may reach it.
//!
//! It is advice, and silence is the default. It answers only from a graph bound
//! to a revision where both files have the contents the sessions started from,
//! and only for direct relationships. An absent, stale or unreadable graph says
//! nothing: "no impact" and "could not tell" must not read alike, and a guess
//! would teach agents to ignore this. A partial graph can still answer, because
//! what partial coverage loses is relationships, not the truth of one it read,
//! and every real repository's graph is partial. It says so, as low confidence.

use std::path::Path;
use std::time::Instant;

use crate::git::GitRepo;
use crate::graph_impact::{
    GRAPH_IMPACT_DEFAULT_BUDGET, GraphImpactContractStatus, GraphImpactMode,
    GraphStoreImpactProvider, revision_bound_impact_report,
};
use crate::overlap_diff::{self, HolderRequest, OverlapChanges};
use crate::types::Session;

/// Other sessions considered per file, and findings reported per tool call.
const MAX_SESSIONS: usize = 8;
/// A session's changed files read per note; each costs a graph query.
const MAX_FILES_PER_SESSION: usize = 12;
const MAX_FINDINGS: usize = 2;

/// One session whose changes the graph relates to `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub session_id: i64,
    /// The other session's changed file.
    pub changed_file: String,
    /// This session's file that depends on it.
    pub path: String,
    pub relationship: &'static str,
    pub graph_revision: String,
    pub confidence: &'static str,
}

/// Another live session, as this check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub id: i64,
    pub base: Option<String>,
    /// The files it changed.
    pub changed: Vec<String>,
}

impl Peer {
    pub(crate) fn new(session: &Session, changed: Vec<String>) -> Self {
        Self {
            id: session.id,
            base: session
                .diff_base
                .clone()
                .or_else(|| session.adoption_base.clone()),
            changed,
        }
    }
}

/// `[leases] overlap_impact`: off unless a repository turns it on. On Aethyme's
/// own history (2026-11-05, 170 pairs of nearby changes) it fired on 5% of pairs,
/// and 6 of its 15 findings named no symbol the other change had touched. A
/// graph query also took about 1.1 s on that 539 MB store, on a host at load 30.
pub(crate) fn enabled(main_root: &Path) -> bool {
    crate::merge::repository_config_text(main_root)
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| {
            value
                .get("leases")?
                .get("overlap_impact")
                .and_then(toml::Value::as_bool)
        })
        .unwrap_or(false)
}

/// The revision the repository's graph is bound to, when it has a usable one.
fn graph_revision(main_root: &Path) -> Option<String> {
    if !main_root.join(".aethyme/graph_store.redb").is_file() {
        return None;
    }
    let coverage = aethyme_graph_storage::read_coverage(main_root).ok()?;
    coverage
        .available
        .then_some(coverage.indexed_revision)
        .flatten()
}

/// Whether `files` read the same at `revision` and `base`, so what the graph
/// says about them at one holds at the other.
fn unchanged_since(main: &GitRepo, revision: &str, base: &str, files: &[String]) -> bool {
    revision == base
        || main
            .changed_between_paths(revision, base, files)
            .is_ok_and(|changed| changed.is_empty())
}

/// Findings for `path`.
pub(crate) fn find(
    main_root: &Path,
    path: &str,
    my_base: Option<&str>,
    peers: &[Peer],
    deadline: Instant,
) -> Vec<Finding> {
    let Some(revision) = graph_revision(main_root) else {
        return Vec::new();
    };
    let main = GitRepo::at_known_root(main_root);
    let mut findings = Vec::new();
    for peer in peers.iter().take(MAX_SESSIONS) {
        if findings.len() >= MAX_FINDINGS || Instant::now() >= deadline {
            break;
        }
        // A file the session itself changes is a same-file overlap.
        if peer.changed.is_empty() || peer.changed.iter().any(|file| file == path) {
            continue;
        }
        let Some(base) = peer.base.as_deref() else {
            continue;
        };
        let mut files = peer.changed.clone();
        files.push(path.to_string());
        if !unchanged_since(&main, &revision, base, &files)
            || my_base.is_some_and(|mine| !unchanged_since(&main, &revision, mine, &files))
        {
            continue;
        }
        if let Some(found) =
            direct_dependency(main_root, &revision, path, peer.id, &peer.changed, deadline)
        {
            findings.push(found);
        }
    }
    findings
}

fn direct_dependency(
    main_root: &Path,
    revision: &str,
    path: &str,
    session_id: i64,
    changed: &[String],
    deadline: Instant,
) -> Option<Finding> {
    for (mode, relationship) in [
        (GraphImpactMode::Calls, "calls"),
        (GraphImpactMode::Imports, "imports"),
    ] {
        for file in changed.iter().take(MAX_FILES_PER_SESSION) {
            if Instant::now() >= deadline {
                return None;
            }
            let report = revision_bound_impact_report(
                main_root,
                revision,
                revision,
                std::slice::from_ref(file),
                mode,
                GRAPH_IMPACT_DEFAULT_BUDGET,
                &GraphStoreImpactProvider,
            )
            .ok()?;
            if !matches!(
                report.status,
                GraphImpactContractStatus::Complete | GraphImpactContractStatus::Partial
            ) {
                return None;
            }
            if report.impact.direct.iter().any(|direct| direct == path) {
                return Some(Finding {
                    session_id,
                    changed_file: file.clone(),
                    path: path.to_string(),
                    relationship,
                    graph_revision: revision.to_string(),
                    confidence: report.confidence.as_str(),
                });
            }
        }
    }
    None
}

/// The hunk the note shows: the other session's change to the file depended on.
pub(crate) fn hunk_request(session: &Session, finding: &Finding) -> HolderRequest {
    HolderRequest::new(session, false, vec![finding.changed_file.clone()])
}

/// The advisory: what relates them, on what evidence, and how sure it is.
pub(crate) fn render(finding: &Finding, hunk: Option<&OverlapChanges>) -> String {
    let revision: String = finding.graph_revision.chars().take(12).collect();
    let mut text = format!(
        "Aethyme: session {} is changing `{}`, which `{}` {}. Advisory, not a conflict: the evidence is a direct relationship in the code graph at {revision}, with {} confidence in that graph's coverage. It says the files are related, not that this change breaks yours. Ignore it if the change does not reach what you use.",
        finding.session_id,
        finding.changed_file,
        finding.path,
        finding.relationship,
        finding.confidence
    );
    if let Some(hunk) = hunk {
        text.push('\n');
        text.push_str(&overlap_diff::render(hunk, "  "));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_drift::tests::{git, graph};

    fn peer(base: &str, changed: &[&str]) -> Peer {
        Peer {
            id: 9,
            base: Some(base.into()),
            changed: changed.iter().map(|file| (*file).to_string()).collect(),
        }
    }

    fn soon() -> Instant {
        Instant::now() + std::time::Duration::from_secs(60)
    }

    fn repo() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), ".aethyme/\n").unwrap();
        for path in ["a.rs", "b.rs"] {
            std::fs::write(root.join(path), "base\n").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        let head = git(&root, &["rev-parse", "HEAD"]);
        (temp, root, head)
    }

    #[test]
    fn a_direct_dependency_at_a_current_graph_is_flagged_with_its_evidence() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        let found = find(
            &root,
            "b.rs",
            Some(&head),
            &[peer(&head, &["a.rs"])],
            soon(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].changed_file, "a.rs");
        assert_eq!(found[0].graph_revision, head);
        let text = render(&found[0], None);
        assert!(text.contains("session 9 is changing `a.rs`"), "{text}");
        assert!(text.contains("Advisory, not a conflict"), "{text}");
        assert!(text.contains("code graph at"), "{text}");
    }

    #[test]
    fn no_relationship_says_nothing() {
        let (_temp, root, head) = repo();
        graph(&root, &head, false);
        assert!(find(&root, "b.rs", None, &[peer(&head, &["a.rs"])], soon()).is_empty());
    }

    #[test]
    fn no_graph_says_nothing() {
        let (_temp, root, head) = repo();
        assert!(find(&root, "b.rs", None, &[peer(&head, &["a.rs"])], soon()).is_empty());
    }

    /// The graph describes one revision. If either file differs at the session's
    /// base, what it says about them is out of date.
    #[test]
    fn a_graph_older_than_a_file_says_nothing() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        std::fs::write(root.join("b.rs"), "moved on\n").unwrap();
        git(&root, &["commit", "-qam", "moved"]);
        let later = git(&root, &["rev-parse", "HEAD"]);
        assert!(find(&root, "b.rs", None, &[peer(&later, &["a.rs"])], soon()).is_empty());
    }

    /// An unrelated file moving does not invalidate what the graph says.
    #[test]
    fn a_graph_older_only_in_unrelated_files_still_answers() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        std::fs::write(root.join("c.rs"), "new\n").unwrap();
        git(&root, &["add", "c.rs"]);
        git(&root, &["commit", "-qm", "unrelated"]);
        let later = git(&root, &["rev-parse", "HEAD"]);
        assert_eq!(
            find(
                &root,
                "b.rs",
                Some(&later),
                &[peer(&later, &["a.rs"])],
                soon()
            )
            .len(),
            1
        );
    }

    /// Partial coverage loses relationships, not the truth of one it read.
    #[test]
    fn a_partial_graph_answers_and_says_how_sure_it_is() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        let mut coverage = aethyme_graph_storage::read_coverage(&root).unwrap();
        coverage.safe_to_use = false;
        coverage.coverage_mode = "partial".into();
        coverage.gaps.push("excluded_files".into());
        aethyme_graph_storage::write_coverage_artifacts(&root, &coverage, &[]).unwrap();
        // Rewriting coverage must not read as fragments newer than the store.
        std::fs::File::options()
            .write(true)
            .open(root.join(".aethyme/graph_store.redb"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        let found = find(&root, "b.rs", None, &[peer(&head, &["a.rs"])], soon());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].confidence, "low");
        assert!(render(&found[0], None).contains("low confidence"));
    }

    /// A store bound to another revision is stale, whatever the files did.
    #[test]
    fn a_stale_graph_says_nothing() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        let store = aethyme_engine::store::redb::graph_store::GraphStore::open(&root).unwrap();
        store
            .set_repo_metadata(&aethyme_engine::store::redb::graph_store::RepoMetadata {
                root_path: root.display().to_string(),
                commit_hash: Some("0".repeat(40)),
                indexed_at_unix: 0,
                file_count: 2,
                languages: vec!["rust".into()],
            })
            .unwrap();
        drop(store);
        assert!(find(&root, "b.rs", None, &[peer(&head, &["a.rs"])], soon()).is_empty());
    }

    #[test]
    fn an_expired_budget_says_nothing() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        let expired = Instant::now();
        assert!(find(&root, "b.rs", None, &[peer(&head, &["a.rs"])], expired).is_empty());
    }

    #[test]
    fn a_file_the_session_changes_itself_is_a_same_file_overlap_not_this() {
        let (_temp, root, head) = repo();
        graph(&root, &head, true);
        assert!(
            find(
                &root,
                "b.rs",
                None,
                &[peer(&head, &["a.rs", "b.rs"])],
                soon()
            )
            .is_empty()
        );
    }
}
