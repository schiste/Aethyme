//! Deciding whether a session's work reached the default branch.
//!
//! `submit` is not the only way work lands. A session may open a pull request
//! and have it merged, which is the entire point of the review path. The
//! provider then writes a commit with a new SHA and no ancestry relationship to
//! the session's commits, and no promotion record exists. Ancestry answers
//! "no"; the work is nevertheless there.
//!
//! This module answers the question by content, against a *fixed historical
//! commit*. That qualifier is the whole design. An earlier attempt compared the
//! session against the default branch **tip**, which is correct only until the
//! next commit touches one of the same files:
//!
//! ```text
//! DIFFERS at main tip: store.rs   <- rewritten later by an unrelated change
//! same:                chau7_tabs.rs
//! ```
//!
//! Tip comparison asks "is this content present right now" when what `finish`
//! needs is "did this work land". Those agree only until the branch advances,
//! so the check would pass in exactly the cases that need no fixing and fail in
//! every real one. A specific historical commit, by contrast, never changes its
//! content, so a verdict computed against one stays true forever — which is
//! also what makes it worth recording rather than recomputing.
//!
//! Two kinds of evidence count, both about fixed commits:
//!
//! - **content**: the earliest commit on the branch holding every blob the
//!   session produced (a squash or a cherry-pick of the net diff);
//! - **patch equivalence**: every session commit has a patch-identical commit
//!   on the branch, one branch commit has all of them in its history, and that
//!   commit has the exact tree entries for every changed path. Patch IDs only
//!   nominate candidates; the fixed tree comparison is still required.
//!
//! [`work_landed`] is the one entry point for "did this work land": the
//! representation scan, the cleanup audit and the cleanup plan all ask it, so
//! they cannot reach opposite verdicts about the same head.
//!
//! Deliberately not used as evidence:
//!
//! - **ancestry**, which a squash or rebase merge destroys by construction;
//! - **the branch tip**, for the reason above;
//! - **a matching subject or author**, which a revert or a reworked commit
//!   also has;
//! - **an operator assertion**, for the same reason `already_represented` is
//!   not a choosable disposition in `main reconcile`: representation is a fact
//!   about content, and letting it be declared would make the check ceremonial.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::{BrokerOpError, GitRepo};

pub const REPRESENTATION_PLAN_SCHEMA_VERSION: u32 = 1;

/// A cap on the history walk, so a session whose work never landed cannot make
/// `finish` scan an entire branch. Candidates are already bounded by the
/// session's base (see [`find_landing`]); this only guards a pathological base.
/// A search that hits the cap reports itself truncated and skips the
/// patch-equivalence tier, whose cost grows with the same history.
pub const DEFAULT_SEARCH_CAP: usize = 2_000;
/// One shared 30-second ceiling for a landing search, not its surrounding
/// command. A configured or caller deadline may shorten it, never lengthen it.
const MAX_LANDING_SEARCH_BUDGET: Duration = Duration::from_secs(30);
/// A large but finite complete commit listing for oldest-first search.
const MAX_LANDING_CANDIDATE_LIST_BYTES: usize = 16 * 1024 * 1024;
const LANDING_SEARCH_ALGORITHM_VERSION: u8 = 3;

fn landing_query_count(candidate_count: usize, path_count: usize) -> Option<usize> {
    candidate_count
        .checked_add(1)? // the session-base comparison
        .checked_mul(path_count)
}

/// The net content a session produced.
///
/// Keyed by every path the session's commits touched, holding the blob at the
/// session head — or `None` where the session's net effect is a deletion.
/// Intermediate states are deliberately absent: a squash merge lands the net
/// diff, so the net diff is what has to be found on the default branch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionContent {
    pub base: String,
    pub head: String,
    pub paths: BTreeMap<String, Option<String>>,
}

impl SessionContent {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Whether a candidate commit carries the session's content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentVerdict {
    Present,
    /// Object IDs agree, but committed entry modes or types differ.
    TreeEntryMismatch {
        paths: Vec<String>,
    },
    /// The first object mismatch, with both sides named.
    Absent {
        path: String,
        wanted: Option<String>,
        found: Option<String>,
    },
}

impl ContentVerdict {
    pub fn is_present(&self) -> bool {
        matches!(self, ContentVerdict::Present)
    }
}

/// Where a session's work was found, or why it was not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingOutcome {
    /// The session's net content is already present at its own base: it
    /// produced no change the default branch does not already have. Recording a
    /// landing commit here would name a commit that did not carry the work.
    NothingToRepresent,
    /// The earliest default-branch commit carrying every path of the session's
    /// content.
    Landed(Landing),
    /// No candidate carried the whole content.
    NotFound {
        /// The candidate that matched the most paths, and the first path it
        /// missed — the most useful thing to show an operator who expected the
        /// work to be there.
        closest: Option<Closest>,
    },
}

/// How work reached a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingEvidence {
    /// The head is an ancestor (fast-forward or merge delivery).
    Ancestry,
    /// A commit on the branch carries the net content (squash delivery).
    Content,
    /// Every session commit's patch is on the branch under another SHA, and
    /// the candidate's complete changed-path tree entries match.
    PatchEquivalent,
    /// The head changes nothing the branch does not already hold.
    NoNetChange,
}

impl LandingEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ancestry => "ancestry",
            Self::Content => "content",
            Self::PatchEquivalent => "patch equivalence",
            Self::NoNetChange => "no net change",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Landing {
    /// The commit that carried the work: the earliest holding the content, or,
    /// for patch equivalence, the earliest whose history holds every
    /// equivalent patch and whose changed-path tree entries match.
    pub commit: String,
    pub subject: String,
    /// Candidates examined before this one; 0 means the oldest candidate.
    pub position: usize,
    pub paths: usize,
    /// Content or fully verified patch-equivalence evidence.
    pub evidence: LandingEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Closest {
    pub commit: String,
    pub subject: String,
    pub matched_paths: usize,
    pub missing_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LandingSearch {
    pub outcome: LandingOutcome,
    pub examined: usize,
    /// True when the walk stopped at [`DEFAULT_SEARCH_CAP`] rather than
    /// exhausting the candidates, so a `NotFound` can say it was not conclusive.
    pub truncated: bool,
}

impl LandingSearch {
    pub fn landing(&self) -> Option<&Landing> {
        match &self.outcome {
            LandingOutcome::Landed(landing) => Some(landing),
            _ => None,
        }
    }

    /// A session is closable when its work landed, or when it produced nothing
    /// the branch does not already hold.
    pub fn represented(&self) -> bool {
        matches!(
            self.outcome,
            LandingOutcome::Landed(_) | LandingOutcome::NothingToRepresent
        )
    }
}

/// The net content a session produced between `base` and `head`.
pub fn session_content(
    repo: &GitRepo,
    base: &str,
    head: &str,
) -> Result<SessionContent, BrokerOpError> {
    let changed = repo
        .changed_between_all_paths(base, head)
        .map_err(|source| BrokerOpError::RepresentationUnavailable {
            reason: format!("cannot diff {} to {}: {source}", short(base), short(head)),
        })?;

    // One `cat-file` for the whole diff, not one `rev-parse` per path. The
    // cleanup plan behind `broker status` builds this map for every retained
    // worktree on every call, so a repository holding thirty sessions with
    // ~46k changed paths between them was forking ~46k processes to answer a
    // question one process can answer. The candidate search in `find_landing`
    // was batched for the same reason (#408); this is the other half.
    let wanted = repo
        .objects_at_many(
            &changed
                .iter()
                .map(|path| (head, path.as_str()))
                .collect::<Vec<_>>(),
        )
        .map_err(|source| BrokerOpError::RepresentationUnavailable {
            reason: format!("cannot read session content at {}: {source}", short(head)),
        })?;

    Ok(SessionContent {
        base: base.to_string(),
        head: head.to_string(),
        paths: changed.into_iter().zip(wanted).collect(),
    })
}

/// Whether `target` — a fixed historical commit — holds the session's content.
pub fn content_at(
    repo: &GitRepo,
    content: &SessionContent,
    target: &str,
) -> Result<ContentVerdict, BrokerOpError> {
    let found = repo
        .objects_at_many(
            &content
                .paths
                .keys()
                .map(|path| (target, path.as_str()))
                .collect::<Vec<_>>(),
        )
        .map_err(|source| BrokerOpError::RepresentationUnavailable {
            reason: format!("cannot read session content at {}: {source}", short(target)),
        })?;

    // Batched, so every path is read before the first mismatch is reported;
    // the comparison still stops at the first mismatch, in the same order, so
    // the path named in `Absent` is unchanged. A read failure propagates
    // rather than reading as "absent": a failed read must never be able to
    // assert that a worktree holds nothing worth keeping.
    for ((path, wanted), found) in content.paths.iter().zip(found) {
        if &found != wanted {
            return Ok(ContentVerdict::Absent {
                path: path.clone(),
                wanted: wanted.clone(),
                found,
            });
        }
    }
    if !tree_entries_equal(repo, content, target)? {
        return Ok(ContentVerdict::TreeEntryMismatch {
            paths: content.paths.keys().cloned().collect(),
        });
    }
    Ok(ContentVerdict::Present)
}

/// Object identity omits executable bits and the distinction between a blob
/// stored as a regular file or a symlink. Verify complete tree entries only
/// after the inexpensive batched object comparison finds a prospective match.
fn tree_entries_equal(
    repo: &GitRepo,
    content: &SessionContent,
    target: &str,
) -> Result<bool, BrokerOpError> {
    repo.paths_equal(
        &content.head,
        target,
        &content.paths.keys().cloned().collect::<Vec<_>>(),
    )
    .map_err(|source| {
        if matches!(&source, crate::GitError::TimedOut { .. }) {
            landing_search_budget_expired(format!(
                "while verifying committed entry modes: {source}"
            ))
        } else {
            BrokerOpError::RepresentationUnavailable {
                reason: format!("cannot verify committed entry types and modes: {source}"),
            }
        }
    })
}

/// Candidates asked about per `cat-file` batch. Most landings are found near
/// the start of the window, so the search stops long before asking about all
/// of it; a batch keeps one query list from growing with the whole window.
const CANDIDATE_BATCH: usize = 64;

/// Find the earliest commit on `branch_tip` that carries the session's work.
///
/// The candidate set is exactly the commits the default branch gained since the
/// session's base. That bound is not a budget but a fact: work cannot have
/// landed before the session branched, so nothing older can be the landing
/// commit. It also means the walk is short in the normal case and terminates
/// without an arbitrary limit.
///
/// Content is tried first; the *earliest* match is taken rather than the latest
/// because that is the commit that carried the work, and every later commit
/// merely inherits it. Patch equivalence is also checked as a candidate source
/// when no commit holds the content (see [`patch_landing`]), but never replaces
/// the exact changed-path tree-entry comparison.
pub fn find_landing(
    repo: &GitRepo,
    content: &SessionContent,
    branch_tip: &str,
    cap: usize,
) -> Result<LandingSearch, BrokerOpError> {
    find_landing_within(repo, content, branch_tip, cap, None)
}

fn find_landing_within(
    repo: &GitRepo,
    content: &SessionContent,
    branch_tip: &str,
    cap: usize,
    deadline: Option<std::time::Instant>,
) -> Result<LandingSearch, BrokerOpError> {
    let started = Instant::now();
    let budget = crate::git::git_timeout().min(MAX_LANDING_SEARCH_BUDGET);
    let requested_deadline =
        started
            .checked_add(budget)
            .ok_or_else(|| BrokerOpError::RepresentationUnavailable {
                reason: "could not establish a bounded landing-search deadline".into(),
            })?;
    let search_deadline =
        deadline.map_or(requested_deadline, |caller| caller.min(requested_deadline));
    let _git_deadline = crate::git::limit_git_until(search_deadline);
    find_landing_with_limits(
        repo,
        content,
        branch_tip,
        cap,
        search_deadline,
        crate::git::MAX_GIT_BATCH_QUERY_COUNT,
        MAX_LANDING_CANDIDATE_LIST_BYTES,
    )
}

fn landing_search_budget_expired(detail: impl std::fmt::Display) -> BrokerOpError {
    BrokerOpError::RepresentationUnavailable {
        reason: format!("landing search stopped at the time budget {detail}"),
    }
}

fn find_landing_with_limits(
    repo: &GitRepo,
    content: &SessionContent,
    branch_tip: &str,
    cap: usize,
    deadline: Instant,
    query_budget: usize,
    candidate_list_byte_budget: usize,
) -> Result<LandingSearch, BrokerOpError> {
    if content.is_empty() {
        return Ok(LandingSearch {
            outcome: LandingOutcome::NothingToRepresent,
            examined: 0,
            truncated: false,
        });
    }

    let unavailable = |what: String| BrokerOpError::RepresentationUnavailable { reason: what };
    if Instant::now() >= deadline {
        return Err(landing_search_budget_expired(
            "before reading its base snapshot",
        ));
    }

    // Content already at the session's own base means the session changed
    // nothing the branch lacks. Naming a landing commit here would be a lie
    // about which commit carried the work.
    if content_at(repo, content, &content.base)?.is_present() {
        return Ok(LandingSearch {
            outcome: LandingOutcome::NothingToRepresent,
            examined: 0,
            truncated: false,
        });
    }

    let candidates = repo
        .commits_between_oldest_bounded(&content.base, branch_tip, candidate_list_byte_budget)
        .map_err(|source| {
            unavailable(format!(
                "cannot list commits {}..{}: {source}",
                short(&content.base),
                short(branch_tip)
            ))
        })?;

    let truncated = candidates.len() > cap;
    let window = &candidates[..candidates.len().min(cap)];
    let paths = content.paths.iter().collect::<Vec<_>>();
    let query_count = landing_query_count(window.len(), paths.len())
        .ok_or_else(|| unavailable("landing candidate/path query count overflowed".into()))?;
    if query_count > query_budget || query_count > crate::git::MAX_GIT_BATCH_QUERY_COUNT {
        return Err(unavailable(format!(
            "exact-content landing search needs {query_count} candidate/path queries; the limit is {}",
            query_budget.min(crate::git::MAX_GIT_BATCH_QUERY_COUNT)
        )));
    }
    let mut best: Option<(String, usize, String)> = None;

    for (batch_index, batch) in window.chunks(CANDIDATE_BATCH).enumerate() {
        if Instant::now() >= deadline {
            return Err(landing_search_budget_expired(format!(
                "after {} of {} candidate commits",
                batch_index * CANDIDATE_BATCH,
                window.len()
            )));
        }
        let queries = batch
            .iter()
            .flat_map(|candidate| {
                paths
                    .iter()
                    .map(move |(path, _)| (candidate.as_str(), path.as_str()))
            })
            .collect::<Vec<_>>();
        let blobs = repo
            .objects_at_many(&queries)
            .map_err(|source| unavailable(format!("cannot read candidate content: {source}")))?;
        for (offset, (candidate, found)) in batch.iter().zip(blobs.chunks(paths.len())).enumerate()
        {
            let position = batch_index * CANDIDATE_BATCH + offset;
            // Paths are compared in order and the first mismatch ends the
            // count, so "closest" means the longest matching prefix.
            let matched = paths
                .iter()
                .zip(found)
                .take_while(|((_, wanted), found)| *wanted == *found)
                .count();
            if matched == paths.len() && tree_entries_equal(repo, content, candidate)? {
                return Ok(LandingSearch {
                    outcome: LandingOutcome::Landed(Landing {
                        commit: candidate.clone(),
                        subject: subject(repo, candidate),
                        position,
                        paths: paths.len(),
                        evidence: LandingEvidence::Content,
                    }),
                    examined: position + 1,
                    truncated: false,
                });
            }
            if matched == paths.len() {
                // Blob equality alone does not establish any complete entry match.
                continue;
            }
            if best.as_ref().is_none_or(|(_, prev, _)| matched > *prev) {
                best = Some((candidate.clone(), matched, paths[matched].0.clone()));
            }
        }
    }

    // A truncated window says nothing about the history beyond it, and the
    // patch comparison would have to read all of that history.
    if !truncated
        && let Some((commit, position)) = patch_landing(repo, content, branch_tip, window)?
    {
        // Patch IDs nominate a candidate; only exact entries on every changed
        // path can establish that the fixed commit carries this session's work.
        if tree_entries_equal(repo, content, &commit)? {
            return Ok(LandingSearch {
                outcome: LandingOutcome::Landed(Landing {
                    subject: subject(repo, &commit),
                    commit,
                    position,
                    paths: paths.len(),
                    evidence: LandingEvidence::PatchEquivalent,
                }),
                examined: window.len(),
                truncated: false,
            });
        }
    }

    Ok(LandingSearch {
        outcome: LandingOutcome::NotFound {
            closest: best.filter(|(_, matched, _)| *matched > 0).map(
                |(commit, matched_paths, missing_path)| Closest {
                    subject: subject(repo, &commit),
                    commit,
                    matched_paths,
                    missing_path,
                },
            ),
        },
        examined: window.len(),
        truncated,
    })
}

/// The earliest candidate whose history holds a patch-identical copy of every
/// session commit, if there is one. The caller still verifies exact changed-
/// path tree entries before treating the candidate as landed.
///
/// This is the rebase-then-merge case (#408): the same patches applied to a
/// newer base can produce different blobs, so no commit holds the session's
/// exact content, yet `git patch-id --verbatim` agrees for every commit. These
/// conditions make the patch result a complete candidate rather than a likely
/// match; the caller's exact tree-entry check remains required:
///
/// - **every** session commit after the base must be accounted for, either by
///   an equivalent on the branch or by being on the branch itself -- one
///   landed commit out of two is not "landed";
/// - a merge commit in the session has no patch id, and can carry a conflict
///   resolution no patch describes, so it counts only if that merge itself is
///   on the branch;
/// - a single branch commit must have all of them in its history, so the
///   candidate names one fixed commit that could have carried the work.
///
/// [`GitRepo::cherry_marked`] compares the session side against the branch
/// commits since the two diverged, including those reached through a merge's
/// second parent.
fn patch_landing(
    repo: &GitRepo,
    content: &SessionContent,
    branch_tip: &str,
    candidates: &[String],
) -> Result<Option<(String, usize)>, BrokerOpError> {
    let unavailable = |source: crate::GitError| BrokerOpError::RepresentationUnavailable {
        reason: format!(
            "cannot compare patches with {}: {source}",
            short(branch_tip)
        ),
    };
    let session_commits = repo
        .commits_between_oldest(&content.base, &content.head)
        .map_err(unavailable)?;
    if session_commits.is_empty() {
        return Ok(None);
    }
    // Non-merge session commits not on the branch, and whether each has an
    // equivalent there. A session commit missing from this list is a merge,
    // or is on the branch already.
    let paired_marks = repo
        .cherry_marked_both(branch_tip, &content.head)
        .map_err(unavailable)?;
    let unmerged = paired_marks
        .right_only
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    if session_commits
        .iter()
        .any(|commit| unmerged.get(commit) == Some(&false))
    {
        return Ok(None);
    }
    // The branch commits carrying the equivalents, plus every session commit
    // that has none -- which the covering search below then requires to be
    // on the branch itself.
    let mut carriers = paired_marks
        .left_only
        .into_iter()
        .filter_map(|(commit, equivalent)| equivalent.then_some(commit))
        .collect::<Vec<_>>();
    carriers.extend(
        session_commits
            .iter()
            .filter(|commit| !unmerged.contains_key(*commit))
            .cloned(),
    );
    // The earliest candidate descending from (or equal to) every carrier.
    let mut covering: Option<std::collections::BTreeSet<String>> = None;
    for carrier in &carriers {
        let mut reach = repo
            .descendants_towards(carrier, branch_tip)
            .map_err(unavailable)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        reach.insert(carrier.clone());
        covering = Some(match covering {
            None => reach,
            Some(previous) => previous.intersection(&reach).cloned().collect(),
        });
    }
    let Some(covering) = covering else {
        return Ok(None);
    };
    Ok(candidates
        .iter()
        .position(|candidate| covering.contains(candidate))
        .map(|position| (candidates[position].clone(), position)))
}

/// The answer to "did this head's work land on `target`".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LandingVerdict {
    Landed {
        evidence: LandingEvidence,
        /// The fixed commit that carried the work; absent for ancestry and
        /// for a head with no net change, where no single commit is needed.
        #[serde(skip_serializing_if = "Option::is_none")]
        landed_by: Option<String>,
    },
    NotLanded {
        examined: usize,
        /// True when the search stopped at [`DEFAULT_SEARCH_CAP`], so the
        /// negative is not conclusive.
        truncated: bool,
    },
}

/// The search behavior that a cached verdict answers. Keeping the cap and
/// algorithm version in the key prevents a result from a narrower or changed
/// search from answering a different query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LandingSearchProfile {
    candidate_cap: usize,
    algorithm_version: u8,
}

impl Default for LandingSearchProfile {
    fn default() -> Self {
        Self {
            candidate_cap: DEFAULT_SEARCH_CAP,
            algorithm_version: LANDING_SEARCH_ALGORITHM_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LandingCacheKey {
    repository_root: PathBuf,
    head: String,
    source_base: Option<String>,
    target: String,
    profile: LandingSearchProfile,
}

/// Complete immutable history verdicts scoped to one broker command. This is
/// intentionally not stored on `Broker` or persisted: callers create it for a
/// plan/audit invocation and pass it only through that invocation.
#[derive(Debug, Default)]
pub(crate) struct LandingVerdictCache {
    verdicts: BTreeMap<LandingCacheKey, LandingVerdict>,
    #[cfg(test)]
    hits: usize,
}

impl LandingVerdictCache {
    fn get(&mut self, key: &LandingCacheKey) -> Option<LandingVerdict> {
        let verdict = self.verdicts.get(key).cloned();
        #[cfg(test)]
        if verdict.is_some() {
            self.hits += 1;
        }
        verdict
    }

    fn insert_complete(&mut self, key: LandingCacheKey, verdict: &LandingVerdict) {
        if matches!(
            verdict,
            LandingVerdict::Landed { .. }
                | LandingVerdict::NotLanded {
                    truncated: false,
                    ..
                }
        ) {
            self.verdicts.insert(key, verdict.clone());
        }
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> usize {
        self.hits
    }
}

impl LandingVerdict {
    pub fn is_landed(&self) -> bool {
        matches!(self, LandingVerdict::Landed { .. })
    }
}

/// Where a head's work is measured from: where it diverged from `target`.
///
/// Deliberately not the session's recorded start. A worktree can be adopted
/// by a second session while holding the first one's unlanded commits; the
/// second session then changed nothing *itself*, and measuring from its start
/// would call a checkout holding lost-if-removed work "no net change". The
/// divergence point counts every commit the head holds that `target` lacks,
/// which is the question both a recording and a removal have to answer.
pub fn landing_base(repo: &GitRepo, head: &str, target: &str) -> Result<String, BrokerOpError> {
    Ok(repo.merge_base(head, target)?)
}

/// Whether `head`'s work landed on `target`: the one predicate behind the
/// representation scan, the cleanup audit and the cleanup plan (#408).
///
/// Tried in order of cost: ancestry, then the content and patch-equivalence
/// search of [`find_landing`] over at most [`DEFAULT_SEARCH_CAP`] commits the
/// target gained since the two diverged (see [`landing_base`]). Every positive names
/// fixed commits, so it stays true as the target advances; a negative only
/// means nothing proved it.
pub fn work_landed(
    repo: &GitRepo,
    head: &str,
    target: &str,
) -> Result<LandingVerdict, BrokerOpError> {
    work_landed_within(repo, head, target, None)
}

/// [`work_landed`], failing with [`BrokerOpError::RepresentationUnavailable`]
/// once `deadline` passes during the content search. A search the deadline
/// stopped proved nothing, so it can never read as landed.
pub fn work_landed_within(
    repo: &GitRepo,
    head: &str,
    target: &str,
    deadline: Option<std::time::Instant>,
) -> Result<LandingVerdict, BrokerOpError> {
    work_landed_with_profile(
        repo,
        head,
        target,
        LandingSearchProfile::default(),
        deadline,
    )
}

/// Reuse a complete verdict for an exact immutable commit pair and search
/// profile. Non-OID inputs use the ordinary path without caching; cleanup
/// callers pass full commit OIDs captured from their target snapshots.
pub(crate) fn work_landed_with_cache(
    repo: &GitRepo,
    head: &str,
    target: &str,
    deadline: Option<std::time::Instant>,
    profile: LandingSearchProfile,
    cache: &mut LandingVerdictCache,
) -> Result<LandingVerdict, BrokerOpError> {
    let (Some(head_oid), Some(target_oid)) = (full_commit_oid(head), full_commit_oid(target))
    else {
        return work_landed_with_profile(repo, head, target, profile, deadline);
    };
    let repository_root = repo.root().to_path_buf();

    if repo.is_ancestor(&head_oid, &target_oid) {
        let key = LandingCacheKey {
            repository_root,
            head: head_oid,
            source_base: None,
            target: target_oid,
            profile,
        };
        if let Some(verdict) = cache.get(&key) {
            return Ok(verdict);
        }
        let verdict = LandingVerdict::Landed {
            evidence: LandingEvidence::Ancestry,
            landed_by: None,
        };
        cache.insert_complete(key, &verdict);
        return Ok(verdict);
    }

    let base = landing_base(repo, &head_oid, &target_oid)?;
    let key = LandingCacheKey {
        repository_root,
        head: head_oid.clone(),
        source_base: Some(base.clone()),
        target: target_oid.clone(),
        profile,
    };
    if let Some(verdict) = cache.get(&key) {
        return Ok(verdict);
    }

    let verdict = work_landed_from_base(repo, &head_oid, &target_oid, &base, profile, deadline)?;
    cache.insert_complete(key, &verdict);
    Ok(verdict)
}

fn full_commit_oid(value: &str) -> Option<String> {
    matches!(value.len(), 40 | 64)
        .then_some(value)
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

fn work_landed_with_profile(
    repo: &GitRepo,
    head: &str,
    target: &str,
    profile: LandingSearchProfile,
    deadline: Option<std::time::Instant>,
) -> Result<LandingVerdict, BrokerOpError> {
    if repo.is_ancestor(head, target) {
        return Ok(LandingVerdict::Landed {
            evidence: LandingEvidence::Ancestry,
            landed_by: None,
        });
    }
    let base = landing_base(repo, head, target)?;
    work_landed_from_base(repo, head, target, &base, profile, deadline)
}

fn work_landed_from_base(
    repo: &GitRepo,
    head: &str,
    target: &str,
    base: &str,
    profile: LandingSearchProfile,
    deadline: Option<std::time::Instant>,
) -> Result<LandingVerdict, BrokerOpError> {
    let content = session_content(repo, base, head)?;
    let search = find_landing_within(repo, &content, target, profile.candidate_cap, deadline)?;
    Ok(match search.outcome {
        LandingOutcome::NothingToRepresent => LandingVerdict::Landed {
            evidence: LandingEvidence::NoNetChange,
            landed_by: None,
        },
        LandingOutcome::Landed(landing) => LandingVerdict::Landed {
            evidence: landing.evidence,
            landed_by: Some(landing.commit),
        },
        LandingOutcome::NotFound { .. } => LandingVerdict::NotLanded {
            examined: search.examined,
            truncated: search.truncated,
        },
    })
}

fn subject(repo: &GitRepo, commit: &str) -> String {
    repo.commit_message(commit)
        .map(|message| message.lines().next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

/// Digest over what a recording would assert, so the apply step is bound to the
/// plan that was reviewed. Covers the session identity, the exact head being
/// represented, and the representing commit — changing any of them invalidates
/// the confirmation.
pub fn plan_digest(session_id: i64, head: &str, representing: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"representation-v1\n");
    hasher.update(session_id.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(head.as_bytes());
    hasher.update(b"\n");
    hasher.update(representing.unwrap_or("none").as_bytes());
    hasher.update(b"\n");
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init(root: &Path) {
        git(root, &["init", "-q", "-b", "main"]);
    }

    fn write(root: &Path, path: &str, body: &str) {
        std::fs::write(root.join(path), body).unwrap();
    }

    fn commit(root: &Path, message: &str) -> String {
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
        git(root, &["rev-parse", "HEAD"])
    }

    /// A repository with `base.txt` on main, returning the base commit.
    fn seeded() -> (tempfile::TempDir, GitRepo, String) {
        let tmp = tempfile::tempdir().unwrap();
        init(tmp.path());
        write(tmp.path(), "base.txt", "base\n");
        let base = commit(tmp.path(), "base");
        let repo = GitRepo::discover(tmp.path()).unwrap();
        (tmp, repo, base)
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_must_land_even_when_blob_identity_is_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, repo, _) = seeded();
        // Brackets are literal path bytes; newline must survive the NUL census.
        let path = "[run]\n.sh";
        write(tmp.path(), path, "#!/bin/sh\nexit 0\n");
        let base = commit(tmp.path(), "regular non-executable baseline");
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        std::fs::set_permissions(
            tmp.path().join(path),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let head = commit(tmp.path(), "make executable");
        assert_eq!(repo.blob_at(&base, path), repo.blob_at(&head, path));
        let content = session_content(&repo, &base, &head).unwrap();
        assert!(matches!(
            content_at(&repo, &content, &base).unwrap(),
            ContentVerdict::TreeEntryMismatch { .. }
        ));
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "unrelated.txt", "target change\n");
        let target = commit(tmp.path(), "unrelated target change");
        assert!(matches!(
            find_landing(&repo, &content, &target, DEFAULT_SEARCH_CAP)
                .unwrap()
                .outcome,
            LandingOutcome::NotFound { .. }
        ));
        assert!(matches!(
            work_landed(&repo, &head, &target).unwrap(),
            LandingVerdict::NotLanded { truncated: false, .. }
        ));
        git(tmp.path(), &["cherry-pick", &head]);
        let landed = git(tmp.path(), &["rev-parse", "HEAD"]);
        assert!(content_at(&repo, &content, &landed).unwrap().is_present());
        assert!(
            find_landing(&repo, &content, &landed, DEFAULT_SEARCH_CAP)
                .unwrap()
                .represented()
        );
        assert!(matches!(
            work_landed(&repo, &head, &landed).unwrap(),
            LandingVerdict::Landed {
                evidence: LandingEvidence::Content,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_type_must_land_even_when_blob_identity_is_unchanged() {
        use std::os::unix::fs::symlink;
        let (tmp, repo, _) = seeded();
        write(tmp.path(), "entry", "destination.txt");
        let base = commit(tmp.path(), "regular file baseline");
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        std::fs::remove_file(tmp.path().join("entry")).unwrap();
        symlink("destination.txt", tmp.path().join("entry")).unwrap();
        let head = commit(tmp.path(), "convert to symlink");
        assert_eq!(repo.blob_at(&base, "entry"), repo.blob_at(&head, "entry"));
        let content = session_content(&repo, &base, &head).unwrap();
        assert!(!content_at(&repo, &content, &base).unwrap().is_present());
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "unrelated.txt", "target change\n");
        let target = commit(tmp.path(), "unrelated target change");
        assert!(matches!(
            find_landing(&repo, &content, &target, DEFAULT_SEARCH_CAP)
                .unwrap()
                .outcome,
            LandingOutcome::NotFound { .. }
        ));
        assert!(matches!(
            work_landed(&repo, &head, &target).unwrap(),
            LandingVerdict::NotLanded { truncated: false, .. }
        ));
        git(tmp.path(), &["cherry-pick", &head]);
        let landed = git(tmp.path(), &["rev-parse", "HEAD"]);
        assert!(content_at(&repo, &content, &landed).unwrap().is_present());
        assert!(
            find_landing(&repo, &content, &landed, DEFAULT_SEARCH_CAP)
                .unwrap()
                .represented()
        );
        assert!(matches!(
            work_landed(&repo, &head, &landed).unwrap(),
            LandingVerdict::Landed {
                evidence: LandingEvidence::Content,
                ..
            }
        ));
    }

    fn cached_landing_fixture() -> (tempfile::TempDir, GitRepo, String, String, String) {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "landed.txt", "session content\n");
        let head = commit(tmp.path(), "session content");

        git(tmp.path(), &["checkout", "-q", "-b", "alternative", &base]);
        write(tmp.path(), "alternative.txt", "unrelated target\n");
        let alternative_target = commit(tmp.path(), "unrelated target");

        git(tmp.path(), &["checkout", "-q", "main"]);
        for index in 0..3 {
            write(tmp.path(), &format!("filler-{index}.txt"), "filler\n");
            if index == 2 {
                write(tmp.path(), "landed.txt", "session content\n");
            }
            commit(tmp.path(), &format!("main candidate {index}"));
        }
        let landed_target = git(tmp.path(), &["rev-parse", "HEAD"]);
        (tmp, repo, head, landed_target, alternative_target)
    }

    #[test]
    fn session_content_records_the_net_effect_not_the_intermediate_states() {
        let (tmp, repo, base) = seeded();
        write(tmp.path(), "a.txt", "first\n");
        commit(tmp.path(), "a first");
        write(tmp.path(), "a.txt", "second\n");
        let head = commit(tmp.path(), "a second");

        let content = session_content(&repo, &base, &head).unwrap();

        assert_eq!(content.paths.len(), 1);
        let blob = content.paths.get("a.txt").unwrap().clone().unwrap();
        assert_eq!(blob, repo.blob_at(&head, "a.txt").unwrap());
        // The intermediate "first" state is deliberately not represented: a
        // squash merge lands the net diff, so the net diff is what must be found.
        assert_ne!(Some(blob), repo.blob_at(&base, "a.txt"));
    }

    #[test]
    fn a_rename_representation_requires_both_the_new_path_and_source_deletion() {
        let (tmp, repo, _) = seeded();
        write(tmp.path(), "old.txt", "same body\n");
        let base = commit(tmp.path(), "old path baseline");
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        git(tmp.path(), &["mv", "old.txt", "new.txt"]);
        let head = commit(tmp.path(), "rename old path to new path");

        let content = session_content(&repo, &base, &head).unwrap();
        assert_eq!(content.paths.len(), 2);
        assert!(content.paths.get("old.txt").unwrap().is_none());
        assert!(content.paths.get("new.txt").unwrap().is_some());

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "new.txt", "same body\n");
        let both_paths = commit(tmp.path(), "add destination while keeping source");

        assert!(
            !content_at(&repo, &content, &both_paths)
                .unwrap()
                .is_present()
        );
        assert!(content_at(&repo, &content, &head).unwrap().is_present());
        assert!(matches!(
            find_landing(&repo, &content, &both_paths, DEFAULT_SEARCH_CAP)
                .unwrap()
                .outcome,
            LandingOutcome::NotFound { .. }
        ));
        assert!(
            find_landing(&repo, &content, &head, DEFAULT_SEARCH_CAP)
                .unwrap()
                .represented()
        );
    }

    /// The batched read must answer exactly what the per-path read answered.
    ///
    /// `session_content` reads every changed path in one `cat-file` instead of
    /// one `rev-parse` per path, because the cleanup plan behind `broker
    /// status` builds this map for every retained worktree. The equivalence is
    /// the whole risk of that change, so it is checked against `blob_at` on a
    /// diff wide enough that a truncation or an ordering slip would show up:
    /// `cat-file --batch-check` answers in input order, and this asserts it.
    #[test]
    fn session_content_batch_matches_the_per_path_read() {
        let (tmp, repo, base) = seeded();
        // Wider than one batch, and past the point where a path list could be
        // dropped or reordered without a single-path test noticing.
        let width = CANDIDATE_BATCH * 2 + 7;
        for i in 0..width {
            write(tmp.path(), &format!("f{i:03}.txt"), &format!("body {i}\n"));
        }
        let head = commit(tmp.path(), "a wide diff");

        let content = session_content(&repo, &base, &head).unwrap();

        assert_eq!(content.paths.len(), width);
        // BTreeMap iteration is sorted; the batch is answered in the order the
        // paths were asked, which is the order `git diff` listed them.
        let expected = repo
            .changed_between(&base, &head)
            .unwrap()
            .iter()
            .map(|path| (path.clone(), repo.blob_at(&head, path)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(content.paths, expected);
    }

    /// Point a gitlink at `target` without a checkout of the submodule.
    fn set_gitlink(root: &Path, path: &str, target: &str) {
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{target},{path}"),
            ],
        );
    }

    /// A session that only moves a submodule pointer has content to keep.
    ///
    /// `cat-file --batch-check` reports a gitlink as `submodule`, not `blob`.
    /// Read as "no object", the old and the new pointer both became `None`,
    /// so the session's content compared equal to its own base and the probe
    /// called the worktree "nothing to represent": the one direction a
    /// cleanup decision must not fail in. The per-path read compared the
    /// pointers themselves, and the batched read must too.
    #[test]
    fn a_submodule_pointer_bump_is_not_already_represented() {
        let (tmp, repo, _) = seeded();
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        set_gitlink(tmp.path(), "vendor/lib", old);
        git(tmp.path(), &["commit", "-q", "-m", "add submodule"]);
        let base = git(tmp.path(), &["rev-parse", "HEAD"]);
        set_gitlink(tmp.path(), "vendor/lib", new);
        git(tmp.path(), &["commit", "-q", "-m", "bump submodule"]);
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);

        let content = session_content(&repo, &base, &head).unwrap();

        assert_eq!(
            content.paths.get("vendor/lib"),
            Some(&Some(new.to_string()))
        );
        assert!(matches!(
            content_at(&repo, &content, &base).unwrap(),
            ContentVerdict::Absent { ref path, ref found, .. }
                if path == "vendor/lib" && found.as_deref() == Some(old)
        ));
        let search = find_landing(&repo, &content, &base, DEFAULT_SEARCH_CAP).unwrap();
        assert_ne!(search.outcome, LandingOutcome::NothingToRepresent);
        assert!(!search.represented());
    }

    /// A trailing carriage return is part of a path, and the batch must not
    /// lose it: Git strips one from each batch input line, which would read
    /// `plain.txt\r` as `plain.txt` and answer a different file's object.
    /// (A newline cannot reach this map at all: the diff parser drops such
    /// paths before they are recorded.)
    #[test]
    fn session_content_keeps_paths_with_spaces_and_carriage_returns_distinct() {
        let (tmp, repo, base) = seeded();
        write(tmp.path(), "plain.txt", "three\n");
        write(tmp.path(), "plain.txt\r", "four\n");
        write(tmp.path(), "with space.txt", "two\n");
        let head = commit(tmp.path(), "awkward names");

        let content = session_content(&repo, &base, &head).unwrap();

        assert_eq!(content.paths.len(), 3);
        for (path, wanted) in &content.paths {
            assert!(wanted.is_some(), "{path:?} has no object");
            assert_eq!(wanted, &repo.blob_at(&head, path), "{path:?}");
        }
        assert_ne!(
            content.paths.get("plain.txt"),
            content.paths.get("plain.txt\r")
        );
        assert!(content_at(&repo, &content, &head).unwrap().is_present());
    }

    /// The batched read answers every kind of tree entry exactly as the
    /// per-path `rev-parse` does: a blob, a tree, a gitlink, a missing path,
    /// and names with spaces and newlines, in input order.
    #[test]
    fn batched_object_read_matches_the_per_path_read_for_every_entry_kind() {
        let (tmp, repo, _) = seeded();
        std::fs::create_dir(tmp.path().join("dir")).unwrap();
        write(tmp.path(), "dir/inner.txt", "inner\n");
        write(tmp.path(), "line\nbreak.txt", "nl\n");
        write(tmp.path(), "with space.txt", "sp\n");
        write(tmp.path(), "base.txt\r", "cr\n");
        git(tmp.path(), &["add", "-A"]);
        set_gitlink(
            tmp.path(),
            "vendor/lib",
            "3333333333333333333333333333333333333333",
        );
        git(tmp.path(), &["commit", "-q", "-m", "every kind"]);
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);

        let paths = [
            "base.txt",
            "dir",
            "vendor/lib",
            "no/such/path",
            "with space.txt",
            "line\nbreak.txt",
            "base.txt\r",
            "base.txt",
        ];
        let queries = paths
            .iter()
            .map(|path| (head.as_str(), *path))
            .collect::<Vec<_>>();
        let batched = repo.objects_at_many(&queries).unwrap();
        let per_path = paths
            .iter()
            .map(|path| repo.blob_at(&head, path))
            .collect::<Vec<_>>();
        assert_eq!(batched, per_path);
        assert_eq!(batched[3], None);
        assert_eq!(batched.iter().filter(|found| found.is_some()).count(), 7);
        assert_ne!(batched[6], batched[7]);
    }

    /// A path Git cannot resolve is `None`, batched or not.
    ///
    /// The batch answers `<input> missing` for an absent path where `rev-parse
    /// --verify --quiet` fails, and both must mean the same thing: a file the
    /// session deleted has no blob to compare, so its content is not present
    /// at a commit that still holds the file.
    #[test]
    fn session_content_batch_reports_absent_paths_as_none() {
        let (tmp, repo, _base) = seeded();
        write(tmp.path(), "kept.txt", "first\n");
        write(tmp.path(), "dropped.txt", "doomed\n");
        let with_both = commit(tmp.path(), "two files");
        write(tmp.path(), "kept.txt", "second\n");
        std::fs::remove_file(tmp.path().join("dropped.txt")).unwrap();
        let head = commit(tmp.path(), "edit one, drop one");

        let content = session_content(&repo, &with_both, &head).unwrap();

        assert_eq!(content.paths.len(), 2);
        // Changed file: the batched read resolves it, exactly as `blob_at` does.
        assert_eq!(
            content.paths.get("kept.txt").cloned().flatten(),
            repo.blob_at(&head, "kept.txt")
        );
        // Deleted file: no blob at `head`, batched or not.
        assert_eq!(content.paths.get("dropped.txt"), Some(&None));
        // Against the commit that still holds the deleted file, the path named
        // as the mismatch is unchanged by batching: every path is read, but the
        // comparison still stops at the first mismatch in the same order.
        assert!(matches!(
            content_at(&repo, &content, &with_both).unwrap(),
            ContentVerdict::Absent { ref path, .. } if path == "dropped.txt"
        ));
    }

    /// The failure that invalidated the tip-comparison approach.
    ///
    /// A session lands, then an unrelated change rewrites one of the same files.
    /// The tip no longer holds the session's content, but the landing commit
    /// still does — and the landing commit is what the verdict must be computed
    /// against.
    #[test]
    fn work_stays_represented_after_a_later_commit_rewrites_the_same_file() {
        let (tmp, repo, base) = seeded();

        // The session, on its own branch.
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "store.rs", "session change\n");
        write(tmp.path(), "other.rs", "session other\n");
        let head = commit(tmp.path(), "session work");
        let content = session_content(&repo, &base, &head).unwrap();

        // The same content lands on main under a new SHA, as a squash would.
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "store.rs", "session change\n");
        write(tmp.path(), "other.rs", "session other\n");
        let landing = commit(tmp.path(), "squashed session work (#1)");

        // An unrelated change then rewrites one of those files.
        write(tmp.path(), "store.rs", "later unrelated rewrite\n");
        let tip = commit(tmp.path(), "unrelated change");

        // Tip comparison — the discarded approach — reports it unrepresented.
        assert!(matches!(
            content_at(&repo, &content, &tip).unwrap(),
            ContentVerdict::Absent { ref path, .. } if path == "store.rs"
        ));

        // The landing commit still carries it, and the walk finds that commit.
        assert!(content_at(&repo, &content, &landing).unwrap().is_present());
        let search = find_landing(&repo, &content, &tip, DEFAULT_SEARCH_CAP).unwrap();
        assert!(search.represented());
        assert_eq!(search.landing().unwrap().commit, landing);
    }

    /// A bounded health check stops the content search at its deadline, and a
    /// stopped search proves nothing: it must fail, never read as landed, even
    /// when the work did land (#460).
    #[test]
    fn a_landing_search_stopped_by_its_deadline_never_reads_as_landed() {
        let (tmp, _repo, _base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "f.rs", "work\n");
        let head = commit(tmp.path(), "session work");
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "f.rs", "work\n");
        let landing = commit(tmp.path(), "squashed (#7)");
        let repo = GitRepo::discover(tmp.path()).unwrap();

        let unbounded = work_landed_within(&repo, &head, &landing, None).unwrap();
        assert!(matches!(unbounded, LandingVerdict::Landed { .. }));

        let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let error = work_landed_within(&repo, &head, &landing, Some(expired)).unwrap_err();
        assert!(
            matches!(&error, BrokerOpError::RepresentationUnavailable { reason } if reason.contains("time budget")),
            "{error}"
        );
    }

    #[test]
    fn landing_verdict_cache_reuses_only_complete_exact_pair_and_profile() {
        let (_tmp, repo, head, landed_target, alternative_target) = cached_landing_fixture();
        let mut cache = LandingVerdictCache::default();
        let narrow = LandingSearchProfile {
            candidate_cap: 1,
            ..LandingSearchProfile::default()
        };
        let truncated =
            work_landed_with_cache(&repo, &head, &landed_target, None, narrow, &mut cache).unwrap();
        assert!(matches!(
            truncated,
            LandingVerdict::NotLanded {
                truncated: true,
                ..
            }
        ));
        assert!(
            cache.verdicts.is_empty(),
            "truncated negatives are not cached"
        );

        let repeated_truncated =
            work_landed_with_cache(&repo, &head, &landed_target, None, narrow, &mut cache).unwrap();
        assert!(matches!(
            repeated_truncated,
            LandingVerdict::NotLanded {
                truncated: true,
                ..
            }
        ));
        assert_eq!(
            cache.hits(),
            0,
            "the incomplete result must be searched again"
        );

        let profile = LandingSearchProfile {
            candidate_cap: 10,
            ..LandingSearchProfile::default()
        };
        let landed =
            work_landed_with_cache(&repo, &head, &landed_target, None, profile, &mut cache)
                .unwrap();
        assert!(matches!(
            &landed,
            LandingVerdict::Landed {
                evidence: LandingEvidence::Content,
                ..
            }
        ));
        assert_eq!(cache.hits(), 0);

        let other_profile = LandingSearchProfile {
            candidate_cap: 11,
            ..LandingSearchProfile::default()
        };
        let same_evidence_different_profile = work_landed_with_cache(
            &repo,
            &head,
            &landed_target,
            None,
            other_profile,
            &mut cache,
        )
        .unwrap();
        assert_eq!(same_evidence_different_profile, landed);
        assert_eq!(
            cache.hits(),
            0,
            "a different search profile must not reuse the verdict"
        );

        assert_eq!(
            work_landed_with_cache(&repo, &head, &landed_target, None, profile, &mut cache,)
                .unwrap(),
            landed
        );
        assert_eq!(cache.hits(), 1, "the identical immutable query is reused");

        let not_landed =
            work_landed_with_cache(&repo, &head, &alternative_target, None, profile, &mut cache)
                .unwrap();
        assert!(matches!(
            &not_landed,
            LandingVerdict::NotLanded {
                truncated: false,
                ..
            }
        ));
        assert_eq!(cache.hits(), 1, "a different target must miss");
        assert_eq!(
            work_landed_with_cache(&repo, &head, &alternative_target, None, profile, &mut cache,)
                .unwrap(),
            not_landed
        );
        assert_eq!(cache.hits(), 2, "complete negative verdicts are reusable");
    }

    #[test]
    fn landing_verdict_cache_does_not_retain_deadline_failures() {
        let (_tmp, repo, head, landed_target, _) = cached_landing_fixture();
        let mut cache = LandingVerdictCache::default();
        let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let error = work_landed_with_cache(
            &repo,
            &head,
            &landed_target,
            Some(expired),
            LandingSearchProfile::default(),
            &mut cache,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            BrokerOpError::RepresentationUnavailable { ref reason }
                if reason.contains("time budget")
        ));
        assert!(cache.verdicts.is_empty(), "failed searches are not cached");

        let retry = work_landed_with_cache(
            &repo,
            &head,
            &landed_target,
            None,
            LandingSearchProfile::default(),
            &mut cache,
        )
        .unwrap();
        assert!(
            retry.is_landed(),
            "a fresh complete retry finds the landing"
        );
        assert_eq!(cache.verdicts.len(), 1);
        assert_eq!(cache.hits(), 0);
    }

    #[test]
    fn ancestry_is_never_consulted_so_a_squash_under_a_new_sha_is_found() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "f.rs", "work\n");
        let head = commit(tmp.path(), "session work");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "f.rs", "work\n");
        let landing = commit(tmp.path(), "squashed (#7)");

        // No ancestry relationship exists in either direction.
        assert!(!repo.is_ancestor(&head, &landing));
        assert!(!repo.is_ancestor(&landing, &head));

        let search = find_landing(&repo, &content, &landing, DEFAULT_SEARCH_CAP).unwrap();
        assert_eq!(search.landing().unwrap().commit, landing);
    }

    #[test]
    fn the_earliest_carrying_commit_is_taken_not_a_later_one_that_inherits_it() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "f.rs", "work\n");
        let head = commit(tmp.path(), "session work");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "f.rs", "work\n");
        let landing = commit(tmp.path(), "squashed (#7)");
        write(tmp.path(), "unrelated.rs", "x\n");
        let later = commit(tmp.path(), "later, still carries f.rs");

        let search = find_landing(&repo, &content, &later, DEFAULT_SEARCH_CAP).unwrap();
        let found = search.landing().unwrap();
        assert_eq!(found.commit, landing, "the commit that carried the work");
        assert_ne!(found.commit, later, "not a commit that merely inherits it");
        assert_eq!(found.position, 0);
    }

    #[test]
    fn a_deletion_is_represented_by_absence_on_the_default_branch() {
        let (tmp, repo, base) = seeded();
        write(tmp.path(), "doomed.txt", "bye\n");
        let with_file = commit(tmp.path(), "add doomed");

        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        std::fs::remove_file(tmp.path().join("doomed.txt")).unwrap();
        let head = commit(tmp.path(), "delete doomed");
        let content = session_content(&repo, &with_file, &head).unwrap();
        assert_eq!(content.paths.get("doomed.txt"), Some(&None));

        // Present at the base, where the file still exists: not represented.
        assert!(
            !content_at(&repo, &content, &with_file)
                .unwrap()
                .is_present()
        );
        // Represented at the original base, which predates the file.
        assert!(content_at(&repo, &content, &base).unwrap().is_present());
    }

    #[test]
    fn a_session_that_changes_nothing_new_has_nothing_to_represent() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "base.txt", "changed\n");
        commit(tmp.path(), "change it");
        write(tmp.path(), "base.txt", "base\n");
        let head = commit(tmp.path(), "and change it back");

        let content = session_content(&repo, &base, &head).unwrap();
        let search = find_landing(&repo, &content, &base, DEFAULT_SEARCH_CAP).unwrap();

        assert_eq!(search.outcome, LandingOutcome::NothingToRepresent);
        assert!(search.represented());
    }

    #[test]
    fn genuinely_unrepresented_work_is_refused_and_names_what_is_missing() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "landed.rs", "landed\n");
        write(tmp.path(), "never.rs", "never landed\n");
        let head = commit(tmp.path(), "two files");
        let content = session_content(&repo, &base, &head).unwrap();

        // Only half the work reaches main.
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "landed.rs", "landed\n");
        let tip = commit(tmp.path(), "half of it (#9)");

        let search = find_landing(&repo, &content, &tip, DEFAULT_SEARCH_CAP).unwrap();
        assert!(!search.represented());
        let LandingOutcome::NotFound { closest } = search.outcome else {
            panic!("expected NotFound");
        };
        let closest = closest.expect("the half-matching commit is the closest");
        assert_eq!(closest.commit, tip);
        assert_eq!(closest.matched_paths, 1);
        assert_eq!(closest.missing_path, "never.rs");
    }

    /// Work split across two pull requests is not claimed as represented: no
    /// single commit carried it, so no single commit can be recorded.
    #[test]
    fn work_split_across_two_landings_is_not_claimed_by_either() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "a.rs", "a\n");
        write(tmp.path(), "b.rs", "b\n");
        let head = commit(tmp.path(), "both");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "a.rs", "a\n");
        commit(tmp.path(), "first half");
        write(tmp.path(), "b.rs", "changed differently\n");
        let tip = commit(tmp.path(), "second half, but not identical");

        assert!(
            !find_landing(&repo, &content, &tip, DEFAULT_SEARCH_CAP)
                .unwrap()
                .represented()
        );
    }

    #[test]
    fn the_walk_reports_truncation_rather_than_a_confident_negative() {
        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "never.rs", "never\n");
        let head = commit(tmp.path(), "unlanded");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "main"]);
        let mut tip = base.clone();
        for n in 0..4 {
            write(tmp.path(), "filler.txt", &format!("{n}\n"));
            tip = commit(tmp.path(), &format!("filler {n}"));
        }

        let capped = find_landing(&repo, &content, &tip, 2).unwrap();
        assert_eq!(capped.examined, 2);
        assert!(
            capped.truncated,
            "a capped walk must not read as conclusive"
        );

        let full = find_landing(&repo, &content, &tip, DEFAULT_SEARCH_CAP).unwrap();
        assert_eq!(full.examined, 4);
        assert!(!full.truncated);
    }

    #[test]
    fn query_budget_counts_the_base_and_all_candidate_paths() {
        assert_eq!(landing_query_count(0, 1), Some(1));
        assert_eq!(landing_query_count(4, 3), Some(15));
        assert_eq!(landing_query_count(usize::MAX, 2), None);

        let (tmp, repo, base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "session.rs", "session change\n");
        let head = commit(tmp.path(), "session change");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "target.rs", "target change\n");
        let target = commit(tmp.path(), "target change");
        let oldest_first = repo.commits_between_oldest(&base, &target).unwrap();
        assert_eq!(
            repo.commits_between_oldest_bounded(&base, &target, MAX_LANDING_CANDIDATE_LIST_BYTES,)
                .unwrap(),
            oldest_first
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let _git_deadline = crate::git::limit_git_until(deadline);
        let error = find_landing_with_limits(
            &repo,
            &content,
            &target,
            DEFAULT_SEARCH_CAP,
            deadline,
            1,
            MAX_LANDING_CANDIDATE_LIST_BYTES,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            BrokerOpError::RepresentationUnavailable { reason }
                if reason.contains("needs 2 candidate/path queries")
                    && reason.contains("limit is 1")
        ));
    }

    #[test]
    fn an_expired_landing_deadline_is_unavailable_not_a_negative() {
        let (tmp, repo, base) = seeded();
        write(tmp.path(), "session.rs", "session change\n");
        let head = commit(tmp.path(), "session change");
        let content = session_content(&repo, &base, &head).unwrap();
        let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();

        let error = find_landing_within(&repo, &content, &base, DEFAULT_SEARCH_CAP, Some(expired))
            .unwrap_err();

        assert!(matches!(
            error,
            BrokerOpError::RepresentationUnavailable { ref reason }
                if reason == "landing search stopped at the time budget before reading its base snapshot"
        ));
    }

    #[test]
    fn candidate_list_output_limit_is_unavailable_not_a_negative() {
        let (tmp, repo, base) = seeded();
        write(tmp.path(), "session.rs", "session change\n");
        let head = commit(tmp.path(), "session change");
        let content = session_content(&repo, &base, &head).unwrap();

        git(tmp.path(), &["checkout", "-q", "--detach", &base]);
        write(tmp.path(), "target.rs", "target change\n");
        let target = commit(tmp.path(), "target change");
        let deadline = Instant::now() + Duration::from_secs(5);
        let _git_deadline = crate::git::limit_git_until(deadline);

        let error = find_landing_with_limits(
            &repo,
            &content,
            &target,
            DEFAULT_SEARCH_CAP,
            deadline,
            crate::git::MAX_GIT_BATCH_QUERY_COUNT,
            1,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            BrokerOpError::RepresentationUnavailable { ref reason }
                if reason.contains("cannot list commits")
                    && reason.contains("output exceeded 1 bytes")
        ));
    }

    /// Ten lines, so an edit at one end is outside the three lines of diff
    /// context around an edit at the other: `git patch-id` hashes context,
    /// and #408's rebase changed the files elsewhere, not beside the edit.
    const STORE_BASE: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
    const STORE_SESSION: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nL10\n";
    const STORE_MAIN: &str = "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";

    /// The #408 shape: the session commit was rebased onto a newer main
    /// before merging, so its parent and every blob the base touched differ,
    /// and the copy reached main only through a merge's second parent.
    fn rebased_then_merged() -> (tempfile::TempDir, GitRepo, String, String, String) {
        let (tmp, repo, _) = seeded();
        write(tmp.path(), "store.rs", STORE_BASE);
        let base = commit(tmp.path(), "store");
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "store.rs", STORE_SESSION);
        let head = commit(tmp.path(), "feat: session work");

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "store.rs", STORE_MAIN);
        commit(tmp.path(), "main edits the same file elsewhere");
        git(tmp.path(), &["checkout", "-q", "-b", "pr"]);
        git(tmp.path(), &["cherry-pick", &head]);
        let copy = git(tmp.path(), &["rev-parse", "HEAD"]);
        git(tmp.path(), &["checkout", "-q", "main"]);
        git(
            tmp.path(),
            &["merge", "-q", "--no-ff", "pr", "-m", "Merge pull request"],
        );
        let tip = git(tmp.path(), &["rev-parse", "HEAD"]);
        (tmp, repo, base, head, format!("{copy} {tip}"))
    }

    #[test]
    fn patch_equivalence_candidate_requires_matching_tree_entries_before_landed() {
        const ROOT_ENV: &str = "AETHYME_REPRESENTATION_TRACE_ROOT";
        const HEAD_ENV: &str = "AETHYME_REPRESENTATION_TRACE_HEAD";
        const TIP_ENV: &str = "AETHYME_REPRESENTATION_TRACE_TIP";
        const TEST: &str =
            "representation::tests::patch_equivalence_candidate_requires_matching_tree_entries_before_landed";
        if let Some(root) = std::env::var_os(ROOT_ENV) {
            let repo = GitRepo::discover(Path::new(&root)).unwrap();
            let head = std::env::var(HEAD_ENV).unwrap();
            let tip = std::env::var(TIP_ENV).unwrap();
            assert!(matches!(
                work_landed(&repo, &head, &tip).unwrap(),
                LandingVerdict::NotLanded { truncated: false, .. }
            ));
            return;
        }

        let (_tmp, repo, base, head, refs) = rebased_then_merged();
        let (copy, tip) = refs.split_once(' ').unwrap();
        assert_ne!(
            repo.blob_at(copy, "store.rs"),
            repo.blob_at(&head, "store.rs"),
            "the rebase changed the blob, so content alone cannot find it"
        );

        let content = session_content(&repo, &base, &head).unwrap();
        let candidates = repo
            .commits_between_oldest(&base, tip)
            .expect("complete candidate list");
        assert_eq!(
            patch_landing(&repo, &content, tip, &candidates)
                .unwrap()
                .map(|(commit, _)| commit),
            Some(copy.to_string()),
            "patch identity should nominate the rebased copy"
        );
        assert!(matches!(
            find_landing(&repo, &content, tip, DEFAULT_SEARCH_CAP)
                .unwrap()
                .outcome,
            LandingOutcome::NotFound { .. }
        ));

        assert_eq!(
            work_landed(&repo, &head, tip).unwrap(),
            LandingVerdict::NotLanded {
                examined: candidates.len(),
                truncated: false,
            }
        );

        let trace_dir = tempfile::tempdir().unwrap();
        let trace = trace_dir.path().join("patch-landing-trace.json");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        let output = child
            .env(ROOT_ENV, _tmp.path())
            .env(HEAD_ENV, &head)
            .env(TIP_ENV, tip)
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let events = std::fs::read_to_string(trace).unwrap();
        let (mut logs, mut patch_ids) = (0, 0);
        for line in events.lines() {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["event"] != "start" {
                continue;
            }
            let Some(argv) = event["argv"].as_array() else {
                continue;
            };
            if argv.iter().any(|arg| arg == "log") {
                logs += 1;
            }
            if argv.iter().any(|arg| arg == "patch-id") {
                patch_ids += 1;
            }
        }
        assert_eq!(logs, 1, "patch_landing lists both sides in one git log");
        assert_eq!(
            patch_ids, 1,
            "work_landed's real patch_landing call shares one patch-id stream"
        );
    }

    #[test]
    fn squash_merged_work_is_found_by_content() {
        let (tmp, repo, _base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "f.rs", "one\n");
        commit(tmp.path(), "first");
        write(tmp.path(), "f.rs", "one\ntwo\n");
        let head = commit(tmp.path(), "second");

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "other.rs", "main moved\n");
        commit(tmp.path(), "unrelated");
        write(tmp.path(), "f.rs", "one\ntwo\n");
        let squash = commit(tmp.path(), "both, squashed (#3)");

        assert_eq!(
            work_landed(&repo, &head, &squash).unwrap(),
            LandingVerdict::Landed {
                evidence: LandingEvidence::Content,
                landed_by: Some(squash.clone()),
            }
        );
    }

    #[test]
    fn unlanded_work_is_not_landed() {
        let (tmp, repo, _base) = seeded();
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "f.rs", "mine\n");
        let head = commit(tmp.path(), "never delivered");
        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "g.rs", "other\n");
        let tip = commit(tmp.path(), "unrelated");

        assert!(matches!(
            work_landed(&repo, &head, &tip).unwrap(),
            LandingVerdict::NotLanded {
                truncated: false,
                ..
            }
        ));
    }

    /// One of two rebased commits merged is not the session's work landing.
    #[test]
    fn one_of_two_rebased_commits_landing_is_not_landed() {
        let (tmp, repo, _) = seeded();
        write(tmp.path(), "store.rs", STORE_BASE);
        commit(tmp.path(), "store");
        git(tmp.path(), &["checkout", "-q", "-b", "session"]);
        write(tmp.path(), "store.rs", STORE_SESSION);
        let first = commit(tmp.path(), "first");
        write(tmp.path(), "second.rs", "second\n");
        let head = commit(tmp.path(), "second");

        git(tmp.path(), &["checkout", "-q", "main"]);
        write(tmp.path(), "store.rs", STORE_MAIN);
        commit(tmp.path(), "main edits the same file elsewhere");
        git(tmp.path(), &["cherry-pick", &first]);
        let tip = git(tmp.path(), &["rev-parse", "HEAD"]);

        assert!(!work_landed(&repo, &head, &tip).unwrap().is_landed());
    }

    /// A merge in the session can carry a conflict resolution no patch id
    /// describes. Here both of the session's ordinary commits reached main,
    /// but the merge joining them changed `side.rs` by hand, and that change
    /// is on no branch.
    #[test]
    fn a_session_holding_a_merge_is_not_landed_by_patch_equivalence() {
        let (_tmp, repo, base, _head, refs) = rebased_then_merged();
        let (_copy, _tip) = refs.split_once(' ').unwrap();
        let root = repo.root().to_path_buf();
        git(&root, &["checkout", "-q", "-b", "side", &base]);
        write(&root, "side.rs", "side\n");
        let side = commit(&root, "side");
        git(&root, &["checkout", "-q", "main"]);
        git(&root, &["cherry-pick", &side]);
        let tip = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["checkout", "-q", "session"]);
        git(&root, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
        write(&root, "side.rs", "resolved by hand\n");
        let merged_head = commit(&root, "merge side");

        assert!(!work_landed(&repo, &merged_head, &tip).unwrap().is_landed());
    }

    #[test]
    fn the_digest_binds_the_session_the_head_and_the_representing_commit() {
        let d = plan_digest(1, "head", Some("landing"));
        assert_eq!(d.len(), 64);
        assert_eq!(d, plan_digest(1, "head", Some("landing")));
        assert_ne!(d, plan_digest(2, "head", Some("landing")));
        assert_ne!(d, plan_digest(1, "other", Some("landing")));
        assert_ne!(d, plan_digest(1, "head", Some("elsewhere")));
        assert_ne!(d, plan_digest(1, "head", None));
    }
}
