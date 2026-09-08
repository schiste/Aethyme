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
//! Deliberately not used as evidence:
//!
//! - **ancestry**, which a squash or rebase merge destroys by construction;
//! - **the branch tip**, for the reason above;
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Landing {
    pub commit: String,
    pub subject: String,
    /// Candidates examined before this one; 0 means the oldest candidate.
    pub position: usize,
    pub paths: usize,
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
    let changed =
        repo.changed_between(base, head)
            .map_err(|source| BrokerOpError::RepresentationUnavailable {
                reason: format!("cannot diff {} to {}: {source}", short(base), short(head)),
            })?;

    let mut paths = BTreeMap::new();
    for path in changed {
        paths.insert(path.clone(), repo.blob_at(head, &path));
    }

    Ok(SessionContent {
        base: base.to_string(),
        head: head.to_string(),
        paths,
    })
}

/// Whether `target` — a fixed historical commit — holds the session's content.
pub fn content_at(repo: &GitRepo, content: &SessionContent, target: &str) -> ContentVerdict {
    for (path, wanted) in &content.paths {
        let found = repo.blob_at(target, path);
        if &found != wanted {
            return ContentVerdict::Absent {
                path: path.clone(),
                wanted: wanted.clone(),
                found,
            };
        }
    }
    ContentVerdict::Present
}

/// How many of the session's paths `target` matches, and the first it does not.
fn match_depth(
    repo: &GitRepo,
    content: &SessionContent,
    target: &str,
) -> (usize, Option<String>) {
    let mut matched = 0usize;
    for (path, wanted) in &content.paths {
        if &repo.blob_at(target, path) == wanted {
            matched += 1;
        } else {
            return (matched, Some(path.clone()));
        }
    }
    (matched, None)
}

/// Find the earliest commit on `branch_tip` that carries the session's content.
///
/// The candidate set is exactly the commits the default branch gained since the
/// session's base. That bound is not a budget but a fact: work cannot have
/// landed before the session branched, so nothing older can be the landing
/// commit. It also means the walk is short in the normal case and terminates
/// without an arbitrary limit.
///
/// The *earliest* match is taken rather than the latest because that is the
/// commit that carried the work; every later commit merely inherits it.
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
    if content_at(repo, content, &content.base).is_present() {
        return Ok(LandingSearch {
            outcome: LandingOutcome::NothingToRepresent,
            examined: 0,
            truncated: false,
        });
    }

    let candidates = repo
        .commits_between_oldest(&content.base, branch_tip)
        .map_err(|source| BrokerOpError::RepresentationUnavailable {
            reason: format!(
                "cannot list commits {}..{}: {source}",
                short(&content.base),
                short(branch_tip)
            ),
        })?;

    let truncated = candidates.len() > cap;
    let mut best: Option<Closest> = None;
    let mut examined = 0usize;

    for candidate in candidates.iter().take(cap) {
        examined += 1;
        let (matched, missing) = match_depth(repo, content, candidate);
        let Some(missing_path) = missing else {
            return Ok(LandingSearch {
                outcome: LandingOutcome::Landed(Landing {
                    commit: candidate.clone(),
                    subject: subject(repo, candidate),
                    position: examined - 1,
                    paths: content.paths.len(),
                }),
                examined,
                truncated: false,
            });
        };
        if best.as_ref().is_none_or(|prev| matched > prev.matched_paths) {
            best = Some(Closest {
                commit: candidate.clone(),
                subject: subject(repo, candidate),
                matched_paths: matched,
                missing_path,
            });
        }
    }

    Ok(LandingSearch {
        outcome: LandingOutcome::NotFound {
            closest: best.filter(|closest| closest.matched_paths > 0),
        },
        examined,
        truncated,
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
            content_at(&repo, &content, &tip),
            ContentVerdict::Absent { ref path, .. } if path == "store.rs"
        ));

        // The landing commit still carries it, and the walk finds that commit.
        assert!(content_at(&repo, &content, &landing).is_present());
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
        assert!(!content_at(&repo, &content, &with_file).is_present());
        // Represented at the original base, which predates the file.
        assert!(content_at(&repo, &content, &base).is_present());
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
        assert!(capped.truncated, "a capped walk must not read as conclusive");

        let full = find_landing(&repo, &content, &tip, DEFAULT_SEARCH_CAP).unwrap();
        assert_eq!(full.examined, 4);
        assert!(!full.truncated);
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
