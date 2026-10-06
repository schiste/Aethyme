//! Overlap between a session's change and the repository's open pull requests.
//!
//! Lease overlap compares live sessions. Since `broker push` (#433) every
//! session publishes its branch and opens a pull request, so in-flight work
//! outlives the session that wrote it: an agent can finish, and its PR can sit
//! in review while a new session edits the same lines. Nothing compared a
//! session with those PRs.
//!
//! Merge conflicts are rare -- measured 2026-09-30, 1-2% of sessions in most
//! repositories, 20% in one -- so this must be a precise warning, never a
//! gate. It compares changed *line ranges*, not paths: two changes conflict in
//! Git only when their hunks overlap or nearly touch, and "same file" alone is
//! reported at the lowest severity.
//!
//! Cost is split by command. `broker push` is an explicit, network-using
//! command: it lists open PRs through `gh`, reads each PR's diff from the local
//! remote-tracking ref when that ref is current and from `gh pr diff` only when
//! it is not, and caches the result in broker `meta`. `status` and `start`
//! read only that cache and local refs; they never touch the network.

use std::collections::BTreeMap;
use std::path::Path;

use crate::session_push::{TrackedDefault, tracked_default};
use crate::{
    Broker, CoordinatedCommand, OperationEffect, OperationProvider, QueueWait, StatusAdvice,
    StatusAdviceSeverity,
};

/// Open PRs one `gh pr list` asks for. A repository with more open PRs than
/// this is checked against the most recent ones; the report says so.
pub(crate) const MAX_OPEN_PRS: usize = 30;
/// PR diffs one push may fetch through `gh pr diff` when no current local ref
/// exists. The rest are reported as unknown rather than slowing the push.
const MAX_GH_DIFFS_PER_PUSH: usize = 8;
/// A diff larger than this is summarized as "unknown" rather than parsed.
const MAX_DIFF_BYTES: usize = 4 << 20;
/// A diff touching more files than this is too broad to say anything precise.
const MAX_FILES_PER_DIFF: usize = 2_000;
/// A push reuses a PR listing younger than this instead of asking `gh` again.
const LIST_REUSE_MS: i64 = 10 * 60_000;
/// `status` ignores a listing older than this: the PRs it names may have
/// merged or moved, and a stale warning is worse than none.
const LIST_STALE_MS: i64 = 24 * 3_600_000;
/// Lines of separation below which two changes are reported as conflicting.
/// Git conflicts on overlapping or directly adjoining hunks; three lines is
/// the default diff context, so changes closer than that also read as one
/// edit to a reviewer.
const ADJACENT_LINES: u32 = 3;
/// A whole-file change (created, deleted, binary, renamed) as a line range.
const WHOLE_FILE: (u32, u32) = (0, u32::MAX);

const OPEN_PRS_KEY: &str = "pr_overlap.open_prs";

fn ranges_key(number: i64) -> String {
    format!("pr_overlap.ranges.{number}")
}

/// Changed old-side line ranges per path, inclusive, sorted and merged.
pub(crate) type FileRanges = BTreeMap<String, Vec<(u32, u32)>>;

/// One open PR this session's change touches.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrOverlap {
    pub pr: i64,
    pub url: String,
    /// Paths both changes touch.
    pub files: Vec<String>,
    /// True when, in at least one of those paths, the changed lines overlap or
    /// lie within three lines of each other -- the shape Git conflicts on.
    pub conflicting_hunks: bool,
}

/// The result of comparing one session with the open PRs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrOverlapCheck {
    pub overlaps: Vec<PrOverlap>,
    /// Open PRs whose change could not be read (no current local ref and no
    /// `gh` diff within budget, or a diff too large to parse).
    pub unknown_prs: Vec<i64>,
    /// When the PR listing used was taken, Unix epoch milliseconds.
    pub listed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct OpenPr {
    number: i64,
    url: String,
    head: String,
    head_oid: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OpenPrListing {
    fetched_at_ms: i64,
    base: String,
    prs: Vec<OpenPr>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedRanges {
    head_oid: String,
    ranges: FileRanges,
}

/// A reporting-only push result. Binding every input lets routine status
/// read it without recomputing ranges, and never present an obsolete warning.
#[derive(serde::Serialize, serde::Deserialize)]
struct RecordedPrOverlap {
    head: String,
    baseline: String,
    baseline_ref: String,
    listing: String,
    check: PrOverlapCheck,
}

fn checked_key(session: i64) -> String {
    format!("pr_overlap.checked.{session}")
}

/// Parse `gh pr list --json number,url,state,headRefName,headRefOid`.
/// Anything that is not an open PR is dropped; unparseable output is empty.
fn parse_open_prs(stdout: &str) -> Vec<OpenPr> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pr| {
            pr["state"]
                .as_str()
                .is_none_or(|state| state.eq_ignore_ascii_case("open"))
        })
        .filter_map(|pr| {
            Some(OpenPr {
                number: pr["number"].as_i64()?,
                url: pr["url"].as_str().unwrap_or_default().to_string(),
                head: pr["headRefName"].as_str()?.to_string(),
                head_oid: pr["headRefOid"]
                    .as_str()
                    .filter(|oid| !oid.is_empty())
                    .map(str::to_string),
            })
        })
        .take(MAX_OPEN_PRS)
        .collect()
}

/// Changed old-side line ranges from a unified diff of any context size.
///
/// Returns `None` when the diff is too large to be precise about. A created,
/// deleted, binary or mode-only file change is recorded as the whole file, so
/// two sessions creating the same path read as conflicting (add/add).
pub(crate) fn parse_unified_diff(text: &str) -> Option<FileRanges> {
    if text.len() > MAX_DIFF_BYTES {
        return None;
    }
    let mut ranges = FileRanges::new();
    let mut old_path: Option<String> = None;
    let mut path: Option<String> = None;
    let mut file_has_hunk = false;
    let mut next_old: u32 = 0;
    // Lines of the current hunk still to read on each side. A hunk ends when
    // both reach zero, so a removed line whose text starts with `-- ` is never
    // mistaken for a file header.
    let mut old_left: u32 = 0;
    let mut new_left: u32 = 0;
    // True right after removed lines: an insertion there replaces them rather
    // than adding a change of its own.
    let mut replacing = false;

    fn mark(ranges: &mut FileRanges, path: &Option<String>, span: (u32, u32)) {
        if let Some(path) = path {
            ranges.entry(path.clone()).or_default().push(span);
        }
    }

    for line in text.lines() {
        if old_left > 0 || new_left > 0 {
            match line.as_bytes().first() {
                Some(b' ') => {
                    replacing = false;
                    next_old = next_old.saturating_add(1);
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'-') => {
                    mark(&mut ranges, &path, (next_old, next_old));
                    next_old = next_old.saturating_add(1);
                    old_left = old_left.saturating_sub(1);
                    replacing = true;
                }
                // An insertion sits between two old lines.
                Some(b'+') => {
                    if !replacing {
                        mark(&mut ranges, &path, (next_old.saturating_sub(1), next_old));
                    }
                    new_left = new_left.saturating_sub(1);
                }
                _ => {}
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if !file_has_hunk {
                mark(&mut ranges, &path, WHOLE_FILE);
            }
            // `a/<path> b/<path>`; refined by the `+++` header when present.
            path = rest
                .rsplit_once(" b/")
                .map(|(_, new)| new.to_string())
                .or_else(|| rest.strip_prefix("a/").map(str::to_string));
            old_path = None;
            file_has_hunk = false;
        } else if let Some(old) = line.strip_prefix("--- ") {
            old_path = old.strip_prefix("a/").map(str::to_string);
        } else if let Some(new) = line.strip_prefix("+++ ") {
            match new.strip_prefix("b/") {
                Some(new) => path = Some(new.to_string()),
                // Deleted file: the old path is the one both sides share.
                None => path = old_path.clone().or(path.take()),
            }
            // A created or deleted file is a whole-file change even though the
            // diff carries a hunk for its contents: two sessions creating the
            // same path conflict (add/add).
            if old_path.is_none() || new == "/dev/null" {
                mark(&mut ranges, &path, WHOLE_FILE);
            }
        } else if let Some(header) = line.strip_prefix("@@ ") {
            let mut sides = header.split_whitespace();
            let (Some(old), Some(new)) = (
                sides.next().and_then(|side| side.strip_prefix('-')),
                sides.next().and_then(|side| side.strip_prefix('+')),
            ) else {
                continue;
            };
            let count = |side: &str| -> Option<(u32, u32)> {
                Some(match side.split_once(',') {
                    Some((start, count)) => (start.parse().ok()?, count.parse().ok()?),
                    None => (side.parse().ok()?, 1),
                })
            };
            let (start, old_count) = count(old)?;
            let (_, new_count) = count(new)?;
            // `-a,0` means the change sits after line `a`.
            next_old = if old_count == 0 { start + 1 } else { start };
            old_left = old_count;
            new_left = new_count;
            replacing = false;
            file_has_hunk = true;
        }
    }
    if !file_has_hunk {
        mark(&mut ranges, &path, WHOLE_FILE);
    }
    if ranges.len() > MAX_FILES_PER_DIFF {
        return None;
    }
    for spans in ranges.values_mut() {
        spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for &(start, end) in spans.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged.dedup();
        *spans = merged;
    }
    Some(ranges)
}

/// Whether any span of `left` lies within [`ADJACENT_LINES`] of one in `right`.
pub(crate) fn spans_conflict(left: &[(u32, u32)], right: &[(u32, u32)]) -> bool {
    left.iter().any(|&(a_start, a_end)| {
        right.iter().any(|&(b_start, b_end)| {
            a_start <= b_end.saturating_add(ADJACENT_LINES)
                && b_start <= a_end.saturating_add(ADJACENT_LINES)
        })
    })
}

/// Compare one session's ranges with one PR's.
fn compare(session: &FileRanges, pr: &FileRanges) -> Option<(Vec<String>, bool)> {
    let mut files = Vec::new();
    let mut conflicting = false;
    for (path, spans) in session {
        if let Some(other) = pr.get(path) {
            files.push(path.clone());
            conflicting |= spans_conflict(spans, other);
        }
    }
    (!files.is_empty()).then_some((files, conflicting))
}

fn wait() -> QueueWait {
    QueueWait::Seconds(60)
}

impl Broker {
    /// The committed change of `branch_ref` against the default branch.
    fn ranges_for_head(&self, default: &TrackedDefault, head: &str) -> Option<FileRanges> {
        let repo = self.repo_handle();
        let base = repo.merge_base(&default.tracking_ref, head).ok()?;
        if base == head {
            return Some(FileRanges::new());
        }
        parse_unified_diff(&repo.unified_diff_zero(&base, head).ok()?)
    }

    fn cached_listing(&self) -> Option<OpenPrListing> {
        let text = self.store_ref().meta_get(OPEN_PRS_KEY).ok()??;
        serde_json::from_str(&text).ok()
    }

    /// Open PR numbers by head branch, from the cached listing only.
    pub(crate) fn cached_open_prs_by_head(&self) -> BTreeMap<String, i64> {
        self.cached_listing()
            .map(|listing| {
                listing
                    .prs
                    .into_iter()
                    .map(|pr| (pr.head, pr.number))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cached_ranges(&self, pr: &OpenPr) -> Option<FileRanges> {
        let head_oid = pr.head_oid.as_deref()?;
        let text = self.store_ref().meta_get(&ranges_key(pr.number)).ok()??;
        let cached: CachedRanges = serde_json::from_str(&text).ok()?;
        (cached.head_oid == head_oid).then_some(cached.ranges)
    }

    /// A PR's ranges from its local remote-tracking ref, when that ref is at
    /// the head GitHub reported (or GitHub reported none).
    fn local_pr_ranges(&self, default: &TrackedDefault, pr: &OpenPr) -> Option<FileRanges> {
        let local = self
            .repo_handle()
            .resolve_ref(&format!("refs/remotes/{}/{}", default.remote, pr.head))?;
        if pr.head_oid.as_deref().is_some_and(|oid| oid != local) {
            return None;
        }
        self.ranges_for_head(default, &local)
    }

    fn run_github_read(
        &mut self,
        session_id: i64,
        repository: &str,
        scope: String,
        args: Vec<String>,
        cwd: &Path,
    ) -> Option<String> {
        let report = self
            .run_coordinated_operation_at_with_wait(
                CoordinatedCommand {
                    session_id,
                    provider: OperationProvider::Github,
                    repository: Some(repository.to_string()),
                    resolved_target: None,
                    scope: Some(scope),
                    declared_effect: Some(OperationEffect::Read),
                    destructive_confirmed: false,
                    cross_session: None,
                    authorization_reason: None,
                    args,
                },
                cwd,
                wait(),
            )
            .ok()?;
        report.ok().then_some(report.stdout)
    }

    /// Refresh the open-PR listing and the PR ranges a push needs, then
    /// compare. Called by `broker push` after it published; every failure
    /// degrades to "unknown" and never fails the push.
    pub(crate) fn check_pr_overlaps_for_push(
        &mut self,
        session_id: i64,
        branch: &str,
        head: &str,
        repository: &str,
        cwd: &Path,
        now_ms: i64,
    ) -> PrOverlapCheck {
        let Some(default) = tracked_default(self.repo_handle()) else {
            return PrOverlapCheck::default();
        };
        let listing = match self.cached_listing() {
            Some(listing)
                if listing.base == default.branch
                    && now_ms.saturating_sub(listing.fetched_at_ms) < LIST_REUSE_MS =>
            {
                Some(listing)
            }
            _ => self
                .run_github_read(
                    session_id,
                    repository,
                    format!("pr-overlap:pr-list:{}", default.branch),
                    vec![
                        "pr".into(),
                        "list".into(),
                        "--state".into(),
                        "open".into(),
                        "--base".into(),
                        default.branch.clone(),
                        "--limit".into(),
                        MAX_OPEN_PRS.to_string(),
                        "--json".into(),
                        "number,url,state,headRefName,headRefOid".into(),
                    ],
                    cwd,
                )
                .map(|stdout| OpenPrListing {
                    fetched_at_ms: now_ms,
                    base: default.branch.clone(),
                    prs: parse_open_prs(&stdout),
                }),
        };
        let Some(listing) = listing else {
            return PrOverlapCheck::default();
        };
        if let Ok(text) = serde_json::to_string(&listing) {
            // The cache only saves the next command a `gh` call; the check
            // this push reports is already computed.
            crate::warn_unrecorded(
                "cache the open pull request listing",
                self.store_ref().meta_set(OPEN_PRS_KEY, &text),
            );
        }
        let Some(session_ranges) = self.ranges_for_head(&default, head) else {
            return PrOverlapCheck {
                unknown_prs: listing.prs.iter().map(|pr| pr.number).collect(),
                listed_at_ms: Some(listing.fetched_at_ms),
                ..Default::default()
            };
        };
        let mut gh_budget = MAX_GH_DIFFS_PER_PUSH;
        let mut check = PrOverlapCheck {
            listed_at_ms: Some(listing.fetched_at_ms),
            ..Default::default()
        };
        for pr in listing.prs.iter().filter(|pr| pr.head != branch) {
            let ranges = match self.cached_ranges(pr) {
                Some(ranges) => Some(ranges),
                None => {
                    let mut fresh = self.local_pr_ranges(&default, pr);
                    if fresh.is_none() && gh_budget > 0 {
                        gh_budget -= 1;
                        fresh = self
                            .run_github_read(
                                session_id,
                                repository,
                                format!("pr-overlap:pr-diff:{}", pr.number),
                                vec!["pr".into(), "diff".into(), pr.number.to_string()],
                                cwd,
                            )
                            .and_then(|diff| parse_unified_diff(&diff));
                    }
                    if let (Some(ranges), Some(head_oid)) = (&fresh, &pr.head_oid)
                        && let Ok(text) = serde_json::to_string(&CachedRanges {
                            head_oid: head_oid.clone(),
                            ranges: ranges.clone(),
                        })
                    {
                        crate::warn_unrecorded(
                            "cache an open pull request's changed lines",
                            self.store_ref().meta_set(&ranges_key(pr.number), &text),
                        );
                    }
                    fresh
                }
            };
            match ranges {
                Some(ranges) => {
                    if let Some((files, conflicting_hunks)) = compare(&session_ranges, &ranges) {
                        check.overlaps.push(PrOverlap {
                            pr: pr.number,
                            url: pr.url.clone(),
                            files,
                            conflicting_hunks,
                        });
                    }
                }
                None => check.unknown_prs.push(pr.number),
            }
        }
        if let Some(listing) = self.store_ref().meta_get(OPEN_PRS_KEY).ok().flatten() {
            let mut cached_check = check.clone();
            for overlap in &mut cached_check.overlaps {
                overlap.files.truncate(5);
            }
            let recorded = RecordedPrOverlap {
                head: head.into(),
                baseline: default.commit.clone(),
                baseline_ref: default.tracking_ref.clone(),
                listing,
                check: cached_check,
            };
            if let Ok(raw) = serde_json::to_string(&recorded) {
                crate::warn_unrecorded(
                    "record PR overlap check",
                    self.store_ref().meta_set(&checked_key(session_id), &raw),
                );
            }
        }
        check
    }

    /// Compare a session with the open PRs using only cached listings, cached
    /// PR ranges and local refs. No network: `status` and `start` call this.
    pub(crate) fn cached_pr_overlaps(
        &self,
        session: &crate::Session,
        now_ms: i64,
    ) -> Option<PrOverlapCheck> {
        let default = tracked_default(self.repo_handle())?;
        let listing = self.cached_listing()?;
        if listing.base != default.branch
            || now_ms.saturating_sub(listing.fetched_at_ms) > LIST_STALE_MS
        {
            return None;
        }
        let head = self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{}", session.branch))?;
        let session_ranges = self.ranges_for_head(&default, &head)?;
        if session_ranges.is_empty() {
            return None;
        }
        let mut check = PrOverlapCheck {
            listed_at_ms: Some(listing.fetched_at_ms),
            ..Default::default()
        };
        for pr in listing.prs.iter().filter(|pr| pr.head != session.branch) {
            let ranges = self
                .cached_ranges(pr)
                .or_else(|| self.local_pr_ranges(&default, pr));
            match ranges {
                Some(ranges) => {
                    if let Some((files, conflicting_hunks)) = compare(&session_ranges, &ranges) {
                        check.overlaps.push(PrOverlap {
                            pr: pr.number,
                            url: pr.url.clone(),
                            files,
                            conflicting_hunks,
                        });
                    }
                }
                None => check.unknown_prs.push(pr.number),
            }
        }
        Some(check)
    }

    /// One `session.pr-overlap` advice row per live session whose change
    /// touches an open PR, from cached data only.
    pub(crate) fn recorded_pr_overlap_advice(
        &self,
        now_ms: i64,
        baseline: &str,
        baseline_ref: &str,
    ) -> Vec<StatusAdvice> {
        let Some(listing_raw) = self.store_ref().meta_get(OPEN_PRS_KEY).ok().flatten() else {
            return Vec::new();
        };
        let Ok(listing) = serde_json::from_str::<OpenPrListing>(&listing_raw) else {
            return Vec::new();
        };
        if now_ms.saturating_sub(listing.fetched_at_ms) > LIST_STALE_MS {
            return Vec::new();
        }
        // A repository without origin/HEAD labels its publication baseline
        // HEAD; use its configured upstream rather than treating that alias
        // as a different default branch or guessing a branch name.
        let actual = if baseline_ref == "HEAD" {
            tracked_default(self.repo_handle())
        } else {
            None
        };
        let baseline = actual
            .as_ref()
            .map(|d| d.commit.as_str())
            .unwrap_or(baseline);
        let baseline_ref = actual
            .as_ref()
            .map(|d| d.tracking_ref.as_str())
            .unwrap_or(baseline_ref);
        let Some(tips) = self.repo_handle().local_branch_tips() else {
            return Vec::new();
        };
        let Ok(sessions) = self.store_ref().live_sessions() else {
            return Vec::new();
        };
        sessions
            .into_iter()
            .filter_map(|session| {
                let raw = self
                    .store_ref()
                    .meta_get(&checked_key(session.id))
                    .ok()
                    .flatten()?;
                let recorded: RecordedPrOverlap = serde_json::from_str(&raw).ok()?;
                if recorded.baseline != baseline
                    || recorded.baseline_ref != baseline_ref
                    || recorded.listing != listing_raw
                    || tips.get(&format!("refs/heads/{}", session.branch)) != Some(&recorded.head)
                {
                    return None;
                }
                let mut row = overlap_advice(session.id, &recorded.check)?;
                row.evidence.push(format!(
                    "recorded at push; PR listing timestamp: {}",
                    listing.fetched_at_ms
                ));
                Some(row)
            })
            .collect()
    }

    pub(crate) fn pr_overlap_advice(&self, now_ms: i64) -> Vec<StatusAdvice> {
        let Ok(sessions) = self.store_ref().live_sessions() else {
            return Vec::new();
        };
        let mut advice = Vec::new();
        for session in sessions {
            let Some(check) = self.cached_pr_overlaps(&session, now_ms) else {
                continue;
            };
            if let Some(row) = overlap_advice(session.id, &check) {
                advice.push(row);
            }
        }
        advice
    }
}

impl Broker {
    /// The one-line heads-up `start` and `adopt` print when a session already
    /// has committed work that overlaps an open PR. A fresh session has no
    /// change yet, so this is silent for it.
    pub(crate) fn pr_overlap_heads_up(&self, session: &crate::Session) -> Option<String> {
        heads_up(&self.cached_pr_overlaps(session, crate::clock::epoch_ms())?)
    }
}

/// The one-line heads-up text, when there is anything to say.
fn heads_up(check: &PrOverlapCheck) -> Option<String> {
    let first = check
        .overlaps
        .iter()
        .find(|overlap| overlap.conflicting_hunks)
        .or_else(|| check.overlaps.first())?;
    Some(format!(
        "Heads-up: open PR #{} {} {} ({}){}",
        first.pr,
        if first.conflicting_hunks {
            "changes the same lines in"
        } else {
            "touches"
        },
        first.files.first().map(String::as_str).unwrap_or_default(),
        first.url,
        match check.overlaps.len() {
            1 => String::new(),
            more => format!(
                "; {} more open PR(s) overlap -- see broker status",
                more - 1
            ),
        }
    ))
}

fn overlap_advice(session_id: i64, check: &PrOverlapCheck) -> Option<StatusAdvice> {
    if check.overlaps.is_empty() {
        return None;
    }
    let conflicting = check
        .overlaps
        .iter()
        .filter(|overlap| overlap.conflicting_hunks)
        .collect::<Vec<_>>();
    let severity = if conflicting.is_empty() {
        StatusAdviceSeverity::Info
    } else {
        StatusAdviceSeverity::Warning
    };
    let named = |overlaps: &[&PrOverlap]| {
        overlaps
            .iter()
            .take(3)
            .map(|overlap| format!("#{}", overlap.pr))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let all = check.overlaps.iter().collect::<Vec<_>>();
    let summary = if conflicting.is_empty() {
        format!(
            "session {session_id} touches files that open PR(s) {} also change, on different lines",
            named(&all)
        )
    } else {
        format!(
            "session {session_id} changes the same lines as open PR(s) {}; expect a conflict \
             when one of them merges",
            named(&conflicting)
        )
    };
    let commands = check
        .overlaps
        .iter()
        .take(3)
        .map(|overlap| {
            if overlap.conflicting_hunks {
                format!(
                    "rebase after #{} merges, or coordinate with #{}",
                    overlap.pr, overlap.pr
                )
            } else {
                format!("coordinate with #{} if the changes interact", overlap.pr)
            }
        })
        .collect();
    Some(StatusAdvice {
        id: "session.pr-overlap",
        severity,
        reason: "the session's change overlaps an open pull request",
        summary,
        session_id: Some(session_id),
        queue_entry_id: None,
        evidence: check
            .overlaps
            .iter()
            .take(3)
            .map(|overlap| {
                format!(
                    "#{} {}: {}",
                    overlap.pr,
                    overlap.url,
                    overlap
                        .files
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect(),
        commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,0 +11,2 @@ fn f() {
+one
+two
@@ -40,2 +42 @@ fn g() {
-x
-y
+z
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/image.png b/image.png
index 5555555..6666666 100644
Binary files a/image.png and b/image.png differ
";

    #[test]
    fn a_unified_diff_becomes_old_side_line_ranges() {
        let ranges = parse_unified_diff(DIFF).unwrap();
        assert_eq!(ranges["src/a.rs"], vec![(10, 11), (40, 41)]);
        assert_eq!(ranges["new.txt"], vec![WHOLE_FILE]);
        assert_eq!(ranges["gone.txt"], vec![WHOLE_FILE]);
        assert_eq!(ranges["image.png"], vec![WHOLE_FILE]);
    }

    #[test]
    fn context_lines_do_not_count_as_changes() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -5,7 +5,7 @@
 a
 b
 c
-d
+D
 e
 f
 g
";
        assert_eq!(parse_unified_diff(diff).unwrap()["f"], vec![(8, 8)]);
    }

    #[test]
    fn nearby_hunks_conflict_and_distant_ones_do_not() {
        assert!(spans_conflict(&[(10, 12)], &[(12, 14)]));
        assert!(spans_conflict(&[(10, 12)], &[(15, 16)]));
        assert!(!spans_conflict(&[(10, 12)], &[(16, 20)]));
        assert!(spans_conflict(&[WHOLE_FILE], &[(500, 501)]));
    }

    #[test]
    fn a_listing_keeps_open_prs_only() {
        let listing = r#"[
            {"number":1,"url":"u1","state":"OPEN","headRefName":"agent/a","headRefOid":"aaa"},
            {"number":2,"url":"u2","state":"MERGED","headRefName":"agent/b","headRefOid":"bbb"},
            {"number":3,"url":"u3","state":"CLOSED","headRefName":"agent/c"}
        ]"#;
        let prs = parse_open_prs(listing);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 1);
        assert_eq!(prs[0].head_oid.as_deref(), Some("aaa"));
        assert!(parse_open_prs("not json").is_empty());
    }

    #[test]
    fn severity_follows_the_strongest_overlap() {
        let disjoint = PrOverlapCheck {
            overlaps: vec![PrOverlap {
                pr: 4,
                url: "u".into(),
                files: vec!["f".into()],
                conflicting_hunks: false,
            }],
            ..Default::default()
        };
        assert_eq!(
            overlap_advice(1, &disjoint).unwrap().severity,
            StatusAdviceSeverity::Info
        );
        let mut conflicting = disjoint.clone();
        conflicting.overlaps[0].conflicting_hunks = true;
        let row = overlap_advice(1, &conflicting).unwrap();
        assert_eq!(row.severity, StatusAdviceSeverity::Warning);
        assert!(
            row.commands[0].contains("rebase after #4"),
            "{:?}",
            row.commands
        );
        assert!(overlap_advice(1, &PrOverlapCheck::default()).is_none());
    }
}
