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
//!   on the branch, and one branch commit has all of them in its history (a
//!   rebase before merge, which changes every blob the base touched, #408).
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

use sha2::{Digest, Sha256};

use crate::{BrokerOpError, GitRepo};

pub const REPRESENTATION_PLAN_SCHEMA_VERSION: u32 = 1;

/// A cap on the history walk, so a session whose work never landed cannot make
/// `finish` scan an entire branch. Candidates are already bounded by the
/// session's base (see [`find_landing`]); this only guards a pathological base.
/// A search that hits the cap reports itself truncated and skips the
/// patch-equivalence tier, whose cost grows with the same history.
pub const DEFAULT_SEARCH_CAP: usize = 2_000;

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
    /// The first path that does not match, with both sides named so a refusal
    /// can say what is missing rather than only that something is.
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
    /// Every session commit's patch is on the branch under another SHA
    /// (rebase or cherry-pick delivery).
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
    /// equivalent patch.
    pub commit: String,
    pub subject: String,
    /// Candidates examined before this one; 0 means the oldest candidate.
    pub position: usize,
    pub paths: usize,
    /// Content, or patch equivalence when no single commit holds the content.
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
    let changed = repo.changed_between(base, head).map_err(|source| {
        BrokerOpError::RepresentationUnavailable {
            reason: format!("cannot diff {} to {}: {source}", short(base), short(head)),
        }
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
    Ok(ContentVerdict::Present)
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
/// merely inherits it. When no commit holds the content -- the work was rebased
/// onto a newer base before it merged, so the base's files differ -- patch
/// equivalence is tried (see [`patch_landing`]).
pub fn find_landing(
    repo: &GitRepo,
    content: &SessionContent,
    branch_tip: &str,
    cap: usize,
) -> Result<LandingSearch, BrokerOpError> {
    if content.is_empty() {
        return Ok(LandingSearch {
            outcome: LandingOutcome::NothingToRepresent,
            examined: 0,
            truncated: false,
        });
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

    let unavailable = |what: String| BrokerOpError::RepresentationUnavailable { reason: what };
    let candidates = repo
        .commits_between_oldest(&content.base, branch_tip)
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
    let mut best: Option<(String, usize, String)> = None;

    for (batch_index, batch) in window.chunks(CANDIDATE_BATCH).enumerate() {
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
            if matched == paths.len() {
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
/// session commit, if there is one.
///
/// This is the rebase-then-merge case (#408): the same patches applied to a
/// newer base produce different blobs wherever the base differs, so no commit
/// holds the session's exact content, yet `git patch-id --stable` agrees for
/// every commit. Three conditions keep the verdict sound rather than likely:
///
/// - **every** session commit after the base must be accounted for, either by
///   an equivalent on the branch or by being on the branch itself -- one
///   landed commit out of two is not "landed";
/// - a merge commit in the session has no patch id, and can carry a conflict
///   resolution no patch describes, so it counts only if that merge itself is
///   on the branch;
/// - a single branch commit must have all of them in its history, so the
///   verdict names one fixed commit that carried the work, as a recorded
///   representation requires.
///
/// `--cherry-mark` compares the session side against the branch commits since
/// the two diverged, including those reached through a merge's second parent.
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
    let unmerged = repo
        .cherry_marked(branch_tip, &content.head, crate::git::CherrySide::Right)
        .map_err(unavailable)?
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
    let mut carriers = repo
        .cherry_marked(branch_tip, &content.head, crate::git::CherrySide::Left)
        .map_err(unavailable)?
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
    if repo.is_ancestor(head, target) {
        return Ok(LandingVerdict::Landed {
            evidence: LandingEvidence::Ancestry,
            landed_by: None,
        });
    }
    let base = landing_base(repo, head, target)?;
    let content = session_content(repo, &base, head)?;
    let search = find_landing(repo, &content, target, DEFAULT_SEARCH_CAP)?;
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
        let output = Command::new("git")
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
    fn rebased_then_merged_work_is_found_by_patch_equivalence() {
        let (_tmp, repo, base, head, refs) = rebased_then_merged();
        let (copy, tip) = refs.split_once(' ').unwrap();
        assert_ne!(
            repo.blob_at(copy, "store.rs"),
            repo.blob_at(&head, "store.rs"),
            "the rebase changed the blob, so content alone cannot find it"
        );

        let content = session_content(&repo, &base, &head).unwrap();
        let search = find_landing(&repo, &content, tip, DEFAULT_SEARCH_CAP).unwrap();
        let landing = search.landing().expect("rebased work landed");
        assert_eq!(landing.evidence, LandingEvidence::PatchEquivalent);
        assert_eq!(landing.commit, copy, "the copy carried it, not the merge");

        assert_eq!(
            work_landed(&repo, &head, tip).unwrap(),
            LandingVerdict::Landed {
                evidence: LandingEvidence::PatchEquivalent,
                landed_by: Some(copy.to_string()),
            }
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
