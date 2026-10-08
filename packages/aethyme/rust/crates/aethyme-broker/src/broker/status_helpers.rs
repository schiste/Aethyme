use super::*;

/// Whether a directory that exists is no longer a usable git worktree.
///
/// True for the remains of an interrupted `git worktree remove`: the files are
/// there, the `.git` file is there, and the gitdir it points at is not, so
/// every git command answers `not a git repository` (#165).
pub(crate) fn is_orphaned_worktree_directory(path: &Path) -> bool {
    path.exists() && GitRepo::discover(path).is_err()
}

/// Whether a directory sitting in a worktree root is the broker's own, rather
/// than something that drifted in.
///
/// The rule is the exact complement of how worktree directories are named:
/// `slugify` emits only `[a-z0-9-]` and trims leading dashes, so a session
/// worktree can never begin with a dot, while the shared state the broker
/// parks beside them -- `.cargo`, the root marker -- always does. Reporting
/// those as unaccounted-for would raise the same advisory on every healthy
/// install, which is how a warning stops being read.
///
/// The cost is that a dotted stray directory goes unreported. That is the
/// right side to err on: this lane's output is a list a human is asked to act
/// on, and a list that is wrong on every machine is worth less than a list
/// that is occasionally incomplete.
pub(super) fn is_worktree_root_infrastructure(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

pub(super) fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

pub(crate) fn directory_size_without_following_links(path: &Path) -> std::io::Result<u64> {
    crate::disk_headroom::directory_usage_without_following_links(path).map(|usage| usage.bytes)
}

/// Size a tree, abandoning the walk when `deadline` passes.
///
/// `None` means the budget ran out. A partial sum is deliberately not
/// returned: written to the size records it would be a wrong number that
/// outlives the walk that produced it, and there is no way to tell it apart
/// from a real one afterwards. A routine check that cannot finish a
/// measurement learns nothing, which is the honest outcome.
pub(crate) fn plural_word(
    count: usize,
    singular: &'static str,
    plural: &'static str,
) -> &'static str {
    if count == 1 { singular } else { plural }
}

/// Whether a `Submitted` entry was deferred because the host could not judge
/// it, rather than still being in flight. Both share the status; only the
/// marker the submission records tells them apart.
pub(crate) fn submission_was_deferred(entry: &MergeQueueEntry) -> bool {
    entry.status == MergeStatus::Submitted
        && entry
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .and_then(|details| details.get("deferred").and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
}

pub(super) fn graph_integrity_advice(
    agent: &AgentView,
    graph: &FinishGraphIntegrity,
) -> Option<StatusAdvice> {
    let (id, severity, reason, summary) = match graph.status.verdict() {
        crate::GraphIntegrityVerdict::Disabled | crate::GraphIntegrityVerdict::Fresh => {
            return None;
        }
        crate::GraphIntegrityVerdict::Stale => (
            "graph.stale",
            StatusAdviceSeverity::Warning,
            "graph_stale",
            format!(
                "session {} last checked tree {} with stale committed graph fragments; this is \
                 advice and blocks nothing. Refresh and commit the graph before relying on it",
                agent.session.id,
                short_commit(&graph.tree_hash)
            ),
        ),
        crate::GraphIntegrityVerdict::Unknown => (
            "graph.unknown",
            StatusAdviceSeverity::Notice,
            "graph_unverified",
            format!(
                "session {} last graph-integrity check on tree {} could not reach a verdict \
                 ({}); treat the committed graph as unverified",
                agent.session.id,
                short_commit(&graph.tree_hash),
                graph.status.as_str()
            ),
        ),
    };
    let mut evidence = vec![format!("tree {}", graph.tree_hash)];
    evidence.extend(
        graph
            .changed_paths
            .iter()
            .take(5)
            .map(|path| format!("stale path {path}")),
    );
    Some(StatusAdvice {
        id,
        severity,
        reason,
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme graph refresh plan --repo .".into()],
    })
}

pub(super) fn deferred_submit_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    let failures = gate_failures(entry.details_json.as_deref());
    let gate_names: Vec<String> = failures
        .iter()
        .map(|failure| failure.name.clone())
        .collect();
    let summary = format!(
        "session {} latest submit qid {} was deferred: {} could not run on this host \
         (resources, disk, or its first timeout), so the change was not judged; free the \
         resource, then resubmit without changing code",
        agent.session.id,
        entry.id,
        if gate_names.is_empty() {
            "a selected gate".to_string()
        } else {
            gate_names.join(", ")
        }
    );
    let mut evidence = queue_evidence(entry);
    evidence.extend(failures.iter().map(GateFailure::evidence));
    StatusAdvice {
        id: "session.latest-submit-deferred",
        severity: StatusAdviceSeverity::Blocked,
        reason: "submit_deferred",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence,
        commands: vec![
            "aethyme broker advanced resources list".into(),
            "aethyme broker gc plan".into(),
            format!("aethyme broker submit --session {}", agent.session.id),
        ],
    }
}

pub(super) fn rejected_submit_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    let failures = gate_failures(entry.details_json.as_deref());
    let gate_names: Vec<String> = failures
        .iter()
        .map(|failure| failure.name.clone())
        .collect();
    let environmental = crate::exit_status::failures_are_environmental(
        failures
            .iter()
            .map(|failure| failure.failure_class.as_deref()),
    );
    let summary = if environmental {
        format!(
            "session {} latest submit qid {} was not verified: {} could not run on this host \
             (low disk, locks or environment), so the code was not judged; free the resource, \
             then resubmit without changing code",
            agent.session.id,
            entry.id,
            gate_names.join(", ")
        )
    } else if gate_names.is_empty() {
        format!(
            "session {} latest submit qid {} was rejected; inspect the gate details, commit a fix, then resubmit",
            agent.session.id, entry.id
        )
    } else {
        format!(
            "session {} latest submit qid {} was rejected by {}; commit a fix, then resubmit",
            agent.session.id,
            entry.id,
            gate_names.join(", ")
        )
    };

    let mut evidence = queue_evidence(entry);
    if failures.is_empty() {
        evidence.push("gate details unavailable or did not include a failing gate".into());
    } else {
        evidence.extend(failures.iter().map(GateFailure::evidence));
    }

    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.latest-submit-rejected",
        severity: StatusAdviceSeverity::Blocked,
        reason: "submit_rejected",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence,
        commands: vec![
            format!("git -C {worktree} status --short"),
            format!(
                "aethyme broker advanced gates run --session {}",
                agent.session.id
            ),
            format!("aethyme broker submit --session {}", agent.session.id),
        ],
    }
}

pub(super) fn conflict_submit_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    let conflicts = details_string_array(entry.details_json.as_deref(), "conflicts");
    let blockers = details_i64_array(entry.details_json.as_deref(), "blocking_sessions");
    let conflict_count = conflicts.len();
    let summary = if conflict_count == 0 {
        format!(
            "session {} latest submit qid {} conflicted; read the action-required file, rebase, then resubmit",
            agent.session.id, entry.id
        )
    } else {
        format!(
            "session {} latest submit qid {} conflicted on {} path(s); rebase, resolve, then resubmit",
            agent.session.id, entry.id, conflict_count
        )
    };

    let mut evidence = queue_evidence(entry);
    evidence.extend(path_evidence("conflict", &conflicts));
    if !blockers.is_empty() {
        let labels: Vec<String> = blockers.iter().map(i64::to_string).collect();
        evidence.push(format!("blocking sessions {}", labels.join(", ")));
    }

    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.latest-submit-conflict",
        severity: StatusAdviceSeverity::Blocked,
        reason: "submit_conflict",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence,
        commands: vec![
            format!(
                "aethyme broker advanced repair --session {}",
                agent.session.id
            ),
            format!("aethyme broker submit --session {}", agent.session.id),
            format!("cat {}/{}", worktree, crate::ACTION_REQUIRED_RELPATH),
        ],
    }
}

pub(super) fn promoted_conflict_advice(
    session_id: i64,
    _worktree_path: &str,
    integration_branch: &str,
    conflicts: &[&PromotedConflict],
) -> StatusAdvice {
    let count = conflicts.len();
    let summary = format!(
        "session {session_id} overlaps promoted integration work on {count} path(s); rebase onto {integration_branch} before submit"
    );
    let mut evidence: Vec<String> = conflicts
        .iter()
        .take(5)
        .map(|conflict| {
            format!(
                "path {} (session {}, integration {})",
                conflict.path, conflict.session_path, conflict.promoted_path
            )
        })
        .collect();
    if count > evidence.len() {
        evidence.push(format!("and {} more path(s)", count - evidence.len()));
    }

    let mut commands = Vec::new();
    commands.push(format!(
        "aethyme broker advanced repair --session {session_id}"
    ));
    commands.push(format!("aethyme broker submit --session {session_id}"));

    StatusAdvice {
        id: "session.promoted-conflict",
        severity: StatusAdviceSeverity::Blocked,
        reason: "promoted_conflict",
        summary,
        session_id: Some(session_id),
        queue_entry_id: None,
        evidence,
        commands,
    }
}

pub(super) fn promoted_clean_finish_advice(
    agent: &AgentView,
    entry: &MergeQueueEntry,
) -> StatusAdvice {
    StatusAdvice {
        id: "session.promoted-clean-finish",
        severity: StatusAdviceSeverity::Notice,
        reason: "promoted_clean_finish",
        summary: format!(
            "session {} is promoted and clean; run aethyme broker finish --session {}",
            agent.session.id, agent.session.id
        ),
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence: vec![
            format!("qid {} promoted", entry.id),
            format!("head {}", short_commit(&entry.head_commit)),
        ],
        commands: vec![format!(
            "aethyme broker finish --session {}",
            agent.session.id
        )],
    }
}

/// The session's head and the commits it made that no remote holds.
///
/// "Made" is measured from the session's own bases, not from integration.
/// A session inherits whatever its base carried, and counting another
/// session's unpublished promotions against every session built on top
/// of them is how one five-week backlog read as dozens of stranded
/// worktrees; integration reports that backlog itself. Excluding
/// integration's tip would do the same today, because promotion replays
/// a session's commits under new SHAs -- but only for as long as it
/// does: once integration reaches the session's head any other way (a
/// fast-forward, a hand merge), the session's own unpushed commits would
/// vanish from the count. The session's bases do not depend on how
/// integration moves: the immutable start base and, after a
/// `start --reuse`, the reuse base -- `diff_base`, unless acceptance has
/// since overwritten it with this session's own head. `integration_head`
/// is the fallback base for a session that recorded neither.
///
/// A free function rather than a method so `unpushed_work_within` can run it
/// on worker threads: it reads only the session's own checkout.
pub(super) fn session_off_remote_work(
    session: &Session,
    upstream: Option<&str>,
    integration_head: Option<&str>,
) -> Result<Option<(String, crate::unpushed::OffRemoteWork)>, crate::GitError> {
    let worktree = Path::new(&session.worktree_path);
    if !worktree.exists() {
        return Ok(None);
    }
    let checkout = GitRepo::discover(worktree)?;
    let head = checkout.head_commit()?;
    let mut excluded = Vec::new();
    if let Some(base) = session.adoption_base.as_deref() {
        excluded.push(base.to_string());
    }
    if let Some(base) = session.diff_base.as_deref()
        && session.accepted_session_head.as_deref() != Some(base)
    {
        excluded.push(base.to_string());
    }
    if excluded.is_empty()
        && let Some(integration) = integration_head
    {
        excluded.push(integration.to_string());
    }
    let excluded = excluded
        .iter()
        .map(String::as_str)
        .filter(|base| *base != "HEAD" && checkout.resolve_ref(base).is_some())
        .collect::<Vec<_>>();
    let work = crate::unpushed::off_remote_work(&checkout, &head, &excluded, upstream)?;
    Ok(Some((head, work)))
}

/// What `status --refresh` reads from one live session's checkout. Each
/// checkout is read once, on a worker thread, and both the dirty count and
/// the per-session advice use the result (#460): reading them serially, twice,
/// was 136 `git status` calls and thirteen seconds on a repository with
/// sixty-eight sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CheckoutInspection {
    /// The time budget ran out first; nothing is known about this checkout.
    NotInspected,
    /// No checkout, or Git could not read it. Status never reported these.
    Unreadable,
    Read {
        dirty: Vec<String>,
        branch: String,
        head: String,
        /// Tip of the session's recorded branch. When the checkout is still
        /// on that branch it is the same as `head`; a different branch is
        /// resolved separately so status can report both sides of drift.
        recorded_branch_head: Option<String>,
    },
}

fn checkout_inspection_error(error: crate::GitError) -> CheckoutInspection {
    match error {
        crate::GitError::TimedOut { .. } => CheckoutInspection::NotInspected,
        _ => CheckoutInspection::Unreadable,
    }
}

pub(super) fn inspect_session_checkouts(
    agents: &[AgentView],
    deadline: Option<std::time::Instant>,
) -> std::collections::BTreeMap<i64, CheckoutInspection> {
    let inspections = crate::worktree_report::inspect_in_parallel(agents, |agent| {
        let expired = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if expired() {
            return CheckoutInspection::NotInspected;
        }
        let checkout = match GitRepo::discover(Path::new(&agent.session.worktree_path)) {
            Ok(checkout) if !expired() => checkout,
            Ok(_) => return CheckoutInspection::NotInspected,
            Err(error) => return checkout_inspection_error(error),
        };
        let branch = match checkout.current_branch() {
            Ok(branch) if !expired() => branch,
            Ok(_) => return CheckoutInspection::NotInspected,
            Err(error) => return checkout_inspection_error(error),
        };
        let dirty = match checkout.dirty_paths() {
            Ok(dirty) if !expired() => dirty,
            Ok(_) => return CheckoutInspection::NotInspected,
            Err(error) => return checkout_inspection_error(error),
        };
        let head = match checkout.head_commit() {
            Ok(head) if !expired() => head,
            Ok(_) => return CheckoutInspection::NotInspected,
            Err(error) => return checkout_inspection_error(error),
        };
        let recorded_branch_head = if branch == agent.session.branch {
            Some(head.clone())
        } else {
            let recorded_ref = format!("refs/heads/{}", agent.session.branch);
            let recorded = checkout.resolve_ref(&recorded_ref);
            if expired() {
                return CheckoutInspection::NotInspected;
            }
            recorded
        };
        CheckoutInspection::Read {
            dirty,
            branch,
            head,
            recorded_branch_head,
        }
    });
    agents
        .iter()
        .map(|agent| agent.session.id)
        .zip(inspections)
        .collect()
}

#[cfg(test)]
mod checkout_inspection_error_tests {
    use super::{CheckoutInspection, checkout_inspection_error};

    #[test]
    fn a_git_timeout_is_an_unknown_checkout_not_a_clean_or_unreadable_one() {
        let inspection = checkout_inspection_error(crate::GitError::TimedOut {
            args: "status --porcelain".into(),
            seconds: 7,
        });

        assert_eq!(inspection, CheckoutInspection::NotInspected);
    }

    #[test]
    fn a_non_timeout_git_error_remains_unreadable() {
        let inspection = checkout_inspection_error(crate::GitError::Git {
            args: "status --porcelain".into(),
            stderr: "repository unavailable".into(),
        });

        assert_eq!(inspection, CheckoutInspection::Unreadable);
    }
}

/// The row that says which refresh checks the inspection budget cut short
/// and for which sessions, so an incomplete answer is never read as a clean
/// one. `None` when nothing was cut.
pub(super) fn budget_cut_advice(
    cut: &[&'static str],
    checkouts: Option<&std::collections::BTreeMap<i64, CheckoutInspection>>,
    unpushed_not_inspected: &[i64],
    upstream_ref: Option<&str>,
) -> Option<StatusAdvice> {
    if cut.is_empty() {
        return None;
    }
    let mut sessions: Vec<i64> = checkouts
        .into_iter()
        .flatten()
        .filter(|(_, inspection)| **inspection == CheckoutInspection::NotInspected)
        .map(|(id, _)| *id)
        .chain(unpushed_not_inspected.iter().copied())
        .collect();
    sessions.sort_unstable();
    sessions.dedup();
    let mut evidence = vec![format!("cut short: {}", cut.join(", "))];
    if !sessions.is_empty() {
        evidence.push(format!(
            "sessions not inspected: {}",
            sessions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(StatusAdvice {
        id: "status.inspection-budget",
        severity: StatusAdviceSeverity::Notice,
        reason: "refresh checks stopped at the inspection time budget",
        summary: format!(
            "{} not completed within the {} ms inspection budget; what they did not reach is unknown, not clean",
            cut.join(", "),
            status_inspection_budget().as_millis()
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        // The unbounded reviewed path, for an operator who needs the answer
        // the budget cut off.
        commands: match upstream_ref {
            Some(upstream) if cut.contains(&"integration_drift") => vec![format!(
                "aethyme broker advanced integration reconcile --upstream {} --dry-run",
                shell_quote(upstream)
            )],
            _ => Vec::new(),
        },
    })
}

pub(super) fn dirty_session_count(
    inspections: &std::collections::BTreeMap<i64, CheckoutInspection>,
) -> usize {
    inspections
        .values()
        .filter(|inspection| {
            matches!(inspection, CheckoutInspection::Read { dirty, .. } if !dirty.is_empty())
        })
        .count()
}

pub(super) fn checkout_drift_advice(
    agent: &AgentView,
    actual_branch: &str,
    actual_head: &str,
    recorded_branch_head: Option<&str>,
) -> Option<StatusAdvice> {
    if actual_branch == agent.session.branch && recorded_branch_head == Some(actual_head) {
        return None;
    }

    let recorded_head = recorded_branch_head.map(short_commit).unwrap_or("missing");
    let worktree = shell_quote(&agent.session.worktree_path);
    let recorded_ref = shell_quote(&format!("refs/heads/{}", agent.session.branch));
    Some(StatusAdvice {
        id: "session.checkout-drift",
        severity: StatusAdviceSeverity::Blocked,
        reason: "checkout_branch_or_head_drifted",
        summary: format!(
            "session {} was recorded on branch {:?}, but its checkout is on {:?}; submit is blocked until the checkout identity is restored or re-adopted",
            agent.session.id, agent.session.branch, actual_branch
        ),
        session_id: Some(agent.session.id),
        queue_entry_id: None,
        evidence: vec![
            format!("recorded branch: {}", agent.session.branch),
            format!("actual branch: {actual_branch}"),
            format!("actual HEAD: {}", short_commit(actual_head)),
            format!("recorded branch HEAD: {recorded_head}"),
        ],
        commands: vec![
            format!("git -C {worktree} status --short --branch"),
            format!("git -C {worktree} rev-parse HEAD"),
            format!("git -C {worktree} rev-parse {recorded_ref}"),
        ],
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct HeadroomReading {
    pub(super) path: PathBuf,
    pub(super) available: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct GateHeadroom {
    pub(super) bytes: Option<HeadroomReading>,
    pub(super) inodes: Option<HeadroomReading>,
}

pub(super) fn lowest_headroom_with(
    probes: &[PathBuf],
    read: impl Fn(&Path) -> Option<crate::disk_headroom::DiskHeadroom>,
) -> GateHeadroom {
    let mut lowest = GateHeadroom::default();
    for probe in probes {
        let Some(headroom) = read(probe) else {
            continue;
        };
        if lowest
            .bytes
            .as_ref()
            .is_none_or(|current| headroom.bytes < current.available)
        {
            lowest.bytes = Some(HeadroomReading {
                path: probe.clone(),
                available: headroom.bytes,
            });
        }
        if let Some(inodes) = headroom.inodes
            && lowest
                .inodes
                .as_ref()
                .is_none_or(|current| inodes < current.available)
        {
            lowest.inodes = Some(HeadroomReading {
                path: probe.clone(),
                available: inodes,
            });
        }
    }
    lowest
}

/// The advisory for a volume with less free space than a gate needs, or `None`.
///
/// Every retention signal is per repository and none of them can see the
/// volume, which is shared: each enrolled repository can sit inside its own
/// budget while the host has less free space than a gate needs to start. That
/// state used to surface only as an evidence line on the retained-worktree
/// advisory -- "host free space: 2.1 GiB of 8.0 GiB a gate needs to start" --
/// under a `notice` or `warning` about housekeeping, and not at all in a
/// repository with nothing retained, while every gate there refused in 0s.
///
/// So it is its own `blocked` row, derived from the same [`SweepUrgency`] the
/// autonomous sweep escalates on and the same threshold gates refuse at, and it
/// points at what reclaims space everywhere it can be: `gc plan` sizes build
/// output and the gate cache (the refusal's own recovery), and `gc storage
/// plan` covers every repository's broker storage on the host. It does not
/// promise that reclaiming this repository is enough -- the volume is shared.
///
/// Below [`DISK_LOW_WARNING_MULTIPLE`] times the gate threshold the same row
/// appears one band earlier, as `host.disk-low` at `warning`: gates still
/// start, but the margin is one large build. Measured 2026-10-03, a machine
/// went from comfortable to 3.4 GiB free with status silent until the gate
/// threshold, by which point every gate already refused.
///
/// Both bands name `gc reclaim plan`. `gc plan` and `gc sweep` touch only
/// closed sessions and sessions idle past `idle_session_artifact_hours`;
/// build output in open sessions that are merely quiet -- the bulk of the
/// 40 GiB reclaimed by hand that day -- is reachable only through the
/// reviewed `gc reclaim` lane.
///
/// Unknown headroom does not escalate, for the reason `refusal` fails open: a
/// reading that could not be taken is not evidence of a full disk.
pub(super) fn gate_headroom_advice(
    available_bytes: Option<u64>,
    byte_probe: Option<&Path>,
    available_inodes: Option<u64>,
    inode_probe: Option<&Path>,
    required_bytes: u64,
    required_inodes: u64,
) -> Option<StatusAdvice> {
    let bytes_starved = available_bytes.is_some_and(|available| available < required_bytes);
    let inodes_starved = available_inodes.is_some_and(|available| available < required_inodes);
    let starved = bytes_starved || inodes_starved;
    let warn_below_bytes = required_bytes.saturating_mul(DISK_LOW_WARNING_MULTIPLE);
    let warn_below_inodes = required_inodes.saturating_mul(DISK_LOW_WARNING_MULTIPLE);
    let bytes_low = available_bytes.is_some_and(|available| available < warn_below_bytes);
    let inodes_low = available_inodes.is_some_and(|available| available < warn_below_inodes);
    if !starved && !bytes_low && !inodes_low {
        return None;
    }

    let mut constraints = Vec::new();
    let mut evidence = Vec::new();
    if let Some(available) = available_bytes {
        let volume = byte_probe
            .map(|probe| format!(" on {}", probe.display()))
            .unwrap_or_default();
        constraints.push(format!(
            "{} free{volume}; a gate needs {}",
            crate::disk_headroom::format_gibibytes(available),
            crate::disk_headroom::format_gibibytes(required_bytes)
        ));
        evidence.push(format!("free/required bytes: {available}/{required_bytes}"));
    }
    if let Some(available) = available_inodes {
        let volume = inode_probe
            .map(|probe| format!(" on {}", probe.display()))
            .unwrap_or_default();
        constraints.push(format!(
            "{} free inodes{volume}; a gate needs {} inodes",
            available, required_inodes
        ));
        evidence.push(format!(
            "free/required inodes: {available}/{required_inodes}"
        ));
    }

    let summary = if starved {
        format!(
            "{}; every gate here refuses before running until resources are reclaimed",
            constraints.join("; ")
        )
    } else {
        format!(
            "{}; one large build could stop every gate here",
            constraints.join("; ")
        )
    };
    evidence.push("the volume is shared: other repositories and files on it count too".into());
    evidence.push("gc reclaim plan covers quiet open sessions' build output".into());
    Some(StatusAdvice {
        id: if starved {
            "host.gate-headroom"
        } else {
            "host.disk-low"
        },
        severity: if starved {
            StatusAdviceSeverity::Blocked
        } else {
            StatusAdviceSeverity::Warning
        },
        reason: if starved {
            "the host has less free byte or inode headroom than a gate needs to start"
        } else {
            "the host is close to the byte or inode headroom a gate needs to start"
        },
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![
            "aethyme broker gc reclaim plan".into(),
            "aethyme broker gc plan".into(),
            "aethyme broker gc storage plan".into(),
        ],
    })
}

/// Free space below this multiple of the gate threshold raises `host.disk-low`.
pub(super) const DISK_LOW_WARNING_MULTIPLE: u64 = 2;

/// Severity for the retained-worktree advisory, from per-repository signals.
///
/// The shared volume is reported by its own advisory, [`gate_headroom_advice`],
/// so a full disk is said once rather than folded into housekeeping here.
pub(super) fn cleanup_retention_severity(
    retained_worktrees: usize,
    retained_bytes: u64,
    oldest_age_days: u64,
    policy_days: u32,
    retained_bytes_budget: u64,
) -> StatusAdviceSeverity {
    if retained_worktrees >= 5
        || (retained_bytes_budget > 0 && retained_bytes >= retained_bytes_budget)
        || oldest_age_days >= u64::from(policy_days)
    {
        StatusAdviceSeverity::Warning
    } else {
        StatusAdviceSeverity::Notice
    }
}

/// How many drifted directories to name inline before deferring to `gc plan`.
///
/// Status advice is read in a terminal. A root that has drifted badly would
/// otherwise bury every other advisory under its own path list, which is the
/// failure mode -- a warning nobody reads -- reproduced in a new place.
pub(super) const UNCLAIMED_EVIDENCE_LIMIT: usize = 5;

/// Drift severity on the same threshold the retained-worktree advice uses.
///
/// One unexplained directory is worth saying; five is worth interrupting for,
/// because at that point the broker's records describe materially less of the
/// disk than the disk holds.
pub(super) fn unclaimed_worktree_severity(unclaimed: usize) -> StatusAdviceSeverity {
    if unclaimed >= 5 {
        StatusAdviceSeverity::Warning
    } else {
        StatusAdviceSeverity::Notice
    }
}

/// One status evidence line for checkouts closed sessions left on disk.
pub(super) fn closed_worktree_evidence(closed: &crate::retention::ClosedWorktreeSummary) -> String {
    let bytes = if closed.unmeasured_count == 0 {
        format!("{} bytes", closed.estimated_bytes)
    } else {
        format!(
            "at least {} bytes ({} never sized)",
            closed.estimated_bytes, closed.unmeasured_count
        )
    };
    let adopted = if closed.adopted_count == 0 {
        String::new()
    } else {
        format!(
            "; {} adopted {} GC never removes",
            closed.adopted_count,
            plural_word(closed.adopted_count, "checkout", "checkouts")
        )
    };
    match closed.command.as_deref() {
        Some(command) => format!(
            "closed sessions with checkouts on disk: {}, {bytes}{adopted}; see which GC would reclaim: {command}",
            closed.count
        ),
        None => "closed sessions with checkouts on disk: 0".into(),
    }
}

pub(super) fn cleanup_retention_warning(retention: &CleanupRetention) -> Option<String> {
    retention.over_retained_bytes_budget.then(|| {
        // Naming the deficit rather than the total is what makes this
        // actionable, and saying when reclamation cannot close it stops the
        // warning from recommending work that would not help (#176).
        //
        // A floor proves the budget is broken -- unmeasured bytes are still
        // bytes -- but it cannot prove the gap is closable: everything nobody
        // walked counts against the deficit and none of it is known to be
        // reclaimable. So an incomplete measurement may report the breach and
        // must not promise the cure.
        if retention.unmeasured_worktree_count > 0 {
            format!(
                "retained broker storage is at least {} bytes over the configured {} byte budget, \
                 and {} retained {} never been sized -- measure first with \
                 `aethyme broker gc plan`",
                retention.retained_bytes_deficit,
                retention.retained_bytes_budget,
                retention.unmeasured_worktree_count,
                if retention.unmeasured_worktree_count == 1 {
                    "worktree has"
                } else {
                    "worktrees have"
                }
            )
        } else if retention.clears_retained_bytes_budget {
            format!(
                "retained broker storage is {} bytes over the configured {} byte budget; \
                 reclaiming eligible worktrees would clear it -- review \
                 `aethyme broker gc plan`",
                retention.retained_bytes_deficit, retention.retained_bytes_budget
            )
        } else {
            format!(
                "retained broker storage is {} bytes over the configured {} byte budget and only \
                 {} bytes are reclaimable; the budget cannot be met by cleanup alone -- review \
                 `aethyme broker gc plan`",
                retention.retained_bytes_deficit,
                retention.retained_bytes_budget,
                retention.estimated_reclaimable_bytes
            )
        }
    })
}

/// One advice row per session pair whose edits would conflict. Pairs that
/// merge cleanly stay in `overlap_pairs` and produce no advice.
pub(super) fn overlap_pair_advice(pairs: &[crate::OverlapPair]) -> Vec<StatusAdvice> {
    pairs
        .iter()
        .filter(|pair| pair.severity == crate::OverlapSeverity::High)
        .map(|pair| StatusAdvice {
            id: "lease.overlap-conflict",
            severity: StatusAdviceSeverity::Warning,
            reason: "overlap_conflict",
            summary: format!(
                "sessions {} and {} would conflict on {} path(s)",
                pair.session_a,
                pair.session_b,
                pair.conflicting_paths.len()
            ),
            session_id: Some(pair.session_a),
            queue_entry_id: None,
            evidence: pair
                .conflicting_paths
                .iter()
                .take(crate::overlap_pairs::OVERLAP_SAMPLE_PATHS)
                .cloned()
                .collect(),
            commands: vec!["aethyme broker advanced leases --json".into()],
        })
        .collect()
}

pub(super) fn retention_config_advice(status: &RetentionConfigStatus) -> Option<StatusAdvice> {
    if status.is_healthy() {
        return None;
    }

    let mut evidence = status
        .warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let (severity, reason, summary, command) = if let Some(error) = &status.error {
        evidence.insert(0, format!("broker.toml: {error}"));
        (
            StatusAdviceSeverity::Warning,
            "retention_config_invalid",
            "retention configuration could not be loaded; status is using conservative defaults and reclamation commands remain blocked".into(),
            "edit .aethyme/broker.toml to fix the named field, then run aethyme broker gc plan",
        )
    } else {
        let count = status.warnings.len();
        (
            StatusAdviceSeverity::Warning,
            "retention_config_unknown_fields",
            format!(
                "retention configuration ignored {count} unknown {}; known retention settings remain active",
                plural_word(count, "field", "fields")
            ),
            "edit .aethyme/broker.toml to remove or correct the named field(s)",
        )
    };

    Some(StatusAdvice {
        id: "retention.config",
        severity,
        reason,
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![command.into()],
    })
}

pub(super) fn status_summary(
    agents: &[AgentView],
    overlap_count: usize,
    overlap_pairs: OverlapPairCounts,
    promoted_conflict_count: usize,
    dirty_sessions: usize,
    integration: &SummaryIntegration,
) -> StatusSummary {
    let live_sessions = agents.len();
    let active_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Active)
        .count();
    let idle_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Idle)
        .count();
    let stale_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Stale)
        .count();
    let may_move_integration = integration.promotes && live_sessions > 0;

    let sessions = session_summary_phrase(
        live_sessions,
        active_sessions,
        idle_sessions,
        stale_sessions,
    );
    let overlaps = overlap_summary_phrase(overlap_pairs, promoted_conflict_count);
    let integration_phrase = integration_summary_phrase(integration);
    let mut notes = Vec::new();
    if dirty_sessions > 0 {
        notes.push(format!(
            "{} dirty {} need commit before submit",
            dirty_sessions,
            plural_word(dirty_sessions, "session", "sessions")
        ));
    }
    notes.push(if !integration.promotes {
        "verify-only: submit does not move integration".to_string()
    } else if active_sessions > 0 {
        "active session may promote new integration work".to_string()
    } else if live_sessions > 0 {
        "live session may promote new integration work".to_string()
    } else {
        "no active submitters".to_string()
    });

    let mut commands = Vec::new();
    if may_move_integration {
        commands.push("aethyme broker advanced integration wait-stable --seconds 30".into());
    }
    if integration.promotes && integration.relation != StatusIntegrationRelation::CurrentWithMain {
        commands.push("aethyme broker advanced integration status".into());
    }

    StatusSummary {
        message: format!(
            "{sessions}; {overlaps}; {integration_phrase}; {}",
            notes.join("; ")
        ),
        live_sessions,
        active_sessions,
        idle_sessions,
        stale_sessions,
        dirty_sessions,
        overlap_count,
        promoted_conflict_count,
        integration_relation: integration.relation,
        integration_ahead_main_commits: integration.ahead_baseline_commits,
        integration_head: integration.head.clone(),
        baseline_ref: integration.baseline_ref.clone(),
        baseline_head: integration.baseline_head.clone(),
        main_head: integration.main_head.clone(),
        integration_ahead_local_main_commits: integration.ahead_main_commits,
        may_move_integration,
        commands,
    }
}

pub(super) fn session_summary_phrase(
    live_sessions: usize,
    active_sessions: usize,
    idle_sessions: usize,
    stale_sessions: usize,
) -> String {
    if live_sessions == 0 {
        return "no live sessions".into();
    }
    if live_sessions == 1 {
        if active_sessions == 1 {
            return "1 active session".into();
        }
        if idle_sessions == 1 {
            return "1 idle session".into();
        }
        if stale_sessions == 1 {
            return "1 stale session".into();
        }
        return "1 live session".into();
    }

    let mut parts = Vec::new();
    if active_sessions > 0 {
        parts.push(format!("{active_sessions} active"));
    }
    if idle_sessions > 0 {
        parts.push(format!("{idle_sessions} idle"));
    }
    if stale_sessions > 0 {
        parts.push(format!("{stale_sessions} stale"));
    }
    if parts.is_empty() {
        format!("{live_sessions} live sessions")
    } else {
        format!("{live_sessions} live sessions ({})", parts.join(", "))
    }
}

/// Overlapping session pairs, and how many of them Git says would conflict.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct OverlapPairCounts {
    pub(super) pairs: usize,
    pub(super) conflicting: usize,
}

pub(super) fn overlap_summary_phrase(
    overlaps: OverlapPairCounts,
    promoted_conflict_count: usize,
) -> String {
    let live = match overlaps {
        OverlapPairCounts { pairs: 0, .. } => None,
        OverlapPairCounts {
            pairs,
            conflicting: 0,
        } => Some(format!(
            "{pairs} overlapping session {}",
            plural_word(pairs, "pair", "pairs")
        )),
        OverlapPairCounts { pairs, conflicting } => Some(format!(
            "{pairs} overlapping session {} ({conflicting} conflicting)",
            plural_word(pairs, "pair", "pairs")
        )),
    };
    let promoted = (promoted_conflict_count > 0).then(|| {
        format!(
            "{} promoted {}",
            promoted_conflict_count,
            plural_word(promoted_conflict_count, "conflict", "conflicts")
        )
    });
    match (live, promoted) {
        (None, None) => "no overlaps".into(),
        (Some(live), None) => live,
        (None, Some(promoted)) => promoted,
        (Some(live), Some(promoted)) => format!("{live}, {promoted}"),
    }
}

/// Short name for a baseline ref: `origin/main`, `main`, or `checkout` for
/// the HEAD fallback, which is not the default branch by any evidence.
pub(super) fn baseline_label(baseline_ref: &str) -> &str {
    if baseline_ref == "HEAD" {
        return "checkout";
    }
    baseline_ref
        .strip_prefix("refs/remotes/")
        .or_else(|| baseline_ref.strip_prefix("refs/heads/"))
        .unwrap_or(baseline_ref)
}

/// The summary's integration clause. It names the baseline it counts
/// against, and when the local checkout sits somewhere else -- typically a
/// fast-forward not yet pushed -- it says where, so the clause cannot be read
/// as contradicting `integration status` (#374).
pub(super) fn integration_summary_phrase(integration: &SummaryIntegration) -> String {
    let branch = &integration.branch;
    let label = baseline_label(&integration.baseline_ref);
    let mut phrase = match integration.relation {
        StatusIntegrationRelation::NotChecked => {
            format!("{branch} relation to {label} not checked")
        }
        StatusIntegrationRelation::CurrentWithMain => format!("{branch} current with {label}"),
        StatusIntegrationRelation::AheadOfMain => format!(
            "{branch} ahead of {label} by {} {}",
            integration.ahead_baseline_commits,
            plural_word(
                integration.ahead_baseline_commits as usize,
                "commit",
                "commits"
            )
        ),
        StatusIntegrationRelation::DivergedFromMain => format!("{branch} diverged from {label}"),
    };
    if integration.relation != StatusIntegrationRelation::NotChecked
        && integration.baseline_ref != "HEAD"
        && integration.main_head != integration.baseline_head
    {
        let local = if integration.main_head == integration.head {
            "local checkout matches integration".to_string()
        } else if integration.main_is_ancestor {
            format!(
                "local checkout {} {} behind integration",
                integration.ahead_main_commits,
                plural_word(integration.ahead_main_commits as usize, "commit", "commits")
            )
        } else {
            "local checkout diverged from integration".to_string()
        };
        phrase.push_str(&format!(" ({local})"));
    }
    phrase
}

pub(super) fn integration_movement_advice(
    integration_branch: &str,
    integration_head: &str,
    agents: &[AgentView],
) -> StatusAdvice {
    let count = agents.len();
    let summary = format!(
        "{} live {} may submit and move {integration_branch}; wait for a stable integration window before treating long checks as current-tip proof",
        count,
        plural_word(count, "session", "sessions")
    );
    let mut evidence: Vec<String> = agents
        .iter()
        .take(5)
        .map(|agent| {
            format!(
                "session {} {} {}",
                agent.session.id,
                agent.derived_status.as_str(),
                agent.session.branch
            )
        })
        .collect();
    if count > evidence.len() {
        evidence.push(format!(
            "and {} more {}",
            count - evidence.len(),
            plural_word(count - evidence.len(), "session", "sessions")
        ));
    }
    evidence.push(format!(
        "integration head {}",
        short_commit(integration_head)
    ));

    StatusAdvice {
        id: "integration.may-move",
        severity: StatusAdviceSeverity::Notice,
        reason: "live_sessions_can_submit",
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![
            "aethyme broker advanced integration wait-stable --seconds 30".into(),
            "aethyme broker advanced agents".into(),
        ],
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn integration_next_action(
    branch: &str,
    integration_head: &str,
    main_head: &str,
    upstream_head: Option<&str>,
    main_is_ancestor: bool,
    latest_delivery_entry_id: Option<i64>,
    promoted_entries: &[PromotedIntegrationEntry],
    changed_files: &[String],
    conflicts: &[PromotedConflict],
) -> IntegrationNextAction {
    use std::collections::BTreeSet;

    let conflict_sessions: BTreeSet<i64> = conflicts
        .iter()
        .map(|conflict| conflict.session_id)
        .collect();
    if !conflict_sessions.is_empty() {
        let count = conflict_sessions.len();
        let noun = if count == 1 { "session" } else { "sessions" };
        let verb = if count == 1 { "overlaps" } else { "overlap" };
        let commands = conflict_sessions
            .iter()
            .take(5)
            .map(|session_id| format!("aethyme broker advanced repair --session {session_id}"))
            .collect();
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Blocked,
            summary: format!(
                "{count} {noun} {verb} the pending integration layer; repair or rebase before submit"
            ),
            commands,
        };
    }

    if main_head == integration_head {
        return IntegrationNextAction {
            state: IntegrationDeliveryState::LocallySynchronized,
            summary: format!("local main is synchronized with {branch}"),
            commands: Vec::new(),
        };
    }

    if upstream_head == Some(integration_head)
        && let Some(entry_id) = latest_delivery_entry_id
    {
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Published,
            summary: format!(
                "{branch} is published at {integration_head}; local main is not synchronized"
            ),
            commands: vec![format!(
                "aethyme broker advanced ship execute --entry {entry_id} --confirm {integration_head} --sync-main"
            )],
        };
    }

    if !promoted_entries.is_empty() {
        let count = promoted_entries.len();
        let noun = if count == 1 {
            "promoted entry"
        } else {
            "promoted entries"
        };
        let verb = if count == 1 { "is" } else { "are" };
        if main_is_ancestor {
            let entry_id = latest_delivery_entry_id
                .expect("visible promoted entries have a delivery queue entry");
            return IntegrationNextAction {
                state: IntegrationDeliveryState::Promoted,
                summary: format!(
                    "{count} {noun} {verb} promoted on {branch} and ready for a ship plan"
                ),
                commands: vec![format!(
                    "aethyme broker advanced ship plan --entry {entry_id}"
                )],
            };
        }
        let entry_id =
            latest_delivery_entry_id.expect("visible promoted entries have a delivery queue entry");
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Blocked,
            summary: format!(
                "{count} {noun} {verb} pending, but main and {branch} have diverged; inspect the blocked ship plan"
            ),
            commands: vec![format!(
                "aethyme broker advanced ship plan --entry {entry_id}"
            )],
        };
    }

    if !changed_files.is_empty() {
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Untracked,
            summary: format!(
                "{branch} differs from main, but no promoted queue entries describe the pending commits; inspect branch history"
            ),
            commands: vec![format!("git log --oneline --left-right HEAD...{branch}")],
        };
    }

    IntegrationNextAction {
        state: IntegrationDeliveryState::Untracked,
        summary: "no promoted work pending outside main".into(),
        commands: Vec::new(),
    }
}

pub(super) fn integration_live_sessions(sessions: Vec<Session>) -> Vec<IntegrationLiveSession> {
    sessions
        .into_iter()
        .map(|session| IntegrationLiveSession {
            id: session.id,
            status: session.status,
            branch: session.branch,
            task: session.task,
        })
        .collect()
}

/// One advice row per session holding unpushed commits, plus one for an
/// integration branch running ahead of upstream.
pub(super) fn unpushed_work_advice(
    report: &crate::UnpushedWorkReport,
    now_ms: i64,
    verify_only: bool,
) -> Vec<StatusAdvice> {
    let age = |at: Option<i64>| {
        crate::unpushed::describe_age(now_ms.saturating_sub(at.unwrap_or(now_ms)))
    };
    let mut advice = report
        .sessions
        .iter()
        .map(|work| {
            let mut commands = vec![work.command.clone()];
            if !report.push_session_branches {
                // Without the repository opt-in the push command refuses, so
                // naming only it would be a dead end.
                commands.push(
                    "opt in: commit `push_session_branches = true` under [delivery] in \
                     .aethyme/config.toml on the default branch"
                        .into(),
                );
            }
            StatusAdvice {
                id: "session.unpushed-commits",
                severity: work.severity,
                reason: "committed session work exists only in this worktree",
                summary: format!(
                    "session {} has {} {} on no remote; the oldest is {} old. Push the session \
                     branch so the work survives this worktree and this disk",
                    work.session_id,
                    work.unpushed_commits,
                    plural_word(work.unpushed_commits as usize, "commit", "commits"),
                    age(work.oldest_unpushed_at_ms),
                ),
                session_id: Some(work.session_id),
                queue_entry_id: None,
                evidence: vec![
                    format!("branch {}", work.branch),
                    format!("head {}", short_commit(&work.head)),
                ],
                commands,
            }
        })
        .collect::<Vec<_>>();
    if let Some(integration) = &report.integration {
        // In a verify-only repository nothing adds to integration any more,
        // but commits promoted before the switch are still work at risk, so
        // the row stays -- without recommending the mode already in force.
        let remedy = if verify_only {
            "This repository is verify-only and no longer uses integration: ship these \
             commits through a pull request, or confirm they landed and drop them"
        } else {
            "Ship it, or, if this repository delivers through pull requests, set \
             [promote] mode = \"verify-only\" so submit stops accumulating work here"
        };
        advice.push(StatusAdvice {
            id: "integration.unpublished-work",
            severity: integration.severity,
            reason: "integration holds promoted work upstream lacks",
            summary: format!(
                "{} carries {} {} {} lacks ({} on no remote at all); the oldest is {} old. \
                 {remedy}",
                integration.branch,
                integration.unpublished_commits,
                plural_word(
                    integration.unpublished_commits as usize,
                    "commit",
                    "commits"
                ),
                integration.upstream_ref,
                integration.on_no_remote,
                age(integration.oldest_unpublished_at_ms),
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![
                format!("{} {}", integration.branch, short_commit(&integration.head)),
                format!("upstream {}", integration.upstream_ref),
            ],
            commands: vec![
                format!(
                    "git log --oneline --cherry-pick --right-only {}...{}",
                    integration.upstream_ref, integration.branch
                ),
                "aethyme broker advanced ship plan --entry <promoted-entry-id>".into(),
            ],
        });
    }
    advice
}

pub(super) fn dirty_worktree_advice(agent: &AgentView, dirty: &[String]) -> StatusAdvice {
    let summary = format!(
        "session {} has {} uncommitted change(s); commit through the managed pre-commit lane before submit because only committed work integrates",
        agent.session.id,
        dirty.len()
    );
    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.dirty-worktree",
        severity: StatusAdviceSeverity::Warning,
        reason: "dirty_worktree",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: None,
        evidence: path_evidence("dirty", dirty),
        commands: vec![
            format!("git -C {worktree} status --short"),
            format!("git -C {worktree} add ..."),
            format!("git -C {worktree} commit"),
        ],
    }
}

#[derive(Debug)]
pub(super) struct GateFailure {
    pub(super) name: String,
    pub(super) tree_hash: Option<String>,
    pub(super) status: String,
    pub(super) failure_class: Option<String>,
    pub(super) cached: bool,
}

impl GateFailure {
    fn evidence(&self) -> String {
        let class = self
            .failure_class
            .as_deref()
            .map(|class| format!("/{class}"))
            .unwrap_or_default();
        let tree = self
            .tree_hash
            .as_deref()
            .map(|tree_hash| format!(" tree {}", short_commit(tree_hash)))
            .unwrap_or_default();
        format!(
            "gate {} status {}{}{}{}",
            self.name,
            self.status,
            class,
            if self.cached { " (cached)" } else { "" },
            tree,
        )
    }
}

pub(super) fn gate_failures(details_json: Option<&str>) -> Vec<GateFailure> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    let Some(gates) = details.get("gates").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    gates
        .iter()
        .filter_map(|gate| {
            let name = gate.get("gate")?.as_str()?;
            let status = gate.get("status")?.as_str()?;
            if status == "pass" {
                return None;
            }
            Some(GateFailure {
                name: name.to_string(),
                tree_hash: gate
                    .get("tree_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                status: status.to_string(),
                failure_class: gate
                    .get("failure_class")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                cached: gate
                    .get("cached")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

pub(super) fn details_string_array(details_json: Option<&str>, key: &str) -> Vec<String> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    details
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

pub(crate) fn details_string_value(details_json: Option<&str>, key: &str) -> Option<String> {
    let details_json = details_json?;
    let details = serde_json::from_str::<serde_json::Value>(details_json).ok()?;
    details.get(key)?.as_str().map(str::to_string)
}

pub(super) fn details_i64_array(details_json: Option<&str>, key: &str) -> Vec<i64> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    details
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_i64)
        .collect()
}

pub(super) fn queue_evidence(entry: &MergeQueueEntry) -> Vec<String> {
    vec![
        format!("head {}", short_commit(&entry.head_commit)),
        format!("base {}", short_commit(&entry.base_commit)),
    ]
}

pub(super) fn path_evidence(label: &str, paths: &[String]) -> Vec<String> {
    let mut evidence: Vec<String> = paths
        .iter()
        .take(5)
        .map(|path| format!("{label} {path}"))
        .collect();
    if paths.len() > evidence.len() {
        evidence.push(format!("and {} more path(s)", paths.len() - evidence.len()));
    }
    evidence
}

pub(super) fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn tail_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > limit {
        lines = lines.split_off(lines.len() - limit);
    }
    lines
}

#[derive(Clone)]
pub(super) struct RepairStepSpec {
    pub(super) component: &'static str,
    pub(super) action: &'static str,
    pub(super) command: Vec<String>,
}

pub(super) struct RepairCommandOutput {
    pub(super) success: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) fn cargo_install_bin_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("CARGO_INSTALL_ROOT") {
        return PathBuf::from(root).join("bin");
    }
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return PathBuf::from(home).join("bin");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cargo/bin")
}

pub(super) fn local_cli_repair_step_specs(
    source_root: Option<&Path>,
    install_bin: Option<&Path>,
) -> Vec<RepairStepSpec> {
    let source_root = source_root
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<integration-worktree>".into());
    let install_bin = install_bin
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<cargo-install-root>/bin".into());
    [
        ("router", "aethyme-cli", "aethyme"),
        ("engine", "aethyme-engine", "aethyme-engine-cli"),
    ]
    .into_iter()
    .flat_map(|(component, crate_name, binary)| {
        let install = RepairStepSpec {
            component,
            action: "install",
            command: vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                format!("{source_root}/packages/aethyme/rust/crates/{crate_name}"),
                "--force".into(),
                "--locked".into(),
            ],
        };
        let verify = RepairStepSpec {
            component,
            action: "verify",
            command: vec![format!("{install_bin}/{binary}"), "--version".into()],
        };
        let verify_active = RepairStepSpec {
            component,
            action: "verify-active",
            command: vec![binary.into(), "--version".into()],
        };
        [install, verify, verify_active]
    })
    .collect()
}

pub(super) fn is_active_version_verification(command: &[String]) -> bool {
    matches!(
        command,
        [binary, flag]
            if matches!(binary.as_str(), "aethyme" | "aethyme-engine-cli")
                && flag == "--version"
    )
}

pub(super) fn active_version_matches(
    command: &[String],
    stdout: &str,
    integration_head: &str,
) -> bool {
    !is_active_version_verification(command)
        || stdout.contains(&integration_head[..7.min(integration_head.len())])
}

pub(super) fn local_cli_repair_commands(
    source_root: Option<&Path>,
    install_bin: Option<&Path>,
) -> Vec<Vec<String>> {
    local_cli_repair_step_specs(source_root, install_bin)
        .into_iter()
        .map(|step| step.command)
        .collect()
}

pub(super) fn execute_version_repair_steps(
    specs: &[RepairStepSpec],
    mut run: impl FnMut(&[String]) -> Result<RepairCommandOutput, String>,
) -> Vec<VersionRepairStep> {
    specs
        .iter()
        .map(|spec| match run(&spec.command) {
            Ok(output) => VersionRepairStep {
                component: spec.component.into(),
                action: spec.action.into(),
                command: spec.command.clone(),
                success: output.success,
                exit_code: output.exit_code,
                stdout_tail: tail_lines(&output.stdout, 12),
                stderr_tail: tail_lines(&output.stderr, 12),
            },
            Err(error) => VersionRepairStep {
                component: spec.component.into(),
                action: spec.action.into(),
                command: spec.command.clone(),
                success: false,
                exit_code: None,
                stdout_tail: Vec::new(),
                stderr_tail: vec![error],
            },
        })
        .collect()
}

pub(super) fn combined_repair_tail(steps: &[VersionRepairStep], stdout: bool) -> Vec<String> {
    let mut lines = steps
        .iter()
        .flat_map(|step| {
            let source = if stdout {
                &step.stdout_tail
            } else {
                &step.stderr_tail
            };
            source
                .iter()
                .map(|line| format!("{} {}: {line}", step.component, step.action))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if lines.len() > 12 {
        lines = lines.split_off(lines.len() - 12);
    }
    lines
}

pub(crate) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'@')
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Newest mtime among the worktree's per-worktree git metadata (`index`,
/// `HEAD` under `<main>/.git/worktrees/<name>/`) — updated by any staging,
/// commit, or checkout the agent performs. Content-free by design (the
/// vendor-artifact decision allows metadata only).
pub(super) fn worktree_activity_ms(main_root: &Path, session: &Session) -> Option<i64> {
    let name = Path::new(&session.worktree_path).file_name()?;
    let meta_dir = main_root.join(".git/worktrees").join(name);
    let mut newest: Option<i64> = None;
    for file in ["index", "HEAD"] {
        if let Ok(meta) = std::fs::metadata(meta_dir.join(file))
            && let Ok(mtime) = meta.modified()
            && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
        {
            let ms = dur.as_millis() as i64;
            newest = Some(newest.map_or(ms, |n: i64| n.max(ms)));
        }
    }
    newest
}

/// True when the PID exists and is not a zombie (macOS/Linux — the v0
/// platforms). `kill -0` alone is wrong here: it succeeds on zombies,
/// and an exited-but-unreaped agent must read as dead.
///
/// Asks the kernel directly where the platform allows it -- `/proc/<pid>/stat`
/// on Linux, `proc_pidinfo` on macOS -- so the common case forks nothing. `ps`
/// is only the fallback for an answer the kernel refused to give (for example
/// another user's process). The distinction matters at fleet scale: `broker
/// status` is every session's first command and calls this once per live
/// session, so a 40-session status forked 40 `ps` processes. The repository's
/// own measurements put a 19-session `status` at 2m54s of wall time.
pub(crate) fn pid_alive(pid: i64) -> bool {
    // PID 0 is the kernel (`kernel_task` on macOS) and negative values name
    // process groups; neither is ever an agent. Answering here also keeps an
    // invalid PID off the `ps` fallback: an unprivileged `proc_pidinfo(0)` is
    // refused with EPERM rather than ESRCH, which would otherwise cost a fork.
    if pid <= 0 || i32::try_from(pid).is_err() {
        return false;
    }
    if let Some(alive) = kernel_pid_alive(pid) {
        return alive;
    }
    match Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        }
        _ => false,
    }
}

/// Whether `pid` is a live, non-zombie process, answered by the kernel without
/// forking. `None` means the kernel did not say, and the caller falls back to
/// `ps`.
///
/// On Linux the state is the field after the parenthesised comm in
/// `/proc/<pid>/stat`. The comm may itself contain spaces and parentheses, so
/// the state is taken from the *last* `)` rather than by splitting the whole
/// line on whitespace. A missing entry is left to `ps` rather than read as
/// dead, so a procfs mounted with restricted visibility cannot make a live
/// agent look gone.
#[cfg(target_os = "linux")]
pub(super) fn kernel_pid_alive(pid: i64) -> Option<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let state = stat.rsplit_once(')')?.1.trim_start().chars().next()?;
    // A zombie still has a `/proc` entry; `kill -0` would report it alive.
    Some(state != 'Z' && state != 'X')
}

/// On macOS, `proc_pidinfo(PROC_PIDTBSDINFO)` is the same data `ps` reads,
/// without the fork. `ESRCH` is a definite "no such process", and is also what
/// the kernel returns for an exited-but-unreaped child (a zombie that `ps`
/// still lists with state `Z`), so zombies read as dead here. Any other
/// failure (`EPERM` for another user's process) is left to `ps`. The `SZOMB`
/// check covers a bsdinfo record that does describe a zombie.
#[cfg(target_os = "macos")]
pub(super) fn kernel_pid_alive(pid: i64) -> Option<bool> {
    // From <sys/proc.h>: a process that has exited but not been reaped.
    const SZOMB: u32 = 5;
    let pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, and
    // proc_pidinfo writes at most `size` bytes into it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        let gone =
            written <= 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        return gone.then_some(false);
    }
    // SAFETY: the buffer was zero-initialized and then fully written, and
    // proc_bsdinfo is plain integers and byte arrays, valid for any bits.
    let info = unsafe { info.assume_init() };
    Some(info.pbi_status != SZOMB)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn kernel_pid_alive(_pid: i64) -> Option<bool> {
    None
}

/// Task → worktree/branch slug: lowercase alphanumerics with dashes,
/// capped at 40 chars, never empty.
pub(crate) fn slugify(task: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in task.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "task".into() } else { slug }
}

/// Cargo defaults for the worktrees beneath a worktree root.
///
/// A session worktree is built a handful of times and then deleted, which
/// makes two of cargo's defaults pure cost. Incremental state exists to make
/// the *next* build of a tree cheaper and measured at 35% of a `target/` here;
/// full debug info is another large share, spent on symbols that a broker
/// session has no debugger attached to read. Gates already decline both
/// through `gates.toml`, against one capped shared cache. The worktree the
/// agent runs its own `cargo test` in had no equivalent, and that asymmetry is
/// where the disk went -- tens of gigabytes of rebuild state for trees that
/// are never rebuilt.
///
/// Placed one directory above the worktrees rather than inside one, for two
/// reasons: it is then never an untracked file in anyone's `git status`, and a
/// repository that ships its own `.cargo/config.toml` sits closer to the build
/// and keeps precedence. Anything explicit still wins -- a `CARGO_PROFILE_*`
/// environment variable, or the gates' own `CARGO_TARGET_DIR` discipline.
pub(crate) const WORKTREE_CARGO_CONFIG: &str = "\
# Written once by the Aethyme broker, for the session worktrees beside it.
#
# These trees are built a few times and then reclaimed, so they do not pay for
# rebuild state nothing will rebuild from, or for debug info nothing reads.
# Backtraces keep their file and line numbers.
#
# Delete this file to build them with cargo's defaults instead; the broker
# writes it only when it is absent.

[build]
incremental = false

[profile.dev]
debug = \"line-tables-only\"
";

/// Write [`WORKTREE_CARGO_CONFIG`] beneath `root`, unless something is already
/// there. An operator who edited or emptied it has said what they want.
pub(super) fn write_worktree_build_defaults(root: &Path) -> std::io::Result<()> {
    let directory = root.join(".cargo");
    let config = directory.join("config.toml");
    if config.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(".config.{}.{}.tmp", std::process::id(), now_ms()));
    std::fs::write(&temporary, WORKTREE_CARGO_CONFIG)?;
    // Rename so a worktree created concurrently never reads a half-written
    // config and builds with a truncated profile.
    std::fs::rename(&temporary, &config).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}
