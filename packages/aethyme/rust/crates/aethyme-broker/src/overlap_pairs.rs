//! Lease overlaps grouped by session pair and ranked by whether they conflict.
//!
//! A lease overlap only says two live sessions changed the same path. Most
//! such overlaps merge cleanly: the sessions edited different hunks, or both
//! carry the same inherited change. Measured on one machine, a repository
//! with 151 sessions recorded 304,749 `lease.overlap` events against two
//! real merge conflicts: one event per overlapping *path*, so a pair of
//! sessions sharing a 1,388-file inherited diff announced itself 1,388 times.
//!
//! This module answers the question an agent can act on -- would these two
//! sessions conflict? -- with Git's own merge condition. Each session's state
//! (HEAD, or a commit built from a private copy of the index when tracked
//! edits are uncommitted, so the session's real index is never rewritten)
//! is merged in memory with `git merge-tree`; a path Git reports as
//! conflicted is high severity, anything else is low. Untracked files are
//! invisible to that merge, so an untracked overlap is high only when the two
//! sessions' files differ. Classification is best-effort: any Git failure
//! leaves the pair low and unclassified, and nothing here stops the broker.
//!
//! Results are cached per pair in broker metadata, keyed by both sessions'
//! state and the overlapping path set, so repeated refreshes run no merge
//! simulation. The same cache records what was last announced, so a
//! `lease.overlap` event is emitted once when a pair starts overlapping and
//! again only when its severity or conflicting paths change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::git::GitRepo;
use crate::leases::Overlap;
use crate::{Broker, BrokerOpError};

/// Broker metadata key holding the per-pair classification cache.
const PAIR_CACHE_META_KEY: &str = "lease.overlap.pairs.v1";

/// How many paths a pair lists before summarising the rest as a count.
pub const OVERLAP_SAMPLE_PATHS: usize = 5;

/// Whether two sessions' overlapping edits would conflict when merged.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum OverlapSeverity {
    /// Same paths, but the edits merge cleanly (or could not be classified).
    Low,
    /// Git reports a conflict on at least one overlapping path.
    High,
}

impl OverlapSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
        }
    }
}

/// Every overlapping path between two live sessions, as one ranked finding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OverlapPair {
    /// Lower session id first; the pair is unordered.
    pub session_a: i64,
    pub session_b: i64,
    pub severity: OverlapSeverity,
    /// Overlapping paths in total.
    pub paths_count: usize,
    /// Paths on which Git reports a conflict (all of them, sorted).
    pub conflicting_paths: Vec<String>,
    /// Up to [`OVERLAP_SAMPLE_PATHS`] overlapping paths, conflicting first.
    pub sample_paths: Vec<String>,
    /// False when the pair has not been classified yet, or classification
    /// failed; the severity is then `low` and `reason` says why.
    pub classified: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct CachedPair {
    /// Digest of both sessions' states and the overlapping path set.
    input_key: String,
    severity: Option<OverlapSeverity>,
    conflicting_paths: Vec<String>,
    classified: bool,
    reason: String,
    /// Signature of the last `lease.overlap` event emitted for this pair.
    announced: Option<String>,
}

type PairKey = (i64, i64);

fn pair_key_string((a, b): PairKey) -> String {
    format!("{a}-{b}")
}

/// Group per-path overlaps into pairs, keeping each pair's paths sorted.
pub fn group_overlaps(overlaps: &[Overlap]) -> BTreeMap<PairKey, Vec<String>> {
    let mut pairs: BTreeMap<PairKey, BTreeSet<String>> = BTreeMap::new();
    for overlap in overlaps {
        pairs
            .entry((overlap.session_a, overlap.session_b))
            .or_default()
            .insert(overlap.path.clone());
    }
    pairs
        .into_iter()
        .map(|(key, paths)| (key, paths.into_iter().collect()))
        .collect()
}

/// Order findings for display: conflicting pairs first, then by size.
pub fn rank(pairs: &mut [OverlapPair]) {
    pairs.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| {
                right
                    .conflicting_paths
                    .len()
                    .cmp(&left.conflicting_paths.len())
            })
            .then_with(|| right.paths_count.cmp(&left.paths_count))
            .then_with(|| (left.session_a, left.session_b).cmp(&(right.session_a, right.session_b)))
    });
}

fn build_pair(key: PairKey, paths: &[String], cached: Option<&CachedPair>) -> OverlapPair {
    let (severity, conflicting, classified, reason) = match cached {
        Some(entry) if entry.classified => (
            entry.severity.unwrap_or(OverlapSeverity::Low),
            entry
                .conflicting_paths
                .iter()
                .filter(|path| paths.contains(path))
                .cloned()
                .collect::<Vec<_>>(),
            true,
            entry.reason.clone(),
        ),
        Some(entry) => (
            OverlapSeverity::Low,
            Vec::new(),
            false,
            entry.reason.clone(),
        ),
        None => (
            OverlapSeverity::Low,
            Vec::new(),
            false,
            "not classified yet; the next lease refresh classifies it".into(),
        ),
    };
    let mut sample: Vec<String> = conflicting
        .iter()
        .take(OVERLAP_SAMPLE_PATHS)
        .cloned()
        .collect();
    for path in paths {
        if sample.len() >= OVERLAP_SAMPLE_PATHS {
            break;
        }
        if !sample.contains(path) {
            sample.push(path.clone());
        }
    }
    OverlapPair {
        session_a: key.0,
        session_b: key.1,
        severity: if conflicting.is_empty() {
            OverlapSeverity::Low
        } else {
            severity
        },
        paths_count: paths.len(),
        conflicting_paths: conflicting,
        sample_paths: sample,
        classified,
        reason,
    }
}

/// Why one session's state could not be read for a merge simulation.
enum StateError {
    /// The worktree is part-way through a merge, rebase, cherry-pick or
    /// revert, or holds unresolved conflicts. Not an error in the broker: the
    /// pair stays unclassified, and the reason says what to finish.
    MidOperation(String),
    Other(String),
}

/// One session's state for a merge simulation: a commit plus the identity
/// that decides whether a cached verdict is still valid.
struct SessionState {
    commit: String,
    /// Tree of the state commit: stable across refreshes, while a rebuilt
    /// working-state commit gets a new id every time.
    identity: String,
    untracked: BTreeSet<String>,
    root: std::path::PathBuf,
}

fn session_state(checkout: &GitRepo, paths: &[String]) -> Result<SessionState, String> {
    let head = checkout.head_commit().map_err(|error| error.to_string())?;
    let commit = checkout
        .working_state_commit(paths)
        .map_err(|error| error.to_string())?
        .unwrap_or(head);
    let identity = checkout
        .commit_tree_id(&commit)
        .map_err(|error| error.to_string())?;
    let untracked = checkout
        .untracked_paths_readonly()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|path| paths.contains(path))
        .collect();
    Ok(SessionState {
        commit,
        identity,
        untracked,
        root: checkout.root().to_path_buf(),
    })
}

fn input_key(left: &SessionState, right: &SessionState, paths: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(left.identity.as_bytes());
    hasher.update(b"\0");
    hasher.update(right.identity.as_bytes());
    for path in paths {
        hasher.update(b"\n");
        hasher.update(path.as_bytes());
        // Untracked content is not in either tree, so it must key the cache.
        for state in [left, right] {
            if state.untracked.contains(path) {
                hasher.update(b"\0untracked\0");
                hasher.update(std::fs::read(state.root.join(path)).unwrap_or_default());
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Classify one pair with Git's merge condition.
fn classify(
    main: &GitRepo,
    left: &SessionState,
    right: &SessionState,
    paths: &[String],
) -> Result<Vec<String>, String> {
    let simulation = main
        .merge_tree_simulate(&left.commit, &right.commit)
        .map_err(|error| error.to_string())?;
    let mut conflicting: BTreeSet<String> = simulation
        .conflicts
        .into_iter()
        .filter(|path| paths.contains(path))
        .collect();
    // An untracked file is invisible to the merge. Two sessions creating the
    // same file conflict exactly when their contents differ.
    for path in paths {
        if left.untracked.contains(path) || right.untracked.contains(path) {
            let read = |state: &SessionState| std::fs::read(state.root.join(path)).ok();
            if read(left) != read(right) {
                conflicting.insert(path.clone());
            }
        }
    }
    Ok(conflicting.into_iter().collect())
}

/// One `session.mid-merge` advice row per live session whose worktree is
/// part-way through a merge, rebase, cherry-pick or revert. Read from each
/// worktree's Git directory without running Git: `status` must not fork per
/// session. An agent stuck mid-merge is often why its work has stopped.
pub(crate) fn mid_operation_advice(agents: &[crate::AgentView]) -> Vec<crate::StatusAdvice> {
    agents
        .iter()
        .filter(|agent| !agent.derived_status.is_closed())
        .filter_map(|agent| {
            let worktree = Path::new(&agent.session.worktree_path);
            let operation = crate::git::worktree_operation_in_progress(worktree)?;
            let quoted = format!("'{}'", agent.session.worktree_path.replace('\'', "'\\''"));
            Some(crate::StatusAdvice {
                id: "session.mid-merge",
                severity: crate::StatusAdviceSeverity::Warning,
                reason: "the session's worktree is part-way through a Git operation",
                summary: format!(
                    "session {} is mid-{operation}; its overlaps cannot be classified and its \
                     work cannot be submitted until the {operation} is finished or aborted",
                    agent.session.id
                ),
                session_id: Some(agent.session.id),
                queue_entry_id: None,
                evidence: vec![agent.session.worktree_path.clone()],
                commands: vec![
                    format!("git -C {quoted} status"),
                    format!("git -C {quoted} {operation} --continue"),
                    format!("git -C {quoted} {operation} --abort"),
                ],
            })
        })
        .collect()
}

fn announcement_signature(pair: &OverlapPair) -> String {
    format!(
        "{}|{}|{}",
        pair.severity.as_str(),
        pair.classified,
        pair.conflicting_paths.join("\n")
    )
}

fn read_cache(store: &crate::store::BrokerStore) -> BTreeMap<String, CachedPair> {
    store
        .meta_get(PAIR_CACHE_META_KEY)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Paths Git reported as conflicting between two sessions at the last
/// classification, or `None` when the pair was never classified. For
/// surfaces that hold a store but no broker, such as the post-commit radar.
pub(crate) fn cached_conflicting_paths(
    store: &crate::store::BrokerStore,
    first: i64,
    second: i64,
) -> Option<Vec<String>> {
    read_cache(store)
        .remove(&pair_key_string((first.min(second), first.max(second))))
        .filter(|entry| entry.classified)
        .map(|entry| entry.conflicting_paths)
}

impl Broker {
    fn overlap_pair_cache(&self) -> BTreeMap<String, CachedPair> {
        read_cache(self.store_ref())
    }

    /// Ranked pairs from the cached classification, running no Git.
    ///
    /// Routine surfaces (`status`, hooks) read this; pairs the last refresh
    /// has not classified are reported as low and unclassified.
    pub fn overlap_pairs_snapshot(&self, overlaps: &[Overlap]) -> Vec<OverlapPair> {
        let cache = self.overlap_pair_cache();
        let mut pairs: Vec<OverlapPair> = group_overlaps(overlaps)
            .into_iter()
            .map(|(key, paths)| build_pair(key, &paths, cache.get(&pair_key_string(key))))
            .collect();
        rank(&mut pairs);
        pairs
    }

    /// Classify every overlapping pair whose inputs changed, and emit one
    /// `lease.overlap` event per pair that started overlapping or whose
    /// severity or conflicting paths changed. Returns the ranked pairs.
    pub(crate) fn classify_and_announce_overlaps(
        &mut self,
        overlaps: &[Overlap],
    ) -> Result<Vec<OverlapPair>, BrokerOpError> {
        let grouped = group_overlaps(overlaps);
        let mut cache = self.overlap_pair_cache();
        // A pair that stopped overlapping is forgotten, so overlapping again
        // later counts as a new start.
        cache.retain(|key, _| grouped.keys().any(|pair| pair_key_string(*pair) == *key));

        let sessions: BTreeMap<i64, String> = self
            .store_ref()
            .live_sessions()?
            .into_iter()
            .map(|session| (session.id, session.worktree_path))
            .collect();
        let state_for = |session: i64, paths: &[String]| -> Result<SessionState, StateError> {
            let worktree = sessions
                .get(&session)
                .ok_or_else(|| StateError::Other(format!("session {session} is not live")))?;
            let checkout = GitRepo::discover(Path::new(worktree))
                .map_err(|error| StateError::Other(format!("session {session}: {error}")))?;
            // An index with unmerged entries cannot become a tree, and a
            // half-finished merge or rebase is not the session's work yet.
            if let Some(operation) = checkout
                .operation_in_progress()
                .map_err(|error| StateError::Other(format!("session {session}: {error}")))?
            {
                return Err(StateError::MidOperation(operation.describe(session)));
            }
            session_state(&checkout, paths).map_err(StateError::Other)
        };

        let mut pairs = Vec::new();
        for (key, paths) in &grouped {
            let entry = cache.entry(pair_key_string(*key)).or_default();
            let left = state_for(key.0, paths);
            let right = state_for(key.1, paths);
            match (left, right) {
                (Ok(left), Ok(right)) => {
                    let current = input_key(&left, &right, paths);
                    if !entry.classified || entry.input_key != current {
                        match classify(self.repo_handle(), &left, &right, paths) {
                            Ok(conflicting) => {
                                entry.severity = Some(if conflicting.is_empty() {
                                    OverlapSeverity::Low
                                } else {
                                    OverlapSeverity::High
                                });
                                entry.reason = if conflicting.is_empty() {
                                    "the overlapping edits merge cleanly".into()
                                } else {
                                    format!(
                                        "Git reports a conflict on {} path(s)",
                                        conflicting.len()
                                    )
                                };
                                entry.conflicting_paths = conflicting;
                                entry.classified = true;
                            }
                            Err(error) => {
                                entry.severity = Some(OverlapSeverity::Low);
                                entry.conflicting_paths.clear();
                                entry.classified = false;
                                entry.reason = format!("could not classify: {error}");
                            }
                        }
                        entry.input_key = current;
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    entry.severity = Some(OverlapSeverity::Low);
                    entry.conflicting_paths.clear();
                    entry.classified = false;
                    entry.reason = match error {
                        StateError::MidOperation(reason) => reason,
                        StateError::Other(error) => format!("could not classify: {error}"),
                    };
                    entry.input_key.clear();
                }
            }
            let pair = build_pair(*key, paths, Some(entry));
            let signature = announcement_signature(&pair);
            if entry.announced.as_deref() != Some(signature.as_str()) {
                let payload = serde_json::json!({
                    "session_a": pair.session_a,
                    "session_b": pair.session_b,
                    // The first sample path, kept so readers of the old
                    // one-event-per-path payload still find a path here.
                    "path": pair.sample_paths.first(),
                    "severity": pair.severity,
                    "paths_count": pair.paths_count,
                    "conflicting_paths": pair.conflicting_paths,
                    "sample_paths": pair.sample_paths,
                    "classified": pair.classified,
                    "reason": pair.reason,
                });
                self.store().append_event(
                    crate::events::LEASE_OVERLAP,
                    Some(pair.session_a),
                    Some(&payload.to_string()),
                )?;
                entry.announced = Some(signature);
            }
            pairs.push(pair);
        }
        self.store()
            .meta_set(PAIR_CACHE_META_KEY, &serde_json::to_string(&cache)?)?;
        rank(&mut pairs);
        Ok(pairs)
    }

    /// The classified pair for two sessions, if they currently overlap.
    pub(crate) fn overlap_pair_between(
        &self,
        overlaps: &[Overlap],
        first: i64,
        second: i64,
    ) -> Option<OverlapPair> {
        let key = (first.min(second), first.max(second));
        self.overlap_pairs_snapshot(overlaps)
            .into_iter()
            .find(|pair| (pair.session_a, pair.session_b) == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(a: i64, b: i64, path: &str) -> Overlap {
        Overlap {
            session_a: a,
            session_b: b,
            path: path.into(),
        }
    }

    #[test]
    fn per_path_overlaps_group_into_one_finding_per_pair() {
        let grouped = group_overlaps(&[
            overlap(1, 2, "b.rs"),
            overlap(1, 2, "a.rs"),
            overlap(1, 3, "a.rs"),
        ]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped[&(1, 2)],
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[test]
    fn conflicting_pairs_rank_first_and_list_conflicts_first() {
        let conflicting = CachedPair {
            classified: true,
            severity: Some(OverlapSeverity::High),
            conflicting_paths: vec!["z.rs".into()],
            ..CachedPair::default()
        };
        let clean = CachedPair {
            classified: true,
            severity: Some(OverlapSeverity::Low),
            ..CachedPair::default()
        };
        let paths: Vec<String> = ["a.rs", "b.rs", "z.rs"].map(String::from).to_vec();
        let mut pairs = vec![
            build_pair((1, 2), &paths, Some(&clean)),
            build_pair((3, 4), &paths, Some(&conflicting)),
        ];
        rank(&mut pairs);
        assert_eq!(
            (pairs[0].session_a, pairs[0].severity),
            (3, OverlapSeverity::High)
        );
        assert_eq!(pairs[0].sample_paths[0], "z.rs");
        assert_eq!(pairs[1].severity, OverlapSeverity::Low);
    }

    #[test]
    fn an_unclassified_pair_is_low_and_says_why() {
        let pair = build_pair((1, 2), &["a.rs".to_string()], None);
        assert_eq!(pair.severity, OverlapSeverity::Low);
        assert!(!pair.classified);
        assert!(pair.reason.contains("not classified"), "{}", pair.reason);
    }
}
