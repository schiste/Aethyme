//! "Land the shared change first" advice for two sessions on one target.
//!
//! When two live sessions change the same lines, or declare conflicting
//! intents on the same symbol, one of them will be reworked against whatever
//! the other lands. Stacking the second on the first's unmerged branch avoids
//! the conflict but chains the two together: a review or CI problem in the
//! first then holds the second, and a rejected first strands it. In a
//! repository that delivers through pull requests there is a cheaper move:
//! land only the shared change as a small PR of its own, and let both
//! sessions continue from the default branch.
//!
//! This module decides when that advice applies and renders it with the
//! sessions, paths and commands filled in. It never changes a branch; the
//! rows are part of `broker status` like every other piece of advice.
//!
//! Only committed work is compared. An overlap in uncommitted files is still
//! reported as a lease overlap, but nothing can be landed from it yet.

use std::collections::BTreeMap;

use crate::git::GitRepo;
use crate::{
    AgentView, Overlap, ScopeConflictSeverity, ScopeOperation, ScopeOverlap, StatusAdvice,
    StatusAdviceSeverity,
};

pub(crate) const ADVICE_ID: &str = "coordination.land-shared-edit-first";

/// One changed region of the base file, in doubled line coordinates: line
/// `n` is `2n`, and the gap after line `n` (where a pure insertion lands) is
/// `2n + 1`. Doubling lets modifications and insertions share one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    start: u64,
    end: u64,
}

/// Parse the old-side ranges of a zero-context (`-U0`) unified diff.
///
/// Only well-formed `@@ -start[,count] +… @@` headers count. A decorated or
/// summarized diff yields no regions, and therefore no advice, rather than
/// advice built on text that is not a patch.
pub(crate) fn parse_regions(diff: &str) -> Vec<Region> {
    diff.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("@@ -")?;
            let (old, rest) = rest.split_once(' ')?;
            if !rest.starts_with('+') || !rest.contains(" @@") {
                return None;
            }
            let (start, count) = match old.split_once(',') {
                Some((start, count)) => (start.parse::<u64>().ok()?, count.parse::<u64>().ok()?),
                None => (old.parse::<u64>().ok()?, 1),
            };
            Some(if count == 0 {
                // A pure insertion after line `start`.
                Region {
                    start: 2 * start + 1,
                    end: 2 * start + 1,
                }
            } else {
                Region {
                    start: 2 * start,
                    end: 2 * (start + count - 1),
                }
            })
        })
        .collect()
}

/// Whether two changed regions would collide when merged.
///
/// Mirrors Git's three-way rule: changes to the same or directly adjacent
/// lines conflict, while changes separated by at least one untouched line of
/// the base merge cleanly. Untouched base lines sit on even coordinates, so
/// the regions are separate exactly when an even coordinate lies strictly
/// between them.
fn regions_collide(a: Region, b: Region) -> bool {
    let (first, second) = if a.start <= b.start { (a, b) } else { (b, a) };
    if second.start <= first.end {
        return true;
    }
    let next_even = if (first.end + 1) % 2 == 0 {
        first.end + 1
    } else {
        first.end + 2
    };
    next_even >= second.start
}

/// Whether any region of one side collides with any region of the other.
pub(crate) fn hunks_collide(a: &[Region], b: &[Region]) -> bool {
    a.iter()
        .any(|left| b.iter().any(|right| regions_collide(*left, *right)))
}

/// Evidence for one side of a colliding pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SideEvidence {
    /// Commit time (seconds) of the oldest commit that touched a shared path.
    pub first_touch: Option<i64>,
    /// The strongest declared intent on a shared symbol, if any.
    pub rewrites: bool,
}

/// Which session should land the shared change, and why, stated so an
/// operator can check the choice rather than trust it.
///
/// The session that already holds the shared change commits it with the
/// least rework, so the earlier first commit wins; a declared rewrite or
/// removal outranks an extension, because the extension would otherwise be
/// built on a shape that is going away. Ties go to the lower session id, so
/// the same pair always yields the same answer.
pub(crate) fn choose_lander(a: (i64, &SideEvidence), b: (i64, &SideEvidence)) -> (i64, String) {
    let ((low, low_side), (high, high_side)) = if a.0 <= b.0 { (a, b) } else { (b, a) };
    match (low_side.first_touch, high_side.first_touch) {
        (Some(x), Some(y)) if x != y => {
            let (lander, other) = if x < y { (low, high) } else { (high, low) };
            return (
                lander,
                format!(
                    "session {lander} committed the shared change first, so it can land it with \
                     the least rework; session {other} then rebases onto it"
                ),
            );
        }
        (Some(_), None) => {
            return (
                low,
                format!("only session {low} has committed the shared change so far"),
            );
        }
        (None, Some(_)) => {
            return (
                high,
                format!("only session {high} has committed the shared change so far"),
            );
        }
        _ => {}
    }
    if low_side.rewrites != high_side.rewrites {
        let lander = if low_side.rewrites { low } else { high };
        return (
            lander,
            format!(
                "session {lander} rewrites or removes the shared target; landing that first \
                 lets the other session build on the shape that will exist"
            ),
        );
    }
    (
        low,
        format!("neither side is further along, so the lower session id ({low}) lands it"),
    )
}

/// Everything known about one colliding pair of sessions.
#[derive(Debug, Default)]
struct PairEvidence {
    paths: Vec<String>,
    symbols: Vec<String>,
    sides: BTreeMap<i64, SideEvidence>,
}

fn quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

fn rebase_command(worktree: &str, remote: &str, branch: &str) -> String {
    let worktree = quote(worktree);
    format!("git -C {worktree} fetch {remote} && git -C {worktree} rebase {remote}/{branch}")
}

fn render(
    session_a: i64,
    session_b: i64,
    pair: &PairEvidence,
    agents: &BTreeMap<i64, &AgentView>,
    (remote, branch): (&str, &str),
) -> Option<StatusAdvice> {
    let default = SideEvidence::default();
    let side = |id| pair.sides.get(&id).unwrap_or(&default);
    let (lander, why) = choose_lander((session_a, side(session_a)), (session_b, side(session_b)));
    let other = if lander == session_a {
        session_b
    } else {
        session_a
    };
    let lander_view = agents.get(&lander)?;
    let other_view = agents.get(&other)?;

    let mut targets = pair.paths.clone();
    targets.extend(pair.symbols.iter().map(|symbol| format!("symbol {symbol}")));
    let targets = targets.join(", ");
    let mut evidence = Vec::new();
    for path in &pair.paths {
        evidence.push(format!(
            "{path}: sessions {session_a} and {session_b} committed changes to the same or adjacent lines"
        ));
    }
    for symbol in &pair.symbols {
        evidence.push(format!(
            "symbol {symbol}: declared intents conflict (one side rewrites or removes it)"
        ));
    }
    evidence.push(format!("why session {lander}: {why}"));
    evidence.push(format!(
        "session {lander} lands only the shared change to {targets} as its own small pull \
         request; unrelated work stays for a follow-up, so the shared change is not held up by it"
    ));
    Some(StatusAdvice {
        id: ADVICE_ID,
        severity: StatusAdviceSeverity::Warning,
        reason: "two live sessions change the same target",
        summary: format!(
            "Land the shared change first: session {lander} commits only the shared edit to \
             {targets}, pushes it and it merges; then sessions {session_a} and {session_b} both \
             rebase on {remote}/{branch} instead of stacking on each other"
        ),
        session_id: Some(lander),
        queue_entry_id: None,
        evidence,
        commands: vec![
            format!("aethyme broker push --session {lander} --pr"),
            rebase_command(&lander_view.session.worktree_path, remote, branch),
            rebase_command(&other_view.session.worktree_path, remote, branch),
        ],
    })
}

/// Collect the committed-hunk evidence for one path shared by two branches.
fn path_evidence(
    repo: &GitRepo,
    heads: (&str, &str),
    path: &str,
) -> Option<(Option<i64>, Option<i64>)> {
    let base = repo.merge_base(heads.0, heads.1).ok()?;
    let a = parse_regions(&repo.zero_context_diff(&base, heads.0, path).ok()?);
    let b = parse_regions(&repo.zero_context_diff(&base, heads.1, path).ok()?);
    if a.is_empty() || b.is_empty() || !hunks_collide(&a, &b) {
        return None;
    }
    Some((
        repo.first_commit_time_touching(&base, heads.0, path)
            .ok()
            .flatten(),
        repo.first_commit_time_touching(&base, heads.1, path)
            .ok()
            .flatten(),
    ))
}

fn rewrites(operation: ScopeOperation) -> bool {
    matches!(operation, ScopeOperation::Replace | ScopeOperation::Remove)
}

/// Advice rows for every colliding pair of live sessions, one row per pair.
///
/// Empty unless the repository delivers through pull requests
/// (`[delivery] push_session_branches = true`): landing a small PR first is
/// only the cheap move when PRs are how work lands.
pub(crate) fn shared_edit_advice(
    repo: &GitRepo,
    agents: &[AgentView],
    overlaps: &[Overlap],
    scope_overlaps: &[ScopeOverlap],
) -> Vec<StatusAdvice> {
    if !crate::session_push::session_push_enabled(repo) {
        return Vec::new();
    }
    let Some((upstream, _)) = repo.tracking_upstream() else {
        return Vec::new();
    };
    // `tracking_upstream` names the ref short (`origin/main`); accept the
    // long form too, as `session_push` does.
    let Some((remote, branch)) = upstream
        .strip_prefix("refs/remotes/")
        .unwrap_or(&upstream)
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
    else {
        return Vec::new();
    };
    let by_id: BTreeMap<i64, &AgentView> = agents
        .iter()
        .map(|agent| (agent.session.id, agent))
        .collect();
    let head = |id: i64| -> Option<String> {
        let agent = by_id.get(&id)?;
        repo.resolve_ref(&format!("refs/heads/{}", agent.session.branch))
    };

    let mut pairs: BTreeMap<(i64, i64), PairEvidence> = BTreeMap::new();
    for overlap in overlaps {
        // A directory claim is intent, not an edit; only files can collide.
        if overlap.path.ends_with('/') {
            continue;
        }
        let (Some(head_a), Some(head_b)) = (head(overlap.session_a), head(overlap.session_b))
        else {
            continue;
        };
        let Some((touch_a, touch_b)) = path_evidence(repo, (&head_a, &head_b), &overlap.path)
        else {
            continue;
        };
        let pair = pairs
            .entry((overlap.session_a, overlap.session_b))
            .or_default();
        pair.paths.push(overlap.path.clone());
        for (id, touch) in [(overlap.session_a, touch_a), (overlap.session_b, touch_b)] {
            let side = pair.sides.entry(id).or_default();
            side.first_touch = match (side.first_touch, touch) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
        }
    }
    for overlap in scope_overlaps {
        if overlap.severity != ScopeConflictSeverity::High
            || !by_id.contains_key(&overlap.session_a)
            || !by_id.contains_key(&overlap.session_b)
        {
            continue;
        }
        let pair = pairs
            .entry((overlap.session_a, overlap.session_b))
            .or_default();
        pair.symbols.push(overlap.value.clone());
        for (id, operation) in [
            (overlap.session_a, overlap.operation_a),
            (overlap.session_b, overlap.operation_b),
        ] {
            let side = pair.sides.entry(id).or_default();
            side.rewrites |= rewrites(operation);
        }
    }

    pairs
        .into_iter()
        .filter_map(|((a, b), mut pair)| {
            pair.paths.sort();
            pair.paths.dedup();
            pair.symbols.sort();
            pair.symbols.dedup();
            render(a, b, &pair, &by_id, (remote, branch))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: &str) -> Vec<Region> {
        parse_regions(&format!("@@ -{old} +1,1 @@ context\n"))
    }

    #[test]
    fn modifications_of_the_same_line_collide() {
        assert!(hunks_collide(&hunk("5"), &hunk("5")));
        assert!(hunks_collide(&hunk("3,4"), &hunk("6,2")));
    }

    #[test]
    fn modifications_of_adjacent_lines_collide() {
        assert!(hunks_collide(&hunk("5"), &hunk("6")));
    }

    #[test]
    fn one_untouched_line_between_changes_keeps_them_apart() {
        assert!(!hunks_collide(&hunk("5"), &hunk("7")));
        assert!(!hunks_collide(&hunk("1,2"), &hunk("10,3")));
    }

    #[test]
    fn an_insertion_collides_with_a_change_to_its_neighbours() {
        // Insert after line 5 versus a change to line 5 or line 6.
        assert!(hunks_collide(&hunk("5,0"), &hunk("5")));
        assert!(hunks_collide(&hunk("5,0"), &hunk("6")));
        // Insertions after lines 5 and 6 are separated by line 6.
        assert!(!hunks_collide(&hunk("5,0"), &hunk("6,0")));
    }

    #[test]
    fn a_decorated_or_summarized_diff_yields_no_regions() {
        assert!(parse_regions("3 files changed, 4 insertions(+)\n").is_empty());
        assert!(parse_regions("@@ not a hunk header\n").is_empty());
        assert_eq!(parse_regions("@@ -4,2 +4,3 @@ fn x\n+a\n").len(), 1);
    }

    fn side(first_touch: Option<i64>, rewrites: bool) -> SideEvidence {
        SideEvidence {
            first_touch,
            rewrites,
        }
    }

    #[test]
    fn the_session_that_committed_first_lands_the_shared_change() {
        let (lander, why) =
            choose_lander((7, &side(Some(200), false)), (3, &side(Some(100), false)));
        assert_eq!(lander, 3);
        assert!(why.contains("committed the shared change first"), "{why}");
        let (lander, _) = choose_lander((3, &side(Some(300), false)), (7, &side(Some(100), false)));
        assert_eq!(lander, 7);
    }

    #[test]
    fn a_rewrite_outranks_an_extension_when_nothing_is_committed() {
        let (lander, why) = choose_lander((3, &side(None, false)), (7, &side(None, true)));
        assert_eq!(lander, 7);
        assert!(why.contains("rewrites or removes"), "{why}");
    }

    #[test]
    fn ties_go_to_the_lower_session_id_whatever_the_argument_order() {
        let tie = side(Some(100), false);
        assert_eq!(choose_lander((9, &tie), (4, &tie)).0, 4);
        assert_eq!(choose_lander((4, &tie), (9, &tie)).0, 4);
        let none = side(None, false);
        assert_eq!(choose_lander((9, &none), (4, &none)).0, 4);
    }
}
