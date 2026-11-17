//! Deterministic removal of disposable session checkouts (#588).
//!
//! The unattended sweep already reclaims build caches. This removes the
//! checkout itself, without an operator confirming a plan, but only when four
//! proofs hold, each re-checked under the GC lock immediately before the
//! directory goes:
//!
//! 1. **Sessions.** Every broker session that names the worktree is closed.
//!    A session still open in any state -- active, idle, stale, exited --
//!    keeps it.
//! 2. **No use.** No process has a file or working directory open in it, no
//!    lease that still holds covers it (a lease whose holder is gone past its
//!    grace does not hold, one of unknown liveness does), and no gate runs.
//! 3. **Clean.** No tracked, staged or untracked change, no stash made on the
//!    branch, no rebase, merge, cherry-pick, revert or bisect in progress, and
//!    no protected ignored content. Only recognized or configured build-output
//!    directories are disposable; strict mode requires no ignored residue.
//! 4. **Contained.** Every commit is in the *fetched remote* default branch,
//!    by ancestry or by the verbatim content/patch proof. A commit that only
//!    reached local integration or a local branch is not contained.
//!
//! Anything that cannot be decided -- a listing that failed, a proof the
//! budget cut, a missing remote -- keeps the checkout and says why. Removal
//! never rests on the absence of evidence. Removal deletes the worktree and
//! its session branch and records `broker.cleanup.auto_removed` with the
//! proof; there is nothing to restore, because by proof the commits are on the
//! remote.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::types::{Session, SessionCleanupState, SessionOrigin};
use crate::{Broker, BrokerOpError, GitRepo};

mod fast_judging;
use fast_judging::{CheckKind, timed};

/// Fallback classification for ignored files and artifact-root symlinks.
/// Each directory is recognised by its exact name and the
/// evidence that proves what it is (#588). A name alone proves nothing:
/// `build/` and `dist/` routinely hold source, assets or data that a
/// repository happens to ignore, so they count only beside the manifest that
/// produces them.
///
/// | Directory | Counts only when |
/// | --- | --- |
/// | `node_modules` | always (package manager output) |
/// | `.pnpm-store` | nonempty package-manager store (shared artifact witness) |
/// | `target` | a sibling `Cargo.toml`, or `CACHEDIR.TAG` inside |
/// | `.venv` | `pyvenv.cfg` inside |
/// | `build`, `dist` | a sibling `package.json`, `setup.py`, `pyproject.toml` or `CMakeLists.txt` |
/// | `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache` | always (tool caches) |
/// | `.hypothesis` | only Hypothesis's own entries (`examples`, `unicode_data`, ...), and no protected name beneath |
/// | `playwright-report` | only `index.html`, `data/` and `trace/`, and no protected name beneath |
/// | `test-results` | Playwright's `.last-run.json` status file inside, and no protected name beneath |
/// | any other name | a `CACHEDIR.TAG` with the standard signature inside, and no protected name (`.env*`, keys, credentials, databases, `.aethyme`) anywhere beneath it |
///
/// Names are compared as whole path components: `target-notes/`,
/// `my_build/` and `targets.json` are not build output.
pub const BUILT_IN_REGENERABLE: &[&str] = &[
    "node_modules",
    ".pnpm-store",
    "target",
    ".venv",
    "build",
    "dist",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".hypothesis",
    "playwright-report",
    "test-results",
];

/// Manifests whose presence beside `build/` or `dist/` proves it is output.
const BUILD_MANIFESTS: &[&str] = &[
    "package.json",
    "setup.py",
    "pyproject.toml",
    "CMakeLists.txt",
];

/// Paths a configured `regenerable` glob must never match: secrets, data and
/// ordinary root files. A glob that matches any of them is rejected at load.
const GLOB_PROBES: &[&str] = &[
    ".env",
    ".env.local",
    "secrets.key",
    "cert.pem",
    "data.sqlite",
    "app.db",
    "notes.txt",
    "README.md",
    ".aethyme",
];

/// Meta key holding the last auto-cleanup report, which `status` and
/// `gc plan` show.
pub(crate) const AUTO_CLEANUP_LAST_REPORT_KEY: &str = "auto_cleanup.last_report";
const AUTO_CLEANUP_CURSOR_KEY: &str = "auto_cleanup.cursor";
const AUTO_CLEANUP_CURSOR_VERSION: u32 = 3;
const CLEANUP_COHORT_SCAN_PAGE_SIZE: usize = 16;
/// Sessions per second a paced cohort-index lap resolves at most in the
/// detached worker. Each one canonicalizes a checkout path and looks up its
/// Git directory; a full lap over several thousand sessions then spreads
/// over tens of seconds instead of one burst of filesystem and SQLite work.
const CLEANUP_COHORT_SCAN_ROWS_PER_SECOND: u32 = 256;
/// Where a paced cohort-index refresh stopped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CleanupCohortScanProgress {
    /// The index state after the last page, `None` when it has none.
    pub(crate) state: Option<crate::store::cleanup_cohort::CleanupCohortScanState>,
    /// The unattended pacer stopped the lap for critical memory pressure.
    pub(crate) pressure_stopped: bool,
}
/// Paging under capacity pressure never selects more checkouts for removal
/// in one pass than a single page could before (32 groups), so removal is
/// no faster per pass than it was; it only stops spending passes on pages
/// of checkouts it keeps.
const AUTO_CLEANUP_MAX_SELECTED_PER_PASS: usize = 32;
const CLEANUP_COHORT_UNBOUNDED_PAGE_CAP: usize = 128;
/// The background compact lane admits an archive attempt only when it can
/// reserve this much time after the alias snapshot and ownership proof.
pub(crate) const AUTO_COMPACTION_MIN_BUDGET: Duration = Duration::from_secs(60);
const SESSION_RETIREMENT_CURSOR_KEY: &str = "auto_cleanup.session_retirement_cursor";
const SESSION_RETIREMENT_CURSOR_VERSION: u32 = 1;
const SESSION_RETIREMENT_PAGE_SIZE: usize = 128;
/// A landed proof that alone exhausts a pass this long is passed over for the
/// rest of the lap instead of holding the retirement cursor.
const LANDED_SEARCH_HOLD_LIMIT: Duration = Duration::from_secs(10);
/// Meta key naming the session a pass left the retirement cursor on because
/// its landed proof ran out of budget. The next pass judges it once more and
/// then moves on whatever the answer, so one session never holds every later
/// session back for more than one pass.
const SESSION_RETIREMENT_BUDGET_HOLD_KEY: &str = "auto_cleanup.session_retirement_budget_hold";
/// Meta key of [`NotDueMemo`]: when each session the abandonment window kept
/// for recent activity can first be due.
const SESSION_RETIREMENT_NOT_DUE_KEY: &str = "auto_cleanup.session_retirement_not_due";
/// Entries the not-due memo keeps at most (the nearest due times win).
const SESSION_RETIREMENT_NOT_DUE_MAX: usize = 4096;

/// When each open session the abandonment window kept for recent activity
/// can first be due again: its newest activity (session row, checkout Git
/// metadata) plus the window. Activity only moves later, so until that time
/// the same judgement would keep it again; a pass skips it
/// (`retirement_not_due_memo_skips`) instead of re-reading its checkout on
/// every command. Skipping only delays a close: it never closes anything,
/// and the landed lane is still asked. A changed window discards the memo.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct NotDueMemo {
    window_ms: i64,
    sessions: std::collections::BTreeMap<i64, i64>,
}

/// A not-due time is at most one window away: Git metadata dated in the
/// future (a skewed clock, a restored checkout) or one that later vanishes
/// can delay a close by one window at most, never longer.
fn clamp_not_due(due_at: i64, now: i64, window_ms: i64) -> i64 {
    due_at.min(now.saturating_add(window_ms.max(0)))
}

impl NotDueMemo {
    pub(crate) fn load(broker: &Broker, window_ms: i64, now: i64) -> Self {
        let mut memo = broker
            .store_ref()
            .meta_get(SESSION_RETIREMENT_NOT_DUE_KEY)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .filter(|memo| memo.window_ms == window_ms)
            .unwrap_or_else(|| Self {
                window_ms,
                ..Self::default()
            });
        memo.sessions.retain(|_, due_at| *due_at > now);
        memo
    }

    pub(crate) fn not_due(&self, session_id: i64, now: i64) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|due_at| *due_at > now)
    }

    fn record(&mut self, broker: &Broker, session_id: i64, due_at: i64, now: i64) {
        let due_at = clamp_not_due(due_at, now, self.window_ms);
        if due_at <= now || self.sessions.get(&session_id) == Some(&due_at) {
            return;
        }
        self.sessions.insert(session_id, due_at);
        while self.sessions.len() > SESSION_RETIREMENT_NOT_DUE_MAX {
            let Some((&latest, _)) = self.sessions.iter().max_by_key(|(_, due_at)| **due_at) else {
                break;
            };
            self.sessions.remove(&latest);
        }
        crate::phase_telemetry::add("retirement_not_due_memo_recorded", Duration::ZERO);
        self.persist(broker);
    }

    fn forget(&mut self, broker: &Broker, session_id: i64) {
        if self.sessions.remove(&session_id).is_some() {
            self.persist(broker);
        }
    }

    fn persist(&self, broker: &Broker) {
        let value = serde_json::to_string(self).unwrap_or_default();
        crate::warn_unrecorded(
            "remember when retirement candidates are due",
            broker
                .store_ref()
                .meta_set(SESSION_RETIREMENT_NOT_DUE_KEY, &value),
        );
    }
}
/// Bound discovery of ignored roots before whole-checkout cleanup considers
/// their existing policy classifications.
const IGNORED_PATH_LISTING_BUDGET: Duration = Duration::from_secs(15);

/// Durable scheduling state for closed-checkout cleanup. It grants no
/// authority to remove a checkout; every candidate still passes the regular
/// ownership, lifecycle, cleanliness and process-use proofs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AutoCleanupCursor {
    version: u32,
    cohort_generation: i64,
    after_session_id: i64,
    high_water_session_id: i64,
}

/// Durable progress observed by the whole-checkout recovery worker. The path
/// inventory, checkout-group planner and abandoned-session pass each advance
/// a separate cursor, so removal count alone is not a complete progress test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AutoCleanupProgressMarker {
    cohort_generation: i64,
    alias_refresh_generation: i64,
    alias_progress_sequence: i64,
    group_cursor_generation: i64,
    group_cursor_after_session_id: i64,
    group_cursor_high_water_session_id: i64,
    retirement_cursor_after_session_id: i64,
    retirement_cursor_high_water_session_id: i64,
}

impl Default for AutoCleanupCursor {
    fn default() -> Self {
        Self {
            version: AUTO_CLEANUP_CURSOR_VERSION,
            cohort_generation: 0,
            after_session_id: 0,
            high_water_session_id: 0,
        }
    }
}

fn decode_auto_cleanup_cursor(value: Option<&str>) -> AutoCleanupCursor {
    value
        .and_then(|value| serde_json::from_str::<AutoCleanupCursor>(value).ok())
        .filter(|cursor| {
            cursor.version == AUTO_CLEANUP_CURSOR_VERSION
                && cursor.cohort_generation >= 0
                && cursor.after_session_id >= 0
                && cursor.high_water_session_id >= cursor.after_session_id
        })
        .unwrap_or_default()
}

/// Durable progress for the bounded open-session retirement lane. It never
/// authorizes closure: each row is reloaded and judged against the current
/// activity and holder evidence before the state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SessionRetirementCursor {
    version: u32,
    after_session_id: i64,
    high_water_session_id: i64,
}

impl Default for SessionRetirementCursor {
    fn default() -> Self {
        Self {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: 0,
        }
    }
}

fn decode_session_retirement_cursor(value: Option<&str>) -> SessionRetirementCursor {
    value
        .and_then(|value| serde_json::from_str::<SessionRetirementCursor>(value).ok())
        .filter(|cursor| {
            cursor.version == SESSION_RETIREMENT_CURSOR_VERSION
                && cursor.after_session_id >= 0
                && cursor.high_water_session_id >= cursor.after_session_id
        })
        .unwrap_or_default()
}

/// Reuse one process-table observation during planning, but take it lazily:
/// most closed checkouts have no retained lease rows to classify. Apply gives
/// each candidate its own fresh instance under the lifecycle claim.
#[derive(Default)]
struct LeaseProcessTableSnapshot {
    snapshot: Option<Option<crate::session_holder::ProcessTable>>,
}

impl LeaseProcessTableSnapshot {
    fn get_or_snapshot(&mut self) -> Option<&crate::session_holder::ProcessTable> {
        self.snapshot
            .get_or_insert_with(crate::session_holder::ProcessTable::snapshot)
            .as_ref()
    }
}

struct AutoCleanupVerdictOptions<'a> {
    deep: bool,
    deadline: Option<Instant>,
    selected: Option<&'a ContainmentProof>,
    lease_process_table: &'a mut LeaseProcessTableSnapshot,
    gate_witnesses: &'a mut GateWitnessSnapshot,
    /// Selection only: the open sessions and their resolved paths the pass
    /// already read, instead of resolving every open session's path again
    /// for each candidate. Apply passes `None` and reads them fresh.
    live_index: Option<(&'a [Session], &'a [ResolvedWorktreePath])>,
}

/// Run-directory entries the per-session gate inventory may read before it
/// reports itself unknown, which keeps the checkout.
const SESSION_GATE_INVENTORY_ENTRY_BUDGET: usize = 4096;

/// The repository's running-gate witnesses, read lazily like the process
/// table above. Selection shares one across its pass, so the gate pidfiles and
/// owner locks are read once per pass rather than once per checkout;
/// revalidation gives each checkout its own instance under the lifecycle
/// claim, so a removal is only ever authorized by a read made immediately
/// before it.
#[derive(Default)]
struct GateWitnessSnapshot {
    witnesses: Option<Vec<ResolvedGateWitness>>,
    /// The strict per-session inventory, read with the witnesses' freshness:
    /// once per selection pass, fresh for each checkout at revalidation. The
    /// inner `None` is an inventory that could not complete (unknown).
    session_inventory: Option<Option<crate::gates::CheckedSessionGates>>,
    stats: AutoCleanupGateChecks,
    elapsed: Duration,
}

/// A witness with its recorded checkout resolved once per read.
struct ResolvedGateWitness {
    witness: crate::gates::RunningGateWitness,
    checkout: Option<ResolvedWorktreePath>,
}

/// Whom one running-gate witness keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateAttribution {
    /// It names one of the checkout's own sessions.
    OwnSession,
    /// Its recorded checkout is the judged one, inside it or contains it.
    Path,
    /// Its recorded checkout is positively some other checkout.
    Elsewhere,
    /// Nothing identifies where it runs: it keeps every checkout.
    Unknown,
}

/// Attribute one witness to the judged checkout. Only a recorded checkout
/// that resolves disjoint from the candidate, with neither path nor Git
/// directory overlapping, sets a gate aside; everything else keeps it.
fn attribute_gate_witness(
    witness: &crate::gates::RunningGateWitness,
    resolved: Option<&ResolvedWorktreePath>,
    candidate: &ResolvedWorktreePath,
    cohort: &[i64],
) -> GateAttribution {
    if witness
        .session_id
        .is_some_and(|session| cohort.contains(&session))
    {
        return GateAttribution::OwnSession;
    }
    match (&witness.checkout, resolved) {
        (crate::gates::GateWitnessCheckout::Recorded(_), Some(resolved)) => {
            match live_worktree_overlap(candidate, resolved) {
                LiveOverlapEvidence::Overlap => GateAttribution::Path,
                LiveOverlapEvidence::Unknown => GateAttribution::Unknown,
                LiveOverlapEvidence::Disjoint => GateAttribution::Elsewhere,
            }
        }
        _ => GateAttribution::Unknown,
    }
}

impl GateWitnessSnapshot {
    /// Why a running gate keeps `candidate`, or `None` when every running
    /// gate is positively identified as running somewhere else.
    ///
    /// A gate keeps the checkout when one of its own sessions (`cohort`)
    /// runs it, when its recorded checkout overlaps the candidate, or when
    /// nothing identifies its checkout: an older binary's record, an
    /// unreadable or malformed one, or a per-session inventory that could not
    /// complete. Only the last kind can keep a checkout no gate is using, and
    /// it keeps every candidate exactly as the repository-wide check did.
    fn keep_reason(
        &mut self,
        main_root: &Path,
        candidate: &ResolvedWorktreePath,
        cohort: &[i64],
    ) -> Option<String> {
        let started = Instant::now();
        let reason = self.judge(main_root, candidate, cohort);
        let elapsed = started.elapsed();
        self.elapsed += elapsed;
        self.stats.elapsed_ms = duration_ms(Some(self.elapsed));
        crate::phase_telemetry::add("auto_cleanup_gate_check", elapsed);
        reason
    }

    fn judge(
        &mut self,
        main_root: &Path,
        candidate: &ResolvedWorktreePath,
        cohort: &[i64],
    ) -> Option<String> {
        self.stats.checks = self.stats.checks.saturating_add(1);
        if self.witnesses.is_none() {
            self.stats.scans = self.stats.scans.saturating_add(1);
            let read = crate::gates::running_gate_witnesses_until(main_root, || false, None)
                .expect("unbounded gate witness scan completes");
            self.witnesses = Some(
                read.into_iter()
                    .map(|witness| {
                        let resolved = match &witness.checkout {
                            crate::gates::GateWitnessCheckout::Recorded(path) => {
                                Some(ResolvedWorktreePath::resolve(path))
                            }
                            crate::gates::GateWitnessCheckout::Unknown => None,
                        };
                        ResolvedGateWitness {
                            witness,
                            checkout: resolved,
                        }
                    })
                    .collect(),
            );
        }
        let witnesses = self.witnesses.as_deref().unwrap_or_default();
        let mut own_session = None;
        let mut path = None;
        let mut unknown = None;
        for ResolvedGateWitness {
            witness,
            checkout: resolved,
        } in witnesses
        {
            self.stats.gates_seen = self.stats.gates_seen.saturating_add(1);
            match attribute_gate_witness(witness, resolved.as_ref(), candidate, cohort) {
                GateAttribution::OwnSession => {
                    self.stats.attributed_here = self.stats.attributed_here.saturating_add(1);
                    crate::phase_telemetry::add("auto_cleanup_gate_own_session", Duration::ZERO);
                    own_session = own_session.or_else(|| Some(witness.evidence.clone()));
                }
                GateAttribution::Path => {
                    self.stats.attributed_here = self.stats.attributed_here.saturating_add(1);
                    crate::phase_telemetry::add("auto_cleanup_gate_path", Duration::ZERO);
                    path = path.or_else(|| Some(witness.evidence.clone()));
                }
                GateAttribution::Elsewhere => {
                    self.stats.attributed_elsewhere =
                        self.stats.attributed_elsewhere.saturating_add(1);
                    crate::phase_telemetry::add("auto_cleanup_gate_elsewhere", Duration::ZERO);
                }
                GateAttribution::Unknown => {
                    self.stats.unknown_owner = self.stats.unknown_owner.saturating_add(1);
                    crate::phase_telemetry::add("auto_cleanup_gate_unknown_owner", Duration::ZERO);
                    unknown = unknown.or_else(|| Some(witness.evidence.clone()));
                }
            }
        }
        if let Some(evidence) = own_session {
            return Some(format!("{GATE_RUNNING_OWN_SESSION}: {evidence}"));
        }
        // The strict per-session inventory guarded retirement uses: it also
        // counts a gate whose leader exited while its process group lives on,
        // and anything it cannot read or verify is unknown, never "idle".
        let inventory = self.session_inventory.get_or_insert_with(|| {
            crate::gates::checked_session_gate_inventory_all_until(
                main_root,
                || false,
                SESSION_GATE_INVENTORY_ENTRY_BUDGET,
            )
        });
        let mut inventory_unknown = false;
        for &session in cohort {
            let gates = inventory
                .as_ref()
                .and_then(|inventory| inventory.session(session));
            match gates {
                Some(gates) if gates.is_empty() => {}
                Some(gates) => {
                    return Some(format!(
                        "{GATE_RUNNING_OWN_SESSION}: session {session} is running gate {}",
                        gates.join(", ")
                    ));
                }
                None => {
                    inventory_unknown = true;
                    unknown = unknown.or_else(|| {
                        Some(format!(
                            "the gate records of session {session} could not be verified"
                        ))
                    });
                }
            }
        }
        if inventory_unknown {
            self.stats.inventory_unknown = self.stats.inventory_unknown.saturating_add(1);
            crate::phase_telemetry::add("auto_cleanup_gate_inventory_unknown", Duration::ZERO);
        }
        if let Some(evidence) = path {
            return Some(format!("{GATE_RUNNING_PATH}: {evidence}"));
        }
        unknown.map(|evidence| format!("{GATE_RUNNING_UNKNOWN_OWNER}: {evidence}"))
    }
}

fn checkout_group_cursor_session_id(group: &AutoCleanupCheckoutGroup) -> i64 {
    group.cursor_session_id
}

/// Sort candidates by their minimum session ID and return the unvisited
/// suffix of the current lap. The fixed high-water mark keeps new sessions
/// from extending a lap forever; a completed lap starts again from the
/// beginning and includes arrivals since the prior snapshot.
#[cfg(test)]
fn auto_cleanup_lap_range<T>(
    candidates: &mut [T],
    cursor: &mut AutoCleanupCursor,
    session_id: impl Fn(&T) -> i64,
) -> std::ops::Range<usize> {
    candidates.sort_by_key(|candidate| session_id(candidate));
    let current_high_water = candidates.last().map(&session_id).unwrap_or(0);
    let current_lap_has_work = cursor.high_water_session_id > cursor.after_session_id
        && candidates.iter().any(|candidate| {
            let id = session_id(candidate);
            id > cursor.after_session_id && id <= cursor.high_water_session_id
        });
    if !current_lap_has_work {
        cursor.after_session_id = 0;
        cursor.high_water_session_id = current_high_water;
    }

    let start = candidates
        .iter()
        .position(|candidate| {
            let id = session_id(candidate);
            id > cursor.after_session_id && id <= cursor.high_water_session_id
        })
        .unwrap_or(candidates.len());
    let end = candidates
        .partition_point(|candidate| session_id(candidate) <= cursor.high_water_session_id);
    start..end
}

/// `[cleanup] abandoned_operation_after_hours`: a merge, rebase, cherry-pick,
/// revert or bisect left in progress in a closed session's checkout longer
/// than this is archived (state, index and changed files) and the checkout
/// retired.
pub(crate) const ABANDONED_OPERATION_CONFIG_KEY: &str = "abandoned_operation_after_hours";
/// The default of [`ABANDONED_OPERATION_CONFIG_KEY`].
pub(crate) const DEFAULT_ABANDONED_OPERATION_AFTER_HOURS: i64 = 24;
const MAX_ABANDONED_OPERATION_AFTER_HOURS: i64 = 24 * 365;

/// Parse [`ABANDONED_OPERATION_CONFIG_KEY`]; absent is the default.
pub(crate) fn parse_abandoned_operation_after_hours(
    value: Option<&toml::Value>,
) -> Result<i64, String> {
    match value {
        None => Ok(DEFAULT_ABANDONED_OPERATION_AFTER_HOURS),
        Some(toml::Value::Integer(hours))
            if (1..=MAX_ABANDONED_OPERATION_AFTER_HOURS).contains(hours) =>
        {
            Ok(*hours)
        }
        Some(_) => Err(format!(
            "[cleanup] {ABANDONED_OPERATION_CONFIG_KEY} must be a whole number of hours between 1 and {MAX_ABANDONED_OPERATION_AFTER_HOURS}"
        )),
    }
}

/// [`ABANDONED_OPERATION_CONFIG_KEY`] of the primary checkout's config, in
/// milliseconds. An unreadable or invalid value keeps the default (the
/// cleanup policy loader reports it).
pub(crate) fn abandoned_operation_after_ms(main_root: &Path) -> i64 {
    let hours = (|| -> Result<i64, String> {
        let Some((path, text)) =
            aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
                .map_err(|error| error.to_string())?
        else {
            return parse_abandoned_operation_after_hours(None);
        };
        let root: toml::Value = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        parse_abandoned_operation_after_hours(
            root.get("cleanup")
                .and_then(|cleanup| cleanup.get(ABANDONED_OPERATION_CONFIG_KEY)),
        )
    })()
    .unwrap_or(DEFAULT_ABANDONED_OPERATION_AFTER_HOURS);
    hours.saturating_mul(3_600_000)
}

/// `[cleanup]` in `.aethyme/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCleanupPolicy {
    /// On by default. `false` turns automatic checkout removal off.
    pub auto_remove: bool,
    /// Require a checkout with no ignored leftovers. Otherwise only recognized
    /// or configured build-output directories are disposable.
    pub strict: bool,
    /// Allow the background artifact sweep to reclaim arbitrary Git-ignored
    /// directory roots in retained closed checkouts. Defaults off so existing
    /// checkouts keep the whole-worktree grace unless an operator enables this
    /// in-place cleanup lane after its host protocol is ready.
    pub ignored_directory_sweep: bool,
    /// Extra globs, relative to the worktree root, that count as regenerable
    /// in addition to [`BUILT_IN_REGENERABLE`].
    pub regenerable: Vec<String>,
    /// Globs matched against the worktree path, or its final component, that
    /// are never removed automatically.
    pub keep: Vec<String>,
}

impl Default for AutoCleanupPolicy {
    fn default() -> Self {
        Self {
            auto_remove: true,
            strict: false,
            ignored_directory_sweep: false,
            regenerable: Vec::new(),
            keep: Vec::new(),
        }
    }
}

impl AutoCleanupPolicy {
    /// Read `[cleanup]`. A missing file or table is the default policy; an
    /// unreadable one is an error, which the sweep turns into "removal off"
    /// rather than guessing what the operator meant.
    pub fn load(main_root: &Path) -> Result<Self, String> {
        // `.aethyme/config.toml`, `.yaml` or `.yml` (#1223), as TOML text.
        let Some((path, text)) =
            aethyme_config::read_config_toml(&main_root.join(aethyme_config::CONFIG_DIR), "config")
                .map_err(|error| error.to_string())?
        else {
            return Ok(Self::default());
        };
        let root: toml::Value = toml::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let Some(table) = root.get("cleanup") else {
            return Ok(Self::default());
        };
        let table = table
            .as_table()
            .ok_or_else(|| "[cleanup] must be a table".to_string())?;
        let mut policy = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "auto_remove" => {
                    policy.auto_remove = value
                        .as_bool()
                        .ok_or_else(|| "[cleanup] auto_remove must be true or false".to_string())?;
                }
                "strict" => {
                    policy.strict = value
                        .as_bool()
                        .ok_or_else(|| "[cleanup] strict must be true or false".to_string())?;
                }
                "ignored_directory_sweep" => {
                    policy.ignored_directory_sweep = value.as_bool().ok_or_else(|| {
                        "[cleanup] ignored_directory_sweep must be true or false".to_string()
                    })?;
                }
                "regenerable" | "keep" => {
                    let list = value
                        .as_array()
                        .and_then(|items| {
                            items
                                .iter()
                                .map(|item| item.as_str().map(str::to_string))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| format!("[cleanup] {key} must be a list of strings"))?;
                    for glob in &list {
                        if key == "regenerable" {
                            validate_regenerable_glob(glob)?;
                        } else {
                            globset::Glob::new(glob).map_err(|error| {
                                format!("[cleanup] {key} glob {glob:?}: {error}")
                            })?;
                        }
                    }
                    if key == "regenerable" {
                        policy.regenerable = list;
                    } else {
                        policy.keep = list;
                    }
                }
                "unattended_unlinks_per_second" => {} // Parsed by gc::unattended_pacing.
                ABANDONED_OPERATION_CONFIG_KEY => {
                    // Read by the checkout retirement lane; validated here.
                    parse_abandoned_operation_after_hours(Some(value))?;
                }
                other => return Err(format!("[cleanup] has no key {other:?}")),
            }
        }
        Ok(policy)
    }

    /// Strict mode (or an unreadable policy) leaves ignored output to an
    /// explicit reviewed cleanup instead of a background artifact sweep.
    pub(crate) fn artifact_sweep_allowed(main_root: &Path) -> bool {
        Self::load(main_root).is_ok_and(|policy| !policy.strict)
    }

    fn glob_set(globs: &[String]) -> globset::GlobSet {
        let mut builder = globset::GlobSetBuilder::new();
        for glob in globs {
            if let Ok(glob) = globset::Glob::new(glob) {
                builder.add(glob);
            }
        }
        builder
            .build()
            .unwrap_or_else(|_| globset::GlobSet::empty())
    }

    /// Whether `path` (or its final component) matches a `keep` pin.
    pub fn is_pinned(&self, worktree: &Path) -> bool {
        if self.keep.is_empty() {
            return false;
        }
        let set = Self::glob_set(&self.keep);
        set.is_match(worktree)
            || worktree
                .file_name()
                .is_some_and(|name| set.is_match(Path::new(name)))
    }

    /// Whether a configured `regenerable` glob names this directory (or
    /// link), relative to the worktree root. `*` may cross `/` here, as it
    /// always has for directory roots.
    fn configured_regenerable_dir(&self, relative_dir: &str) -> bool {
        !self.regenerable.is_empty() && Self::glob_set(&self.regenerable).is_match(relative_dir)
    }

    /// Whether a configured `regenerable` glob names this standalone regular
    /// file by its whole path relative to the worktree root. Stricter than
    /// the directory match so a pattern written for directories does not
    /// reach files in unrelated places: `*`, `?` and `[...]` stay within one
    /// path component, and a pattern containing `**` never names a file.
    fn configured_regenerable_file(&self, relative_file: &str) -> bool {
        !self.regenerable.is_empty() && file_glob_set(&self.regenerable).is_match(relative_file)
    }
}

/// The matcher [`AutoCleanupPolicy::configured_regenerable_file`] uses.
fn file_glob_set(globs: &[String]) -> globset::GlobSet {
    let mut builder = globset::GlobSetBuilder::new();
    for glob in globs.iter().filter(|glob| !glob.contains("**")) {
        if let Ok(glob) = globset::GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
        {
            builder.add(glob);
        }
    }
    builder
        .build()
        .unwrap_or_else(|_| globset::GlobSet::empty())
}

/// A `[cleanup] regenerable` glob is valid when it compiles and matches none
/// of [`GLOB_PROBES`], under either the directory or the file rule.
fn validate_regenerable_glob(glob: &str) -> Result<(), String> {
    let matcher = globset::Glob::new(glob)
        .map_err(|error| format!("[cleanup] regenerable glob {glob:?}: {error}"))?
        .compile_matcher();
    if let Some(probe) = GLOB_PROBES.iter().find(|probe| matcher.is_match(probe)) {
        return Err(format!(
            "[cleanup] regenerable glob {glob:?} matches {probe:?}; it may only name regenerable output"
        ));
    }
    Ok(())
}

/// Whether any component of `relative` is a protected name
/// ([`protected_cache_entry`]). Judged by name only; nothing is opened.
fn protected_ignored_path(relative: &str) -> bool {
    relative
        .trim_end_matches('/')
        .split('/')
        .any(protected_cache_entry)
}

/// Why a protected standalone ignored entry stays even though a configured
/// `regenerable` glob names it.
const PROTECTED_NAME_REASON: &str =
    "has a protected name, which [cleanup] regenerable never releases";

/// The `.aethyme/config.toml` line that would release exactly this
/// standalone ignored file, or why no line can.
pub(crate) fn regenerable_file_config_line(relative_file: &str) -> Result<String, String> {
    if protected_ignored_path(relative_file) {
        return Err(
            "protected name: [cleanup] regenerable never releases it; review and delete it, or keep the checkout"
                .into(),
        );
    }
    const UNNAMEABLE: &str = "this path cannot be named by a [cleanup] regenerable pattern (it contains a backslash or a control character); review and delete it, or keep the checkout";
    // `globset::escape` leaves `\` as an escape and control characters make
    // the line unreadable, so neither can be named exactly.
    if relative_file
        .chars()
        .any(|character| character == '\\' || character.is_control())
    {
        return Err(UNNAMEABLE.into());
    }
    let glob = globset::escape(relative_file);
    if globset::Glob::new(&glob).is_err()
        || !file_glob_set(std::slice::from_ref(&glob)).is_match(relative_file)
    {
        return Err(UNNAMEABLE.into());
    }
    validate_regenerable_glob(&glob).map_err(|_| {
        "[cleanup] regenerable refuses a pattern naming it, since that pattern matches a secret or ordinary root-file name; review and delete it, or keep the checkout".to_string()
    })?;
    Ok(format!("[cleanup] regenerable = [{}]", toml_string(&glob)))
}

/// Whether `relative_dir` (no trailing slash) is a recognised build-output
/// directory of `worktree`, by [`BUILT_IN_REGENERABLE`]'s evidence rules or a
/// configured glob. `Some(true)` marks a Cargo target directory, where the
/// compiler's own `.aethyme`-named fingerprints are not Aethyme state.
fn regenerable_dir(
    worktree: &Path,
    relative_dir: &str,
    policy: &AutoCleanupPolicy,
) -> Option<bool> {
    let path = worktree.join(relative_dir);
    let name = Path::new(relative_dir).file_name()?.to_str()?;
    let parent = path.parent()?;
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_dir() && !metadata.file_type().is_symlink() {
        return None;
    }
    // Never probe an artifact-root symlink's target for a witness. Known
    // dependency/cache links themselves are disposable; their targets are not.
    if metadata.file_type().is_symlink() {
        return match name {
            "node_modules" | ".pnpm-store" | ".venv" | "__pycache__" | ".pytest_cache"
            | ".mypy_cache" | ".ruff_cache" => Some(false),
            _ if policy.configured_regenerable_dir(relative_dir) => Some(false),
            _ => None,
        };
    }
    let recognised = match name {
        "node_modules" | "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" => {
            Some(false)
        }
        ".hypothesis" if hypothesis_database(&path) => Some(false),
        "playwright-report" if playwright_html_report(&path) => Some(false),
        "test-results" if playwright_output_directory(&path) => Some(false),
        "target" if parent.join("Cargo.toml").is_file() || path.join("CACHEDIR.TAG").is_file() => {
            Some(true)
        }
        ".venv" if path.join("pyvenv.cfg").is_file() => Some(false),
        "build" | "dist"
            if BUILD_MANIFESTS
                .iter()
                .any(|manifest| parent.join(manifest).is_file()) =>
        {
            Some(false)
        }
        // Share the artifact-sweep catalog rather than silently leaving a
        // recognized package-manager store behind in whole-checkout cleanup.
        // The recursive safety scan still runs after this classification.
        _ => crate::artifact_catalog::unattended_witness(name, &[])
            .filter(|witness| witness.confirms(&path))
            .map(|_| false),
    };
    recognised
        .or_else(|| tagged_cache_without_protected_entries(&path).then_some(false))
        .or_else(|| {
            policy
                .configured_regenerable_dir(relative_dir)
                .then_some(false)
        })
}

/// The first bytes of a `CACHEDIR.TAG` under the Cache Directory Tagging
/// specification (<https://bford.info/cachedir/>).
const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Entries a tagged cache may hold before cleanup stops checking it for
/// protected names and keeps it.
const TAGGED_CACHE_ENTRY_LIMIT: usize = 20_000;

/// Whether `directory` holds a regular `CACHEDIR.TAG` that begins with the
/// standard signature: the tool that wrote the cache declared it regenerable.
fn has_cache_directory_tag(directory: &Path) -> bool {
    use std::io::Read;
    let tag = directory.join("CACHEDIR.TAG");
    let Ok(metadata) = std::fs::symlink_metadata(&tag) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let mut prefix = [0_u8; CACHEDIR_TAG_SIGNATURE.len()];
    std::fs::File::open(&tag)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .is_ok_and(|()| prefix.as_slice() == CACHEDIR_TAG_SIGNATURE)
}

/// Names that never become disposable because a cache tag or a test-tool
/// witness sits above them: the secrets, keys, credentials, databases and
/// broker state a configured `regenerable` glob may never match
/// ([`GLOB_PROBES`]), plus SSH private keys (`id_*` without `.pub`) and
/// package-manager and password-store credential files. Case-insensitive:
/// `.env*`, anything containing `.sqlite` (its `-wal`, `-shm` and
/// `-journal` siblings included), `*.db` and its `-wal`, `-shm` and
/// `-journal` siblings, `*.tfstate` and `*.tfstate.*`, key stores and
/// credential files.
fn protected_cache_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(".env")
        || lower == ".aethyme"
        || (lower.starts_with("id_") && !lower.ends_with(".pub"))
        || lower.contains(".sqlite")
        || lower.contains(".tfstate.")
        || [
            ".netrc",
            ".npmrc",
            ".pypirc",
            "credentials.json",
            ".git-credentials",
            ".pgpass",
        ]
        .contains(&lower.as_str())
        || [
            ".key",
            ".pem",
            ".p12",
            ".pfx",
            ".db",
            ".db-wal",
            ".db-shm",
            ".db-journal",
            ".kdbx",
            ".tfstate",
            ".jks",
            ".keystore",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// A tagged cache directory whose bounded walk finds no protected name.
fn tagged_cache_without_protected_entries(directory: &Path) -> bool {
    has_cache_directory_tag(directory) && subtree_free_of_protected_entries(directory)
}

/// A bounded walk of the whole subtree finds no protected name. Symlinks are
/// judged by name and never followed; an unreadable entry, a non-Unicode
/// name or a walk past [`TAGGED_CACHE_ENTRY_LIMIT`] keeps the directory.
fn subtree_free_of_protected_entries(directory: &Path) -> bool {
    let mut pending = vec![directory.to_path_buf()];
    let mut examined = 0_usize;
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            return false;
        };
        for entry in entries {
            examined += 1;
            if examined > TAGGED_CACHE_ENTRY_LIMIT {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                return false;
            };
            if protected_cache_entry(&name) {
                return false;
            }
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    true
}

/// Hypothesis's example database (`.hypothesis/`): only the subdirectories
/// and ignore file Hypothesis writes, at least one of its stores, and no
/// protected name anywhere beneath. Any other entry keeps it.
fn hypothesis_database(directory: &Path) -> bool {
    only_known_entries(
        directory,
        &[
            "examples",
            "unicode_data",
            "constants",
            "patches",
            "observed",
            ".gitignore",
            ".DS_Store",
        ],
        &["examples", "unicode_data"],
    ) && subtree_free_of_protected_entries(directory)
}

/// Playwright's HTML report: `index.html` plus its `data/` and `trace/`
/// folders, nothing else, and no protected name anywhere beneath.
fn playwright_html_report(directory: &Path) -> bool {
    only_known_entries(
        directory,
        &["index.html", "data", "trace", ".DS_Store"],
        &["index.html"],
    ) && subtree_free_of_protected_entries(directory)
}

/// Playwright's test output folder, proved by the `.last-run.json` status
/// file Playwright writes there. Playwright empties this folder at the start
/// of every run, so nothing a person puts in it survives the next run; a
/// protected name anywhere beneath still keeps it.
fn playwright_output_directory(directory: &Path) -> bool {
    let last_run = directory.join(".last-run.json");
    let Ok(metadata) = std::fs::symlink_metadata(&last_run) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return false;
    }
    std::fs::read(&last_run)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value
                .get("status")
                .is_some_and(serde_json::Value::is_string)
        })
        && subtree_free_of_protected_entries(directory)
}

/// Every top-level entry of `directory` is in `allowed`, and at least one
/// name in `required` is present. Unreadable entries keep the directory.
fn only_known_entries(directory: &Path, allowed: &[&str], required: &[&str]) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };
    let mut seen = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return false;
        };
        if !allowed.contains(&name.as_str()) {
            return false;
        }
        seen.push(name);
    }
    required
        .iter()
        .any(|name| seen.iter().any(|seen| seen == name))
}

/// The nearest enclosing build-output directory of an ignored path, as
/// `(relative_dir, is_cargo_target)`.
fn enclosing_regenerable(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Option<(String, bool)> {
    let components = relative
        .trim_end_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    (1..=components.len()).find_map(|depth| {
        let prefix = components[..depth].join("/");
        regenerable_dir(worktree, &prefix, policy).map(|cargo| (prefix, cargo))
    })
}

/// Entries a directory of generated artifacts may hold before cleanup stops
/// looking and preserves it. Aethyme writes a few dozen such files.
const GENERATED_DIRECTORY_ENTRY_LIMIT: usize = 256;

/// Whether `relative` is a regular file whose bytes equal what an Aethyme
/// writer produces at that path, by [`aethyme_enhance::deploy::generated_artifact_contents`].
fn unmodified_generated_file(
    worktree: &Path,
    relative: &str,
    metadata: &std::fs::Metadata,
) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    let candidates = aethyme_enhance::deploy::generated_artifact_contents(relative);
    // Compare lengths first so an unrelated large file is never read.
    if !candidates
        .iter()
        .any(|content| content.len() as u64 == metadata.len())
    {
        return false;
    }
    std::fs::read(worktree.join(relative)).is_ok_and(|bytes| {
        candidates
            .iter()
            .any(|content| content.as_bytes() == bytes.as_slice())
    })
}

/// Whether an ignored path is provably Aethyme's own unmodified output: a
/// generated file with its exact bytes, or a directory Aethyme may create
/// whose every entry is such a file, a nested directory of the same kind, or
/// Finder metadata. Symlinks, special files, unreadable entries and anything
/// past [`GENERATED_DIRECTORY_ENTRY_LIMIT`] are not proven.
fn unmodified_generated_artifact(
    worktree: &Path,
    relative: &str,
    metadata: &std::fs::Metadata,
) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    if !metadata.is_dir() {
        return unmodified_generated_file(worktree, relative, metadata);
    }
    if !aethyme_enhance::deploy::may_hold_generated_artifacts(relative) {
        return false;
    }
    let mut pending = vec![relative.to_string()];
    let mut examined = 0_usize;
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(worktree.join(&directory)) else {
            return false;
        };
        for entry in entries {
            examined += 1;
            if examined > GENERATED_DIRECTORY_ENTRY_LIMIT {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                return false;
            };
            let child = format!("{directory}/{name}");
            let Ok(child_metadata) = std::fs::symlink_metadata(entry.path()) else {
                return false;
            };
            let disposable = if child_metadata.file_type().is_symlink() {
                false
            } else if child_metadata.is_dir() {
                let nested = aethyme_enhance::deploy::may_hold_generated_artifacts(&child);
                if nested {
                    pending.push(child);
                }
                nested
            } else {
                (child_metadata.is_file() && name == ".DS_Store")
                    || unmodified_generated_file(worktree, &child, &child_metadata)
            };
            if !disposable {
                return false;
            }
        }
    }
    true
}

/// Classification used by recovery archives, which preserve protected
/// standalone ignored files while still discarding policy-approved output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryIgnoredClassification {
    /// An ignored directory root. Its contents stay opaque and are disposable
    /// under the existing cleanup policy.
    DisposableDirectory,
    /// A standalone path the current cleanup policy permits removing.
    DisposableOutput,
    /// A standalone path the policy protects and recovery must archive.
    Preserve(&'static str),
    /// Strict mode requires an explicit operator decision for every leftover.
    StrictRefusal,
}

/// Classify an entry from Git's ignored listing. Only recognized or explicitly
/// configured output directories are disposable. Unknown directories can hold
/// local data and must be preserved even when Git ignores them.
/// Strict mode keeps every ignored leftover for explicit operator cleanup.
pub(crate) fn classify_ignored_for_recovery(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Result<RecoveryIgnoredClassification, String> {
    classify_ignored_entry(worktree, relative, policy).map(|(classification, _)| classification)
}

/// Which rule decided a standalone ignored entry, for phase counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IgnoredEntryRule {
    /// Any rule other than the two below.
    Other,
    /// A configured `regenerable` glob released a standalone regular file.
    FilePattern,
    /// A configured `regenerable` glob named a protected standalone entry,
    /// which therefore stays.
    ProtectedRefusal,
}

/// [`classify_ignored_for_recovery`], with the rule that decided it.
fn classify_ignored_entry(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Result<(RecoveryIgnoredClassification, IgnoredEntryRule), String> {
    let other = |classification| (classification, IgnoredEntryRule::Other);
    if policy.strict {
        return Ok(other(RecoveryIgnoredClassification::StrictRefusal));
    }
    let trimmed = relative.trim_end_matches('/');
    if trimmed.is_empty()
        || Path::new(trimmed)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(format!("invalid ignored path {relative:?}"));
    }
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if (!relative.ends_with('/') && name == ".DS_Store")
        || crate::runtime_paths::is_broker_runtime_path(relative)
    {
        return Ok(other(RecoveryIgnoredClassification::DisposableOutput));
    }
    // Never follow a link: the entry is judged as the link itself.
    let metadata = std::fs::symlink_metadata(worktree.join(trimmed))
        .map_err(|error| format!("cannot stat ignored path {relative}: {error}"))?;
    // Aethyme's own deploy output, byte for byte, holds no user data. Anything
    // else at the same path (a merged or edited settings file) falls through
    // to the protection below with its existing reason.
    if unmodified_generated_artifact(worktree, trimmed, &metadata) {
        return Ok(other(if metadata.is_dir() {
            RecoveryIgnoredClassification::DisposableDirectory
        } else {
            RecoveryIgnoredClassification::DisposableOutput
        }));
    }
    // Git ignore rules do not prove a directory is regenerable. Do not walk
    // arbitrary ignored trees or follow symlinks: preserve unknown roots.
    if metadata.is_dir() {
        return Ok(other(
            if regenerable_dir(worktree, trimmed, policy).is_some() {
                RecoveryIgnoredClassification::DisposableDirectory
            } else {
                RecoveryIgnoredClassification::Preserve(
                    "is not a recognised build-output directory",
                )
            },
        ));
    }
    if let Some((enclosing, cargo_target)) = enclosing_regenerable(worktree, trimmed, policy) {
        // The entry itself matched: a link with a recognised dependency
        // name or one a configured glob names. Removal only unlinks it, but
        // a configured glob still never releases a protected name.
        if enclosing == trimmed && protected_ignored_path(trimmed) {
            return Ok((
                RecoveryIgnoredClassification::Preserve(PROTECTED_NAME_REASON),
                IgnoredEntryRule::ProtectedRefusal,
            ));
        }
        if trimmed.split('/').any(|component| component == ".aethyme") && !cargo_target {
            return Ok(other(RecoveryIgnoredClassification::Preserve(
                "holds Aethyme state",
            )));
        }
        // The policy declares this output disposable. Package fixtures routinely
        // contain keys, databases, dangling links and hundreds of thousands of files;
        // inspecting those contents both invents blockers and scales with the output.
        // Worktree removal unlinks symlinks; it never follows their targets.
        return Ok(other(RecoveryIgnoredClassification::DisposableOutput));
    }
    // A standalone regular file the repository names by its whole path. A
    // protected name stays whatever the configuration says.
    if !relative.ends_with('/') && metadata.is_file() && policy.configured_regenerable_file(trimmed)
    {
        return Ok(if protected_ignored_path(trimmed) {
            (
                RecoveryIgnoredClassification::Preserve(PROTECTED_NAME_REASON),
                IgnoredEntryRule::ProtectedRefusal,
            )
        } else {
            (
                RecoveryIgnoredClassification::DisposableOutput,
                IgnoredEntryRule::FilePattern,
            )
        });
    }
    Ok(other(RecoveryIgnoredClassification::Preserve(
        "is not inside a recognised build-output directory",
    )))
}

/// Cleanup's removal verdict remains conservative: only paths with an
/// explicit disposable classification pass.
pub(crate) fn classify_ignored(
    worktree: &Path,
    relative: &str,
    policy: &AutoCleanupPolicy,
) -> Result<(), String> {
    ignored_verdict(
        relative,
        classify_ignored_for_recovery(worktree, relative, policy)?,
    )
}

/// The removal verdict for one classified ignored entry.
fn ignored_verdict(
    relative: &str,
    classification: RecoveryIgnoredClassification,
) -> Result<(), String> {
    match classification {
        RecoveryIgnoredClassification::DisposableDirectory
        | RecoveryIgnoredClassification::DisposableOutput => Ok(()),
        RecoveryIgnoredClassification::Preserve(reason) => {
            Err(format!("ignored path {relative} {reason}"))
        }
        RecoveryIgnoredClassification::StrictRefusal => Err(format!(
            "strict cleanup requires no ignored leftovers; remove {relative} manually or review an explicit discard"
        )),
    }
}

/// Snapshot the ignored directory roots that passed the same cleanup
/// classifier before finish starts removal. The resulting prefixes are part
/// of its durable recovery proof; an orphan retry may discard contents only
/// beneath these roots after Git metadata has gone.
pub(crate) fn disposable_ignored_directory_prefixes(
    worktree: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + IGNORED_PATH_LISTING_BUDGET;
    let expected_root = worktree
        .canonicalize()
        .map_err(|error| format!("cannot resolve cleanup checkout root: {error}"))?;
    let checkout = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
    let discovered_root = checkout
        .root()
        .canonicalize()
        .map_err(|error| format!("cannot resolve discovered Git root: {error}"))?;
    if discovered_root != expected_root {
        return Err("Git resolved a different root than the cleanup checkout".into());
    }

    let ignored = checkout
        .ignored_paths_readonly_until(deadline)
        .map_err(|error| error.to_string())?;
    let mut prefixes = Vec::new();
    for entry in ignored {
        let relative = if entry.directory_marker {
            format!("{}/", entry.relative)
        } else {
            entry.relative.clone()
        };
        match classify_ignored_for_recovery(worktree, &relative, policy)? {
            RecoveryIgnoredClassification::DisposableDirectory if entry.directory_marker => {
                prefixes.push(format!("{}/", entry.relative));
            }
            RecoveryIgnoredClassification::DisposableOutput if !entry.directory_marker => {}
            RecoveryIgnoredClassification::DisposableDirectory => {
                return Err(format!(
                    "ignored path {relative} changed shape while finish cleanup was being proved"
                ));
            }
            // These entries receive no discard prefix. Keeping the identity
            // proof lets registered recovery re-run the active policy after
            // an operator resolves them; orphan recovery will reject any
            // surviving path that the proof did not authorize.
            RecoveryIgnoredClassification::Preserve(_)
            | RecoveryIgnoredClassification::StrictRefusal
            | RecoveryIgnoredClassification::DisposableOutput => {}
        }
    }
    prefixes.sort();
    prefixes.dedup();
    Ok(prefixes)
}

/// One checkout auto-cleanup removed, with its proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoRemovedCheckout {
    pub worktree: String,
    pub sessions: Vec<i64>,
    pub branch: Option<String>,
    pub head: String,
    /// The remote ref the commits were proved against.
    pub contained_in: String,
    /// The upstream commit that contains the head: the ref tip for an
    /// ancestry proof, the landing commit for a content or patch proof.
    pub containing_commit: String,
    /// `ancestry`, `content`, `patch equivalence` or `no net change`.
    pub proof: String,
    /// Bytes the last recorded measurement held for the worktree, when one
    /// exists. Removal does not walk the tree to count them.
    pub bytes: Option<u64>,
    /// Tracked paths already missing when this removal began: an earlier
    /// removal stopped part-way and this one finished it. Every missing file
    /// is in the contained HEAD, so nothing was lost. Absent (0) for an
    /// ordinary removal.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub resumed_interrupted_removal: usize,
    /// The ownership re-proof this removal used because the saved spawn
    /// witness no longer matched exactly: `device_renumber` for a schema-2
    /// witness whose mounts macOS renumbered. Absent for an exact match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_reproof: Option<String>,
}

/// One candidate auto-cleanup kept, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoKeptCheckout {
    pub worktree: String,
    pub sessions: Vec<i64>,
    pub reason: String,
    /// Machine-readable form of `reason` ([`kept_reason_code`]): stable
    /// across wording changes, so tools can count keeps by cause.
    #[serde(default)]
    pub code: String,
    /// Ignored paths, relative to the checkout, that keep it (code
    /// `ignored_content`). Each is a root Git listed as ignored that no
    /// built-in witness or `[cleanup] regenerable` glob recognises.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_blockers: Vec<String>,
    /// Set to `device_renumber` when the device-renumber ownership re-proof
    /// was tried for this checkout and refused (code
    /// `ownership_reproof_refused`); the reason names the failed condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_reproof: Option<String>,
    /// With code `gate_running`: which gate witness kept the checkout,
    /// `own_session` (a gate of one of the checkout's own sessions),
    /// `path` (a gate whose recorded checkout is this one, inside it or
    /// contains it) or `unknown_owner` (a gate whose checkout no record
    /// positively identifies). Absent for other codes and for keeps written
    /// before the gate check was narrowed to the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_keep: Option<String>,
}

/// Keep-reason prefixes of the gate check. Each contains the
/// `a gate may be running` needle, so all three keep code `gate_running`.
pub(crate) const GATE_RUNNING_OWN_SESSION: &str = "a gate may be running (own session)";
pub(crate) const GATE_RUNNING_PATH: &str = "a gate may be running (this checkout's path)";
pub(crate) const GATE_RUNNING_UNKNOWN_OWNER: &str = "a gate may be running (owner unknown)";

/// The `gate_keep` value of a `gate_running` keep reason, if it says which
/// witness kept the checkout.
pub(crate) fn gate_keep_kind(reason: &str) -> Option<&'static str> {
    [
        (GATE_RUNNING_OWN_SESSION, "own_session"),
        (GATE_RUNNING_PATH, "path"),
        (GATE_RUNNING_UNKNOWN_OWNER, "unknown_owner"),
    ]
    .into_iter()
    .find(|(needle, _)| reason.contains(needle))
    .map(|(_, kind)| kind)
}

/// What the gate check of automatic checkout removal saw, summed over the
/// checks of a pass (or of a capacity slice's passes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupGateChecks {
    /// Candidate checkouts the gate check judged.
    pub checks: usize,
    /// Repository-wide reads of the gate pidfiles and owner locks. Selection
    /// reads them once per pass; revalidation reads them fresh for each
    /// checkout immediately before removing it.
    pub scans: usize,
    /// Running-gate witnesses seen, summed over the checks.
    pub gates_seen: usize,
    /// Of those, witnesses attributed to the judged checkout: one of its own
    /// sessions' gates, or a gate whose recorded checkout overlaps it.
    pub attributed_here: usize,
    /// Witnesses positively identified as running in another checkout; they
    /// no longer keep the judged one.
    pub attributed_elsewhere: usize,
    /// Witnesses whose checkout no record identifies (an older binary's
    /// record, an unreadable or malformed one); each still keeps the checkout.
    pub unknown_owner: usize,
    /// Checks whose strict per-session inventory of the checkout's own
    /// sessions came back unknown (the run directory changed during the
    /// read, or a record could not be verified); each keeps the checkout.
    #[serde(default)]
    pub inventory_unknown: usize,
    /// Time the gate checks took, including the reads.
    pub elapsed_ms: u64,
}

impl AutoCleanupGateChecks {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn absorb(&mut self, other: &Self) {
        self.checks = self.checks.saturating_add(other.checks);
        self.scans = self.scans.saturating_add(other.scans);
        self.gates_seen = self.gates_seen.saturating_add(other.gates_seen);
        self.attributed_here = self.attributed_here.saturating_add(other.attributed_here);
        self.attributed_elsewhere = self
            .attributed_elsewhere
            .saturating_add(other.attributed_elsewhere);
        self.unknown_owner = self.unknown_owner.saturating_add(other.unknown_owner);
        self.inventory_unknown = self
            .inventory_unknown
            .saturating_add(other.inventory_unknown);
        self.elapsed_ms = self.elapsed_ms.saturating_add(other.elapsed_ms);
    }
}

/// How many checks of one kind ran and how long they took together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckTiming {
    pub count: u64,
    pub ms: u64,
}

impl CheckTiming {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn absorb(&mut self, other: &Self) {
        self.count = self.count.saturating_add(other.count);
        self.ms = self.ms.saturating_add(other.ms);
    }
}

/// Where judging closed checkouts spent its time, per check kind: the
/// database-only keep and the remembered-keep check (both free of Git), the
/// group inspection (Git discovery, spawn witness, ownership), the remote
/// default branch tip, and each proof of the verdict. Selection and
/// revalidation are summed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupCheckTimings {
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub database: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub memo: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub inspect: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub upstream: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub sessions: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub cohort: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub ownership: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub live_paths: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub leases: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub gates: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub status: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub ignored: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub containment: CheckTiming,
    #[serde(default, skip_serializing_if = "CheckTiming::is_empty")]
    pub blocker_listing: CheckTiming,
}

impl AutoCleanupCheckTimings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn absorb(&mut self, other: &Self) {
        self.database.absorb(&other.database);
        self.memo.absorb(&other.memo);
        self.inspect.absorb(&other.inspect);
        self.upstream.absorb(&other.upstream);
        self.sessions.absorb(&other.sessions);
        self.cohort.absorb(&other.cohort);
        self.ownership.absorb(&other.ownership);
        self.live_paths.absorb(&other.live_paths);
        self.leases.absorb(&other.leases);
        self.gates.absorb(&other.gates);
        self.status.absorb(&other.status);
        self.ignored.absorb(&other.ignored);
        self.containment.absorb(&other.containment);
        self.blocker_listing.absorb(&other.blocker_listing);
    }
}

/// Kept checkouts the keep memo decided without judging them again, per
/// keep code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupMemoHits {
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub ignored_content: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub uncommitted_changes: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub unmarked_tracked_deletions: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub not_contained: usize,
}

impl AutoCleanupMemoHits {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn count(&mut self, code: &str) {
        let (field, phase) = match code {
            fast_judging::MEMO_UNCOMMITTED => (
                &mut self.uncommitted_changes,
                "auto_cleanup_memo_hit_uncommitted_changes",
            ),
            fast_judging::MEMO_UNMARKED_DELETIONS => (
                &mut self.unmarked_tracked_deletions,
                "auto_cleanup_memo_hit_unmarked_tracked_deletions",
            ),
            fast_judging::MEMO_NOT_CONTAINED => (
                &mut self.not_contained,
                "auto_cleanup_memo_hit_not_contained",
            ),
            _ => (
                &mut self.ignored_content,
                "auto_cleanup_memo_hit_ignored_content",
            ),
        };
        *field = field.saturating_add(1);
        crate::phase_telemetry::add(phase, Duration::ZERO);
    }

    pub(crate) fn absorb(&mut self, other: &Self) {
        self.ignored_content = self.ignored_content.saturating_add(other.ignored_content);
        self.uncommitted_changes = self
            .uncommitted_changes
            .saturating_add(other.uncommitted_changes);
        self.unmarked_tracked_deletions = self
            .unmarked_tracked_deletions
            .saturating_add(other.unmarked_tracked_deletions);
        self.not_contained = self.not_contained.saturating_add(other.not_contained);
    }
}

/// Classify a keep reason into a stable code. The reasons are written by the
/// proofs above; an unrecognised one is `other`, never a removal.
pub fn kept_reason_code(reason: &str) -> &'static str {
    let reason = reason
        .strip_prefix("changed before removal: ")
        .unwrap_or(reason);
    const CODES: &[(&str, &str)] = &[
        // Ownership causes first: their reasons quote failed conditions such
        // as "the session is not closed" that later needles would misfile.
        (
            "device renumber re-proof refused",
            "ownership_reproof_refused",
        ),
        (
            OWNERSHIP_WITNESS_NEVER_RECORDED,
            "ownership_witness_missing",
        ),
        (OWNERSHIP_WITNESS_LEGACY, "ownership_witness_legacy"),
        (OWNERSHIP_ADOPTED_ONLY, "ownership_adopted_only"),
        (CHECKOUT_DIRECTORY_MISSING, "checkout_missing"),
        (
            "closed checkout cohort or ownership witness changed: cohort unavailable",
            "cohort_unavailable",
        ),
        (
            "ignored content requires explicit cleanup",
            "ignored_content",
        ),
        ("ignored-path classification", "ignored_content_unverified"),
        (
            "deleted with no recorded removal attempt",
            "unmarked_tracked_deletions",
        ),
        (
            "strict cleanup removes only pristine checkouts",
            "strict_tracked_deletions",
        ),
        ("uncommitted or untracked path", "uncommitted_changes"),
        ("is in progress", "operation_in_progress"),
        ("a stash was made", "stash"),
        ("closed-worktree grace period", "in_grace"),
        ("is still open", "session_open"),
        ("is open in this worktree", "session_open"),
        ("is not closed", "session_open"),
        (
            "not contained in the remote default branch",
            "not_contained",
        ),
        ("landing search hit its candidate cap", "not_contained"),
        ("containment could not be proved", "containment_unproven"),
        ("no fetched remote default branch", "no_remote_default"),
        ("pinned by [cleanup] keep", "pinned"),
        ("still holds", "lease_held"),
        ("lease liveness", "lease_held"),
        ("a gate may be running", "gate_running"),
        ("artifact producer", "artifact_use"),
        ("artifact-use", "artifact_use"),
        ("open file", "process_use"),
        ("spawn-time", "ownership_unproven"),
        ("broker-owned worktree", "ownership_unproven"),
        ("no recorded owner", "ownership_unproven"),
        ("primary checkout", "primary_checkout"),
        ("cleanup policy", "policy_changed"),
        ("could not be resolved", "inventory_unresolved"),
        ("removal failed", "removal_failed"),
    ];
    CODES
        .iter()
        .find(|(needle, _)| reason.contains(needle))
        .map_or("other", |(_, code)| code)
}

/// Time and work each lane of one automatic maintenance pass received.
/// Removal frees checkout capacity and retirement only prepares it, so the
/// removal lane always keeps its own share of the pass (and the larger one
/// once registered checkouts reach three quarters of `max_checkouts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupLanes {
    /// Registered linked checkouts were at or above three quarters of
    /// `[worktrees] max_checkouts` when the pass started.
    pub capacity_pressure: bool,
    /// Budget the session-retirement lane was given; 0 when unbounded.
    pub retirement_budget_ms: u64,
    pub retirement_elapsed_ms: u64,
    /// Budget left for closed-checkout removal; 0 when unbounded.
    pub removal_budget_ms: u64,
    /// Time the removal lane spent selecting, before revalidation and removal.
    pub removal_elapsed_ms: u64,
    /// Closed checkouts the removal lane judged in this pass.
    pub removal_examined: usize,
    /// Of those, kept checkouts skipped because the remembered keep verdict
    /// still matched their HEAD and ignored leftovers.
    pub removal_memo_skips: usize,
    /// Checkouts selection handed to revalidation and removal.
    #[serde(default)]
    pub removal_selected: usize,
    /// Part of the removal budget selection may not use, kept for the
    /// apply step's path verification and its first removal; 0 when
    /// unbounded.
    #[serde(default)]
    pub removal_apply_reserve_ms: u64,
    /// Time selection spent obtaining its inventory: finishing the checkout
    /// path index, reading a page of closed-checkout groups and resolving
    /// open sessions' paths.
    #[serde(default)]
    pub removal_scan_ms: u64,
    /// Selection judged nothing because no complete checkout inventory was
    /// available before its deadline (the path index was still mid-lap, or
    /// the deadline passed while reading it).
    #[serde(default)]
    pub removal_inventory_unavailable: bool,
    /// Closed-checkout groups selection judged ahead of its oldest-first lap
    /// because a session's pull request is recorded merged or its checkout
    /// cleanup request is due. Included in `removal_examined`.
    #[serde(default)]
    pub removal_priority_examined: usize,
    /// Of the checkouts selection handed on, those it judged ahead of the
    /// lap (`removal_priority_examined`), so a removal can be credited to
    /// the priority lane or to the lap.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub removal_priority_selected: usize,
    /// Priority candidates the priority memo skipped in this pass: judged
    /// within the re-judge interval with the same merged state, or kept for
    /// a reason only a state change lifts (adopted-only or unproven
    /// ownership, a legacy or missing spawn witness, a missing checkout
    /// directory, ignored leftovers) with that state unchanged.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub removal_priority_memo_skips: usize,
    /// Time the capacity-pressure probe took before selection's inventory.
    #[serde(default)]
    pub removal_probe_ms: u64,
    /// With `removal_inventory_unavailable`: what the inventory was waiting
    /// on when the deadline passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal_inventory_wait: Option<InventoryWait>,
    /// What the gate check saw in selection and revalidation.
    #[serde(default, skip_serializing_if = "AutoCleanupGateChecks::is_empty")]
    pub removal_gate_checks: AutoCleanupGateChecks,
    /// Lap groups kept from the database alone, before any Git process or
    /// worktree walk: a session inside the closed-worktree grace
    /// (`in_grace`), or an open session using the checkout
    /// (`session_open`). Not in `removal_examined`.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub removal_db_in_grace: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub removal_db_session_open: usize,
    /// Priority candidates left out of the priority lane for the same
    /// database reasons; the lap reports them when it reaches them, and a
    /// candidate whose grace ends becomes due through its cleanup request.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub removal_priority_db_skips: usize,
    /// Of `removal_memo_skips`, how many per keep code.
    #[serde(default, skip_serializing_if = "AutoCleanupMemoHits::is_empty")]
    pub removal_memo_hits: AutoCleanupMemoHits,
    /// Count and time of each check kind, selection and revalidation summed.
    #[serde(default, skip_serializing_if = "AutoCleanupCheckTimings::is_empty")]
    pub removal_check_timings: AutoCleanupCheckTimings,
}

/// What selection's checkout inventory was waiting on when it ran out of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryWait {
    /// The capacity-pressure probe used the deadline before the scan began.
    CapacityProbe,
    /// The checkout path index was mid-lap (its progress is kept and resumed).
    CohortIndex,
    /// Reading the page of closed-checkout groups.
    GroupPage,
    /// Resolving open sessions' checkout paths.
    LivePaths,
}

impl InventoryWait {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapacityProbe => "capacity_probe",
            Self::CohortIndex => "cohort_index",
            Self::GroupPage => "group_page",
            Self::LivePaths => "live_paths",
        }
    }
}

/// Whether `deadline` had already passed at `instant`.
fn deadline_expired_at(deadline: Option<Instant>, instant: Instant) -> bool {
    deadline.is_some_and(|end| instant >= end)
}

/// What one auto-cleanup pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoCleanupReport {
    /// Whether automatic checkout removal is enabled. Session retirement is
    /// governed separately by the retention policy.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    pub evaluated_at_ms: i64,
    pub removed: Vec<AutoRemovedCheckout>,
    pub kept: Vec<AutoKeptCheckout>,
    /// Candidates, or one incomplete cleanup or retirement pass, left for
    /// later because its cursor or time budget did not allow completion (#460).
    pub deferred: usize,
    /// Open sessions this pass retired after the configured abandonment
    /// window and current holder-liveness checks allowed closure.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub sessions_retired: usize,
    /// Why an otherwise due session-retirement pass did not finish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retirement_deferred_reasons: Vec<SessionRetirementDeferredReason>,
    /// Open sessions idle past `session_landed_after_hours` that the landed
    /// lane judged in this report.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub landed_sessions_examined: usize,
    /// Of those, the sessions it closed because their work is on the fetched
    /// default branch, their worktree is clean and no process holds them.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub landed_sessions_closed: usize,
    /// Sessions the landed lane kept, counted per machine-readable reason
    /// (`LandedSessionDecline::code`). A session the abandonment window
    /// closed afterwards is not counted here.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub landed_sessions_declined: std::collections::BTreeMap<String, usize>,
    /// Where the open-session retirement cursor stood when the report was
    /// written: successive passes resume after it instead of restarting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_cursor: Option<SessionRetirementCursorPosition>,
    /// Milliseconds the landed lane spent judging sessions.
    #[serde(default, skip_serializing_if = "is_zero_ms")]
    pub landed_retirement_elapsed_ms: u64,
    /// Of `landed_sessions_examined`, the spawned sessions with every pull
    /// request merged that the shorter `session_merged_after_hours` window
    /// judged, and of `landed_sessions_closed`, the ones it closed.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub merged_pull_request_sessions_examined: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub merged_pull_request_sessions_closed: usize,
    /// Sessions this report closed, per lane reason
    /// (`merged_pull_request_landed`, `landed`, `abandoned`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sessions_closed_by_reason: std::collections::BTreeMap<String, usize>,
    /// Sessions past the abandonment window that the oldest-first lap asked
    /// that window first, closed or not.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub abandonment_sessions_examined: usize,
    /// Of those, the sessions the abandonment window closed.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub abandonment_sessions_closed: usize,
    /// Of those, the sessions it kept, per `AbandonmentKeepReason` code.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub abandonment_sessions_kept: std::collections::BTreeMap<String, usize>,
    /// Where the retirement cursor stood when the first pass of this report
    /// began, so a reader can tell whether the lap moved
    /// (`retirement_cursor` is where it ended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_cursor_start: Option<SessionRetirementCursorPosition>,
    /// Sessions this report closed, per holder evidence (`gone`,
    /// `moved_on`, `lease_expired`, `unbound`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sessions_closed_by_holder_evidence: std::collections::BTreeMap<String, usize>,
    /// Closed sessions whose missing checkout the lane judged, finished
    /// (recorded as cleaned) and kept per reason, in this report.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub missing_checkout_examined: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub missing_checkout_finished: usize,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub missing_checkout_kept: std::collections::BTreeMap<String, usize>,
    /// When the retirement counts above were evaluated, when they were
    /// carried from an earlier report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_evaluated_at_ms: Option<i64>,
    /// How the pass divided its budget between retirement and removal.
    /// Absent from reports written by the retirement-only worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lanes: Option<AutoCleanupLanes>,
    /// Ignored paths that keep closed checkouts, across every checkout the
    /// removal lane remembers keeping for ignored leftovers, most common
    /// first, each with the config line that would release it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by_ignored: Vec<BlockedByIgnored>,
    /// The background worker's last capacity removal slice, read from its
    /// own meta key when the report is shown (`gc plan`, `status`); never
    /// stored inside the report, so no later pass overwrites it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_removal: Option<CapacityRemovalSlice>,
}

/// Meta key of the background worker's last capacity removal slice.
pub(crate) const CAPACITY_REMOVAL_SLICE_KEY: &str = "auto_cleanup.capacity_removal";

/// What the background worker's last capacity removal slice did, kept under
/// its own meta key so the worker's later lanes cannot overwrite it. It says
/// why a slice removed nothing: no inventory yet, time ran out mid-lap, or
/// every examined checkout was kept, counted per keep code, with the ignored
/// directories that keep the most checkouts and the line that would release
/// each.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapacityRemovalSlice {
    pub evaluated_at_ms: i64,
    /// What ran the slice: `pressure` (registered checkouts at three
    /// quarters of `[worktrees] max_checkouts`, the capacity limits) or
    /// `steady` (no pressure: checkouts whose work landed are removed at the
    /// steady background rate, so the count follows merges). Empty in a
    /// record written before the steady trigger existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trigger: String,
    /// Checkouts the priority lane (merged pull request, due cleanup
    /// request) judged ahead of the oldest-first lap, and of those removed.
    /// The two lanes take turns, priority first, so the lap gets every
    /// other judgment of a pass.
    #[serde(default)]
    pub priority_examined: usize,
    #[serde(default)]
    pub priority_removed: usize,
    /// Checkouts the oldest-first lap judged, and of those removed.
    #[serde(default)]
    pub lap_examined: usize,
    #[serde(default)]
    pub lap_removed: usize,
    /// With trigger `steady`: the idle streak that set this slice's retry.
    /// It is `empty_laps` when the slice ended the lap (or moved nothing)
    /// with nothing removed, and 0 while the lap is unfinished and moving,
    /// because such a slice is due again a minute later whatever the
    /// streak.
    #[serde(default)]
    pub idle_streak: u32,
    /// With trigger `steady`: consecutive steady slices that reached the end
    /// of the closed-checkout lap (or moved nothing) without removing
    /// anything; each doubles the wait before the next (5, 10, 20, then 30
    /// minutes). A slice that removed nothing with the lap unfinished, or
    /// that pressure kept from running any pass
    /// (`pressure_before_selection`), leaves it unchanged; a removal resets
    /// it to 0.
    #[serde(default)]
    pub empty_laps: u32,
    /// Lap groups kept from the database alone (`lanes.removal_db_in_grace`
    /// and `lanes.removal_db_session_open`), and priority candidates left
    /// out of the priority lane for the same reasons, summed over the
    /// passes. None of them cost a Git process.
    #[serde(default)]
    pub db_in_grace: usize,
    #[serde(default)]
    pub db_session_open: usize,
    #[serde(default)]
    pub priority_db_skips: usize,
    /// Of `memo_skips`, how many per keep code.
    #[serde(default, skip_serializing_if = "AutoCleanupMemoHits::is_empty")]
    pub memo_hits: AutoCleanupMemoHits,
    /// Count and time of each check kind over the slice's passes
    /// (`lanes.removal_check_timings`, summed).
    #[serde(default, skip_serializing_if = "AutoCleanupCheckTimings::is_empty")]
    pub check_timings: AutoCleanupCheckTimings,
    /// Groups judged (`examined` plus the database keeps) per minute of
    /// selection time (`selection_ms`); 0 when nothing was judged.
    #[serde(default)]
    pub groups_judged_per_min: u64,
    /// Whether the worker run took this slice right after its previous one
    /// because that one stopped with the lap unfinished and nothing
    /// blocking (`run_slices` counts the run's slices).
    #[serde(default)]
    pub continued: bool,
    /// Whether the slice's passes reached the end of the closed-checkout lap
    /// (the cursor has no group left after it in a complete index).
    #[serde(default)]
    pub lap_completed: bool,
    /// Priority candidates the priority memo skipped, summed over the
    /// slice's passes (`lanes.removal_priority_memo_skips`).
    #[serde(default)]
    pub priority_memo_skips: usize,
    /// Examined checkouts kept from a remembered keep verdict (ignored
    /// content, uncommitted changes, unmarked tracked deletions, commits not
    /// contained) without judging them again, summed over the passes.
    #[serde(default)]
    pub memo_skips: usize,
    /// `removed`; `lock_busy` (another maintenance pass held the GC lock);
    /// `inventory_unavailable` (no complete checkout inventory before the
    /// deadline); `lap_unfinished` (time ran out before the closed-checkout
    /// lap reached its end); `out_of_time_before_selection` (the budget ran
    /// out before one removal pass started, see `budget_consumed_by`; retried
    /// after one minute); `pressure_before_selection` (pressure kept the
    /// slice from running any pass, see `pressure_seen`: memory pressure
    /// that stopped it retries after ten minutes; load only when the slice
    /// reached the pre-pass check with at least the minimum slice left and
    /// the bounded load wait left too little for a pass (stop `budget`),
    /// retrying after one minute; neither counts as an idle lap. A slice
    /// whose budget went on counting or other pre-selection work, or whose
    /// pass failed, stays `out_of_time_before_selection` whatever load it
    /// saw); `nothing_removable` (the lap ended and every examined
    /// checkout was kept or deferred).
    pub outcome: String,
    pub removed: usize,
    pub examined: usize,
    pub selected: usize,
    pub kept: usize,
    pub deferred: usize,
    /// Kept checkouts per machine-readable keep code (`kept[].code`).
    pub kept_by_code: std::collections::BTreeMap<String, usize>,
    /// Ignored paths keeping the most closed checkouts, with the
    /// `[cleanup] regenerable` line that would release a directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by_ignored: Vec<BlockedByIgnored>,
    /// Whether the checkout path index was complete when the slice ended.
    pub index_complete: bool,
    /// Closed-checkout groups after the planner cursor in the current lap.
    pub lap_remaining: usize,
    pub cursor_after_session_id: i64,
    pub cursor_high_water_session_id: i64,
    /// Removal passes the slice ran (one removal each at most).
    pub passes: usize,
    pub budget_ms: u64,
    /// Selection's inventory time (`lanes.removal_scan_ms`), summed.
    pub scan_ms: u64,
    /// Selection time including the inventory, summed.
    pub selection_ms: u64,
    /// Revalidation and removal time, summed.
    pub apply_ms: u64,
    /// Unattended pacer wait between removals.
    pub paced_ms: u64,
    pub elapsed_ms: u64,
    /// When the next slice with the same trigger may run; absent when a
    /// pressure slice removed something and the next one may run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<i64>,
    /// Time the slice spent counting registered checkouts.
    #[serde(default)]
    pub count_ms: u64,
    /// Time from the slice start to its first removal pass (or to giving up).
    #[serde(default)]
    pub pre_selection_ms: u64,
    /// With outcome `out_of_time_before_selection`: what used the budget,
    /// `checkout_count` or `pre_selection`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_consumed_by: Option<String>,
    /// Time the removal passes' capacity-pressure probes took, summed.
    #[serde(default)]
    pub probe_ms: u64,
    /// What the removal passes' gate checks saw, summed
    /// (`lanes.removal_gate_checks`).
    #[serde(default, skip_serializing_if = "AutoCleanupGateChecks::is_empty")]
    pub gate_checks: AutoCleanupGateChecks,
    /// With outcome `inventory_unavailable`: what the inventory was waiting
    /// on (`capacity_probe`, `cohort_index`, `group_page`, `live_paths`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    /// The checkout path index lap's position when the slice ended: the last
    /// session id indexed and the session id the lap runs to (done of total).
    #[serde(default)]
    pub index_after_session_id: i64,
    #[serde(default)]
    pub index_high_water_session_id: i64,
    /// Why the slice stopped: `cap` (made its allowance of removals),
    /// `budget` (too little time left for another pass), `memory_backoff`
    /// (critical memory pressure outlasted the pacer's pause budget, before a pass or
    /// while paying for a removal; ten-minute retry), `load_throttled`
    /// (load stayed elevated through a pre-pass check's bounded wait, so
    /// the slice made one removal and stopped; the next slice runs at the
    /// normal cadence but no sooner than `throttle_gap_ms`; load is never a
    /// back-off), `memory_warning_throttled` (memory pressure stayed at the
    /// warning level through the bounded wait: one removal, then at least
    /// two minutes, a hard retry; only critical pressure is
    /// `memory_backoff`), `slice_deadline` (the slice
    /// ended mid pacer wait; `pacer_unpaid_ms` is owed before the next
    /// slice), `yielded_to_foreground` (a foreground command waited for the
    /// GC lock), `nothing_selected` (a pass selected nothing and moved
    /// neither the lap nor the priority lane on), `lap_complete` (steady: a
    /// pass that selected nothing reached the end of the lap),
    /// `kept_only_limit` (its passes that removed nothing ran out),
    /// `lock_busy`, `failed` or `removal_failures` (Git failed to remove
    /// two checkouts after it had started deleting). Records written by
    /// older binaries may say `pacer_backoff` (memory or load pressure
    /// outlasted the pacer's pause budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// The slice's removal allowance; kept checkouts cost none of it.
    #[serde(default)]
    pub removal_cap: usize,
    /// Passes that removed nothing (the selected checkout was kept or
    /// deferred at the last step).
    #[serde(default)]
    pub kept_only_passes: usize,
    /// Checkouts Git failed to remove after it had started deleting; the
    /// slice stops after two.
    #[serde(default)]
    pub removal_failures: usize,
    /// Selection and revalidation budget of one pass.
    #[serde(default)]
    pub pass_budget_ms: u64,
    /// Time the passes held the GC lock, summed.
    #[serde(default)]
    pub lock_held_ms: u64,
    /// Time spent on a pass that found the GC lock held.
    #[serde(default)]
    pub lock_wait_ms: u64,
    /// Time the slice waited for the host-wide single-deleter slot right
    /// before its removals (each wait bounded).
    #[serde(default)]
    pub host_slot_wait_ms: u64,
    /// Time the slice held the host deleter slot: each removal and the
    /// pacer charge that paid for it, nothing else.
    #[serde(default)]
    pub host_slot_held_ms: u64,
    /// Of `host_slot_held_ms`, the time not spent in the rate limiter's
    /// sleep: Git deleting and the bookkeeping around it.
    #[serde(default)]
    pub host_slot_deleting_ms: u64,
    /// Times in this slice the host deleter slot stayed with another
    /// repository's worker through a bounded wait (a removal put off, or a
    /// charge that went on without it after handing it over).
    #[serde(default)]
    pub host_slot_busy: u64,
    /// Time the slice waited for a host-wide judging permit before it
    /// counted and judged.
    #[serde(default)]
    pub judge_permit_wait_ms: u64,
    /// Time the slice kept the GC lock free between passes beyond the pacer
    /// wait: a short fixed gap (long enough for a waiting command's poll)
    /// after a pass that removed nothing, and at least as long as the last
    /// pass held the lock after one that removed a checkout.
    #[serde(default)]
    pub lock_yield_ms: u64,
    /// Time Git spent removing checkouts, the only time credited against
    /// the pacer.
    #[serde(default)]
    pub git_removal_ms: u64,
    /// Time spent in the pressure checks before passes (memory waits and
    /// bounded load waits).
    #[serde(default)]
    pub pressure_wait_ms: u64,
    /// Time the pacer paused for memory pressure during the slice, pre-pass
    /// checks and removal charges together.
    #[serde(default)]
    pub memory_wait_ms: u64,
    /// Time the pacer waited for elevated load (or Linux I/O pressure)
    /// during the slice, each wait bounded, pre-pass checks and removal
    /// charges together.
    #[serde(default)]
    pub load_wait_ms: u64,
    /// Which pressure the slice saw: `memory`, `load` or `memory_and_load`;
    /// absent when it saw none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_seen: Option<String>,
    /// A pre-pass check found load still elevated after its bounded wait,
    /// so the slice ran throttled (one removal at most).
    #[serde(default)]
    pub throttled: bool,
    /// How much the load throttle lengthened the wait before the next
    /// slice: with stop `load_throttled` under trigger `pressure`, the
    /// 30-second gap (instead of running again at once); 0 otherwise,
    /// including under `steady`, whose minute already exceeds it.
    #[serde(default)]
    pub throttle_gap_ms: u64,
    /// This slice's number in its worker run (1 for the run's first slice).
    #[serde(default)]
    pub run_slices: usize,
    /// Checkouts the worker run's slices removed so far, this one included.
    #[serde(default)]
    pub run_removed: usize,
    /// Wall time of the worker run so far, from when it took the host's
    /// deleter slot to the end of this slice.
    #[serde(default)]
    pub run_elapsed_ms: u64,
    /// Time the worker run's slices so far spent with Git deleting
    /// checkouts (`git_removal_ms`, summed).
    #[serde(default)]
    pub run_deleting_ms: u64,
    /// Time the worker run's slices so far spent waiting: pacer charges,
    /// pressure checks and GC lock gaps and waits (`paced_ms`,
    /// `pressure_wait_ms`, `lock_yield_ms` and `lock_wait_ms`, summed).
    #[serde(default)]
    pub run_waiting_ms: u64,
    /// `run_deleting_ms` and `run_waiting_ms` as whole percent of
    /// `run_elapsed_ms`: the worker run's duty cycle.
    #[serde(default)]
    pub run_deleting_pct: u64,
    #[serde(default)]
    pub run_waiting_pct: u64,
    /// Entries Git's own removal time paid for at the configured rate.
    #[serde(default)]
    pub pacer_credited_entries: u64,
    /// Entries the pacer slept for after the removals.
    #[serde(default)]
    pub pacer_charged_entries: u64,
    /// Pacer wait the slice deadline left unpaid; the next slice waits it.
    #[serde(default)]
    pub pacer_unpaid_ms: u64,
    /// Cache lane time this worker run keeps from capacity slices (0 when
    /// the cache lane is not due).
    #[serde(default)]
    pub cache_lane_reserved_ms: u64,
    /// The wait chosen before the next slice with the same trigger; absent
    /// when the next one may run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<i64>,
}

impl CapacityRemovalSlice {
    /// Count `report`'s kept checkouts by code and carry its ignored-path
    /// blockers; the caller fills the outcome, position and timings.
    pub(crate) fn from_report(report: Option<&AutoCleanupReport>) -> Self {
        let Some(report) = report else {
            return Self::default();
        };
        let mut kept_by_code = std::collections::BTreeMap::<String, usize>::new();
        for kept in &report.kept {
            let code = if kept.code.is_empty() {
                kept_reason_code(&kept.reason).to_string()
            } else {
                kept.code.clone()
            };
            *kept_by_code.entry(code).or_default() += 1;
        }
        Self {
            removed: report.removed.len(),
            kept: report.kept.len(),
            deferred: report.deferred,
            kept_by_code,
            blocked_by_ignored: report.blocked_by_ignored.clone(),
            ..Self::default()
        }
    }
}

/// Where the closed-checkout planner stands, for the capacity slice: whether
/// the path index is complete, the groups left in the current lap, and the
/// planner cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapacityRemovalLapPosition {
    pub index_complete: bool,
    pub lap_remaining: usize,
    pub after_session_id: i64,
    pub high_water_session_id: i64,
    /// How far the checkout path index lap has got, as the last session id
    /// it indexed and the session id it runs to.
    pub index_after_session_id: i64,
    pub index_high_water_session_id: i64,
}

/// The retirement cursor as reported: the last session judged in the current
/// lap and the highest open session id the lap covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionRetirementCursorPosition {
    pub after_session_id: i64,
    pub high_water_session_id: i64,
}

/// The landed-session retirement counts `broker status` shows, taken from the
/// last automatic-maintenance report. Present only once the landed lane has
/// judged at least one session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LandedSessionRetirementView {
    pub evaluated_at_ms: i64,
    pub examined: usize,
    pub closed: usize,
    /// Of `examined` and `closed`, the sessions judged in the merged-pull-
    /// request window (`session_merged_after_hours`).
    #[serde(skip_serializing_if = "is_zero_count")]
    pub merged_pull_request_examined: usize,
    #[serde(skip_serializing_if = "is_zero_count")]
    pub merged_pull_request_closed: usize,
    /// Sessions closed per lane reason (`merged_pull_request_landed`,
    /// `landed`, `abandoned`).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub closed_by_reason: std::collections::BTreeMap<String, usize>,
    /// Sessions past the abandonment window asked that window first.
    #[serde(skip_serializing_if = "is_zero_count")]
    pub abandonment_examined: usize,
    /// What the abandonment window did with them: closed, and kept per
    /// reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandonment: Option<AbandonmentRetirementView>,
    /// Sessions closed per holder evidence (`gone`, `moved_on`,
    /// `lease_expired`, `unbound`).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub closed_by_holder_evidence: std::collections::BTreeMap<String, usize>,
    /// Kept sessions per reason; the reasons are `LandedSessionDecline` codes.
    pub declined: std::collections::BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<SessionRetirementCursorPosition>,
    /// Where the cursor stood when the reported passes began; equal to
    /// `cursor` when the lap did not move.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_start: Option<SessionRetirementCursorPosition>,
}

/// What the abandonment window (`session_abandoned_after_hours`) did with the
/// sessions past it: how many it judged and closed, and why it kept the rest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AbandonmentRetirementView {
    pub examined: usize,
    pub closed: usize,
    /// Kept sessions per `AbandonmentKeepReason` code (`lease_held`,
    /// `holder_alive_current`, `open_files_in_checkout`, ...).
    pub kept: std::collections::BTreeMap<String, usize>,
}

impl AutoCleanupReport {
    /// Give every kept checkout its machine-readable code.
    pub(crate) fn assign_kept_codes(&mut self) {
        for kept in &mut self.kept {
            if kept.code.is_empty() {
                kept.code = kept_reason_code(&kept.reason).to_string();
            }
            if kept.code == "ownership_reproof_refused" && kept.ownership_reproof.is_none() {
                kept.ownership_reproof = Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER.to_string());
            }
            if kept.code == "gate_running" && kept.gate_keep.is_none() {
                kept.gate_keep = gate_keep_kind(&kept.reason).map(str::to_string);
            }
        }
    }

    /// Fold one retirement summary's landed-lane counts into this report.
    fn absorb_landed_retirement(&mut self, retirement: &SessionRetirementSummary) {
        self.landed_sessions_examined = self
            .landed_sessions_examined
            .saturating_add(retirement.landed_examined);
        self.landed_sessions_closed = self
            .landed_sessions_closed
            .saturating_add(retirement.landed_closed);
        for (reason, count) in &retirement.landed_declined {
            let entry = self
                .landed_sessions_declined
                .entry((*reason).to_string())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        if retirement.cursor.is_some() {
            self.retirement_cursor = retirement.cursor;
        }
        self.landed_retirement_elapsed_ms = self
            .landed_retirement_elapsed_ms
            .saturating_add(retirement.landed_elapsed_ms);
        self.merged_pull_request_sessions_examined = self
            .merged_pull_request_sessions_examined
            .saturating_add(retirement.merged_examined);
        self.merged_pull_request_sessions_closed = self
            .merged_pull_request_sessions_closed
            .saturating_add(retirement.merged_closed);
        self.abandonment_sessions_examined = self
            .abandonment_sessions_examined
            .saturating_add(retirement.abandonment_examined);
        self.abandonment_sessions_closed = self
            .abandonment_sessions_closed
            .saturating_add(retirement.abandonment_closed);
        for (reason, count) in &retirement.abandonment_kept {
            let entry = self
                .abandonment_sessions_kept
                .entry((*reason).to_string())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        if self.retirement_cursor_start.is_none() {
            self.retirement_cursor_start = retirement.cursor_start;
        }
        for (evidence, count) in &retirement.closed_by_holder_evidence {
            let entry = self
                .sessions_closed_by_holder_evidence
                .entry((*evidence).to_string())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        for (reason, count) in &retirement.closed_by_reason {
            let entry = self
                .sessions_closed_by_reason
                .entry((*reason).to_string())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
    }

    /// Fold a later report's landed-lane counts into this aggregate, as the
    /// background retirement worker does across its passes.
    pub(crate) fn absorb_landed_report(&mut self, later: &AutoCleanupReport) {
        self.landed_sessions_examined = self
            .landed_sessions_examined
            .saturating_add(later.landed_sessions_examined);
        self.landed_sessions_closed = self
            .landed_sessions_closed
            .saturating_add(later.landed_sessions_closed);
        for (reason, count) in &later.landed_sessions_declined {
            let entry = self
                .landed_sessions_declined
                .entry(reason.clone())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        if later.retirement_cursor.is_some() {
            self.retirement_cursor = later.retirement_cursor;
        }
        self.landed_retirement_elapsed_ms = self
            .landed_retirement_elapsed_ms
            .saturating_add(later.landed_retirement_elapsed_ms);
        self.merged_pull_request_sessions_examined = self
            .merged_pull_request_sessions_examined
            .saturating_add(later.merged_pull_request_sessions_examined);
        self.merged_pull_request_sessions_closed = self
            .merged_pull_request_sessions_closed
            .saturating_add(later.merged_pull_request_sessions_closed);
        self.abandonment_sessions_examined = self
            .abandonment_sessions_examined
            .saturating_add(later.abandonment_sessions_examined);
        self.abandonment_sessions_closed = self
            .abandonment_sessions_closed
            .saturating_add(later.abandonment_sessions_closed);
        for (reason, count) in &later.abandonment_sessions_kept {
            let entry = self
                .abandonment_sessions_kept
                .entry(reason.clone())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        if self.retirement_cursor_start.is_none() {
            self.retirement_cursor_start = later.retirement_cursor_start;
        }
        self.missing_checkout_examined = self
            .missing_checkout_examined
            .saturating_add(later.missing_checkout_examined);
        self.missing_checkout_finished = self
            .missing_checkout_finished
            .saturating_add(later.missing_checkout_finished);
        for (code, count) in &later.missing_checkout_kept {
            let entry = self.missing_checkout_kept.entry(code.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }
        for (evidence, count) in &later.sessions_closed_by_holder_evidence {
            let entry = self
                .sessions_closed_by_holder_evidence
                .entry(evidence.clone())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
        for (reason, count) in &later.sessions_closed_by_reason {
            let entry = self
                .sessions_closed_by_reason
                .entry(reason.clone())
                .or_default();
            *entry = entry.saturating_add(*count);
        }
    }

    /// Carry the retirement counts of `previous` into a report whose own
    /// retirement lane judged nothing (a broker open's sliver, `gc plan`'s
    /// zero-budget pass), so `status` keeps showing what the worker did.
    fn carry_retirement_counts(&mut self, previous: &AutoCleanupReport) {
        self.landed_sessions_examined = previous.landed_sessions_examined;
        self.landed_sessions_closed = previous.landed_sessions_closed;
        self.landed_sessions_declined = previous.landed_sessions_declined.clone();
        self.landed_retirement_elapsed_ms = previous.landed_retirement_elapsed_ms;
        self.merged_pull_request_sessions_examined = previous.merged_pull_request_sessions_examined;
        self.merged_pull_request_sessions_closed = previous.merged_pull_request_sessions_closed;
        self.sessions_closed_by_reason = previous.sessions_closed_by_reason.clone();
        self.abandonment_sessions_examined = previous.abandonment_sessions_examined;
        self.abandonment_sessions_closed = previous.abandonment_sessions_closed;
        self.abandonment_sessions_kept = previous.abandonment_sessions_kept.clone();
        self.retirement_cursor_start = previous.retirement_cursor_start;
        self.sessions_closed_by_holder_evidence =
            previous.sessions_closed_by_holder_evidence.clone();
        self.retirement_evaluated_at_ms = previous
            .retirement_evaluated_at_ms
            .or(Some(previous.evaluated_at_ms));
        if self.retirement_cursor.is_none() {
            self.retirement_cursor = previous.retirement_cursor;
        }
    }

    /// What `broker status` reports about the landed lane, if it judged any
    /// session in this report.
    pub fn landed_session_retirement_view(&self) -> Option<LandedSessionRetirementView> {
        (self.landed_sessions_examined > 0
            || self.abandonment_sessions_examined > 0
            || !self.sessions_closed_by_reason.is_empty())
        .then(|| LandedSessionRetirementView {
            abandonment_examined: self.abandonment_sessions_examined,
            abandonment: (self.abandonment_sessions_examined > 0).then(|| {
                AbandonmentRetirementView {
                    examined: self.abandonment_sessions_examined,
                    closed: self.abandonment_sessions_closed,
                    kept: self.abandonment_sessions_kept.clone(),
                }
            }),
            cursor_start: self.retirement_cursor_start,
            closed_by_holder_evidence: self.sessions_closed_by_holder_evidence.clone(),
            evaluated_at_ms: self
                .retirement_evaluated_at_ms
                .unwrap_or(self.evaluated_at_ms),
            closed_by_reason: self.sessions_closed_by_reason.clone(),
            examined: self.landed_sessions_examined,
            closed: self.landed_sessions_closed,
            merged_pull_request_examined: self.merged_pull_request_sessions_examined,
            merged_pull_request_closed: self.merged_pull_request_sessions_closed,
            declined: self.landed_sessions_declined.clone(),
            cursor: self.retirement_cursor,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRetirementDeferredReason {
    /// The abandonment window could not be established from repository policy.
    RetentionPolicyUnavailable { message: String },
    /// The bounded maintenance deadline elapsed before the next session was checked.
    DeadlineExceeded { after_session_id: i64 },
    /// The recorded task holder could not be observed completely.
    HolderLivenessUnavailable { session_id: i64 },
    /// The existing worktree's Git activity metadata was not readable.
    WorktreeActivityUnavailable { session_id: i64 },
    /// Another lifecycle operation held the nonblocking close claim.
    LifecycleBusy { session_id: i64 },
    /// An admitted session command still holds the task operation guard.
    OperationGuardBusy { session_id: i64 },
    /// A gate, artifact producer, or other process is still using this task.
    OperationActive { session_id: i64 },
    /// Task operation or process-use evidence could not be established.
    OperationEvidenceUnavailable { session_id: i64 },
    /// Artifact-use markers saved before a device renumber keep the checkout
    /// (rule `legacy_marker_device_renumber`); `markers` counts them.
    LegacyArtifactMarkers { session_id: i64, markers: usize },
    /// A concurrent SQLite writer prevented the atomic abandonment transition.
    DatabaseBusy { session_id: i64 },
    /// A concurrent SQLite writer prevented retirement cursor progress.
    CursorBusy,
    /// Lease liveness evidence was unavailable or an active lease still held.
    LeaseLivenessUnavailable { session_id: i64 },
    /// More open sessions remain in the current fixed-high-water lap.
    MoreSessionsRemain {
        after_session_id: i64,
        high_water_session_id: i64,
    },
}

#[derive(Debug, Default)]
struct SessionRetirementSummary {
    retired: usize,
    deferred: usize,
    reasons: Vec<SessionRetirementDeferredReason>,
    landed_examined: usize,
    landed_closed: usize,
    landed_declined: std::collections::BTreeMap<&'static str, usize>,
    landed_elapsed_ms: u64,
    /// Of `landed_examined`/`landed_closed`, the sessions judged in the
    /// merged-pull-request window (`session_merged_after_hours`).
    merged_examined: usize,
    merged_closed: usize,
    /// Sessions closed, per lane reason: `merged_pull_request_landed`,
    /// `landed`, `abandoned`.
    closed_by_reason: std::collections::BTreeMap<&'static str, usize>,
    /// Sessions the oldest-first lap asked the abandonment window first.
    abandonment_examined: usize,
    /// Of those, the sessions it closed, and the ones it kept per
    /// `AbandonmentKeepReason` code.
    abandonment_closed: usize,
    abandonment_kept: std::collections::BTreeMap<&'static str, usize>,
    /// The cursor as this pass read it, before any lap restart.
    cursor_start: Option<SessionRetirementCursorPosition>,
    /// Sessions closed, per holder evidence: `gone`, `moved_on`,
    /// `lease_expired`, `unbound`.
    closed_by_holder_evidence: std::collections::BTreeMap<&'static str, usize>,
    cursor: Option<SessionRetirementCursorPosition>,
}

impl Broker {
    /// The holder evidence the close of `session_id` recorded in its latest
    /// `session.abandoned` event.
    fn closed_holder_evidence(&self, session_id: i64) -> &'static str {
        let evidence = self
            .store_ref()
            .latest_session_abandoned_event(session_id)
            .ok()
            .flatten()
            .and_then(|event| event.payload_json)
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|payload| payload["holder_evidence"].as_str().map(str::to_string));
        match evidence.as_deref() {
            Some("gone") => "gone",
            Some("moved_on") => "moved_on",
            Some("lease_expired") => "lease_expired",
            Some("unbound") => "unbound",
            _ => "unknown",
        }
    }
}

/// The order of the likely-landed queue: sessions with a merged pull
/// request first, then the rotation after the session last judged, then id.
pub(crate) fn likely_landed_order(
    session_id: i64,
    merged_pull_requests: &std::collections::BTreeSet<i64>,
    after: i64,
) -> (bool, bool, i64) {
    (
        !merged_pull_requests.contains(&session_id),
        session_id <= after,
        session_id,
    )
}

/// Whether the merged-pull-request window applies to `session`: it is
/// enabled, the broker spawned the session and the cached provider data
/// records a merged pull request for it. The close itself proves that every
/// recorded pull request merged and the work landed.
fn merged_pull_request_lane(
    session: &Session,
    merged_pull_requests: &std::collections::BTreeSet<i64>,
    follow_ups_on_spawned_checkouts: &std::collections::BTreeSet<i64>,
    merged_window_ms: i64,
) -> bool {
    merged_window_ms > 0
        && (session.origin == SessionOrigin::Spawned
            || follow_ups_on_spawned_checkouts.contains(&session.id))
        && merged_pull_requests.contains(&session.id)
}

impl SessionRetirementSummary {
    fn record_holder_evidence(&mut self, evidence: &'static str) {
        *self.closed_by_holder_evidence.entry(evidence).or_default() += 1;
    }

    fn record_close(&mut self, reason: &'static str) {
        *self.closed_by_reason.entry(reason).or_default() += 1;
        crate::phase_telemetry::add(
            match reason {
                crate::session_auto_close::CLOSE_REASON_MERGED_PULL_REQUEST => {
                    "sessions_closed_merged_pull_request_landed"
                }
                crate::session_auto_close::CLOSE_REASON_LANDED => "sessions_closed_landed",
                _ => "sessions_closed_abandoned",
            },
            Duration::ZERO,
        );
    }

    fn record_cursor(&mut self, cursor: &SessionRetirementCursor) {
        self.cursor = Some(SessionRetirementCursorPosition {
            after_session_id: cursor.after_session_id,
            high_water_session_id: cursor.high_water_session_id,
        });
    }
}

fn is_zero_count(count: &usize) -> bool {
    *count == 0
}

fn is_zero_ms(ms: &u64) -> bool {
    *ms == 0
}

/// A closed checkout and every session that names it.
#[derive(Debug, Clone)]
pub(crate) struct AutoCleanupCandidate {
    pub worktree: PathBuf,
    /// The retained spawned session whose creation record proves this path
    /// belongs to the broker. Adopted sessions can refer to the same checkout
    /// without being ownership evidence.
    pub spawned_owner: Session,
    /// The instance identity captured from the GitRepo returned by spawn and
    /// committed with `spawned_owner`. `None` is diagnostic-only and is
    /// refused before containment or removal.
    pub spawned_instance_witness: Option<crate::events::SpawnedWorktreeInstanceWitness>,
    pub sessions: Vec<Session>,
}

/// A complete canonical checkout cohort, before any deletion proof is made.
/// Grouping all retained rows is required so aliases and colocated sessions
/// cannot be split into independent cleanup candidates.
#[derive(Debug, Clone)]
struct AutoCleanupCheckoutGroup {
    worktree: PathBuf,
    sessions: Vec<Session>,
    complete: bool,
    cursor_session_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct ClosedArtifactSessionSnapshot {
    id: i64,
    worktree_path: String,
    branch: String,
    origin: SessionOrigin,
    spawned_instance_witness: Option<crate::events::SpawnedWorktreeInstanceWitness>,
}

/// Complete retained-session identity for one artifact-reclamation checkout.
/// Equality pins the closed canonical cohort and spawn-time owner across the
/// discovery-to-removal window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ClosedArtifactCheckoutCohort {
    worktree: PathBuf,
    sessions: Vec<ClosedArtifactSessionSnapshot>,
    owner_session_id: i64,
    alias_refresh_generation: i64,
    /// Cleaned spawns that earlier occupied this path, each proven removed
    /// before the owner's checkout was created. Marker review only; reclaim
    /// leaves this empty.
    retired_predecessors: Vec<ClosedArtifactSessionSnapshot>,
    /// Cleaned sessions that `--replace-stale` retired while their successor
    /// kept working in this same checkout, each with a replacement chain that
    /// ends at a cohort member. They never owned a separate checkout, so
    /// their aliases do not block the cohort and their markers name this
    /// live checkout. They grant no cleanup authority of their own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    handed_over_predecessors: Vec<i64>,
    /// Set when a spawned row's saved witness matched only through
    /// [`OWNERSHIP_REPROOF_DEVICE_RENUMBER`] or
    /// [`OWNERSHIP_REPROOF_VOLUME_UUID`]. Reclaiming purposes only.
    #[serde(skip_serializing_if = "Option::is_none")]
    ownership_reproof: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosedArtifactCheckoutCohortUnavailableReason {
    IndexMissing,
    IndexIncomplete,
    DeadlineExceeded,
    SessionChanged,
    PathResolutionFailed,
    PathNotUtf8,
    CohortTruncated,
    CohortMembershipChanged,
    NonClosedAlias,
    AliasResolutionFailed,
    AliasOverlapOutsideCohort,
    LiveAliasUnknown,
    CheckoutMissing,
    GitCheckoutUnavailable,
    SpawnedWitnessMismatch,
    /// The saved spawn witness names the same objects under other device
    /// numbers, but the Git half of the ownership re-proof refused.
    OwnershipReproofRefused,
    SpawnedOwnerMissing,
    IndexChanged,
    WitnessChanged,
}

impl ClosedArtifactCheckoutCohortUnavailableReason {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::IndexMissing => "index_missing",
            Self::IndexIncomplete => "index_incomplete",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::SessionChanged => "session_changed",
            Self::PathResolutionFailed => "path_resolution_failed",
            Self::PathNotUtf8 => "path_not_utf8",
            Self::CohortTruncated => "cohort_truncated",
            Self::CohortMembershipChanged => "cohort_membership_changed",
            Self::NonClosedAlias => "non_closed_alias",
            Self::AliasResolutionFailed => "alias_resolution_failed",
            Self::AliasOverlapOutsideCohort => "alias_overlap_outside_cohort",
            Self::LiveAliasUnknown => "live_alias_unknown",
            Self::CheckoutMissing => "checkout_missing",
            Self::GitCheckoutUnavailable => "git_checkout_unavailable",
            Self::SpawnedWitnessMismatch => "spawned_witness_mismatch",
            Self::OwnershipReproofRefused => "ownership_reproof_refused",
            Self::SpawnedOwnerMissing => "spawned_owner_missing",
            Self::IndexChanged => "index_changed",
            Self::WitnessChanged => "witness_changed",
        }
    }
}

/// What a closed checkout cohort will authorize. Marker review removes only
/// artifact-use markers, so it tolerates a renumbered mount on a witness saved
/// without volume UUIDs and never advances the index.
///
/// `Reclaim` (GC: ignored-directory reclamation and the checkout compaction
/// decision), `Retirement` (the retirement plan and archive cleanup a
/// compaction runs, which recheck that cohort) and `CheckoutRemoval`
/// (automatic whole-checkout removal) can delete checkout contents. All
/// three compare the witness exactly and, when that fails, accept only the
/// shared ownership re-proof ([`Broker::checkout_ownership_reproof`]); they
/// build the same cohort and differ only in the lane their telemetry names.
/// Artifact reclamation's final unlink guard re-makes
/// the filesystem half of the same re-proof
/// ([`ClosedArtifactCheckoutCohort::matches_checkout_instance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosedCohortPurpose {
    Reclaim,
    Retirement,
    CheckoutRemoval,
    MarkerReview,
}

impl ClosedCohortPurpose {
    fn reclaims(self) -> bool {
        matches!(
            self,
            Self::Reclaim | Self::Retirement | Self::CheckoutRemoval
        )
    }

    fn reproof_lane(self) -> OwnershipReproofLane {
        match self {
            Self::CheckoutRemoval | Self::MarkerReview => OwnershipReproofLane::Automatic,
            Self::Reclaim => OwnershipReproofLane::Gc,
            Self::Retirement => OwnershipReproofLane::Retirement,
        }
    }
}

/// The re-proof that authorized a reclaim whose saved spawn witness no longer
/// matched exactly: macOS renumbered the mounts of a schema-2 witness.
pub(crate) const OWNERSHIP_REPROOF_DEVICE_RENUMBER: &str = "device_renumber";
/// The re-proof that authorized a reclaim whose saved identity recorded
/// volume UUIDs that still match today while the device numbers changed (a
/// finish-cleanup recovery proof saved before a remount).
pub(crate) const OWNERSHIP_REPROOF_VOLUME_UUID: &str = "volume_uuid";

/// Which deletion lane asked for an ownership re-proof; it names the
/// telemetry counters. Every lane uses the same proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipReproofLane {
    /// Automatic whole-checkout removal (`auto_cleanup_ownership_reproof_*`).
    Automatic,
    /// GC reclamation and checkout compaction (`gc_ownership_reproof_*`).
    Gc,
    /// The retirement plan and archive cleanup a checkout compaction runs,
    /// which recheck the cohort GC chose (`retirement_ownership_reproof_*`).
    Retirement,
    /// `finish` / `finish cleanup` and its interrupted-cleanup recovery
    /// proof (`finish_ownership_reproof_*`).
    Finish,
    /// The same proof made again for a read-only plan, a report or a recheck
    /// of a decision another call already counted: it records nothing, so
    /// each decision and removal is counted once.
    Quiet,
}

impl OwnershipReproofLane {
    /// `(reproved, refused, unavailable, registration phase)` counter names.
    fn counters(self) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
        match self {
            Self::Automatic => Some((
                "auto_cleanup_ownership_reproved",
                "auto_cleanup_ownership_reproof_refused",
                "auto_cleanup_ownership_reproof_unavailable",
                "auto_cleanup_ownership_reproof_registration",
            )),
            Self::Gc => Some((
                "gc_ownership_reproved",
                "gc_ownership_reproof_refused",
                "gc_ownership_reproof_unavailable",
                "gc_ownership_reproof_registration",
            )),
            Self::Retirement => Some((
                "retirement_ownership_reproved",
                "retirement_ownership_reproof_refused",
                "retirement_ownership_reproof_unavailable",
                "retirement_ownership_reproof_registration",
            )),
            Self::Finish => Some((
                "finish_ownership_reproved",
                "finish_ownership_reproof_refused",
                "finish_ownership_reproof_unavailable",
                "finish_ownership_reproof_registration",
            )),
            Self::Quiet => None,
        }
    }

    /// Count one accepted re-proof in this lane.
    pub(crate) fn record_reproved(self) {
        if let Some((reproved, ..)) = self.counters() {
            crate::phase_telemetry::add(reproved, Duration::ZERO);
        }
    }

    /// Count one refusal in this lane and under its reason's own counter.
    pub(crate) fn record_refused(self, reason: &str) {
        if let Some((_, refused, ..)) = self.counters() {
            crate::phase_telemetry::add(refused, Duration::ZERO);
            crate::phase_telemetry::add(ownership_reproof_refusal_counter(reason), Duration::ZERO);
        }
    }

    fn record_unavailable(self) {
        if let Some((_, _, unavailable, _)) = self.counters() {
            crate::phase_telemetry::add(unavailable, Duration::ZERO);
        }
    }

    fn record_registration(self, elapsed: Duration) {
        if let Some((.., registration)) = self.counters() {
            crate::phase_telemetry::add(registration, elapsed);
        }
    }
}

/// Refusal of a finish orphan recovery proof saved without volume UUIDs:
/// Git has already deregistered the checkout, so the registration half of
/// the re-proof cannot be made.
pub(crate) const REPROOF_ORPHAN_LEGACY_PROOF: &str =
    "a finish proof without volume UUIDs cannot be re-proven after Git deregistered the checkout";

/// Every ownership re-proof refusal reason and the counter it adds, so each
/// refusal is countable on its own.
const OWNERSHIP_REPROOF_REFUSAL_COUNTERS: &[(&str, &str)] = &[
    (
        crate::events::IDENTITY_SAVED_UNSUPPORTED,
        "ownership_reproof_refused_saved_unsupported",
    ),
    (
        crate::events::IDENTITY_CURRENT_UNSUPPORTED,
        "ownership_reproof_refused_current_unsupported",
    ),
    (
        crate::events::IDENTITY_PATH_CHANGED,
        "ownership_reproof_refused_path_changed",
    ),
    (
        crate::events::IDENTITY_GENERATION_CHANGED,
        "ownership_reproof_refused_generation_changed",
    ),
    (
        crate::events::IDENTITY_INODE_CHANGED,
        "ownership_reproof_refused_inode_changed",
    ),
    (
        crate::events::IDENTITY_VOLUME_UUID_DIFFERS,
        "ownership_reproof_refused_volume_uuid_differs",
    ),
    (
        crate::events::IDENTITY_VOLUME_UUID_UNAVAILABLE,
        "ownership_reproof_refused_volume_uuid_unavailable",
    ),
    (
        crate::events::IDENTITY_DEVICE_SHARING_CHANGED,
        "ownership_reproof_refused_device_sharing_changed",
    ),
    (
        REPROOF_NOTHING_TO_REPROVE,
        "ownership_reproof_refused_nothing_to_reprove",
    ),
    (REPROOF_NOT_SPAWNED, "ownership_reproof_refused_not_spawned"),
    (REPROOF_NOT_CLOSED, "ownership_reproof_refused_not_closed"),
    (
        REPROOF_SESSION_PATH,
        "ownership_reproof_refused_session_path",
    ),
    (
        REPROOF_OUTSIDE_BROKER_ROOT,
        "ownership_reproof_refused_outside_broker_root",
    ),
    (
        REPROOF_OTHER_REPOSITORY,
        "ownership_reproof_refused_other_repository",
    ),
    (
        REGISTRATION_REFUSAL,
        "ownership_reproof_refused_not_registered",
    ),
    (
        REPROOF_NOT_ON_SESSION_BRANCH,
        "ownership_reproof_refused_not_on_session_branch",
    ),
    (
        REPROOF_HEAD_NOT_BRANCH_TIP,
        "ownership_reproof_refused_head_not_branch_tip",
    ),
    (REPROOF_HEAD_MOVED, "ownership_reproof_refused_head_moved"),
    (
        REPROOF_ORPHAN_LEGACY_PROOF,
        "ownership_reproof_refused_orphan_legacy_proof",
    ),
];

/// The counter of one re-proof refusal reason.
pub(crate) fn ownership_reproof_refusal_counter(reason: &str) -> &'static str {
    OWNERSHIP_REPROOF_REFUSAL_COUNTERS
        .iter()
        .find(|(known, _)| *known == reason)
        .map_or("ownership_reproof_refused_other", |(_, counter)| counter)
}

/// Keep-reason phrase for a spawned row that never persisted a witness.
const OWNERSHIP_WITNESS_NEVER_RECORDED: &str =
    "spawn-time worktree instance witness was never recorded";
/// Keep-reason phrase for a schema-1 witness, which has no spawn generation.
const OWNERSHIP_WITNESS_LEGACY: &str =
    "spawn-time worktree instance witness is a legacy record without a spawn generation";
/// Keep-reason phrase for a checkout only adopted sessions use.
const OWNERSHIP_ADOPTED_ONLY: &str = "only adopted sessions use this checkout";
/// Keep-reason phrase for a closed checkout whose directory is gone.
const CHECKOUT_DIRECTORY_MISSING: &str = "the checkout directory no longer exists";

/// What the device-renumber re-proof observes beyond the two witnesses. Each
/// field is one condition, gathered by
/// [`Broker::renumbered_witness_reproof`]; [`renumber_reproof_refusal`]
/// decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenumberReproofEvidence {
    /// The session row was created by `start` (origin `spawned`).
    pub origin_spawned: bool,
    /// The session row is closed and its checkout not yet cleaned.
    pub closed: bool,
    /// The session row names this exact checkout path.
    pub session_path_matches: bool,
    /// The checkout sits directly under the broker-owned worktree root and
    /// its Git common directory is this repository's
    /// ([`Broker::is_broker_owned_worktree_checked`]).
    pub under_broker_root: bool,
    /// The live `.git` pointer resolves into this repository's common
    /// directory.
    pub common_dir_is_this_repository: bool,
    /// This repository registers the checkout: its Git admin directory is
    /// `<common>/worktrees/<name>` and that registration's `gitdir` names the
    /// checkout's own `.git` file, the record `git worktree list` reads. A
    /// registration whose `gitdir` target is gone (prunable) is not one.
    pub registered: bool,
    /// The branch the registration's `HEAD` has checked out.
    pub checkout_branch: Option<String>,
    /// The commit that `HEAD` resolves to through this repository's refs.
    pub checkout_head: Option<String>,
    /// The tip of `refs/heads/<session branch>` in this repository. When
    /// `HEAD` is a symbolic ref to that branch, both resolve the same ref, so
    /// this alone only proves the branch resolves to a commit.
    pub branch_tip: Option<String>,
    /// A commit the broker recorded for this checkout before the identity
    /// changed (the finish recovery proof's head), when one exists. `HEAD`
    /// must still be that commit. Witness lanes have no such record.
    pub recorded_head: Option<String>,
}

const REGISTRATION_REFUSAL: &str = "this repository does not register the checkout";
const REPROOF_NOTHING_TO_REPROVE: &str = "the identity matches exactly, so nothing was re-proven";
const REPROOF_NOT_SPAWNED: &str = "the session was not spawned by the broker";
const REPROOF_NOT_CLOSED: &str = "the session is not closed";
const REPROOF_SESSION_PATH: &str = "the session row names another path";
const REPROOF_OUTSIDE_BROKER_ROOT: &str = "the checkout is not under the broker worktree root";
const REPROOF_OTHER_REPOSITORY: &str = "the checkout's .git does not point into this repository";
const REPROOF_NOT_ON_SESSION_BRANCH: &str = "the checkout is not on the session branch";
const REPROOF_HEAD_NOT_BRANCH_TIP: &str = "the checkout HEAD is not the session branch tip";
const REPROOF_HEAD_MOVED: &str = "the checkout HEAD moved from the commit the broker recorded";

/// Decide the device-renumber re-proof. `None` means every condition holds;
/// otherwise the first failed condition, which keeps the checkout.
#[cfg(test)]
pub(crate) fn renumber_reproof_refusal(
    saved: &crate::events::SpawnedWorktreeInstanceWitness,
    current: &crate::events::SpawnedWorktreeInstanceWitness,
    session_branch: &str,
    evidence: &RenumberReproofEvidence,
) -> Option<&'static str> {
    ownership_reproof_decision(saved, current, session_branch, evidence).err()
}

/// Decide the ownership re-proof every deletion lane shares. On success,
/// the re-proof that held ([`OWNERSHIP_REPROOF_DEVICE_RENUMBER`] for a
/// record saved without volume UUIDs, [`OWNERSHIP_REPROOF_VOLUME_UUID`] when
/// every recorded UUID still matches); otherwise the first failed condition,
/// which keeps the checkout. An exact identity is refused here because it
/// needs no re-proof: callers compare exactly first.
pub(crate) fn ownership_reproof_decision(
    saved: &crate::events::SpawnedWorktreeInstanceWitness,
    current: &crate::events::SpawnedWorktreeInstanceWitness,
    session_branch: &str,
    evidence: &RenumberReproofEvidence,
) -> Result<&'static str, &'static str> {
    use crate::events::CheckoutIdentityMatch;
    let reproof = match saved.compare_checkout_identity(current) {
        CheckoutIdentityMatch::DeviceRenumber => OWNERSHIP_REPROOF_DEVICE_RENUMBER,
        CheckoutIdentityMatch::VolumeUuid => OWNERSHIP_REPROOF_VOLUME_UUID,
        CheckoutIdentityMatch::Exact => return Err(REPROOF_NOTHING_TO_REPROVE),
        CheckoutIdentityMatch::Differs(condition) => return Err(condition),
    };
    if !evidence.origin_spawned {
        return Err(REPROOF_NOT_SPAWNED);
    }
    if !evidence.closed {
        return Err(REPROOF_NOT_CLOSED);
    }
    if !evidence.session_path_matches {
        return Err(REPROOF_SESSION_PATH);
    }
    if !evidence.under_broker_root {
        return Err(REPROOF_OUTSIDE_BROKER_ROOT);
    }
    if !evidence.common_dir_is_this_repository {
        return Err(REPROOF_OTHER_REPOSITORY);
    }
    if !evidence.registered {
        return Err(REGISTRATION_REFUSAL);
    }
    if session_branch.is_empty() || evidence.checkout_branch.as_deref() != Some(session_branch) {
        return Err(REPROOF_NOT_ON_SESSION_BRANCH);
    }
    match (&evidence.checkout_head, &evidence.branch_tip) {
        (Some(head), Some(tip)) if !head.is_empty() && head == tip => {}
        _ => return Err(REPROOF_HEAD_NOT_BRANCH_TIP),
    }
    match &evidence.recorded_head {
        Some(recorded) if evidence.checkout_head.as_ref() != Some(recorded) => {
            Err(REPROOF_HEAD_MOVED)
        }
        _ => Ok(reproof),
    }
}

/// Why a device-renumber re-proof did not authorize a reclaim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReproofFailure {
    /// A condition failed: keep the checkout, with this reason.
    Refused(String),
    /// Evidence could not be gathered in time (a bounded Git call ran out
    /// or failed): judge it again later, never as a refusal.
    Unavailable(String),
}

/// Whether the admin directory `admin` is this repository's registration of
/// `worktree`: it lives in `<common>/worktrees/` and its `gitdir` file names
/// the checkout's existing `.git` file. Reads two paths; runs no Git process.
fn registration_names_checkout(common: &Path, admin: &Path, worktree: &Path) -> bool {
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    let Some(admin) = canonical(admin) else {
        return false;
    };
    let Some(registrations) = canonical(&common.join("worktrees")) else {
        return false;
    };
    if admin.parent() != Some(registrations.as_path()) {
        return false;
    }
    let Ok(gitdir) = std::fs::read_to_string(admin.join("gitdir")) else {
        return false;
    };
    let pointer = worktree.join(".git");
    if !std::fs::symlink_metadata(&pointer).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    match (canonical(Path::new(gitdir.trim())), canonical(&pointer)) {
        (Some(named), Some(pointer)) => named == pointer,
        _ => false,
    }
}

/// Resolve `refname` (`refs/heads/...`) to a commit from the loose ref or
/// `packed-refs` in `common`. Anything else (a symbolic or malformed ref,
/// another ref backend) is `None`, which refuses the re-proof.
fn resolve_branch_ref_in_common(common: &Path, refname: &str) -> Option<String> {
    let valid_id = |value: &str| {
        matches!(value.len(), 40 | 64)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    let branch = refname.strip_prefix("refs/heads/")?;
    if branch.is_empty()
        || Path::new(branch)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    if let Ok(loose) = std::fs::read_to_string(common.join(refname)) {
        let loose = loose.trim();
        return valid_id(loose).then(|| loose.to_string());
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        if line.starts_with('#') || line.starts_with('^') {
            return None;
        }
        let (id, name) = line.split_once(' ')?;
        (name == refname && valid_id(id)).then(|| id.to_string())
    })
}

/// What one registration's `HEAD` file says: the branch it has checked out
/// (`None` when detached) and the commit it resolves to.
fn registration_head(common: &Path, admin: &Path) -> (Option<String>, Option<String>) {
    let Ok(head) = std::fs::read_to_string(admin.join("HEAD")) else {
        return (None, None);
    };
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(refname) => (
            refname.strip_prefix("refs/heads/").map(str::to_string),
            resolve_branch_ref_in_common(common, refname),
        ),
        None => (
            None,
            (matches!(head.len(), 40 | 64)
                && head
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            .then(|| head.to_string()),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClosedArtifactCheckoutCohortResolution {
    Available(ClosedArtifactCheckoutCohort),
    Unavailable(ClosedArtifactCheckoutCohortUnavailableReason),
}

impl ClosedArtifactCheckoutCohort {
    pub(crate) fn owner_session_id(&self) -> i64 {
        self.owner_session_id
    }

    pub(crate) fn alias_refresh_generation(&self) -> i64 {
        self.alias_refresh_generation
    }

    pub(crate) fn worktree(&self) -> &Path {
        &self.worktree
    }

    pub(crate) fn session_ids(&self) -> Vec<i64> {
        self.sessions.iter().map(|session| session.id).collect()
    }

    /// Whether `session_id` worked in this live checkout: a cohort member, or
    /// a predecessor `--replace-stale` handed it over from.
    pub(crate) fn worked_in_live_checkout(&self, session_id: i64) -> bool {
        self.sessions.iter().any(|session| session.id == session_id)
            || self.handed_over_predecessors.contains(&session_id)
    }

    /// The saved spawn witness of a cleaned predecessor proven removed before
    /// this checkout instance. Its markers named that old checkout object.
    pub(crate) fn retired_predecessor_witness(
        &self,
        session_id: i64,
    ) -> Option<&crate::events::SpawnedWorktreeInstanceWitness> {
        self.retired_predecessors
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.spawned_instance_witness.as_ref())
    }

    /// The re-proof this cohort's ownership needed, if the saved witness did
    /// not match exactly.
    pub(crate) fn ownership_reproof(&self) -> Option<&'static str> {
        self.ownership_reproof
    }

    pub(crate) fn owner_witness(&self) -> Option<&crate::events::SpawnedWorktreeInstanceWitness> {
        self.sessions
            .iter()
            .find(|session| session.id == self.owner_session_id)
            .and_then(|session| session.spawned_instance_witness.as_ref())
    }

    /// Whether a spawn witness in this verified cohort saw `device` on this
    /// repository's own `.git`. It is durable evidence that a legacy marker's
    /// saved primary device number named this repository's volume.
    pub(crate) fn witnesses_primary_git_device(&self, primary_root: &Path, device: u64) -> bool {
        let git_common = primary_root.join(".git");
        self.sessions.iter().any(|session| {
            session
                .spawned_instance_witness
                .as_ref()
                .is_some_and(|witness| {
                    witness.git_common_device == device
                        && Path::new(&witness.git_common_path) == git_common
                })
        })
    }

    /// Whether a spawn witness in this verified cohort saw `device` on this
    /// exact checkout root object. A legacy marker saved the checkout's
    /// mount-scoped device number; macOS renumbers it on remount, so the
    /// witness, not the live mount, is the evidence that it named this checkout.
    pub(crate) fn witnesses_checkout_root_device(
        &self,
        checkout_root: &Path,
        device: u64,
        inode: u64,
    ) -> bool {
        self.sessions.iter().any(|session| {
            session
                .spawned_instance_witness
                .as_ref()
                .is_some_and(|witness| {
                    witness.worktree_device == device
                        && witness.worktree_inode == inode
                        && Path::new(&witness.worktree_path) == checkout_root
                })
        })
    }

    /// The final mutation guard: the checkout is still the instance this
    /// cohort proved. A cohort that needed an ownership re-proof accepts only
    /// the same re-proof's filesystem half again (unchanged paths, inodes and
    /// root generation under the same kind of device change); its Git half
    /// was made when the cohort was built, moments before.
    pub(crate) fn matches_checkout_instance(&self, checkout: &GitRepo) -> bool {
        self.worktree.as_path() == checkout.root()
            && self
                .sessions
                .iter()
                .find(|session| session.id == self.owner_session_id)
                .and_then(|session| session.spawned_instance_witness.as_ref())
                .is_some_and(|expected| {
                    checkout
                        .spawned_worktree_instance_witness()
                        .as_ref()
                        .is_some_and(|current| {
                            cohort_identity_still_holds(self.ownership_reproof, expected, current)
                        })
                })
    }
}

/// The final-guard rule of [`ClosedArtifactCheckoutCohort::matches_checkout_instance`]:
/// an exact witness match, or the same kind of re-proof the cohort recorded.
/// A cohort proven exactly never accepts a later renumber, and a re-proven
/// cohort never accepts another kind of device change or changed objects.
fn cohort_identity_still_holds(
    cohort_reproof: Option<&'static str>,
    expected: &crate::events::SpawnedWorktreeInstanceWitness,
    current: &crate::events::SpawnedWorktreeInstanceWitness,
) -> bool {
    use crate::events::CheckoutIdentityMatch;
    expected.matches_current_instance(current)
        || matches!(
            (cohort_reproof, expected.compare_checkout_identity(current)),
            (
                Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER),
                CheckoutIdentityMatch::DeviceRenumber
            ) | (
                Some(OWNERSHIP_REPROOF_VOLUME_UUID),
                CheckoutIdentityMatch::VolumeUuid
            )
        )
}

fn proves_removed_historical_instance(
    alias: &Session,
    owner: &Session,
    old: &crate::events::SpawnedWorktreeInstanceWitness,
    current: &crate::events::SpawnedWorktreeInstanceWitness,
    handoff: &crate::SessionHandoffReport,
) -> bool {
    let valid_generation = |value: &Option<String>| {
        value.as_ref().is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    };
    alias.id != owner.id
        && alias.origin == SessionOrigin::Spawned
        && owner.origin == SessionOrigin::Spawned
        && alias.status == crate::types::SessionStatus::Cleaned
        && alias.cleanup_state == SessionCleanupState::Cleaned
        && handoff.handoff.session_id == alias.id
        && matches!(
            handoff.handoff.status,
            crate::FinishStatus::Cleaned | crate::FinishStatus::AlreadyCleaned
        )
        && handoff.handoff.cleanup.completed
        && handoff.handoff.cleanup.worktree_removed
        && !handoff.handoff.cleanup.kept
        && handoff.handoff.cleanup.recovery_action.is_none()
        && alias
            .cleanup_completed_at
            .is_some_and(|at| at >= alias.created_at && at <= handoff.recorded_at)
        && handoff.recorded_at < owner.created_at
        && old.is_supported()
        && current.is_supported()
        && old.worktree_path == current.worktree_path
        && old.worktree_path == alias.worktree_path
        && current.worktree_path == owner.worktree_path
        && old.same_git_common(current)
        && valid_generation(&old.root_generation_sha256)
        && valid_generation(&current.root_generation_sha256)
        && old.root_generation_sha256 != current.root_generation_sha256
        && !old.same_checkout_objects(current)
}

struct AutoCleanupGroupScan {
    groups: Vec<AutoCleanupCheckoutGroup>,
    live: Vec<Session>,
    live_worktrees: Vec<ResolvedWorktreePath>,
    unresolved_rows: i64,
    alias_refresh_generation: i64,
}

enum AutoCleanupGroupInspection {
    Candidate(Box<AutoCleanupCandidate>),
    Kept(AutoKeptCheckout),
    Deferred,
}

impl AutoCleanupCandidate {
    fn session_ids(&self) -> Vec<i64> {
        self.sessions.iter().map(|session| session.id).collect()
    }

    fn branch(&self) -> &str {
        self.spawned_owner.branch.as_str()
    }
}

/// Group all extant retained rows by canonical checkout path. This pass is
/// intentionally complete: a partial cohort is never deletion authority.
#[cfg(test)]
fn group_closed_checkout_sessions(
    closed: Vec<Session>,
    deadline: Option<Instant>,
) -> Option<Vec<AutoCleanupCheckoutGroup>> {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut by_path: std::collections::BTreeMap<PathBuf, Vec<Session>> =
        std::collections::BTreeMap::new();
    for session in closed {
        if deadline_expired(deadline) {
            return None;
        }
        if session.cleanup_state == SessionCleanupState::Cleaned {
            continue;
        }
        let path = PathBuf::from(&session.worktree_path);
        if !path.is_dir() {
            if deadline_expired(deadline) {
                return None;
            }
            continue;
        }
        let canonical_path = canonical(&path);
        if deadline_expired(deadline) {
            return None;
        }
        by_path.entry(canonical_path).or_default().push(session);
    }
    if deadline_expired(deadline) {
        return None;
    }
    Some(
        by_path
            .into_iter()
            .map(|(worktree, sessions)| AutoCleanupCheckoutGroup {
                worktree,
                cursor_session_id: sessions.iter().map(|session| session.id).min().unwrap_or(0),
                sessions,
                complete: true,
            })
            .collect(),
    )
}

/// Checkouts selected for removal, not yet removed.
#[derive(Debug, Clone)]
pub struct AutoCleanupPlan {
    report: AutoCleanupReport,
    policy: AutoCleanupPolicy,
    upstream: Option<(String, String)>,
    selected: Vec<(AutoCleanupCandidate, ContainmentProof)>,
    /// The selection's deadline, which removal honors too: a pass inside a
    /// broker open must not pay unbounded removals after a bounded selection.
    deadline: Option<Instant>,
    /// The complete filesystem alias lap used for selection.
    alias_refresh_generation: Option<i64>,
    /// Revalidation before removal honors `deadline` too: a selected
    /// checkout that cannot be revalidated in time is deferred, not removed.
    /// Set for the background worker's capacity passes; elsewhere the first
    /// selected checkout is revalidated without a deadline so a pass whose
    /// selection spent its budget still converges.
    bound_revalidation: bool,
}

impl AutoCleanupPlan {
    fn nothing(report: AutoCleanupReport) -> Self {
        Self {
            report,
            policy: AutoCleanupPolicy::default(),
            upstream: None,
            selected: Vec::new(),
            deadline: None,
            alias_refresh_generation: None,
            bound_revalidation: false,
        }
    }

    /// The worktrees this plan would remove if nothing changes.
    pub fn selected_worktrees(&self) -> Vec<String> {
        self.selected
            .iter()
            .map(|(candidate, _)| candidate.worktree.to_string_lossy().into_owned())
            .collect()
    }

    /// Replace the deadline removal honors.
    ///
    /// Production plans carry the selection's deadline. This hook lets tests
    /// put removal past its budget without racing the wall clock.
    #[doc(hidden)]
    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = deadline;
        self
    }
}

/// The proof a candidate passed, as of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainmentProof {
    pub head: String,
    pub containing_commit: String,
    pub proof: String,
    /// Tracked paths proof 3 found already deleted by an interrupted
    /// removal; 0 for an intact checkout.
    pub interrupted_removal_deletions: usize,
    /// The ownership re-proof the verdict needed, if the saved spawn witness
    /// did not match exactly ([`OWNERSHIP_REPROOF_DEVICE_RENUMBER`]).
    pub ownership_reproof: Option<&'static str>,
}

/// Why a candidate is not removed in this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotRemoved {
    Keep(String),
    /// The deep proof needs time this pass does not have.
    Defer,
}

/// The canonical git directory a checkout's `.git` resolves to. A missing
/// `.git` target is known absence; permission, malformed metadata and other
/// I/O failures remain unknown so they cannot authorize cleanup.
fn checkout_git_dir_checked(worktree: &Path) -> Result<Option<PathBuf>, std::io::Error> {
    crate::git::checked_worktree_git_dir(worktree)
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedWorktreePath {
    raw: PathBuf,
    canonical_or_raw: PathBuf,
    git_dir: Option<PathBuf>,
    resolution_errors: Vec<String>,
}

impl ResolvedWorktreePath {
    pub(crate) fn resolve(path: &Path) -> Self {
        let (canonical_or_raw, path_error) = match std::fs::canonicalize(path) {
            Ok(path) => (path, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Preserve the prior raw-path fallback for a genuinely missing
                // live checkout while keeping non-missing errors distinguishable.
                (path.to_path_buf(), None)
            }
            Err(error) => (
                path.to_path_buf(),
                Some(format!("worktree path could not be resolved: {error}")),
            ),
        };
        let (git_dir, git_error) = match checkout_git_dir_checked(path) {
            Ok(git_dir) => (git_dir, None),
            Err(error) => (
                None,
                Some(format!("Git directory could not be resolved: {error}")),
            ),
        };
        let resolution_errors = path_error.into_iter().chain(git_error).collect();
        Self {
            raw: path.to_path_buf(),
            canonical_or_raw,
            git_dir,
            resolution_errors,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveOverlapEvidence {
    Disjoint,
    Overlap,
    Unknown,
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Resolve every live path once, retaining the store's order so a candidate
/// reports the same first matching session as the former per-pair check. An
/// expired or partial index is never used to authorize cleanup.
pub(crate) fn resolve_live_worktree_paths(
    live: &[Session],
    deadline: Option<Instant>,
) -> Option<Vec<ResolvedWorktreePath>> {
    resolve_worktree_paths_until(
        live.iter().map(|session| Path::new(&session.worktree_path)),
        || deadline_expired(deadline),
        ResolvedWorktreePath::resolve,
    )
}

fn resolve_worktree_paths_until<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
    mut expired: impl FnMut() -> bool,
    mut resolve: impl FnMut(&Path) -> ResolvedWorktreePath,
) -> Option<Vec<ResolvedWorktreePath>> {
    let mut resolved = Vec::new();
    for path in paths {
        if expired() {
            return None;
        }
        resolved.push(resolve(path));
        if expired() {
            return None;
        }
    }
    Some(resolved)
}

pub(crate) fn live_worktree_overlap(
    candidate: &ResolvedWorktreePath,
    live: &ResolvedWorktreePath,
) -> LiveOverlapEvidence {
    let candidate_paths = [&candidate.raw, &candidate.canonical_or_raw];
    let live_paths = [&live.raw, &live.canonical_or_raw];
    let paths_overlap = candidate_paths.iter().any(|candidate_path| {
        live_paths.iter().any(|live_path| {
            live_path.starts_with(candidate_path) || candidate_path.starts_with(live_path)
        })
    });
    let git_identity_matches = candidate
        .git_dir
        .as_ref()
        .zip(live.git_dir.as_ref())
        .is_some_and(|(candidate_git_dir, live_git_dir)| candidate_git_dir == live_git_dir);
    if paths_overlap || git_identity_matches {
        LiveOverlapEvidence::Overlap
    } else if !candidate.resolution_errors.is_empty() || !live.resolution_errors.is_empty() {
        LiveOverlapEvidence::Unknown
    } else {
        LiveOverlapEvidence::Disjoint
    }
}

fn first_live_worktree_overlap(
    candidate: &ResolvedWorktreePath,
    live: &[ResolvedWorktreePath],
) -> Option<(usize, LiveOverlapEvidence)> {
    live.iter().enumerate().find_map(
        |(index, live)| match live_worktree_overlap(candidate, live) {
            LiveOverlapEvidence::Disjoint => None,
            evidence => Some((index, evidence)),
        },
    )
}

pub(crate) fn overlap_resolution_error(
    candidate: &ResolvedWorktreePath,
    live: &ResolvedWorktreePath,
) -> String {
    candidate
        .resolution_errors
        .iter()
        .chain(&live.resolution_errors)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// The most output one proof-3 listing may print before it counts as failed.
const PROOF_GIT_OUTPUT_LIMIT: usize = 256 * 1024 * 1024;
/// Ignored entries one combined cleanliness listing may report before the
/// checkout counts as unknown (the ignored-path listing's own cap).
const PROOF_IGNORED_ENTRY_LIMIT: usize = 16_384;
/// The longest ignored path one combined listing may report.
const PROOF_IGNORED_PATH_BYTES: usize = 4_096;

#[cfg(test)]
thread_local! {
    /// Test seam: the script proof-3 listings run (with `/bin/sh`, so a
    /// freshly written script is never executed directly) instead of Git.
    static PROOF_GIT_PROGRAM_FOR_TEST: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// Test seam: the output limit of a proof-3 listing.
    static PROOF_GIT_OUTPUT_LIMIT_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Restores the previous proof-3 listing seams when dropped.
#[cfg(test)]
pub(crate) struct ProofGitSeam(Option<PathBuf>, Option<usize>);

#[cfg(test)]
impl Drop for ProofGitSeam {
    fn drop(&mut self) {
        PROOF_GIT_PROGRAM_FOR_TEST.with(|cell| cell.replace(self.0.take()));
        PROOF_GIT_OUTPUT_LIMIT_FOR_TEST.with(|cell| cell.set(self.1));
    }
}

/// Run this thread's proof-3 listings as `/bin/sh <script> <git args>`
/// instead of Git and/or with `output_limit`, until the guard drops.
#[cfg(test)]
pub(crate) fn proof_git_seam_for_test(
    script: Option<PathBuf>,
    output_limit: Option<usize>,
) -> ProofGitSeam {
    ProofGitSeam(
        PROOF_GIT_PROGRAM_FOR_TEST.with(|cell| cell.replace(script)),
        PROOF_GIT_OUTPUT_LIMIT_FOR_TEST.with(|cell| cell.replace(output_limit)),
    )
}

fn proof_git_command(dir: &Path, args: &[&str]) -> std::process::Command {
    #[cfg(test)]
    if let Some(script) = PROOF_GIT_PROGRAM_FOR_TEST.with(|cell| cell.borrow().clone()) {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg(script).args(args).current_dir(dir);
        return command;
    }
    crate::git::readonly_listing_command(dir, args)
}

fn proof_git_output_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = PROOF_GIT_OUTPUT_LIMIT_FOR_TEST.with(std::cell::Cell::get) {
        return limit;
    }
    PROOF_GIT_OUTPUT_LIMIT
}

/// How long a proof-3 listing may run: until the thread's active Git
/// deadline, never longer than the general Git limit
/// (`AETHYME_GIT_TIMEOUT_SECS`, ten minutes by default); zero once the
/// deadline has passed.
fn proof_git_budget() -> Duration {
    match crate::git::active_git_deadline() {
        Some(deadline) if deadline <= Instant::now() => Duration::ZERO,
        _ => crate::git::git_timeout(),
    }
}

/// What one proof-3 listing printed.
enum ProofListing {
    /// The whole listing.
    Complete(String),
    /// The listing up to the record the caller was looking for; the process
    /// group was stopped there.
    Matched(String),
}

#[cfg(unix)]
fn capture_proof_git(
    command: &mut std::process::Command,
    budget: Duration,
    limit: usize,
    matched: &mut dyn FnMut(&[u8]) -> bool,
) -> std::io::Result<crate::bounded_output::OwnedGroupEnd> {
    crate::bounded_output::output_limited_in_owned_group_until_matched(
        command, budget, limit, matched,
    )
}

#[cfg(not(unix))]
fn capture_proof_git(
    command: &mut std::process::Command,
    budget: Duration,
    limit: usize,
    _matched: &mut dyn FnMut(&[u8]) -> bool,
) -> std::io::Result<crate::bounded_output::OwnedGroupEnd> {
    crate::bounded_output::output_limited(command, budget, limit)
        .map(crate::bounded_output::OwnedGroupEnd::Output)
}

/// One proof-3 Git listing (`status`, `rev-parse`, `stash list`,
/// `ls-files`). It runs in a process group of its own, stops at the
/// thread's active Git deadline ([`crate::git::limit_git_until`]; the
/// spawn-lock wait counts against it), is timed as `subprocess_git`, and
/// fails rather than answers when stopped: every caller treats a failed
/// listing as keep. A stopped listing takes its whole group with it, so a
/// submodule `git status` it started is not left running. `matched` may end
/// it early, once the stdout printed so far holds what the caller needs.
fn run_proof_git(
    dir: &Path,
    args: &[&str],
    matched: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<ProofListing, String> {
    use crate::bounded_output::{LimitedOutput, OwnedGroupEnd};
    let budget = proof_git_budget();
    if budget.is_zero() {
        crate::phase_telemetry::add("proof_git_deadline_before_start", Duration::ZERO);
        return Err(format!(
            "git {}: the time budget ran out before it started",
            args.join(" ")
        ));
    }
    let limit = proof_git_output_limit();
    let mut command = proof_git_command(dir, args);
    match capture_proof_git(&mut command, budget, limit, matched) {
        Ok(OwnedGroupEnd::Output(LimitedOutput::Completed(output))) => {
            if output.status.success() {
                Ok(ProofListing::Complete(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                ))
            } else {
                Err(format!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        }
        Ok(OwnedGroupEnd::Matched(stdout)) => {
            crate::phase_telemetry::add("proof_git_stopped_at_first_record", Duration::ZERO);
            Ok(ProofListing::Matched(
                String::from_utf8_lossy(&stdout).into_owned(),
            ))
        }
        Ok(OwnedGroupEnd::Output(LimitedOutput::Deadline) | OwnedGroupEnd::Stopped) => {
            crate::phase_telemetry::add("proof_git_stopped_at_deadline", budget);
            Err(format!(
                "git {}: stopped at its time budget",
                args.join(" ")
            ))
        }
        Ok(OwnedGroupEnd::Output(LimitedOutput::OutputLimit)) => {
            crate::phase_telemetry::add("proof_git_output_limit", Duration::ZERO);
            Err(format!(
                "git {}: output exceeded {limit} bytes",
                args.join(" ")
            ))
        }
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}

/// [`run_proof_git`] for the whole listing.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    match run_proof_git(dir, args, &mut |_: &[u8]| false)? {
        ProofListing::Complete(listing) | ProofListing::Matched(listing) => Ok(listing),
    }
}

/// The records of a `git status --porcelain=v1 -z` listing as
/// `(status letters, path)`, complete fields only (a listing cut mid-record
/// yields the records before the cut); a rename's or copy's source field is
/// consumed with its record.
fn status_records(listing: &str) -> Vec<(&str, &str)> {
    let complete = match listing.rfind('\0') {
        Some(end) => &listing[..end],
        None => return Vec::new(),
    };
    let mut records = Vec::new();
    let mut fields = complete.split('\0');
    while let Some(field) = fields.next() {
        if field.len() < 4 || !field.is_char_boundary(2) || !field.is_char_boundary(3) {
            continue;
        }
        let (code, path) = (&field[..2], &field[3..]);
        if code.contains('R') || code.contains('C') {
            fields.next();
        }
        records.push((code, path));
    }
    records
}

/// Whether one status record is somebody's change: anything but an ignored
/// entry, and (outside strict cleanup) Finder metadata.
fn status_record_counts(code: &str, path: &str, strict: bool) -> bool {
    if code == "!!" {
        return false;
    }
    strict || !(code == "??" && path.rsplit('/').next() == Some(".DS_Store"))
}

/// The first record of a status listing that is somebody's change.
fn first_counting_record(listing: &str, strict: bool) -> Option<String> {
    status_records(listing)
        .into_iter()
        .find(|(code, path)| status_record_counts(code, path, strict))
        .map(|(_, path)| path.to_string())
}

/// The early-stop test for a streamed `status -z` listing, read
/// incrementally: each call scans only the bytes after the last complete
/// field it read, so a long listing costs one pass, not one per read.
#[derive(Debug)]
struct CountingRecordScan {
    strict: bool,
    /// Bytes of complete fields already read.
    scanned: usize,
    /// The next field is a rename's or copy's source, not a record.
    skip_next: bool,
}

impl CountingRecordScan {
    fn new(strict: bool) -> Self {
        Self {
            strict,
            scanned: 0,
            skip_next: false,
        }
    }

    /// Whether `stdout` (the whole listing so far) holds a record that is
    /// somebody's change; the same rule as [`status_record_counts`].
    fn holds_counting_record(&mut self, stdout: &[u8]) -> bool {
        while let Some(length) = stdout
            .get(self.scanned..)
            .and_then(|rest| rest.iter().position(|byte| *byte == 0))
        {
            let field = &stdout[self.scanned..self.scanned + length];
            self.scanned += length + 1;
            if std::mem::take(&mut self.skip_next) || field.len() < 4 {
                continue;
            }
            let (code, path) = (&field[..2], &field[3..]);
            if code.contains(&b'R') || code.contains(&b'C') {
                self.skip_next = true;
            }
            if code == b"!!" {
                continue;
            }
            let finder_metadata = code == b"??"
                && path.rsplit(|byte| *byte == b'/').next() == Some(&b".DS_Store"[..]);
            if self.strict || !finder_metadata {
                return true;
            }
        }
        false
    }
}

/// The keep reason of a listing ended at its first change.
fn first_change_reason(listing: &str, strict: bool) -> String {
    format!(
        "at least 1 uncommitted or untracked path(s), first: {}",
        first_counting_record(listing, strict).unwrap_or_default()
    )
}

/// Proof 3: nothing in the checkout is somebody's unrecorded work.
///
/// `Ok(None)` is clean; `Ok(Some(reason))` names what keeps it; `Err` is a
/// listing that failed, which the caller also treats as keep. One
/// `git status --ignored=matching` pass answers both the uncommitted and the
/// ignored question, so a clean checkout is walked once; it stops at the
/// first change, so a dirty one answers as soon as Git prints it.
pub(crate) fn unclean_reason(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    let _span = crate::phase_telemetry::span("proof_unclean_check");
    let strict = policy.strict;
    let listing = match run_proof_git(
        worktree,
        &[
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--ignore-submodules=none",
        ],
        &mut {
            let mut scan = CountingRecordScan::new(strict);
            move |stdout: &[u8]| scan.holds_counting_record(stdout)
        },
    )? {
        ProofListing::Matched(listing) => return Ok(Some(first_change_reason(&listing, strict))),
        ProofListing::Complete(listing) => listing,
    };
    let records = status_records(&listing);
    let changed = records
        .iter()
        .filter(|(code, path)| status_record_counts(code, path, strict))
        .collect::<Vec<_>>();
    if let Some((_, first)) = changed.first() {
        return Ok(Some(format!(
            "{} uncommitted or untracked path(s), first: {first}",
            changed.len()
        )));
    }
    if let Some(reason) = in_progress_or_stash_reason(worktree, branch, main_root)? {
        return Ok(Some(reason));
    }
    ignored_reason_from_status(worktree, &records, policy)
}

/// The ignored-content verdict ([`ignored_content_reason`]) over the `!!`
/// records of a combined status listing: the same root check, budget,
/// limits and per-entry classification.
fn ignored_reason_from_status(
    worktree: &Path,
    records: &[(&str, &str)],
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    let _span = crate::phase_telemetry::span("proof_ignored_check");
    let deadline = Instant::now() + IGNORED_PATH_LISTING_BUDGET;
    crate::git::ignored_listing_preflight()?;
    ensure_ignored_content_before(deadline)?;
    let expected_root = worktree
        .canonicalize()
        .map_err(|error| format!("cannot resolve cleanup checkout root: {error}"))?;
    let repo = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
    let discovered_root = repo
        .root()
        .canonicalize()
        .map_err(|error| format!("cannot resolve discovered Git root: {error}"))?;
    if discovered_root != expected_root {
        return Err("Git resolved a different root than the cleanup checkout".into());
    }
    let ignored = records
        .iter()
        .filter(|(code, _)| *code == "!!")
        .map(|(_, path)| *path)
        .collect::<Vec<_>>();
    if ignored.len() > PROOF_IGNORED_ENTRY_LIMIT {
        return Err(format!(
            "more than {PROOF_IGNORED_ENTRY_LIMIT} ignored paths; ignored content is unknown"
        ));
    }
    let mut released_by_file_pattern = false;
    for relative in ignored {
        ensure_ignored_content_before(deadline)?;
        if relative.len() > PROOF_IGNORED_PATH_BYTES {
            return Err(format!(
                "an ignored path longer than {PROOF_IGNORED_PATH_BYTES} bytes"
            ));
        }
        // A classification error keeps the checkout, exactly as
        // `ignored_content_blockers` reports it.
        let verdict = classify_ignored_entry(worktree, relative, policy).and_then(
            |(classification, rule)| {
                match rule {
                    IgnoredEntryRule::FilePattern => released_by_file_pattern = true,
                    IgnoredEntryRule::ProtectedRefusal => crate::phase_telemetry::add(
                        "cleanup_regenerable_protected_refused",
                        Duration::ZERO,
                    ),
                    IgnoredEntryRule::Other => {}
                }
                ignored_verdict(relative, classification)
            },
        );
        ensure_ignored_content_before(deadline)?;
        if let Err(reason) = verdict {
            return Ok(Some(format!(
                "ignored content requires explicit cleanup: {reason}"
            )));
        }
    }
    if released_by_file_pattern {
        crate::phase_telemetry::add("cleanup_checkout_released_by_file_pattern", Duration::ZERO);
    }
    Ok(None)
}

/// Proof 3 without the ignored-path listing: no uncommitted or untracked
/// path, no merge, rebase or similar in progress, and no stash made on
/// `branch`. The status listing stops at the first change.
pub(crate) fn uncommitted_reason(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    uncommitted_verdict(worktree, branch, main_root, policy, TrackedDeletions::Keep)
        .map(|(reason, _)| reason)
}

/// How proof 3 treats a checkout whose only changes are tracked deletions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackedDeletions {
    /// Like any other uncommitted change (`finish`, the landed lane).
    Keep,
    /// Automatic removal: resumable only when the broker recorded that it
    /// had started removing this checkout at this HEAD (`marked`); kept
    /// under a distinct reason otherwise, so such checkouts are countable.
    Automatic { marked: bool },
}

/// Proof 3 for automatic removal of a closed checkout: [`unclean_reason`],
/// except that a checkout the broker itself had started removing (a
/// recorded removal attempt for this path, owner and HEAD: `marked`) and
/// that the stopped removal left part-deleted (see
/// [`interrupted_removal_deletions`]) is not kept for its missing tracked
/// files. Without the record a pure deletion is somebody's change and keeps
/// the checkout, as before. Returns the keep reason, or `None` and how many
/// tracked paths were already gone. The caller must still prove HEAD
/// contained: that is what makes every missing file recoverable.
pub(crate) fn unclean_reason_resuming_interrupted_removal(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
    policy: &AutoCleanupPolicy,
    marked: bool,
) -> Result<(Option<String>, usize), String> {
    let (reason, deleted) = timed(CheckKind::Status, || {
        uncommitted_verdict(
            worktree,
            branch,
            main_root,
            policy,
            TrackedDeletions::Automatic { marked },
        )
    })?;
    if reason.is_some() {
        return Ok((reason, 0));
    }
    Ok((
        timed(CheckKind::Ignored, || {
            ignored_content_reason(worktree, policy)
        })?,
        deleted,
    ))
}

/// Of a checkout's porcelain v1 `-z` status, the tracked paths a removal
/// that stopped part-way deleted, when that is all the listing holds.
///
/// Every entry must be ` D`: deleted from the working tree while the index
/// still matches HEAD for that path, so the missing content is exactly
/// HEAD's. Any staged change, modification, rename, type change or
/// untracked path (Finder's `.DS_Store` aside unless `strict`) returns
/// `None`, as does a deletion of less than a quarter of the `tracked` paths.
/// This shape alone proves nothing about who deleted the files: the caller
/// also requires the broker's recorded removal attempt.
pub(crate) fn interrupted_removal_deletions(
    status_z: &str,
    tracked: usize,
    strict: bool,
) -> Option<usize> {
    let mut deleted = 0usize;
    for entry in status_z.split('\0').filter(|entry| !entry.is_empty()) {
        if !strict && entry.starts_with("?? ") && entry[3..].rsplit('/').next() == Some(".DS_Store")
        {
            continue;
        }
        if entry.len() > 3 && entry.starts_with(" D ") {
            deleted += 1;
            continue;
        }
        return None;
    }
    (deleted > 0 && deleted.saturating_mul(4) >= tracked).then_some(deleted)
}

/// [`interrupted_removal_deletions`] against the checkout's index. A
/// checkout tracking a submodule is never treated as interrupted: removing
/// it could take the submodule's own unpushed history with it.
fn interrupted_removal_in(worktree: &Path, status_z: &str, strict: bool) -> Option<usize> {
    let staged = git(
        worktree,
        &["--no-optional-locks", "ls-files", "-z", "--stage"],
    )
    .ok()?;
    interrupted_removal_against_index(status_z, &staged, strict)
}

/// [`interrupted_removal_deletions`] given the `ls-files -z --stage`
/// listing: `None` whenever the index tracks a submodule (mode `160000`).
pub(crate) fn interrupted_removal_against_index(
    status_z: &str,
    staged_z: &str,
    strict: bool,
) -> Option<usize> {
    let entries = staged_z
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.iter().any(|entry| entry.starts_with("160000 ")) {
        return None;
    }
    interrupted_removal_deletions(status_z, entries.len(), strict)
}

fn uncommitted_verdict(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
    policy: &AutoCleanupPolicy,
    tracked_deletions: TrackedDeletions,
) -> Result<(Option<String>, usize), String> {
    // `--no-optional-locks`: asking must not refresh the index, whose
    // modification time is the worktree activity the idle windows read.
    // `core.fsmonitor=false`: a background judge must not start a monitor
    // daemon for each of hundreds of checkouts; the answer is the same.
    // Timed through the in-progress markers and the stash listing below.
    let _span = crate::phase_telemetry::span("proof_uncommitted_check");
    let args = [
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    // Keeping on any change needs only the first one: the listing stops
    // there. Automatic removal reads the whole list (tracked deletions).
    let status = match tracked_deletions {
        TrackedDeletions::Keep => {
            let strict = policy.strict;
            let mut scan = CountingRecordScan::new(strict);
            match run_proof_git(worktree, &args, &mut |stdout: &[u8]| {
                scan.holds_counting_record(stdout)
            })? {
                ProofListing::Matched(listing) => {
                    return Ok((Some(first_change_reason(&listing, strict)), 0));
                }
                ProofListing::Complete(listing) => listing,
            }
        }
        TrackedDeletions::Automatic { .. } => git(worktree, &args)?,
    };
    let changed = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .filter(|entry| {
            // Finder metadata is not somebody's change.
            policy.strict
                || !(entry.starts_with("?? ") && entry[3..].rsplit('/').next() == Some(".DS_Store"))
        })
        .map(|entry| entry[3..].to_string())
        .collect::<Vec<_>>();
    let mut interrupted_removal = 0;
    if let Some(first) = changed.first() {
        let deleted = match tracked_deletions {
            TrackedDeletions::Keep => None,
            TrackedDeletions::Automatic { .. } => {
                interrupted_removal_in(worktree, &status, policy.strict)
            }
        };
        match (tracked_deletions, deleted) {
            (TrackedDeletions::Automatic { marked: true }, Some(deleted)) if !policy.strict => {
                interrupted_removal = deleted;
            }
            (TrackedDeletions::Automatic { marked: true }, Some(deleted)) => {
                return Ok((
                    Some(format!(
                        "{deleted} tracked path(s) deleted by an interrupted removal; strict cleanup removes only pristine checkouts"
                    )),
                    0,
                ));
            }
            (TrackedDeletions::Automatic { marked: false }, Some(deleted)) => {
                return Ok((
                    Some(format!(
                        "{deleted} tracked path(s) deleted with no recorded removal attempt, first: {first}"
                    )),
                    0,
                ));
            }
            _ => {
                return Ok((
                    Some(format!(
                        "{} uncommitted or untracked path(s), first: {first}",
                        changed.len()
                    )),
                    0,
                ));
            }
        }
    }

    Ok((
        in_progress_or_stash_reason(worktree, branch, main_root)?,
        interrupted_removal,
    ))
}

/// A merge, rebase or similar in progress in the checkout, or a stash made
/// on `branch`.
fn in_progress_or_stash_reason(
    worktree: &Path,
    branch: Option<&str>,
    main_root: &Path,
) -> Result<Option<String>, String> {
    let git_dir = git(worktree, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(git_dir.trim());
    for (marker, what) in [
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or am"),
    ] {
        if git_dir.join(marker).exists() {
            return Ok(Some(format!("{what} is in progress")));
        }
    }

    if let Some(branch) = branch {
        let stashes = git(main_root, &["stash", "list", "--format=%gs"])?;
        let made_here = [format!("WIP on {branch}:"), format!("On {branch}:")];
        if let Some(entry) = stashes
            .lines()
            .find(|line| made_here.iter().any(|prefix| line.starts_with(prefix)))
        {
            return Ok(Some(format!(
                "a stash was made on {branch}: {}",
                entry.trim()
            )));
        }
    }

    Ok(None)
}

/// Shared ignored-content verdict for automatic and finish-time cleanup.
/// Callers still prove tracked cleanliness, ownership, liveness and delivery.
pub(crate) fn ignored_content_reason(
    worktree: &Path,
    policy: &AutoCleanupPolicy,
) -> Result<Option<String>, String> {
    let _span = crate::phase_telemetry::span("proof_ignored_check");
    Ok(ignored_content_blockers(worktree, policy, 1)?
        .into_iter()
        .next()
        .map(|(_, reason)| format!("ignored content requires explicit cleanup: {reason}")))
}

/// Up to `limit` ignored roots that keep the checkout, as
/// `(relative path, reason)`; a directory ends in `/`. Empty means every
/// ignored root is disposable under `policy`.
pub(crate) fn ignored_content_blockers(
    worktree: &Path,
    policy: &AutoCleanupPolicy,
    limit: usize,
) -> Result<Vec<(String, String)>, String> {
    let deadline = Instant::now() + IGNORED_PATH_LISTING_BUDGET;
    crate::git::with_git_deadline(IGNORED_PATH_LISTING_BUDGET, || {
        ensure_ignored_content_before(deadline)?;
        let expected_root = worktree
            .canonicalize()
            .map_err(|error| format!("cannot resolve cleanup checkout root: {error}"))?;
        let repo = GitRepo::discover(worktree).map_err(|error| error.to_string())?;
        let discovered_root = repo
            .root()
            .canonicalize()
            .map_err(|error| format!("cannot resolve discovered Git root: {error}"))?;
        if discovered_root != expected_root {
            return Err("Git resolved a different root than the cleanup checkout".into());
        }
        let ignored = repo
            .ignored_paths_readonly_until(deadline)
            .map_err(|error| error.to_string())?;
        // Git supplies ignored roots; never recursively walk their contents.
        // Keep every listed entry in the existing whole-checkout classifier:
        // typed protection here is only for future in-place artifact discovery.
        let mut blockers = Vec::new();
        let mut released_by_file_pattern = false;
        for entry in ignored {
            ensure_ignored_content_before(deadline)?;
            let relative = if entry.directory_marker {
                format!("{}/", entry.relative)
            } else {
                entry.relative
            };
            // A classification error keeps the checkout as a blocker, exactly
            // as `classify_ignored` reports it.
            let verdict = classify_ignored_entry(worktree, &relative, policy).and_then(
                |(classification, rule)| {
                    match rule {
                        IgnoredEntryRule::FilePattern => released_by_file_pattern = true,
                        IgnoredEntryRule::ProtectedRefusal => crate::phase_telemetry::add(
                            "cleanup_regenerable_protected_refused",
                            Duration::ZERO,
                        ),
                        IgnoredEntryRule::Other => {}
                    }
                    ignored_verdict(&relative, classification)
                },
            );
            ensure_ignored_content_before(deadline)?;
            if let Err(reason) = verdict {
                blockers.push((relative, reason));
                if blockers.len() >= limit.max(1) {
                    break;
                }
            }
        }
        ensure_ignored_content_before(deadline)?;
        if blockers.is_empty() && released_by_file_pattern {
            crate::phase_telemetry::add(
                "cleanup_checkout_released_by_file_pattern",
                Duration::ZERO,
            );
        }
        Ok(blockers)
    })
}

fn ensure_ignored_content_before(deadline: Instant) -> Result<(), String> {
    let deadline =
        crate::git::active_git_deadline().map_or(deadline, |active| active.min(deadline));
    if Instant::now() >= deadline {
        Err("ignored-path classification exceeded its time budget".into())
    } else {
        Ok(())
    }
}

/// Test seam callback for [`AFTER_REMOVAL_COHORT_HOOK`].
#[cfg(test)]
type RemovalCohortHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    /// Test seam: the retirement lane consumes its whole deadline.
    static RETIREMENT_EXHAUSTS_ITS_SHARE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Test seam: every landed proof of the oldest-first lap runs out of
    /// budget, as one does when the likely-landed queue spent the pass.
    pub(crate) static LAP_LANDED_PROOF_EXHAUSTS_BUDGET: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Test seam: selection consumes its whole deadline after selecting.
    static SELECTION_EXHAUSTS_ITS_SHARE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Test seam: cohort-index discovery pages of this many sessions, at
    /// most this many pages per refresh call, so a small fixture can stand
    /// in for a lap longer than one call covers.
    static COHORT_SCAN_LIMITS: std::cell::Cell<Option<(usize, usize)>> =
        const { std::cell::Cell::new(None) };
    /// Test seam: runs in the removal verdict right after the cohort (and
    /// its ownership re-proof) was built, so a test can change the checkout
    /// before the verdict's own fresh re-check.
    static AFTER_REMOVAL_COHORT_HOOK: std::cell::RefCell<Option<RemovalCohortHook>> =
        const { std::cell::RefCell::new(None) };
    /// Test seam: another pass moves the lap cursor while a put-off group
    /// moves it back.
    static PUT_OFF_CURSOR_RACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test seam: Git spends time deleting each checkout and then fails to
    /// remove it, as a removal stopped part-way (`ENOTEMPTY`) does.
    pub(crate) static GIT_REMOVAL_FAILS_PART_WAY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Sessions per cohort-index discovery page.
fn cleanup_cohort_scan_page_size() -> usize {
    #[cfg(test)]
    if let Some((page_size, _)) = COHORT_SCAN_LIMITS.with(std::cell::Cell::get) {
        return page_size;
    }
    CLEANUP_COHORT_SCAN_PAGE_SIZE
}

/// Discovery pages one cohort-index refresh call may advance.
fn cleanup_cohort_page_cap() -> usize {
    #[cfg(test)]
    if let Some((_, page_cap)) = COHORT_SCAN_LIMITS.with(std::cell::Cell::get) {
        return page_cap;
    }
    CLEANUP_COHORT_UNBOUNDED_PAGE_CAP
}

/// Meta key of the remembered keep verdicts for closed checkouts kept for a
/// reason only a change lifts: ignored leftovers, uncommitted changes,
/// unmarked tracked deletions, commits not contained (see
/// [`fast_judging::keep_memo_entry_holds`]).
pub(crate) const KEEP_MEMO_KEY: &str = "auto_cleanup.keep_memo";
const KEEP_MEMO_VERSION: u32 = 1;
/// Remembered verdicts kept at most; the oldest go first.
const KEEP_MEMO_MAX_ENTRIES: usize = 4096;
/// A remembered verdict is re-proved at least this often even when nothing
/// it fingerprints changed.
const KEEP_MEMO_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
/// Ignored roots recorded per kept checkout.
const IGNORED_BLOCKER_LIMIT: usize = 16;

/// Meta key of the closed-checkout groups the removal lane last judged ahead
/// of its lap, so a kept one yields to the others before it is judged again.
pub(crate) const PRIORITY_MEMO_KEY: &str = "auto_cleanup.priority_memo";
/// Priority groups one pass judges with the full proofs, taking turns with
/// the lap page.
const PRIORITY_GROUPS_PER_PASS: usize = 8;
/// Priority groups one pass reads after the priority memo, so groups the
/// database or the keep memo decides do not use up its judged share.
const PRIORITY_QUEUE_LIMIT: usize = 64;
/// Lap groups one steady (no capacity pressure) selection judges at most,
/// after the priority lane, so the lap still reaches old removable
/// checkouts the priority lane never names, at a lower rate than under
/// pressure. Read with the existing cursor; never a full listing.
pub(crate) const STEADY_LAP_GROUPS_PER_PASS: usize = 16;

/// Which closed-checkout lanes one removal selection runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemovalSelection {
    /// The capacity probe decides: under pressure the priority lane and a
    /// lap that goes on page by page while time remains, otherwise one lap
    /// page.
    Probe,
    /// The background worker without capacity pressure: the priority lane,
    /// then at most [`STEADY_LAP_GROUPS_PER_PASS`] groups of the lap. No
    /// capacity probe runs.
    Steady,
}
/// Priority candidates read per pass before the memo filters them.
const PRIORITY_CANDIDATE_LIMIT: usize = 256;
/// A kept or selected priority group is judged again after this long, or at
/// once when its merged state changes.
const PRIORITY_REJUDGE_MS: i64 = 30 * 60_000;
/// A deferred priority group (its proofs ran out of time) is judged ahead of
/// the lap again after this long, so it cannot hold every pass.
const PRIORITY_DEFER_MS: i64 = 5 * 60_000;
/// Remembered priority verdicts kept at most; the oldest go first.
const PRIORITY_MEMO_MAX_ENTRIES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PriorityMemoEntry {
    judged_at_ms: i64,
    merged: bool,
    /// Not judged ahead of the lap again before this time.
    #[serde(default)]
    retry_at_ms: i64,
    /// `selected`, `kept` or `deferred` at selection.
    #[serde(default)]
    outcome: String,
    /// The keep code apply recorded when it kept a selected group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apply_code: Option<String>,
    /// Set when selection kept the group for a reason only a change of its
    /// state can lift ([`stable_priority_keep_code`]): the group is not
    /// judged ahead of the lap again until that state differs. The lap
    /// still judges it on every pass over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stable: Option<StablePriorityKeep>,
}

/// The state a stable priority keep depended on. A difference in any part
/// makes the group a priority candidate again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct StablePriorityKeep {
    /// The keep code selection gave.
    code: String,
    /// Each session of the group: id, origin, status and cleanup state.
    sessions: Vec<String>,
    /// With code `ignored_content`: the ignored roots that kept it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blockers: Vec<String>,
    /// Modification times of the checkout root and of each blocker and its
    /// parent (`-` for a path that does not exist), as the keep memo takes
    /// them: no Git, no walk.
    fingerprint: Vec<String>,
}

impl StablePriorityKeep {
    /// The stable keep for a group selection kept with `code`, or `None`
    /// when the code can lift without a state change (a running gate, the
    /// grace period, a held lease, an open file...).
    fn for_kept_group(
        group: &AutoCleanupCheckoutGroup,
        code: &str,
        blockers: &[String],
    ) -> Option<Self> {
        if !stable_priority_keep_code(code) || (code == "ignored_content" && blockers.is_empty()) {
            return None;
        }
        Some(Self {
            code: code.to_string(),
            sessions: priority_group_signature(&group.sessions),
            blockers: blockers.to_vec(),
            fingerprint: keep_memo_fingerprint(&group.worktree, blockers),
        })
    }

    /// Whether the state this keep depended on is unchanged for `worktree`
    /// and its current `sessions`.
    fn still_holds(&self, worktree: &Path, sessions: &[Session]) -> bool {
        self.sessions == priority_group_signature(sessions)
            && keep_memo_fingerprint(worktree, &self.blockers) == self.fingerprint
    }
}

/// Keep codes that no running process, timer or lock can lift: ownership
/// that cannot be proved (adopted only, a legacy or missing spawn witness,
/// an unreadable or foreign checkout), a checkout directory that is gone, and
/// ignored leftovers. Each lifts only when the group's sessions or the
/// checkout's files change, which [`StablePriorityKeep`] fingerprints.
fn stable_priority_keep_code(code: &str) -> bool {
    matches!(
        code,
        "ownership_adopted_only"
            | "ownership_witness_legacy"
            | "ownership_witness_missing"
            | "ownership_unproven"
            | "checkout_missing"
            | "ignored_content"
    )
}

/// The part of a group's session rows a stable keep depends on.
fn priority_group_signature(sessions: &[Session]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| {
            format!(
                "{}:{:?}:{:?}:{:?}",
                session.id, session.origin, session.status, session.cleanup_state
            )
        })
        .collect()
}

/// How selection left a priority group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PriorityOutcome {
    Selected,
    Kept,
    Deferred,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PriorityMemo {
    #[serde(default)]
    entries: std::collections::BTreeMap<String, PriorityMemoEntry>,
}

impl PriorityMemo {
    fn decode(raw: Option<&str>) -> Self {
        raw.and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default()
    }

    /// Whether `path` was judged recently with the same merged state.
    fn recently_judged(&self, path: &str, merged: bool, now_ms: i64) -> bool {
        self.entries
            .get(path)
            .is_some_and(|entry| entry.merged == merged && now_ms < entry.retry_at_ms)
    }

    /// Remember a judged priority group, whatever selection made of it: a
    /// selected one that apply then keeps, or a deferred one, must not be
    /// picked first on every pass and hold the lane.
    #[cfg(test)]
    fn remember(&mut self, path: String, merged: bool, outcome: PriorityOutcome, now_ms: i64) {
        self.remember_with(path, merged, outcome, None, now_ms);
    }

    /// [`Self::remember`], with the stable keep a kept group got. A stable
    /// keep is not judged ahead of the lap again until its state changes
    /// (checked by the caller against the group, see
    /// [`StablePriorityKeep::still_holds`]) or its merged state does; the
    /// time limit is the keep memo's own re-proof age, far longer than a lap.
    fn remember_with(
        &mut self,
        path: String,
        merged: bool,
        outcome: PriorityOutcome,
        stable: Option<StablePriorityKeep>,
        now_ms: i64,
    ) {
        let stable = stable.filter(|_| outcome == PriorityOutcome::Kept);
        let (wait, outcome) = match outcome {
            PriorityOutcome::Selected => (PRIORITY_REJUDGE_MS, "selected"),
            PriorityOutcome::Kept if stable.is_some() => (KEEP_MEMO_MAX_AGE_MS, "kept"),
            PriorityOutcome::Kept => (PRIORITY_REJUDGE_MS, "kept"),
            PriorityOutcome::Deferred => (PRIORITY_DEFER_MS, "deferred"),
        };
        self.entries.insert(
            path,
            PriorityMemoEntry {
                judged_at_ms: now_ms,
                merged,
                retry_at_ms: now_ms.saturating_add(wait),
                outcome: outcome.into(),
                apply_code: None,
                stable,
            },
        );
    }

    fn prune(&mut self, now_ms: i64) {
        self.entries.retain(|_, entry| now_ms < entry.retry_at_ms);
        if self.entries.len() > PRIORITY_MEMO_MAX_ENTRIES {
            let mut ages = self
                .entries
                .iter()
                .map(|(path, entry)| (entry.judged_at_ms, path.clone()))
                .collect::<Vec<_>>();
            ages.sort();
            let excess = self.entries.len() - PRIORITY_MEMO_MAX_ENTRIES;
            for (_, path) in ages.into_iter().take(excess) {
                self.entries.remove(&path);
            }
        }
    }
}
/// Names `blocked_by_ignored` lists, most common first.
const BLOCKED_BY_IGNORED_LIMIT: usize = 10;

/// One closed checkout the removal lane kept for a reason only a change
/// lifts. Re-examined after seven days, and as soon as its sessions, the
/// cleanup policy or what its code depends on changes: for ignored
/// leftovers, HEAD and the modification time of the checkout root, a
/// blocker or a blocker's parent; for the other codes, the change signal
/// [`fast_judging::keep_memo_entry_holds`] names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct KeepMemoEntry {
    head: String,
    policy: String,
    fingerprint: Vec<String>,
    blockers: Vec<String>,
    sessions: Vec<i64>,
    reason: String,
    recorded_at_ms: i64,
    /// The keep code remembered; empty in an entry written before codes
    /// were recorded, which is an `ignored_content` keep.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    code: String,
    /// With code `not_contained`: the remote default branch tip the
    /// containment proof failed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct KeepMemo {
    version: u32,
    entries: std::collections::BTreeMap<String, KeepMemoEntry>,
}

fn decode_keep_memo(raw: Option<&str>) -> KeepMemo {
    raw.and_then(|raw| serde_json::from_str::<KeepMemo>(raw).ok())
        .filter(|memo| memo.version == KEEP_MEMO_VERSION)
        .unwrap_or(KeepMemo {
            version: KEEP_MEMO_VERSION,
            entries: std::collections::BTreeMap::new(),
        })
}

/// The part of the cleanup policy a keep verdict depends on.
fn keep_memo_policy(policy: &AutoCleanupPolicy) -> String {
    format!(
        "strict={};regenerable={:?};keep={:?}",
        policy.strict, policy.regenerable, policy.keep
    )
}

fn checkout_head(worktree: &Path) -> Option<String> {
    GitRepo::discover(worktree).ok()?.head_commit().ok()
}

/// The contents of a small regular file, or `None` for anything else: the
/// file is opened without following a symlink and without blocking (a FIFO
/// or device never stalls the pass), then checked to be a regular file of at
/// most 4 KiB.
fn read_small_regular_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return None;
    }
    let mut contents = String::new();
    file.take(4096).read_to_string(&mut contents).ok()?;
    Some(contents)
}

/// A linked checkout's HEAD commit read from its files: the `.git` pointer,
/// then the worktree's `HEAD`, resolved against `tips` (one `for-each-ref`
/// listing) when it names a branch. `None` whenever anything is unusual, so
/// the caller falls back to Git.
pub(crate) fn head_from_checkout_files(
    worktree: &Path,
    tips: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let pointer = read_small_regular_file(&worktree.join(".git"))?;
    let git_dir = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        worktree.join(git_dir)
    };
    let head = read_small_regular_file(&git_dir.join("HEAD"))?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => tips.get(reference.trim()).cloned(),
        None if head.len() >= 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            Some(head.to_string())
        }
        None => None,
    }
}

fn modified_ns(path: &Path) -> String {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(|| "-".to_string(), |since| since.as_nanos().to_string())
}

/// Modification times that change when an ignored blocker is removed,
/// replaced or joined by a new top-level entry. Cheap: no Git, no walk.
fn keep_memo_fingerprint(worktree: &Path, blockers: &[String]) -> Vec<String> {
    let mut paths = vec![String::new()];
    for blocker in blockers {
        let trimmed = blocker.trim_end_matches('/');
        paths.push(trimmed.to_string());
        if let Some((parent, _)) = trimmed.rsplit_once('/') {
            paths.push(parent.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|relative| {
            let path = if relative.is_empty() {
                worktree.to_path_buf()
            } else {
                worktree.join(&relative)
            };
            format!("{relative}={}", modified_ns(&path))
        })
        .collect()
}

/// One ignored root that keeps closed checkouts, aggregated across every
/// remembered keep verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockedByIgnored {
    /// The ignored path relative to the checkout; a directory ends in `/`.
    pub name: String,
    /// Closed checkouts it keeps.
    pub checkouts: usize,
    /// The `.aethyme/config.toml` line that would declare it regenerable:
    /// the directory itself, or for a standalone file its exact path as a
    /// glob. The repository decides: configure it only when the path holds
    /// nothing but output its tools regenerate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_line: Option<String>,
    /// Why no configuration line can release this standalone file (a
    /// protected name, or a path a pattern cannot name on its own). Absent
    /// whenever `config_line` is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_refusal: Option<String>,
}

/// Aggregate remembered keep verdicts whose checkout still exists.
fn blocked_by_ignored_from_memo(memo: &KeepMemo) -> Vec<BlockedByIgnored> {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for (worktree, entry) in &memo.entries {
        if !Path::new(worktree).is_dir() {
            continue;
        }
        for blocker in &entry.blockers {
            *counts.entry(blocker.as_str()).or_default() += 1;
        }
    }
    let mut rows = counts
        .into_iter()
        .map(|(name, checkouts)| {
            let (config_line, config_refusal) = match name.strip_suffix('/') {
                Some(directory) => (
                    Some(format!(
                        "[cleanup] regenerable = [{}]",
                        toml_string(directory)
                    )),
                    None,
                ),
                None => match regenerable_file_config_line(name) {
                    Ok(line) => (Some(line), None),
                    Err(refusal) => (None, Some(refusal)),
                },
            };
            BlockedByIgnored {
                name: name.to_string(),
                checkouts,
                config_line,
                config_refusal,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .checkouts
            .cmp(&left.checkouts)
            .then_with(|| left.name.cmp(&right.name))
    });
    rows.truncate(BLOCKED_BY_IGNORED_LIMIT);
    rows
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// What `broker start` says when the linked-checkout limit still refuses
/// after its removal pass: what that pass removed and kept, the top keep
/// codes, and the change that would release the most closed checkouts.
pub(crate) fn checkout_capacity_refusal_detail(report: Option<&AutoCleanupReport>) -> String {
    let Some(report) = report else {
        return "the removal pass could not run (another maintenance pass held the GC lock); retry shortly or run `aethyme broker gc sweep --json`".into();
    };
    let mut codes = std::collections::BTreeMap::<&str, usize>::new();
    for kept in &report.kept {
        let code = if kept.code.is_empty() {
            kept_reason_code(&kept.reason)
        } else {
            kept.code.as_str()
        };
        *codes.entry(code).or_default() += 1;
    }
    let mut codes = codes.into_iter().collect::<Vec<_>>();
    codes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let top = codes
        .iter()
        .take(3)
        .map(|(code, count)| format!("{code} {count}"))
        .collect::<Vec<_>>();
    let mut detail = format!(
        "the removal pass removed {} closed checkout(s), kept {}{}, deferred {}",
        report.removed.len(),
        report.kept.len(),
        if top.is_empty() {
            String::new()
        } else {
            format!(" ({})", top.join(", "))
        },
        report.deferred
    );
    if let Some(blocked) = report.blocked_by_ignored.first() {
        match (&blocked.config_line, &blocked.config_refusal) {
            (Some(line), _) => detail.push_str(&format!(
                "; ignored {} keeps {} closed checkout(s): if it holds only regenerable output, adding `{line}` to .aethyme/config.toml releases them",
                blocked.name, blocked.checkouts
            )),
            (None, refusal) => detail.push_str(&format!(
                "; ignored file {} keeps {} closed checkout(s): {}",
                blocked.name,
                blocked.checkouts,
                refusal.as_deref().unwrap_or("review and delete it")
            )),
        }
    } else if report.deferred > 0 {
        detail.push_str("; run `aethyme broker gc sweep --json` to continue the removal lap");
    }
    detail
}

/// `blocked_by_ignored` from the stored keep memo, for `broker status`.
pub(crate) fn blocked_by_ignored_from_meta(raw: Option<&str>) -> Vec<BlockedByIgnored> {
    blocked_by_ignored_from_memo(&decode_keep_memo(raw))
}

/// Meta key of remembered landed-lane declines that cannot change while the
/// session head, the fetched default-branch tip and the session's pull
/// request and representation evidence stay the same.
const LANDED_DECLINE_MEMO_KEY: &str = "auto_cleanup.landed_decline_memo";
const LANDED_DECLINE_MEMO_MAX_ENTRIES: usize = 4096;
/// Where the likely-landed queue resumes, so one session whose proof keeps
/// running out of budget cannot hold the front of the queue.
const LIKELY_LANDED_CURSOR_KEY: &str = "auto_cleanup.likely_landed_after";
/// Likely-landed sessions one pass may take, before the oldest-first cursor.
const LIKELY_LANDED_PASS_LIMIT: usize = 64;

/// Landed-lane declines (`no_session_commits`, `no_landing_evidence`) keyed
/// by everything their proof read. A remembered decline only skips the
/// landed lane; it never closes anything, and the abandonment window still
/// applies.
#[derive(Debug, Default)]
struct LandedDeclineMemo {
    entries: std::collections::BTreeMap<i64, (String, String)>,
    /// Local branch tips from one `for-each-ref` listing, so a memo key
    /// reads a checkout's HEAD from its files instead of two Git processes.
    branch_tips: Option<std::collections::BTreeMap<String, String>>,
}

impl LandedDeclineMemo {
    fn load(broker: &Broker) -> Self {
        let entries = broker
            .store_ref()
            .meta_get(LANDED_DECLINE_MEMO_KEY)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let branch_tips = {
            let _span = crate::phase_telemetry::span("landed_memo_branch_tips");
            broker.repo_handle().local_branch_tips()
        };
        Self {
            entries,
            branch_tips,
        }
    }

    /// What the decline depended on, or `None` when it cannot be read (then
    /// nothing is remembered or skipped).
    fn key(
        &self,
        broker: &Broker,
        session: &Session,
        upstream_tip: Option<&str>,
    ) -> Option<String> {
        let tip = upstream_tip?;
        let worktree = Path::new(&session.worktree_path);
        let head = match self
            .branch_tips
            .as_ref()
            .and_then(|tips| head_from_checkout_files(worktree, tips))
        {
            Some(head) => {
                crate::phase_telemetry::add("landed_memo_head_from_files", Duration::ZERO);
                head
            }
            None => checkout_head(worktree)?,
        };
        let store = broker.store_ref();
        let merged = store
            .session_has_merged_pull_request_link(session.id)
            .ok()?;
        let represented = store
            .session_representation(session.id, &head)
            .ok()?
            .is_some();
        // A `holder_unbound` decline holds until a holder is bound, which
        // appends a holder event; `holder_alive` is never remembered, since
        // a process can exit without any event.
        let holder = store
            .latest_session_holder_event(session.id)
            .ok()?
            .map_or_else(|| "none".to_string(), |event| event.id.to_string());
        Some(format!(
            "{head} {tip} merged={merged} represented={represented} holder={holder}"
        ))
    }

    fn remembered(
        &self,
        session_id: i64,
        key: &str,
    ) -> Option<crate::broker::LandedSessionDecline> {
        let (recorded, code) = self.entries.get(&session_id)?;
        if recorded != key {
            return None;
        }
        match code.as_str() {
            "no_session_commits" => Some(crate::broker::LandedSessionDecline::NoSessionCommits),
            "no_landing_evidence" => Some(crate::broker::LandedSessionDecline::NoLandingEvidence),
            "holder_unbound" => Some(crate::broker::LandedSessionDecline::HolderUnbound),
            _ => None,
        }
    }

    /// Remember a cacheable decline under the key read before the proof ran;
    /// forget the session on any other outcome.
    fn record(
        &mut self,
        broker: &Broker,
        session_id: i64,
        key: Option<&str>,
        decline: Option<crate::broker::LandedSessionDecline>,
    ) {
        use crate::broker::LandedSessionDecline as Decline;
        let entry = match (key, decline) {
            (
                Some(key),
                Some(
                    reason @ (Decline::NoSessionCommits
                    | Decline::NoLandingEvidence
                    | Decline::HolderUnbound),
                ),
            ) => Some((key.to_string(), reason.code().to_string())),
            _ => None,
        };
        let changed = match entry {
            Some(entry) => self.entries.insert(session_id, entry.clone()) != Some(entry),
            None => self.entries.remove(&session_id).is_some(),
        };
        if !changed {
            return;
        }
        while self.entries.len() > LANDED_DECLINE_MEMO_MAX_ENTRIES {
            self.entries.pop_first();
        }
        if let Ok(text) = serde_json::to_string(&self.entries) {
            crate::warn_unrecorded(
                "remember a landed-lane decline",
                broker.store_ref().meta_set(LANDED_DECLINE_MEMO_KEY, &text),
            );
        }
    }
}

/// Passes shorter than this (a broker open's inline sweep) do not spend part
/// of their budget counting registered checkouts.
const CAPACITY_PROBE_MIN_BUDGET: Duration = Duration::from_secs(1);

/// Meta key of the last linked-checkout count `git worktree list` gave, as
/// `{"count":N,"maximum":M,"at_ms":T}`. `broker start` records it on every
/// allocation and the background worker on every removal slice, so a pass
/// too short to run the listing (a broker open's inline sweep) and the
/// background worker's admission still see capacity pressure.
pub(crate) const CHECKOUT_USAGE_KEY: &str = "auto_cleanup.checkout_usage";
/// How long a recorded count stands in for a fresh one.
const CHECKOUT_USAGE_MAX_AGE_MS: i64 = 15 * 60 * 1_000;
/// A recorded count this recent stands in for a fresh one in a long pass's
/// pressure probe (the worker records one at the start of every slice).
const CHECKOUT_USAGE_REUSE_MS: i64 = 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RecordedCheckoutUsage {
    count: usize,
    maximum: u32,
    at_ms: i64,
}

/// Meta key prefix of the record automatic removal writes, before Git
/// deletes anything, for the checkout path that follows the prefix. Cleared
/// once the removal finished; a record left behind means it stopped part-way.
const REMOVAL_ATTEMPT_KEY_PREFIX: &str = "auto_cleanup.removal_attempt:";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RemovalAttempt {
    owner_session_id: i64,
    head: String,
    at_ms: i64,
}

fn removal_attempt_key(worktree: &Path) -> String {
    format!("{REMOVAL_ATTEMPT_KEY_PREFIX}{}", worktree.to_string_lossy())
}

/// Registered linked checkouts at or above three quarters of the limit.
fn usage_under_pressure(count: usize, maximum: u32) -> bool {
    maximum > 0 && count.saturating_mul(4) >= (maximum as usize).saturating_mul(3)
}

thread_local! {
    /// Time Git spent removing checkouts on this thread since the last
    /// [`take_git_removal_time`]: the background worker credits it, and only
    /// it, against its unlink pacer.
    static GIT_REMOVAL_TIME: std::cell::Cell<Duration> =
        const { std::cell::Cell::new(Duration::ZERO) };
}

fn note_git_removal_time(spent: Duration) {
    GIT_REMOVAL_TIME.with(|total| total.set(total.get().saturating_add(spent)));
}

/// Time Git spent removing checkouts on this thread since the last call.
pub(crate) fn take_git_removal_time() -> Duration {
    GIT_REMOVAL_TIME.with(|total| total.replace(Duration::ZERO))
}

/// Entries assumed for a removed checkout no measurement counted, when the
/// background worker charges Git's removal to its unlink pacer.
const UNMEASURED_CHECKOUT_ENTRY_ESTIMATE: u64 = 10_000;

/// How many filesystem entries `report`'s removals deleted, from the last
/// recorded measurement of each checkout or a fixed estimate. Used only to
/// pace the background worker.
pub(crate) fn estimated_removed_entries(main_root: &Path, report: &AutoCleanupReport) -> u64 {
    if report.removed.is_empty() {
        return 0;
    }
    let records = crate::measurement::load_size_records(main_root);
    report
        .removed
        .iter()
        .map(|removed| {
            records
                .get(&removed.worktree)
                .and_then(|record| record.inodes)
                .unwrap_or(UNMEASURED_CHECKOUT_ENTRY_ESTIMATE)
        })
        .fold(0u64, u64::saturating_add)
}

/// How many filesystem entries `report`'s failed removals may have deleted
/// before Git gave up (a removal that stopped part-way, such as `ENOTEMPTY`
/// after most of the tree was gone), from the last recorded measurement of
/// each checkout or a fixed estimate. Used only to pace the background
/// worker when Git spent time removing but nothing was reported removed.
pub(crate) fn estimated_failed_removal_entries(
    main_root: &Path,
    report: &AutoCleanupReport,
) -> u64 {
    let failed = report
        .kept
        .iter()
        .filter(|kept| kept.code == "removal_failed")
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return 0;
    }
    let records = crate::measurement::load_size_records(main_root);
    failed
        .iter()
        .map(|kept| {
            records
                .get(&kept.worktree)
                .and_then(|record| record.inodes)
                .unwrap_or(UNMEASURED_CHECKOUT_ENTRY_ESTIMATE)
        })
        .fold(0u64, u64::saturating_add)
}

/// Selection may use this share of the removal lane. The rest is held for
/// the apply step: a selection that ran to the lane deadline used to leave
/// apply's filesystem path verification an expired deadline, so the pass
/// deferred every selected checkout and removed none.
const REMOVAL_SELECTION_SHARE: (u32, u32) = (3, 4);

/// The share of a pass the session-retirement lane may use, as
/// `(numerator, denominator)`: half normally, a quarter under capacity
/// pressure, when removal is what frees a slot.
fn retirement_share(capacity_pressure: bool) -> (u32, u32) {
    if capacity_pressure { (1, 4) } else { (1, 2) }
}

/// The deadline of a lane that may use `share` of what is left of the pass.
/// Unbounded passes stay unbounded.
fn lane_deadline(
    started: Instant,
    deadline: Option<Instant>,
    (numerator, denominator): (u32, u32),
) -> Option<Instant> {
    deadline.map(|end| {
        let total = end.saturating_duration_since(started);
        started + total * numerator / denominator.max(1)
    })
}

fn duration_ms(duration: Option<Duration>) -> u64 {
    duration.map_or(0, |duration| {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    })
}

/// Phase-level counters for one maintenance pass (`aethyme broker advanced
/// metrics` and `command-metrics.jsonl`).
fn record_lane_telemetry(report: &AutoCleanupReport) {
    let Some(lanes) = report.lanes else {
        return;
    };
    let count = |name: &'static str, times: usize| {
        for _ in 0..times.min(4096) {
            crate::phase_telemetry::add(name, Duration::ZERO);
        }
    };
    count("auto_cleanup_removal_examined", lanes.removal_examined);
    count("auto_cleanup_removal_memo_skips", lanes.removal_memo_skips);
    count(
        "auto_cleanup_removal_db_kept",
        lanes
            .removal_db_in_grace
            .saturating_add(lanes.removal_db_session_open),
    );
    count("auto_cleanup_removal_selected", lanes.removal_selected);
    count("auto_cleanup_removed", report.removed.len());
    count(
        "auto_cleanup_interrupted_removals_resumed",
        report
            .removed
            .iter()
            .filter(|removed| removed.resumed_interrupted_removal > 0)
            .count(),
    );
    count("auto_cleanup_kept", report.kept.len());
    if lanes.capacity_pressure {
        count("auto_cleanup_capacity_pressure", 1);
    }
}

/// Proof 4: every commit at `head` is in `upstream_tip`.
///
/// `deep` allows the content and patch search; without it only ancestry is
/// tried and anything else is deferred.
pub(crate) fn containment(
    repo: &GitRepo,
    head: &str,
    upstream_tip: &str,
    deep: bool,
    deadline: Option<Instant>,
) -> Result<ContainmentProof, NotRemoved> {
    if repo.is_ancestor(head, upstream_tip) {
        return Ok(ContainmentProof {
            head: head.to_string(),
            containing_commit: upstream_tip.to_string(),
            proof: crate::LandingEvidence::Ancestry.as_str().to_string(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        });
    }
    if !deep {
        return Err(NotRemoved::Defer);
    }
    match crate::representation::work_landed_within(repo, head, upstream_tip, deadline) {
        Ok(crate::LandingVerdict::Landed {
            evidence,
            landed_by,
        }) => Ok(ContainmentProof {
            head: head.to_string(),
            containing_commit: landed_by.unwrap_or_else(|| upstream_tip.to_string()),
            proof: evidence.as_str().to_string(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        }),
        Ok(crate::LandingVerdict::NotLanded { truncated, .. }) => {
            Err(NotRemoved::Keep(if truncated {
                "the landing search hit its candidate cap without proving the commits are on the remote default branch".into()
            } else {
                "commits are not contained in the remote default branch".into()
            }))
        }
        Err(BrokerOpError::RepresentationUnavailable { reason })
            if reason.contains("time budget") =>
        {
            Err(NotRemoved::Defer)
        }
        Err(error) => Err(NotRemoved::Keep(format!(
            "containment could not be proved: {error}"
        ))),
    }
}

impl Broker {
    fn auto_cleanup_cursor(
        &mut self,
    ) -> Result<(AutoCleanupCursor, Option<String>), BrokerOpError> {
        let raw = self.store().meta_get(AUTO_CLEANUP_CURSOR_KEY)?;
        Ok((decode_auto_cleanup_cursor(raw.as_deref()), raw))
    }

    fn persist_auto_cleanup_cursor(
        &mut self,
        cursor: &AutoCleanupCursor,
        expected_raw: &mut Option<String>,
    ) -> Result<(), BrokerOpError> {
        // This structure contains only integer fields, so JSON serialization
        // cannot fail. A storage write failure or stale checkpoint must stop
        // planning before any selected checkout can be applied.
        let value =
            serde_json::to_string(cursor).expect("serializing the cleanup cursor is infallible");
        if !self.store().meta_compare_and_set(
            AUTO_CLEANUP_CURSOR_KEY,
            expected_raw.as_deref(),
            &value,
        )? {
            return Err(BrokerOpError::AutoCleanupCursorChanged);
        }
        *expected_raw = Some(value);
        Ok(())
    }

    /// Capture every durable cursor that can advance during an automatic
    /// whole-checkout pass. A pass may discover inventory or move through
    /// checkout groups without deleting anything.
    pub(crate) fn auto_cleanup_progress_marker(
        &mut self,
    ) -> Result<AutoCleanupProgressMarker, BrokerOpError> {
        let cohort = self
            .store()
            .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)?;
        let (group_cursor, _) = self.auto_cleanup_cursor()?;
        let (retirement_cursor, _) = self.session_retirement_cursor()?;
        Ok(AutoCleanupProgressMarker {
            cohort_generation: cohort.map_or(0, |state| state.generation),
            alias_refresh_generation: cohort.map_or(0, |state| state.alias_refresh_generation),
            alias_progress_sequence: cohort.map_or(0, |state| state.progress_sequence),
            group_cursor_generation: group_cursor.cohort_generation,
            group_cursor_after_session_id: group_cursor.after_session_id,
            group_cursor_high_water_session_id: group_cursor.high_water_session_id,
            retirement_cursor_after_session_id: retirement_cursor.after_session_id,
            retirement_cursor_high_water_session_id: retirement_cursor.high_water_session_id,
        })
    }

    /// Advance one durable, bounded page of checkout-path discovery. The
    /// indexed rows are candidate evidence only; selected worktrees still go
    /// through the full lifecycle, alias, witness, and filesystem checks.
    pub(crate) fn advance_cleanup_cohort_scan(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<crate::store::cleanup_cohort::CleanupCohortScanState>, BrokerOpError> {
        let page = self.store().cleanup_cohort_scan_page(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            cleanup_cohort_scan_page_size(),
            now_ms(),
        )?;
        if !page.dirty_sessions.is_empty() {
            let mut dirty_rows = Vec::with_capacity(page.dirty_sessions.len());
            for dirty in &page.dirty_sessions {
                if deadline_expired(deadline) {
                    break;
                }
                let row = dirty.session.as_ref().map(|session| {
                    let resolved = ResolvedWorktreePath::resolve(Path::new(&session.worktree_path));
                    let mut errors = resolved.resolution_errors;
                    let canonical_path = match resolved.canonical_or_raw.to_str() {
                        Some(path) => path.to_owned(),
                        None => {
                            errors.push("canonical checkout path is not Unicode".into());
                            session.worktree_path.clone()
                        }
                    };
                    let git_dir = resolved.git_dir.and_then(|path| match path.to_str() {
                        Some(path) => Some(path.to_owned()),
                        None => {
                            errors.push("Git directory path is not Unicode".into());
                            None
                        }
                    });
                    crate::store::cleanup_cohort::CleanupCohortResolvedRow {
                        session_id: session.id,
                        raw_path: session.worktree_path.clone(),
                        canonical_path,
                        git_dir,
                        resolution_error: (!errors.is_empty()).then(|| errors.join("; ")),
                        cleanup_state: session.cleanup_state.as_str().to_owned(),
                    }
                });
                dirty_rows.push(
                    crate::store::cleanup_cohort::CleanupCohortDirtyResolvedRow {
                        session_id: dirty.session_id,
                        revision: dirty.revision,
                        row,
                    },
                );
            }
            if dirty_rows.is_empty() {
                return Ok(self.store().cleanup_cohort_scan_state(
                    crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                )?);
            }
            let outcome = self.store().persist_cleanup_cohort_dirty_page(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                page.state.generation,
                page.state.alias_refresh_generation,
                &dirty_rows,
                now_ms(),
            )?;
            if outcome == crate::store::cleanup_cohort::CleanupCohortPersistOutcome::Changed {
                return Ok(self.store().cleanup_cohort_scan_state(
                    crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                )?);
            }
            return Ok(self.store().cleanup_cohort_scan_state(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            )?);
        }
        if page.sessions.is_empty() {
            return Ok(Some(page.state));
        }
        let expected_after = page.state.after_session_id;
        let mut resolved_rows = Vec::with_capacity(page.sessions.len());
        let resolve_started = Instant::now();
        for session in &page.sessions {
            if deadline_expired(deadline) {
                break;
            }
            crate::phase_telemetry::add("cleanup_cohort_scan_rows_resolved", Duration::ZERO);
            let resolved = ResolvedWorktreePath::resolve(Path::new(&session.worktree_path));
            let mut errors = resolved.resolution_errors;
            let canonical_path = match resolved.canonical_or_raw.to_str() {
                Some(path) => path.to_owned(),
                None => {
                    errors.push("canonical checkout path is not Unicode".into());
                    session.worktree_path.clone()
                }
            };
            let git_dir = resolved.git_dir.and_then(|path| match path.to_str() {
                Some(path) => Some(path.to_owned()),
                None => {
                    errors.push("Git directory path is not Unicode".into());
                    None
                }
            });
            resolved_rows.push(crate::store::cleanup_cohort::CleanupCohortResolvedRow {
                session_id: session.id,
                raw_path: session.worktree_path.clone(),
                canonical_path,
                git_dir,
                resolution_error: (!errors.is_empty()).then(|| errors.join("; ")),
                cleanup_state: session.cleanup_state.as_str().to_owned(),
            });
        }
        crate::phase_telemetry::add("cleanup_cohort_scan_resolve", resolve_started.elapsed());
        if resolved_rows.is_empty() {
            return Ok(self.store().cleanup_cohort_scan_state(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            )?);
        }
        let reached_high_water = resolved_rows
            .last()
            .is_some_and(|row| row.session_id >= page.state.high_water_session_id);
        let _persist = crate::phase_telemetry::span("cleanup_cohort_scan_persist");
        let outcome = self.store().persist_cleanup_cohort_scan_page(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            page.state.revision,
            page.state.generation,
            page.state.alias_refresh_generation,
            expected_after,
            &resolved_rows,
            reached_high_water,
            now_ms(),
        )?;
        if outcome == crate::store::cleanup_cohort::CleanupCohortPersistOutcome::Changed {
            return Ok(self.store().cleanup_cohort_scan_state(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            )?);
        }
        Ok(self
            .store()
            .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)?)
    }

    /// Advance discovery pages until the lap completes, the deadline passes,
    /// or the page cap is reached. When `paced`, every page first goes
    /// through the unattended pacer: in the detached worker it is held to
    /// [`CLEANUP_COHORT_SCAN_ROWS_PER_SECOND`], waits a bounded time under
    /// load or a memory warning, and stops resumably at critical memory
    /// pressure. Foreground callers install no pacer and never wait.
    fn complete_cleanup_cohort_scan(
        &mut self,
        deadline: Option<Instant>,
        paced: bool,
    ) -> Result<CleanupCohortScanProgress, BrokerOpError> {
        let mut pages = 0usize;
        let mut previous_page_rows = 0u64;
        loop {
            if !paced {
                let Some(state) = self.advance_cleanup_cohort_scan(deadline)? else {
                    return Ok(CleanupCohortScanProgress {
                        state: None,
                        pressure_stopped: false,
                    });
                };
                pages += 1;
                if state.complete
                    || deadline_expired(deadline)
                    || pages >= cleanup_cohort_page_cap()
                {
                    return Ok(CleanupCohortScanProgress {
                        state: Some(state),
                        pressure_stopped: false,
                    });
                }
                continue;
            }
            let pacer_started = Instant::now();
            let verdict = crate::gc::unattended_pacing::before_unattended_scan_batch(
                deadline,
                previous_page_rows,
                CLEANUP_COHORT_SCAN_ROWS_PER_SECOND,
            );
            crate::phase_telemetry::add("cleanup_cohort_scan_pacer_wait", pacer_started.elapsed());
            match verdict {
                crate::gc::unattended_pacing::PressureVerdict::Clear => {}
                crate::gc::unattended_pacing::PressureVerdict::LoadThrottled => {
                    crate::phase_telemetry::add(
                        "cleanup_cohort_scan_load_throttled",
                        Duration::ZERO,
                    );
                }
                crate::gc::unattended_pacing::PressureVerdict::MemoryWarningThrottled => {
                    crate::phase_telemetry::add(
                        "cleanup_cohort_scan_memory_warning_throttled",
                        Duration::ZERO,
                    );
                }
                crate::gc::unattended_pacing::PressureVerdict::MemoryBackoff => {
                    crate::phase_telemetry::add(
                        "cleanup_cohort_scan_memory_backoff",
                        Duration::ZERO,
                    );
                    return Ok(CleanupCohortScanProgress {
                        state: self.store().cleanup_cohort_scan_state(
                            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                        )?,
                        pressure_stopped: true,
                    });
                }
            }
            let Some(state) = self.advance_cleanup_cohort_scan(deadline)? else {
                return Ok(CleanupCohortScanProgress {
                    state: None,
                    pressure_stopped: false,
                });
            };
            pages += 1;
            // The next page pays for this one as a full page: a short last
            // page only makes the wait before a page that is not taken.
            previous_page_rows = cleanup_cohort_scan_page_size() as u64;
            if state.complete || deadline_expired(deadline) || pages >= cleanup_cohort_page_cap() {
                return Ok(CleanupCohortScanProgress {
                    state: Some(state),
                    pressure_stopped: false,
                });
            }
        }
    }

    /// Start or resume one complete, bounded-by-deadline filesystem alias
    /// refresh. This is called once per maintenance pass; proof lookups only
    /// consume the resulting lap and never rewind it. Production lanes use
    /// the resume-only or paced variants; tests drive a full lap with it.
    #[cfg(test)]
    pub(crate) fn refresh_cleanup_cohort_aliases(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<crate::store::cleanup_cohort::CleanupCohortScanState>, BrokerOpError> {
        self.store().begin_cleanup_cohort_alias_refresh(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            now_ms(),
        )?;
        Ok(self.complete_cleanup_cohort_scan(deadline, false)?.state)
    }

    /// [`Self::refresh_cleanup_cohort_aliases`] with every page paced by the
    /// installed unattended pacer (the compaction lane of the detached
    /// worker), also saying whether the pacer stopped the lap for memory
    /// pressure. Without an installed pacer it never waits.
    ///
    /// Only `may_rewind` lets it start a new lap over a complete one; a
    /// walker that chose to drain dirty rows or finish a lap from state it
    /// read earlier passes `false`, so a lap another walker completed
    /// meanwhile is kept.
    pub(crate) fn refresh_cleanup_cohort_aliases_paced(
        &mut self,
        deadline: Option<Instant>,
        may_rewind: bool,
    ) -> Result<CleanupCohortScanProgress, BrokerOpError> {
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        if may_rewind {
            self.store()
                .begin_cleanup_cohort_alias_refresh(namespace, now_ms())?;
        } else {
            self.store()
                .resume_cleanup_cohort_alias_refresh(namespace, now_ms())?;
        }
        self.complete_cleanup_cohort_scan(deadline, true)
    }

    /// Resume an unfinished lap or drain dirty rows without ever starting a
    /// new lap over a complete one (unpaced).
    fn resume_cleanup_cohort_aliases(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<crate::store::cleanup_cohort::CleanupCohortScanState>, BrokerOpError> {
        self.store().resume_cleanup_cohort_alias_refresh(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            now_ms(),
        )?;
        Ok(self.complete_cleanup_cohort_scan(deadline, false)?.state)
    }

    /// Recheck every registered raw session path with filesystem-only
    /// canonical and Git-directory resolution while the lifecycle lock is
    /// held. A symlink retarget does not change SQLite revisions, so a cached
    /// alias index alone cannot authorize a destructive pass.
    pub(crate) fn cleanup_cohort_filesystem_aliases_match(
        &mut self,
        expected_alias_refresh_generation: i64,
        deadline: Option<Instant>,
    ) -> Result<bool, BrokerOpError> {
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let Some(before) = self.store().cleanup_cohort_scan_state(namespace)? else {
            return Ok(false);
        };
        if !before.complete
            || before.unresolved_rows != 0
            || before.pending_dirty_rows != 0
            || before.alias_refresh_generation != expected_alias_refresh_generation
            || deadline_expired(deadline)
        {
            return Ok(false);
        }
        let authority_before = self.store().cleanup_authority_revisions()?;
        let mut after_session_id = 0;
        let mut resolved_paths = std::collections::HashMap::<String, ResolvedWorktreePath>::new();
        loop {
            if deadline_expired(deadline) {
                return Ok(false);
            }
            let rows =
                self.store()
                    .cleanup_cohort_indexed_rows_page(namespace, after_session_id, 32)?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                if deadline_expired(deadline) {
                    return Ok(false);
                }
                if row.resolution_error.is_some() {
                    self.store()
                        .mark_cleanup_cohort_rows_dirty(namespace, &[row.session_id])?;
                    return Ok(false);
                }
                let current = resolved_paths
                    .entry(row.raw_path.clone())
                    .or_insert_with(|| ResolvedWorktreePath::resolve(Path::new(&row.raw_path)));
                if !current.resolution_errors.is_empty()
                    || current.canonical_or_raw.to_str() != Some(row.canonical_path.as_str())
                    || current.git_dir.as_deref().and_then(Path::to_str) != row.git_dir.as_deref()
                {
                    self.store()
                        .mark_cleanup_cohort_rows_dirty(namespace, &[row.session_id])?;
                    return Ok(false);
                }
                after_session_id = row.session_id;
            }
        }
        if deadline_expired(deadline)
            || self.store().cleanup_authority_revisions()? != authority_before
        {
            return Ok(false);
        }
        Ok(self
            .store()
            .cleanup_cohort_scan_state(namespace)?
            .is_some_and(|current| {
                current.complete
                    && current.unresolved_rows == 0
                    && current.pending_dirty_rows == 0
                    && current.alias_refresh_generation == expected_alias_refresh_generation
            }))
    }

    /// Construct one complete cohort from the persisted path index. The full
    /// apply-time snapshot below remains the authority and detects any change
    /// since this revision-bound lookup.
    pub(crate) fn closed_artifact_checkout_cohort_for_session(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<Option<ClosedArtifactCheckoutCohort>, BrokerOpError> {
        self.closed_artifact_checkout_cohort_for_session_mode(
            session_id,
            worktree,
            deadline,
            ClosedCohortPurpose::Reclaim,
        )
    }

    /// [`Self::closed_artifact_checkout_cohort_for_session`] for the
    /// retirement plan and archive cleanup a checkout compaction runs. It
    /// builds the same cohort; its ownership re-proof decisions are counted
    /// in their own lane ([`ClosedCohortPurpose::Retirement`]).
    pub(crate) fn closed_artifact_checkout_cohort_for_retirement(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<Option<ClosedArtifactCheckoutCohort>, BrokerOpError> {
        self.closed_artifact_checkout_cohort_for_session_mode(
            session_id,
            worktree,
            deadline,
            ClosedCohortPurpose::Retirement,
        )
    }

    /// Whether every pending index change provably leaves this checkout's
    /// indexed rows correct: the last discovery lap completed, no row is
    /// unresolved, and no pending session's new or previously indexed path
    /// overlaps the checkout or shares its Git directory. Any doubt, including
    /// a path that cannot be resolved or more rows than the bound, is `false`.
    /// It never writes the index.
    fn pending_cleanup_delta_spares_checkout(
        &mut self,
        canonical_path: &str,
        git_dir: Option<&Path>,
        deadline: Option<Instant>,
    ) -> Result<bool, BrokerOpError> {
        let Some(delta) = self.store().cleanup_cohort_pending_delta(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            CLEANUP_COHORT_SCAN_PAGE_SIZE,
        )?
        else {
            return Ok(false);
        };
        let Some(rows) = delta.rows else {
            return Ok(false);
        };
        // With nothing pending, an incomplete index means a lap restart that
        // the delta cannot describe.
        if !delta.lap_complete || delta.unresolved_rows != 0 || rows.is_empty() {
            return Ok(false);
        }
        let checkout = Path::new(canonical_path);
        let overlaps = |path: &Path| path.starts_with(checkout) || checkout.starts_with(path);
        let same_git_dir = |other: &Path| git_dir.is_some_and(|git_dir| git_dir == other);
        for row in &rows {
            if deadline_expired(deadline) {
                return Ok(false);
            }
            if let Some((raw, indexed_canonical, indexed_git_dir)) = &row.indexed
                && (overlaps(Path::new(raw))
                    || overlaps(Path::new(indexed_canonical))
                    || indexed_git_dir
                        .as_deref()
                        .is_some_and(|dir| same_git_dir(Path::new(dir))))
            {
                return Ok(false);
            }
            if let Some(raw) = &row.current_raw_path {
                let resolved = ResolvedWorktreePath::resolve(Path::new(raw));
                if !resolved.resolution_errors.is_empty()
                    || overlaps(Path::new(raw))
                    || overlaps(&resolved.canonical_or_raw)
                    || resolved.git_dir.as_deref().is_some_and(same_git_dir)
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Read-only variant for explicit artifact-use marker review. An
    /// incomplete path index is a refusal here: planning must not advance or
    /// otherwise mutate the cleanup cohort index. See [`ClosedCohortPurpose`]
    /// for how its witness comparison differs from reclaim.
    pub(crate) fn closed_artifact_checkout_cohort_for_session_readonly(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<ClosedArtifactCheckoutCohortResolution, BrokerOpError> {
        self.closed_artifact_checkout_cohort_for_session_detailed_mode(
            session_id,
            worktree,
            deadline,
            ClosedCohortPurpose::MarkerReview,
        )
    }

    fn closed_artifact_checkout_cohort_for_session_mode(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
        purpose: ClosedCohortPurpose,
    ) -> Result<Option<ClosedArtifactCheckoutCohort>, BrokerOpError> {
        Ok(
            match self.closed_artifact_checkout_cohort_for_session_detailed_mode(
                session_id, worktree, deadline, purpose,
            )? {
                ClosedArtifactCheckoutCohortResolution::Available(cohort) => Some(cohort),
                ClosedArtifactCheckoutCohortResolution::Unavailable(_) => None,
            },
        )
    }

    /// Construct one complete cohort and retain the first failed proof as a
    /// typed reason for bounded worker diagnostics. The apply-time snapshot
    /// remains authoritative and detects any change since this lookup.
    pub(crate) fn closed_artifact_checkout_cohort_for_session_detailed(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<ClosedArtifactCheckoutCohortResolution, BrokerOpError> {
        self.closed_artifact_checkout_cohort_for_session_detailed_mode(
            session_id,
            worktree,
            deadline,
            ClosedCohortPurpose::Reclaim,
        )
    }

    fn closed_artifact_checkout_cohort_for_session_detailed_mode(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
        purpose: ClosedCohortPurpose,
    ) -> Result<ClosedArtifactCheckoutCohortResolution, BrokerOpError> {
        use self::ClosedArtifactCheckoutCohortResolution::{Available, Unavailable};
        use self::ClosedArtifactCheckoutCohortUnavailableReason as Reason;

        let unavailable = Unavailable;
        let Some(mut index_state) = self
            .store()
            .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)?
        else {
            return Ok(unavailable(Reason::IndexMissing));
        };
        // Successful removals and concurrent authority changes enqueue only
        // their affected rows. Drain those deltas in place; do not restart the
        // full filesystem lap between candidates in one pass.
        while purpose.reclaims()
            && !index_state.complete
            && index_state.after_session_id >= index_state.high_water_session_id
            && index_state.pending_dirty_rows > 0
        {
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            let Some(next) = self.advance_cleanup_cohort_scan(deadline)? else {
                return Ok(unavailable(if deadline_expired(deadline) {
                    Reason::DeadlineExceeded
                } else {
                    Reason::IndexIncomplete
                }));
            };
            index_state = next;
        }
        if deadline_expired(deadline) {
            return Ok(unavailable(Reason::DeadlineExceeded));
        }
        // Reclaim needs the whole index. Marker review only needs the rows for
        // this checkout, so a busy repository's unrelated session churn is
        // checked below against the pending delta instead of refusing here.
        let scoped_index_check = !index_state.complete || index_state.unresolved_rows != 0;
        if scoped_index_check && purpose.reclaims() {
            return Ok(unavailable(Reason::IndexIncomplete));
        }
        let witness_revision = self.store().cleanup_witness_revision()?;
        let selected = self.store().session(session_id)?;
        if !selected.status.is_closed() || selected.cleanup_state != SessionCleanupState::Closed {
            return Ok(unavailable(Reason::SessionChanged));
        }
        let resolved = ResolvedWorktreePath::resolve(worktree);
        if !resolved.resolution_errors.is_empty() {
            return Ok(unavailable(Reason::PathResolutionFailed));
        }
        if deadline_expired(deadline) {
            return Ok(unavailable(Reason::DeadlineExceeded));
        }
        let Some(canonical_path) = resolved.canonical_or_raw.to_str().map(str::to_owned) else {
            return Ok(unavailable(Reason::PathNotUtf8));
        };
        let checkout_git_dir = resolved.git_dir.clone();
        if scoped_index_check
            && !self.pending_cleanup_delta_spares_checkout(
                &canonical_path,
                checkout_git_dir.as_deref(),
                deadline,
            )?
        {
            return Ok(unavailable(Reason::IndexIncomplete));
        }
        let (sessions, group_truncated) = self.store().cleanup_cohort_sessions_for_path(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            &canonical_path,
        )?;
        if group_truncated {
            return Ok(unavailable(Reason::CohortTruncated));
        }
        if sessions.is_empty() || !sessions.iter().any(|session| session.id == session_id) {
            return Ok(unavailable(Reason::CohortMembershipChanged));
        }
        if sessions.iter().any(|session| {
            !session.status.is_closed() || session.cleanup_state != SessionCleanupState::Closed
        }) {
            return Ok(unavailable(Reason::NonClosedAlias));
        }
        let group_ids = sessions
            .iter()
            .map(|session| session.id)
            .collect::<std::collections::BTreeSet<_>>();

        let mut candidate_paths = std::collections::BTreeSet::new();
        let mut candidate_ancestors = std::collections::BTreeSet::new();
        for path in [&resolved.raw, &resolved.canonical_or_raw] {
            let Some(text) = path.to_str() else {
                return Ok(unavailable(Reason::PathNotUtf8));
            };
            candidate_paths.insert(text.to_owned());
            for ancestor in path.ancestors() {
                let Some(text) = ancestor.to_str() else {
                    return Ok(unavailable(Reason::PathNotUtf8));
                };
                candidate_ancestors.insert(text.to_owned());
            }
        }
        let candidate_paths = candidate_paths.into_iter().collect::<Vec<_>>();
        let candidate_ancestors = candidate_ancestors.into_iter().collect::<Vec<_>>();
        let git_dir = resolved.git_dir.as_deref().and_then(Path::to_str);
        let alias_rows = self.store().cleanup_cohort_alias_rows(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            &candidate_ancestors,
            &candidate_paths,
            git_dir,
        )?;
        let resolved_candidate = resolved;

        let current_witness = if sessions
            .iter()
            .any(|session| session.origin == SessionOrigin::Spawned)
        {
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            let checkout_result = {
                let _git_deadline = deadline.map(crate::git::limit_git_until);
                GitRepo::discover(&resolved_candidate.canonical_or_raw)
            };
            let checkout = match checkout_result {
                Ok(checkout) => checkout,
                Err(_) => {
                    let reason = if deadline_expired(deadline) {
                        Reason::DeadlineExceeded
                    } else if matches!(
                        std::fs::symlink_metadata(&resolved_candidate.canonical_or_raw),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    ) {
                        Reason::CheckoutMissing
                    } else {
                        Reason::GitCheckoutUnavailable
                    };
                    return Ok(unavailable(reason));
                }
            };
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            let witness = {
                let _git_deadline = deadline.map(crate::git::limit_git_until);
                checkout.spawned_worktree_instance_witness()
            };
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            witness
        } else {
            None
        };

        let mut snapshots = Vec::with_capacity(sessions.len());
        let mut owner_session_id = None;
        let mut ownership_reproof = None;
        for session in &sessions {
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            let spawned_instance_witness = if session.origin == SessionOrigin::Spawned {
                self.store().spawned_worktree_instance_witness(session.id)?
            } else {
                None
            };
            if session.origin == SessionOrigin::Spawned {
                let saved = spawned_instance_witness
                    .as_ref()
                    .filter(|witness| witness.worktree_path == canonical_path);
                let exact = saved.is_some_and(|witness| {
                    current_witness
                        .as_ref()
                        .is_some_and(|current| match purpose {
                            ClosedCohortPurpose::Reclaim
                            | ClosedCohortPurpose::Retirement
                            | ClosedCohortPurpose::CheckoutRemoval => {
                                witness.matches_current_instance(current)
                            }
                            ClosedCohortPurpose::MarkerReview => {
                                witness.matches_instance_for_marker_review(current)
                            }
                        })
                });
                if !exact {
                    let reproof = match (purpose.reclaims(), saved, current_witness.as_ref()) {
                        (true, Some(saved), Some(current)) => {
                            match self.checkout_ownership_reproof(
                                purpose.reproof_lane(),
                                session,
                                Path::new(&canonical_path),
                                saved,
                                current,
                                None,
                                deadline,
                            ) {
                                Ok(reproof) => Some(reproof),
                                // The objects are the same but the Git half
                                // refused: a reason of its own, so the
                                // refusal is countable apart from a replaced
                                // checkout.
                                Err(ReproofFailure::Refused(_))
                                    if !matches!(
                                        saved.compare_checkout_identity(current),
                                        crate::events::CheckoutIdentityMatch::Differs(_)
                                    ) =>
                                {
                                    return Ok(unavailable(Reason::OwnershipReproofRefused));
                                }
                                Err(ReproofFailure::Refused(_)) => None,
                                // Unproven for now, not refused: retry.
                                Err(ReproofFailure::Unavailable(_)) => {
                                    return Ok(unavailable(Reason::DeadlineExceeded));
                                }
                            }
                        }
                        _ => None,
                    };
                    let Some(reproof) = reproof else {
                        return Ok(unavailable(Reason::SpawnedWitnessMismatch));
                    };
                    ownership_reproof = Some(reproof);
                }
            }
            if owner_session_id.is_none()
                && session.origin == SessionOrigin::Spawned
                && spawned_instance_witness
                    .as_ref()
                    .is_some_and(|witness| witness.worktree_path == canonical_path)
            {
                owner_session_id = Some(session.id);
            }
            snapshots.push(ClosedArtifactSessionSnapshot {
                id: session.id,
                worktree_path: session.worktree_path.clone(),
                branch: session.branch.clone(),
                origin: session.origin,
                spawned_instance_witness,
            });
        }
        let Some(owner_session_id) = owner_session_id else {
            return Ok(unavailable(Reason::SpawnedOwnerMissing));
        };
        // Marker review may also prove a removed alias against the owner's
        // saved witness: it was verified against the live checkout above, and
        // unlike a fresh observation it shares the predecessor's mount epoch,
        // so a later remount's renumbering does not break the comparison.
        let saved_owner_witness = match purpose {
            ClosedCohortPurpose::Reclaim
            | ClosedCohortPurpose::Retirement
            | ClosedCohortPurpose::CheckoutRemoval => None,
            ClosedCohortPurpose::MarkerReview => snapshots
                .iter()
                .find(|snapshot| snapshot.id == owner_session_id)
                .and_then(|snapshot| snapshot.spawned_instance_witness.clone()),
        };
        let mut retired_predecessors = Vec::new();
        let mut handed_over_predecessors = Vec::new();
        for row in alias_rows {
            if deadline_expired(deadline) {
                return Ok(unavailable(Reason::DeadlineExceeded));
            }
            if row.resolution_error.is_some() {
                return Ok(unavailable(Reason::AliasResolutionFailed));
            }
            let row_path = ResolvedWorktreePath {
                raw: PathBuf::from(&row.raw_path),
                canonical_or_raw: PathBuf::from(&row.canonical_path),
                git_dir: row.git_dir.map(PathBuf::from),
                resolution_errors: Vec::new(),
            };
            match live_worktree_overlap(&resolved_candidate, &row_path) {
                LiveOverlapEvidence::Disjoint => {}
                LiveOverlapEvidence::Overlap
                    if row.cleanup_state == "closed" && group_ids.contains(&row.session_id) => {}
                LiveOverlapEvidence::Overlap
                    if row.cleanup_state == "cleaned"
                        && row.canonical_path == canonical_path
                        && self.replacement_chain_reaches_cohort(
                            row.session_id,
                            &canonical_path,
                            &group_ids,
                        )? =>
                {
                    if !handed_over_predecessors.contains(&row.session_id) {
                        handed_over_predecessors.push(row.session_id);
                    }
                }
                LiveOverlapEvidence::Overlap => {
                    if let Some(current_witness) = current_witness.as_ref() {
                        let owner = sessions
                            .iter()
                            .find(|session| session.id == owner_session_id)
                            .expect("validated cohort owner");
                        let proof_witnesses = std::iter::once(current_witness)
                            .chain(saved_owner_witness.as_ref())
                            .collect::<Vec<_>>();
                        if let Some(predecessor) = self.removed_historical_checkout_alias(
                            row.session_id,
                            owner,
                            &proof_witnesses,
                        )? {
                            if purpose == ClosedCohortPurpose::MarkerReview
                                && !retired_predecessors.iter().any(
                                    |known: &ClosedArtifactSessionSnapshot| {
                                        known.id == predecessor.id
                                    },
                                )
                            {
                                retired_predecessors.push(predecessor);
                            }
                            continue;
                        }
                    }
                    return Ok(unavailable(Reason::AliasOverlapOutsideCohort));
                }
                LiveOverlapEvidence::Unknown => {
                    return Ok(unavailable(Reason::LiveAliasUnknown));
                }
            }
        }

        let index_now = self
            .store()
            .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)?;
        let index_moved = index_now.as_ref().is_none_or(|current| {
            current.revision != index_state.revision
                || current.alias_refresh_generation != index_state.alias_refresh_generation
        });
        // A scoped review re-proves the pending delta, which may have grown
        // while the cohort was read; otherwise the index must still be complete.
        let index_still_valid = !index_moved
            && if scoped_index_check {
                self.pending_cleanup_delta_spares_checkout(
                    &canonical_path,
                    checkout_git_dir.as_deref(),
                    deadline,
                )?
            } else {
                index_now.is_some_and(|current| current.complete)
            };
        if !index_still_valid {
            return Ok(unavailable(Reason::IndexChanged));
        }
        if self.store().cleanup_witness_revision()? != witness_revision {
            return Ok(unavailable(Reason::WitnessChanged));
        }
        Ok(Available(ClosedArtifactCheckoutCohort {
            worktree: resolved_candidate.canonical_or_raw,
            sessions: snapshots,
            owner_session_id,
            alias_refresh_generation: index_state.alias_refresh_generation,
            retired_predecessors,
            handed_over_predecessors,
            ownership_reproof,
        }))
    }

    /// Whether `alias_id` was retired by `--replace-stale` into this checkout:
    /// following replacement links from it reaches a member of `group_ids`,
    /// and every session on the way is cleaned and recorded at this exact
    /// canonical path. A replaced session never had a checkout of its own, so
    /// such an alias is the cohort's own history, not an outside occupant.
    fn replacement_chain_reaches_cohort(
        &self,
        alias_id: i64,
        canonical_path: &str,
        group_ids: &std::collections::BTreeSet<i64>,
    ) -> Result<bool, BrokerOpError> {
        const MAX_REPLACEMENT_LINKS: usize = 64;
        let same_checkout = |session: &Session| {
            ResolvedWorktreePath::resolve(Path::new(&session.worktree_path))
                .canonical_or_raw
                .to_str()
                == Some(canonical_path)
        };
        let mut current = alias_id;
        for _ in 0..MAX_REPLACEMENT_LINKS {
            let session = self.store_ref().session(current)?;
            if session.status != crate::types::SessionStatus::Cleaned
                || session.cleanup_state != SessionCleanupState::Cleaned
                || !same_checkout(&session)
            {
                return Ok(false);
            }
            let Some(successor) = self.store_ref().session_replacement_successor(current)? else {
                return Ok(false);
            };
            if successor <= current {
                return Ok(false);
            }
            if group_ids.contains(&successor) {
                return Ok(same_checkout(&self.store_ref().session(successor)?));
            }
            current = successor;
        }
        Ok(false)
    }

    /// Re-prove broker ownership of a closed checkout whose schema-2 spawn
    /// witness differs from today's observation only by renumbered devices,
    /// for the automatic lane ([`Self::checkout_ownership_reproof`]).
    /// Gathers every condition of [`RenumberReproofEvidence`] and refuses on
    /// the first that fails. Callers must already have required that the
    /// exact comparison failed; this never widens a schema-1 check, and a
    /// schema-3 witness reaches it only when a recorded UUID differs or
    /// cannot be observed, which it refuses.
    ///
    /// Registration and branch evidence are read from this checkout's own
    /// registration files under the common directory, fresh on every call
    /// and without listing every worktree; the one Git call (the broker-root
    /// ownership check) is bounded by `deadline`. Evidence that cannot be
    /// gathered in time is [`ReproofFailure::Unavailable`], never a refusal.
    pub(crate) fn renumbered_witness_reproof(
        &self,
        session: &Session,
        worktree: &Path,
        saved: &crate::events::SpawnedWorktreeInstanceWitness,
        current: &crate::events::SpawnedWorktreeInstanceWitness,
        deadline: Option<Instant>,
    ) -> Result<(), ReproofFailure> {
        self.checkout_ownership_reproof(
            OwnershipReproofLane::Automatic,
            session,
            worktree,
            saved,
            current,
            None,
            deadline,
        )
        .map(|_| ())
    }

    /// The one ownership re-proof every deletion lane (automatic removal, GC
    /// reclamation and compaction, `finish` and its interrupted-cleanup
    /// recovery) uses when a saved identity no longer matches today's
    /// checkout exactly. The filesystem half
    /// ([`crate::events::SpawnedWorktreeInstanceWitness::compare_checkout_identity`])
    /// requires unchanged paths, inodes and root generation, and either equal
    /// recorded volume UUIDs or, for a record saved without them, an unchanged
    /// device-sharing pattern; the Git half requires a closed spawned session
    /// naming the checkout under the broker root, this repository's
    /// registration of it, its `HEAD` on the session branch with that branch
    /// resolving to the same commit, and, where the broker recorded a commit
    /// for this checkout (`recorded_head`: the finish recovery proof's head),
    /// `HEAD` still at that commit. Witness lanes record no commit, so for
    /// them the branch condition binds `HEAD` to the branch, not to a commit.
    /// Returns the re-proof that held. Each decision is counted in `lane`
    /// (refusals also under the reason's own counter,
    /// [`ownership_reproof_refusal_counter`]); [`OwnershipReproofLane::Quiet`]
    /// counts nothing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn checkout_ownership_reproof(
        &self,
        lane: OwnershipReproofLane,
        session: &Session,
        worktree: &Path,
        saved: &crate::events::SpawnedWorktreeInstanceWitness,
        current: &crate::events::SpawnedWorktreeInstanceWitness,
        recorded_head: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<&'static str, ReproofFailure> {
        let refuse = |condition: &str| -> Result<&'static str, ReproofFailure> {
            lane.record_refused(condition);
            Err(ReproofFailure::Refused(format!(
                "spawn-time worktree identity device renumber re-proof refused: {condition}"
            )))
        };
        let unavailable = |detail: String| -> Result<&'static str, ReproofFailure> {
            lane.record_unavailable();
            Err(ReproofFailure::Unavailable(detail))
        };
        // The filesystem half first: it needs no Git process.
        use crate::events::CheckoutIdentityMatch;
        match saved.compare_checkout_identity(current) {
            CheckoutIdentityMatch::DeviceRenumber | CheckoutIdentityMatch::VolumeUuid => {}
            CheckoutIdentityMatch::Exact => return refuse(REPROOF_NOTHING_TO_REPROVE),
            CheckoutIdentityMatch::Differs(condition) => return refuse(condition),
        }
        if deadline_expired(deadline) {
            return unavailable("the pass deadline passed before the re-proof".into());
        }
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let worktree = canonical(worktree);
        let under_broker_root = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            match self.is_broker_owned_worktree_checked(session, &worktree) {
                Ok(owned) => owned,
                Err(error) => {
                    return unavailable(format!("the broker root could not be checked: {error}"));
                }
            }
        };
        if deadline_expired(deadline) {
            return unavailable("the pass deadline passed during the re-proof".into());
        }
        let this_common = match self.repo_handle().git_common_dir() {
            Ok(common) => canonical(&common),
            Err(error) => {
                return unavailable(format!(
                    "this repository's common directory is unreadable: {error}"
                ));
            }
        };
        let registration_started = Instant::now();
        let common = Path::new(&current.git_common_path);
        let admin = Path::new(&current.git_admin_path);
        let registered = registration_names_checkout(common, admin, &worktree);
        let (checkout_branch, checkout_head) = registration_head(common, admin);
        let branch_tip =
            resolve_branch_ref_in_common(common, &format!("refs/heads/{}", session.branch));
        lane.record_registration(registration_started.elapsed());
        let evidence = RenumberReproofEvidence {
            origin_spawned: session.origin == SessionOrigin::Spawned,
            closed: session.status.is_closed()
                && session.cleanup_state == SessionCleanupState::Closed,
            session_path_matches: canonical(Path::new(&session.worktree_path)) == worktree,
            under_broker_root,
            common_dir_is_this_repository: common == this_common.as_path(),
            registered,
            checkout_branch,
            checkout_head,
            branch_tip,
            recorded_head: recorded_head.map(str::to_string),
        };
        match ownership_reproof_decision(saved, current, &session.branch, &evidence) {
            Ok(reproof) => {
                lane.record_reproved();
                Ok(reproof)
            }
            Err(condition) => refuse(condition),
        }
    }

    /// [`Self::closed_artifact_checkout_cohort_for_session_detailed`] for
    /// automatic whole-checkout removal. It accepts the same ownership
    /// re-proof as GC reclaim; only its telemetry lane differs
    /// ([`ClosedCohortPurpose::CheckoutRemoval`]).
    fn closed_checkout_cohort_for_removal(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<ClosedArtifactCheckoutCohortResolution, BrokerOpError> {
        self.closed_artifact_checkout_cohort_for_session_detailed_mode(
            session_id,
            worktree,
            deadline,
            ClosedCohortPurpose::CheckoutRemoval,
        )
    }

    /// Exclude a removed spawn's historical alias only after proving that its
    /// cleanup preceded the current checkout instance, described by any one of
    /// `current_witnesses`. Returns the proven predecessor's snapshot.
    fn removed_historical_checkout_alias(
        &self,
        alias_id: i64,
        owner: &Session,
        current_witnesses: &[&crate::events::SpawnedWorktreeInstanceWitness],
    ) -> Result<Option<ClosedArtifactSessionSnapshot>, BrokerOpError> {
        let alias = self.store_ref().session(alias_id)?;
        if alias.origin != SessionOrigin::Spawned
            || alias.status != crate::types::SessionStatus::Cleaned
            || alias.cleanup_state != SessionCleanupState::Cleaned
        {
            return Ok(None);
        }
        let Some(old_witness) = self
            .store_ref()
            .spawned_worktree_instance_witness(alias_id)?
        else {
            return Ok(None);
        };
        let handoff = match self.latest_handoff_for_session(alias_id) {
            Ok(handoff) => handoff,
            Err(BrokerOpError::HandoffNotFoundForSession { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let proven = current_witnesses.iter().any(|current| {
            proves_removed_historical_instance(&alias, owner, &old_witness, current, &handoff)
        });
        Ok(proven.then(|| ClosedArtifactSessionSnapshot {
            id: alias.id,
            worktree_path: alias.worktree_path.clone(),
            branch: alias.branch.clone(),
            origin: alias.origin,
            spawned_instance_witness: Some(old_witness),
        }))
    }

    /// Snapshot one retained checkout's full canonical cohort and its
    /// spawn-time owner. Adopted rows participate in the cohort identity but
    /// cannot supply automatic cleanup authority.
    pub(crate) fn closed_artifact_checkout_cohort_snapshot(
        &mut self,
        session_id: i64,
        worktree: &Path,
        deadline: Option<Instant>,
    ) -> Result<Option<ClosedArtifactCheckoutCohort>, BrokerOpError> {
        let Some(cohort) =
            self.closed_artifact_checkout_cohort_for_session(session_id, worktree, deadline)?
        else {
            return Ok(None);
        };
        if cohort.owner_session_id() != session_id {
            return Ok(None);
        }
        Ok(Some(cohort))
    }

    /// Collect one bounded page of complete canonical cohorts from the
    /// revision-bound path index. A partial inventory is never used to choose
    /// a candidate; a complete inventory is paged before per-group Git or
    /// ownership proof begins.
    fn auto_cleanup_checkout_groups(
        &mut self,
        cursor: &mut AutoCleanupCursor,
        deadline: Option<Instant>,
        waiting_on: &mut Option<InventoryWait>,
    ) -> Result<Option<AutoCleanupGroupScan>, BrokerOpError> {
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        // A completed, revision-current path index remains useful for planning.
        // The destructive apply path performs a fresh filesystem-only audit of
        // every registered raw path under the lifecycle lock, so routine opens
        // do not needlessly canonicalize the entire history again.
        let existing = self.store().cleanup_cohort_scan_state(namespace)?;
        let index_state = match existing {
            Some(state) if state.complete => state,
            _ => {
                // One refresh call advances a bounded number of pages. A
                // bounded pass keeps resuming the same lap while its deadline
                // allows and each call makes progress: a lap longer than one
                // call (thousands of registered sessions) used to leave every
                // pass without an inventory, so selection judged nothing.
                // Resuming never rewinds a lap; an unbounded pass keeps the
                // single bounded call.
                let _span = crate::phase_telemetry::span("auto_cleanup_cohort_index_refresh");
                let mut last_progress = None;
                loop {
                    // This lane only ever catches an index up: a lap another
                    // walker completed since `existing` was read is kept.
                    let Some(state) = self.resume_cleanup_cohort_aliases(deadline)? else {
                        *waiting_on = Some(InventoryWait::CohortIndex);
                        return Ok(None);
                    };
                    crate::phase_telemetry::add(
                        "auto_cleanup_cohort_index_refresh_calls",
                        Duration::ZERO,
                    );
                    if state.complete
                        || deadline.is_none()
                        || deadline_expired(deadline)
                        || last_progress == Some(state.progress_sequence)
                    {
                        break state;
                    }
                    last_progress = Some(state.progress_sequence);
                }
            }
        };
        if !index_state.complete {
            *waiting_on = Some(InventoryWait::CohortIndex);
            return Ok(None);
        }
        if index_state.unresolved_rows > 0 {
            return Ok(Some(AutoCleanupGroupScan {
                groups: Vec::new(),
                live: Vec::new(),
                live_worktrees: Vec::new(),
                unresolved_rows: index_state.unresolved_rows,
                alias_refresh_generation: index_state.alias_refresh_generation,
            }));
        }
        let maximum_group_id = self
            .store()
            .cleanup_cohort_group_max_session_id(namespace)?;
        if cursor.cohort_generation != index_state.generation {
            *cursor = AutoCleanupCursor {
                cohort_generation: index_state.generation,
                after_session_id: 0,
                high_water_session_id: maximum_group_id,
                ..AutoCleanupCursor::default()
            };
        } else {
            cursor.high_water_session_id = cursor.high_water_session_id.max(maximum_group_id);
            if self
                .store()
                .cleanup_cohort_group_page(
                    namespace,
                    cursor.after_session_id,
                    cursor.high_water_session_id,
                    1,
                )?
                .is_empty()
            {
                cursor.after_session_id = 0;
                cursor.high_water_session_id = maximum_group_id;
            }
        }
        if deadline_expired(deadline) {
            *waiting_on = Some(InventoryWait::GroupPage);
            return Ok(None);
        }
        let indexed_groups = self.store().cleanup_cohort_group_page(
            namespace,
            cursor.after_session_id,
            cursor.high_water_session_id,
            32,
        )?;
        let mut groups = Vec::with_capacity(indexed_groups.len());
        for (canonical_path, first_session_id) in indexed_groups {
            if deadline_expired(deadline) {
                *waiting_on = Some(InventoryWait::GroupPage);
                return Ok(None);
            }
            let (sessions, truncated) = self
                .store()
                .cleanup_cohort_sessions_for_path(namespace, &canonical_path)?;
            groups.push(AutoCleanupCheckoutGroup {
                worktree: PathBuf::from(canonical_path),
                sessions,
                complete: !truncated,
                cursor_session_id: first_session_id,
            });
        }
        let live = self.store().live_sessions()?;
        if deadline_expired(deadline) {
            *waiting_on = Some(InventoryWait::LivePaths);
            return Ok(None);
        }
        let Some(live_worktrees) = resolve_live_worktree_paths(&live, deadline) else {
            *waiting_on = Some(InventoryWait::LivePaths);
            return Ok(None);
        };
        Ok(Some(AutoCleanupGroupScan {
            groups,
            live,
            live_worktrees,
            unresolved_rows: 0,
            alias_refresh_generation: index_state.alias_refresh_generation,
        }))
    }

    /// Up to [`PRIORITY_QUEUE_LIMIT`] complete closed-checkout groups most
    /// likely removable now (see `cleanup_cohort_priority_groups` in the
    /// store; those whose closed-worktree grace of `grace_hours` has passed
    /// come first), skipping one judged within [`PRIORITY_REJUDGE_MS`] with
    /// the same merged state, and one kept for a stable reason whose state
    /// is unchanged ([`StablePriorityKeep`]). The planner judges at most
    /// [`PRIORITY_GROUPS_PER_PASS`] of them with the full proofs; groups the
    /// database or the keep memo decides cost none of that share. Each is
    /// built exactly like a lap group and judged by the same proofs; only
    /// the order changes. Returns each group with its merged state, and how
    /// many candidates the memo skipped.
    fn auto_cleanup_priority_groups(
        &mut self,
        memo: &PriorityMemo,
        now_ms: i64,
        grace_hours: u32,
        deadline: Option<Instant>,
    ) -> Result<(Vec<(AutoCleanupCheckoutGroup, bool)>, usize), BrokerOpError> {
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let in_grace_after_ms =
            now_ms.saturating_sub(i64::from(grace_hours).saturating_mul(3_600_000));
        let candidates = self.store().cleanup_cohort_priority_groups(
            namespace,
            now_ms,
            in_grace_after_ms,
            PRIORITY_CANDIDATE_LIMIT,
        )?;
        let mut groups = Vec::new();
        let mut memo_skips = 0_usize;
        for (canonical_path, first_session_id, merged) in candidates {
            if groups.len() >= PRIORITY_QUEUE_LIMIT || deadline_expired(deadline) {
                break;
            }
            let stable = if memo.recently_judged(&canonical_path, merged, now_ms) {
                match memo
                    .entries
                    .get(&canonical_path)
                    .and_then(|entry| entry.stable.as_ref())
                {
                    Some(stable) => Some(stable),
                    None => {
                        memo_skips += 1;
                        continue;
                    }
                }
            } else {
                None
            };
            let (sessions, truncated) = self
                .store()
                .cleanup_cohort_sessions_for_path(namespace, &canonical_path)?;
            if stable
                .is_some_and(|stable| stable.still_holds(Path::new(&canonical_path), &sessions))
            {
                // Kept for a reason only a state change lifts, and nothing
                // changed: the lap still judges it when it reaches it.
                memo_skips += 1;
                continue;
            }
            groups.push((
                AutoCleanupCheckoutGroup {
                    worktree: PathBuf::from(canonical_path),
                    sessions,
                    complete: !truncated,
                    cursor_session_id: first_session_id,
                },
                merged,
            ));
        }
        Ok((groups, memo_skips))
    }

    /// Make the existing ownership and live-session proofs for one complete
    /// cohort. A raw group returned by auto_cleanup_checkout_groups never
    /// authorizes deletion by itself.
    fn auto_cleanup_candidate_for_group(
        &mut self,
        group: &AutoCleanupCheckoutGroup,
        live: &[Session],
        live_worktrees: &[ResolvedWorktreePath],
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupGroupInspection, BrokerOpError> {
        let path = &group.worktree;
        let sessions = &group.sessions;
        let ids = sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let keep = |reason: String| {
            AutoCleanupGroupInspection::Kept(AutoKeptCheckout {
                worktree: path.to_string_lossy().into_owned(),
                sessions: ids.clone(),
                reason,
                ..Default::default()
            })
        };
        if !group.complete {
            return Ok(keep(
                "checkout ownership cohort exceeded the bounded discovery limit".into(),
            ));
        }
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let main_root = canonical(self.main_root());
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if *path == main_root {
            return Ok(keep("the primary checkout is never removed".into()));
        }
        let spawned_sessions = sessions
            .iter()
            .rev()
            .filter(|session| {
                session.origin == SessionOrigin::Spawned
                    && session.cleanup_state == SessionCleanupState::Closed
            })
            .collect::<Vec<_>>();
        if spawned_sessions.is_empty() {
            if sessions
                .iter()
                .all(|session| session.origin == SessionOrigin::Adopted)
            {
                return Ok(keep(format!(
                    "no matching spawn-time worktree instance witness: {OWNERSHIP_ADOPTED_ONLY}, \
                     so the broker cannot prove it created it; inspect it, then remove it with \
                     `aethyme broker finish cleanup {}`",
                    ids.last().copied().unwrap_or_default()
                )));
            }
            return Ok(keep(
                "no matching spawn-time worktree instance witness".into(),
            ));
        }
        // A directory that is gone holds no checkout slot. Judge it with one
        // stat instead of a Git process, under its own countable code.
        if matches!(
            std::fs::symlink_metadata(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            crate::phase_telemetry::add("auto_cleanup_checkout_missing", Duration::ZERO);
            return Ok(keep(format!(
                "not a readable broker-owned worktree: {CHECKOUT_DIRECTORY_MISSING}"
            )));
        }
        // Only a still-retained spawned row with a persisted witness for this
        // exact Git worktree instance proves broker creation. Adopted rows and
        // legacy spawned rows without a witness cannot authorize cleanup.
        let checkout_result = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            GitRepo::discover(path)
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let checkout = match checkout_result {
            Ok(checkout) => checkout,
            Err(_) => return Ok(keep("not a readable broker-owned worktree".into())),
        };
        let current_witness = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            checkout.spawned_worktree_instance_witness()
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        let mut matched_owner = None;
        let mut diagnostic_owner = None;
        let mut unreadable_witness = None;
        for session in spawned_sessions {
            if deadline_expired(deadline) {
                return Ok(AutoCleanupGroupInspection::Deferred);
            }
            let witness = match self.store().spawned_worktree_instance_witness(session.id) {
                Ok(Some(witness)) => Some(witness),
                Ok(None) => None,
                Err(error) => {
                    unreadable_witness = Some(error.to_string());
                    continue;
                }
            };
            if deadline_expired(deadline) {
                return Ok(AutoCleanupGroupInspection::Deferred);
            }
            if witness
                .as_ref()
                .is_some_and(|witness| witness.worktree_path != path.to_string_lossy().as_ref())
            {
                continue;
            }
            if current_witness.as_ref().is_some_and(|current| {
                witness
                    .as_ref()
                    .is_some_and(|witness| witness.matches_current_instance(current))
            }) {
                matched_owner = Some((session.clone(), witness));
                break;
            }
            // Keep an unproven spawn row only as a diagnostic candidate. The
            // planner still refuses it before containment or removal.
            if diagnostic_owner.is_none() {
                diagnostic_owner = Some((session.clone(), witness));
            }
        }
        let owner = matched_owner.or(diagnostic_owner);
        let Some((spawned_owner, spawned_instance_witness)) = owner else {
            let reason = unreadable_witness
                .map(|error| format!("spawn-time worktree identity is unreadable: {error}"))
                .unwrap_or_else(|| "no matching spawn-time worktree instance witness".into());
            return Ok(keep(reason));
        };
        let is_owned = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            self.is_broker_owned_worktree(&spawned_owner, Path::new(&spawned_owner.worktree_path))
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if !is_owned {
            return Ok(keep("not a broker-owned worktree".into()));
        }
        // A session still open anywhere in this tree keeps it, whatever state
        // it reports. The full live index was resolved before any groups.
        let candidate_worktree = ResolvedWorktreePath::resolve(path);
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        if let Some((index, evidence)) =
            first_live_worktree_overlap(&candidate_worktree, live_worktrees)
        {
            let open = &live[index];
            let reason = match evidence {
                LiveOverlapEvidence::Overlap => format!(
                    "session {} is still open ({}) in this worktree",
                    open.id,
                    open.status.as_str()
                ),
                LiveOverlapEvidence::Unknown => format!(
                    "session {} worktree overlap could not be resolved: {}",
                    open.id,
                    overlap_resolution_error(&candidate_worktree, &live_worktrees[index])
                ),
                LiveOverlapEvidence::Disjoint => {
                    unreachable!("only possible overlaps are returned")
                }
            };
            return Ok(keep(reason));
        }
        let candidate = AutoCleanupCandidate {
            worktree: path.clone(),
            spawned_owner,
            spawned_instance_witness,
            sessions: sessions.clone(),
        };
        if deadline_expired(deadline) {
            return Ok(AutoCleanupGroupInspection::Deferred);
        }
        Ok(AutoCleanupGroupInspection::Candidate(Box::new(candidate)))
    }

    /// Proofs 1-4 for one candidate, against the live state right now.
    ///
    /// Called once to select candidates and again, under the same GC lock,
    /// immediately before removal: a file created, a session adopted or a
    /// lease taken in between keeps the checkout.
    ///
    /// `selected` is the proof selection found. Containment depends only on
    /// HEAD and the upstream tip, and the plan pins the tip, so an unchanged
    /// HEAD reuses it instead of repeating the deep landing search with no
    /// deadline; every other proof is re-read (#460).
    fn auto_cleanup_verdict(
        &mut self,
        candidate: &AutoCleanupCandidate,
        policy: &AutoCleanupPolicy,
        upstream: &(String, String),
        options: AutoCleanupVerdictOptions<'_>,
    ) -> Result<ContainmentProof, NotRemoved> {
        let AutoCleanupVerdictOptions {
            deep,
            deadline,
            selected,
            lease_process_table,
            gate_witnesses,
            live_index,
        } = options;
        let keep = NotRemoved::Keep;
        // Only a checkout a broker session record proves it created may go.
        // Candidates come from session records, never from scanning the
        // worktree root, so this is the guard behind that invariant.
        if candidate.sessions.is_empty() {
            return Err(keep("no recorded owner".into()));
        }
        if !candidate
            .sessions
            .iter()
            .any(|session| session.id == candidate.spawned_owner.id)
        {
            return Err(keep("no recorded owner".into()));
        }
        if candidate
            .spawned_instance_witness
            .as_ref()
            .is_some_and(|witness| {
                witness.worktree_path != candidate.worktree.to_string_lossy().as_ref()
            })
        {
            return Err(keep("spawn-time worktree identity path changed".into()));
        }
        if policy.is_pinned(&candidate.worktree) {
            return Err(keep("pinned by [cleanup] keep".into()));
        }
        // Proof 1, re-read: the store may have changed since selection.
        let sessions_check = fast_judging::check_timer(CheckKind::Sessions);
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        // Reload retention for every verdict, including apply: a policy
        // changed after selection must take effect before any removal.
        let retention = crate::load_retention_policy(self.main_root())
            .map_err(|error| keep(format!("retention policy unreadable: {error}")))?;
        let evaluated_at_ms = now_ms();
        let mut current_spawned_owner = None;
        for session in &candidate.sessions {
            let current = self
                .store()
                .session(session.id)
                .map_err(|error| keep(format!("session {} unreadable: {error}", session.id)))?;
            if !current.status.is_closed() || current.cleanup_state != SessionCleanupState::Closed {
                return Err(keep(format!(
                    "session {} is not closed ({})",
                    session.id,
                    current.status.as_str()
                )));
            }
            if retention.closed_worktree_in_grace(&current, evaluated_at_ms) {
                return Err(keep(format!(
                    "session {} is within the {} hour closed-worktree grace period",
                    current.id, retention.closed_worktree_grace_hours
                )));
            }
            if canonical(Path::new(&current.worktree_path)) != candidate.worktree {
                return Err(keep(format!("session {} changed worktree", session.id)));
            }
            if current.id == candidate.spawned_owner.id {
                current_spawned_owner = Some(current);
            }
        }
        drop(sessions_check);
        let mut expected_session_ids = candidate.session_ids();
        expected_session_ids.sort_unstable();
        let owner = current_spawned_owner.ok_or_else(|| keep("no recorded owner".into()))?;
        let cohort_check = fast_judging::check_timer(CheckKind::Cohort);
        let current_cohort = match self
            .closed_checkout_cohort_for_removal(owner.id, &candidate.worktree, options.deadline)
            .map_err(|error| keep(format!("closed checkout cohort unreadable: {error}")))?
        {
            ClosedArtifactCheckoutCohortResolution::Available(cohort) => cohort,
            ClosedArtifactCheckoutCohortResolution::Unavailable(reason) => {
                return Err(
                    self.closed_cohort_unavailable_verdict(reason, candidate, &owner, deadline)
                );
            }
        };
        let ownership_reproof = current_cohort.ownership_reproof();
        #[cfg(test)]
        AFTER_REMOVAL_COHORT_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(&candidate.worktree);
            }
        });
        let mut current_session_ids = current_cohort.session_ids();
        current_session_ids.sort_unstable();
        if current_session_ids != expected_session_ids {
            return Err(keep("closed session set changed".into()));
        }
        if owner.origin != SessionOrigin::Spawned || owner.branch != candidate.spawned_owner.branch
        {
            return Err(keep("spawned ownership record changed".into()));
        }
        let persisted_witness = current_cohort.owner_witness().cloned();
        if persisted_witness.as_ref() != candidate.spawned_instance_witness.as_ref() {
            return Err(keep(
                "spawn-time worktree identity changed or is missing".into(),
            ));
        }
        drop(cohort_check);
        let ownership_check = fast_judging::check_timer(CheckKind::Ownership);
        let checkout = GitRepo::discover(&candidate.worktree)
            .map_err(|error| keep(format!("worktree could not be re-opened: {error}")))?;
        let current_witness = checkout.spawned_worktree_instance_witness();
        match candidate.spawned_instance_witness.as_ref() {
            Some(expected)
                if !current_witness
                    .as_ref()
                    .is_some_and(|current| expected.matches_current_instance(current)) =>
            {
                // Only the re-proof the cohort itself accepted, re-made now
                // against this fresh observation, may stand in for the exact
                // comparison.
                let reproved = if ownership_reproof.is_some()
                    && let Some(current) = current_witness.as_ref()
                {
                    match self.checkout_ownership_reproof(
                        OwnershipReproofLane::Automatic,
                        &owner,
                        &candidate.worktree,
                        expected,
                        current,
                        None,
                        deadline,
                    ) {
                        // The same kind of re-proof the cohort accepted.
                        Ok(reproof) => ownership_reproof == Some(reproof),
                        Err(ReproofFailure::Refused(_)) => false,
                        Err(ReproofFailure::Unavailable(_)) => return Err(NotRemoved::Defer),
                    }
                } else {
                    false
                };
                if !reproved {
                    return Err(keep(
                        "worktree instance no longer matches its spawn record".into(),
                    ));
                }
            }
            None => {}
            Some(_) => {}
        }
        if !self
            .is_broker_owned_worktree_checked(&owner, &candidate.worktree)
            .map_err(|error| keep(format!("worktree ownership could not be checked: {error}")))?
        {
            return Err(keep("not a broker-owned worktree".into()));
        }
        drop(ownership_check);
        let live_paths_check = fast_judging::check_timer(CheckKind::LivePaths);
        let fresh_live;
        let (live, live_worktrees): (&[Session], &[ResolvedWorktreePath]) = match live_index {
            Some(index) => index,
            None => {
                let live = self
                    .store()
                    .live_sessions()
                    .map_err(|error| keep(format!("live sessions unreadable: {error}")))?;
                let Some(resolved) = resolve_live_worktree_paths(&live, deadline) else {
                    return Err(NotRemoved::Defer);
                };
                fresh_live = (live, resolved);
                (fresh_live.0.as_slice(), fresh_live.1.as_slice())
            }
        };
        if deadline_expired(deadline) {
            return Err(NotRemoved::Defer);
        }
        let candidate_worktree = ResolvedWorktreePath::resolve(&candidate.worktree);
        if deadline_expired(deadline) {
            return Err(NotRemoved::Defer);
        }
        if let Some((index, evidence)) =
            first_live_worktree_overlap(&candidate_worktree, live_worktrees)
        {
            let reason = match evidence {
                LiveOverlapEvidence::Overlap => {
                    format!("session {} is open in this worktree", live[index].id)
                }
                LiveOverlapEvidence::Unknown => format!(
                    "session {} worktree overlap could not be resolved: {}",
                    live[index].id,
                    overlap_resolution_error(&candidate_worktree, &live_worktrees[index])
                ),
                LiveOverlapEvidence::Disjoint => {
                    unreachable!("only possible overlaps are returned")
                }
            };
            return Err(keep(reason));
        }
        drop(live_paths_check);

        // Proof 2: leases and gates. Processes are checked by the caller
        // with one system-wide snapshot.
        let leases_check = fast_judging::check_timer(CheckKind::Leases);
        let now = now_ms();
        let ids = candidate.session_ids();
        let leases = self
            .store()
            .unreleased_unexpired_leases_for_sessions_at(&ids, now)
            .map_err(|error| keep(format!("lease liveness unreadable: {error}")))?;
        if !leases.is_empty() {
            let lease_policy = crate::lease_liveness::LeaseLivenessPolicy::load(self.main_root());
            let liveness = crate::lease_liveness::assess(
                self.store(),
                &leases,
                lease_process_table.get_or_snapshot(),
                now,
                lease_policy,
            )
            .map_err(|error| keep(format!("lease liveness unreadable: {error}")))?;
            if let Some(lease) = liveness.iter().find(|lease| lease.liveness_evidence.holds) {
                return Err(keep(format!(
                    "lease on {} is {} and still holds",
                    lease.path,
                    lease.liveness.as_str()
                )));
            }
        }
        drop(leases_check);
        // Only a gate that could be using this checkout keeps it: one of its
        // own sessions' gates, one whose recorded checkout overlaps it, or one
        // whose checkout nothing identifies. A gate positively recorded in
        // another checkout no longer does. Gate admission takes the lifecycle
        // claim apply holds and registers an artifact-use producer the apply
        // reclaimer excludes, so no gate starts in this checkout after this.
        let gates_check = fast_judging::check_timer(CheckKind::Gates);
        if let Some(reason) =
            gate_witnesses.keep_reason(self.main_root(), &candidate_worktree, &ids)
        {
            return Err(keep(reason));
        }
        drop(gates_check);

        // Proof 3. Tracked files an interrupted removal already deleted do
        // not keep the checkout: their content is HEAD's, and proof 4 below
        // must still show HEAD contained before anything is removed.
        let marked = timed(CheckKind::Status, || {
            checkout.head_commit().ok().is_some_and(|head| {
                self.removal_attempt_recorded(&candidate.worktree, owner.id, &head)
            })
        });
        let interrupted_removal_deletions = match unclean_reason_resuming_interrupted_removal(
            &candidate.worktree,
            Some(candidate.branch()),
            self.main_root(),
            policy,
            marked,
        ) {
            Ok((None, deleted)) => deleted,
            Ok((Some(reason), _)) => return Err(keep(reason)),
            Err(error) => return Err(keep(format!("cleanliness could not be checked: {error}"))),
        };

        // Without a spawn-time identity, the prior proofs are useful only for
        // accurate diagnostics. Never allow a legacy or unsupported-platform
        // row to proceed to containment or removal based on current-path
        // observations alone.
        if candidate.spawned_instance_witness.is_none() || current_witness.is_none() {
            return Err(keep(
                "spawn-time worktree instance identity is unavailable".into(),
            ));
        }

        // Proof 4.
        let _containment_check = fast_judging::check_timer(CheckKind::Containment);
        let checkout = GitRepo::discover(&candidate.worktree)
            .map_err(|error| keep(format!("not a readable checkout: {error}")))?;
        let head = checkout
            .head_commit()
            .map_err(|error| keep(format!("HEAD unreadable: {error}")))?;
        let mut proof = match selected.filter(|proof| proof.head == head) {
            Some(proof) => proof.clone(),
            None => containment(self.repo_handle(), &head, &upstream.1, deep, deadline)?,
        };
        proof.interrupted_removal_deletions = interrupted_removal_deletions;
        proof.ownership_reproof = ownership_reproof;
        Ok(proof)
    }

    /// The verdict for a cohort automatic removal could not build: a
    /// specific, countable keep reason, or a deferral when the cohort or its
    /// re-proof only ran out of time. Every reason keeps the leading phrase
    /// older readers match.
    fn closed_cohort_unavailable_verdict(
        &self,
        reason: ClosedArtifactCheckoutCohortUnavailableReason,
        candidate: &AutoCleanupCandidate,
        owner: &Session,
        deadline: Option<Instant>,
    ) -> NotRemoved {
        const PREFIX: &str = "closed checkout cohort or ownership witness changed";
        if reason == ClosedArtifactCheckoutCohortUnavailableReason::DeadlineExceeded {
            return NotRemoved::Defer;
        }
        if !matches!(
            reason,
            ClosedArtifactCheckoutCohortUnavailableReason::SpawnedWitnessMismatch
                | ClosedArtifactCheckoutCohortUnavailableReason::OwnershipReproofRefused
        ) {
            return NotRemoved::Keep(format!("{PREFIX}: cohort unavailable ({})", reason.code()));
        }
        let manual = format!(
            "inspect it, then remove it with `aethyme broker finish cleanup {}`",
            owner.id
        );
        let Some(saved) = candidate.spawned_instance_witness.as_ref() else {
            return NotRemoved::Keep(format!(
                "{PREFIX}: {OWNERSHIP_WITNESS_NEVER_RECORDED} for session {}; {manual}",
                owner.id
            ));
        };
        if !saved.is_supported() {
            return NotRemoved::Keep(format!(
                "{PREFIX}: {OWNERSHIP_WITNESS_LEGACY} (schema {}); {manual}",
                saved.schema_version
            ));
        }
        let current = {
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            GitRepo::discover(&candidate.worktree)
                .ok()
                .and_then(|checkout| checkout.spawned_worktree_instance_witness())
        };
        let Some(current) = current else {
            return NotRemoved::Keep(format!(
                "{PREFIX}: spawn-time worktree instance identity is unavailable"
            ));
        };
        match self.renumbered_witness_reproof(owner, &candidate.worktree, saved, &current, deadline)
        {
            Err(ReproofFailure::Refused(refusal)) => {
                NotRemoved::Keep(format!("{PREFIX}: {refusal}"))
            }
            Err(ReproofFailure::Unavailable(_)) => NotRemoved::Defer,
            Ok(()) => NotRemoved::Keep(format!(
                "{PREFIX}: cohort membership changed during re-proof"
            )),
        }
    }

    fn session_retirement_cursor(
        &mut self,
    ) -> Result<(SessionRetirementCursor, Option<String>), BrokerOpError> {
        let raw = self.store().meta_get(SESSION_RETIREMENT_CURSOR_KEY)?;
        Ok((decode_session_retirement_cursor(raw.as_deref()), raw))
    }

    fn persist_session_retirement_cursor(
        &mut self,
        cursor: &SessionRetirementCursor,
        expected_raw: &mut Option<String>,
    ) -> Result<bool, BrokerOpError> {
        let value = serde_json::to_string(cursor)
            .expect("serializing the session-retirement cursor is infallible");
        let changed = match self.store().meta_compare_and_set_for_session_retirement(
            SESSION_RETIREMENT_CURSOR_KEY,
            expected_raw.as_deref(),
            &value,
        ) {
            Ok(changed) => changed,
            Err(crate::BrokerError::SessionRetirementDatabaseBusy) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if !changed {
            return Err(BrokerOpError::AutoCleanupCursorChanged);
        }
        *expected_raw = Some(value);
        Ok(true)
    }

    /// Retire a bounded keyset page of sessions whose existing abandonment
    /// window has elapsed. This runs only in the mutating maintenance path;
    /// cleanup planning and read-only session snapshots remain unchanged.
    /// Each pass reports whether it moved the cursor
    /// (`retirement_cursor_advanced` or `retirement_cursor_unmoved`).
    fn retire_abandoned_sessions(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<SessionRetirementSummary, BrokerOpError> {
        let summary = self.retire_abandoned_session_page(deadline)?;
        if let (Some(start), Some(end)) = (summary.cursor_start, summary.cursor) {
            crate::phase_telemetry::add(
                if start == end {
                    "retirement_cursor_unmoved"
                } else {
                    "retirement_cursor_advanced"
                },
                Duration::ZERO,
            );
        }
        Ok(summary)
    }

    /// The session a previous pass left the retirement cursor on because its
    /// landed proof ran out of budget, if any.
    fn retirement_budget_hold(&self) -> Option<i64> {
        self.store_ref()
            .meta_get(SESSION_RETIREMENT_BUDGET_HOLD_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|session_id| *session_id > 0)
    }

    fn set_retirement_budget_hold(&self, session_id: Option<i64>) {
        crate::warn_unrecorded(
            "remember the retirement budget hold",
            self.store_ref().meta_set(
                SESSION_RETIREMENT_BUDGET_HOLD_KEY,
                &session_id.unwrap_or(0).to_string(),
            ),
        );
    }

    fn retire_abandoned_session_page(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<SessionRetirementSummary, BrokerOpError> {
        let mut summary = SessionRetirementSummary::default();
        #[cfg(test)]
        if RETIREMENT_EXHAUSTS_ITS_SHARE.with(std::cell::Cell::get)
            && let Some(deadline) = deadline
        {
            // Stand-in for a landed lap whose proofs use every millisecond
            // the lane is given.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: 0,
                });
            return Ok(summary);
        }
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: 0,
                });
            return Ok(summary);
        }

        let policy = match crate::load_retention_policy(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                summary.deferred = 1;
                summary.reasons.push(
                    SessionRetirementDeferredReason::RetentionPolicyUnavailable {
                        message: error.to_string(),
                    },
                );
                return Ok(summary);
            }
        };
        let window_ms = i64::from(policy.session_abandoned_after_hours).saturating_mul(3_600_000);
        if window_ms <= 0 {
            return Ok(summary);
        }
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: 0,
                });
            return Ok(summary);
        }

        let (mut cursor, mut expected_raw) = self.session_retirement_cursor()?;
        summary.cursor_start = Some(SessionRetirementCursorPosition {
            after_session_id: cursor.after_session_id,
            high_water_session_id: cursor.high_water_session_id,
        });
        let current_high_water = self.store().open_session_max_id()?;
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        if cursor.after_session_id >= cursor.high_water_session_id
            || current_high_water <= cursor.after_session_id
        {
            cursor.after_session_id = 0;
            cursor.high_water_session_id = current_high_water;
            if !self.persist_session_retirement_cursor(&cursor, &mut expected_raw)? {
                summary.deferred = 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::CursorBusy);
                return Ok(summary);
            }
        }
        summary.record_cursor(&cursor);
        if cursor.high_water_session_id == 0 {
            return Ok(summary);
        }

        let landed_window_ms =
            i64::from(policy.session_landed_after_hours).saturating_mul(3_600_000);
        let mut decline_memo = LandedDeclineMemo::load(self);
        let upstream_tip = self.repo_handle().upstream_default().map(|(_, tip)| tip);
        // Sessions that already show landing evidence go first, ahead of
        // the oldest-first cursor whose head is full of sessions that will
        // never close. Same proof, same re-checks: only the order changes.
        let merged_window_ms =
            i64::from(policy.session_merged_after_hours).saturating_mul(3_600_000);
        let prioritized = if landed_window_ms > 0 || merged_window_ms > 0 {
            self.retire_likely_landed_sessions(
                landed_window_ms,
                merged_window_ms,
                lane_deadline(Instant::now(), deadline, (1, 2)),
                upstream_tip.as_deref(),
                &mut decline_memo,
                &mut summary,
            )?
        } else {
            std::collections::BTreeSet::new()
        };

        let sessions = self.store().open_session_page(
            cursor.after_session_id,
            cursor.high_water_session_id,
            SESSION_RETIREMENT_PAGE_SIZE,
        )?;
        if deadline_expired(deadline) {
            summary.deferred = 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        if sessions.is_empty() {
            cursor.after_session_id = cursor.high_water_session_id;
            if !self.persist_session_retirement_cursor(&cursor, &mut expected_raw)? {
                summary.deferred = 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::CursorBusy);
            }
            return Ok(summary);
        }

        let retirement_now = now_ms();
        let candidate_window_ms = if landed_window_ms > 0 {
            window_ms.min(landed_window_ms)
        } else {
            window_ms
        };
        // The no-work lane judges sessions from its shorter window on.
        let no_work_window_ms = policy.no_work_abandonment_window_ms();
        let candidate_window_ms = if no_work_window_ms > 0 {
            candidate_window_ms.min(no_work_window_ms)
        } else {
            candidate_window_ms
        };
        let retirement_candidates = sessions
            .iter()
            .filter(|session| {
                crate::decide_abandonment(
                    &crate::SessionActivity {
                        session_id: session.id,
                        last_activity_at: session.last_activity_at,
                        created_at: session.created_at,
                        agent_alive: None,
                        closed: false,
                    },
                    retirement_now,
                    candidate_window_ms,
                )
                .is_abandoned()
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        let mut holder_process_table = crate::session_holder::RecordedHolderProcessTable::default();
        holder_process_table.prime(self.store_ref(), &retirement_candidates, deadline);
        let mut not_due_memo = NotDueMemo::load(self, window_ms, retirement_now);
        let pass_started = Instant::now();
        let mut landed_judged_in_pass = 0_usize;
        let mut budget_hold = self.retirement_budget_hold();
        for session in sessions {
            if deadline_expired(deadline) {
                summary.deferred += 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::DeadlineExceeded {
                        after_session_id: cursor.after_session_id,
                    });
                return Ok(summary);
            }

            let allow_snapshot = match deadline {
                Some(deadline) => {
                    deadline.saturating_duration_since(Instant::now())
                        > crate::session_holder::PROCESS_TABLE_TIMEOUT + Duration::from_millis(100)
                }
                None => true,
            };
            let now = now_ms();
            let old_enough_for_abandonment = crate::decide_abandonment(
                &crate::SessionActivity {
                    session_id: session.id,
                    last_activity_at: session.last_activity_at,
                    created_at: session.created_at,
                    agent_alive: None,
                    closed: false,
                },
                now,
                window_ms,
            )
            .is_abandoned();
            let idle_enough_for_landed = landed_window_ms > 0
                && !prioritized.contains(&session.id)
                && crate::decide_abandonment(
                    &crate::SessionActivity {
                        session_id: session.id,
                        last_activity_at: session.last_activity_at,
                        created_at: session.created_at,
                        agent_alive: None,
                        closed: false,
                    },
                    now,
                    landed_window_ms,
                )
                .is_abandoned();
            // The landed lane is asked first, whatever the session's age: its
            // proof (work on the default branch, clean worktree, no process)
            // is stronger than idleness, so it may outlive a holder process
            // the abandonment window must wait for. A decline that leaves the
            // question open falls through to the abandonment window.
            let mut landed_decline = None;
            // A session already past the abandonment window is asked that
            // question first: it needs no Git, and closing deletes nothing
            // whichever lane closes it. The landed proof used to run first
            // for every such session and spend the pass on content searches
            // and worktree scans, so sessions days past the window stayed
            // open behind them. The landed lane is still asked when only it
            // could close the session: a live holder whose task lease expired.
            // A session kept for recent activity is not judged again before
            // its activity plus the window: the judgement would keep it.
            let memo_skipped = old_enough_for_abandonment && not_due_memo.not_due(session.id, now);
            if memo_skipped {
                crate::phase_telemetry::add("retirement_not_due_memo_skips", Duration::ZERO);
            }
            let abandonment_tried = old_enough_for_abandonment && !memo_skipped;
            let mut outcome = if abandonment_tried {
                let _span = crate::phase_telemetry::span("retirement_abandonment_first");
                self.try_close_abandoned_session(
                    session.id,
                    now,
                    window_ms,
                    &mut holder_process_table,
                    allow_snapshot,
                    deadline,
                )?
            } else {
                crate::broker::AbandonedSessionCloseOutcome::NotDue
            };
            if abandonment_tried {
                summary.abandonment_examined += 1;
                let closed = matches!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed);
                crate::phase_telemetry::add(
                    if closed {
                        "retirement_abandonment_first_closed"
                    } else {
                        "retirement_abandonment_first_kept"
                    },
                    Duration::ZERO,
                );
                match holder_process_table.not_due_before() {
                    Some(due_at) if !closed => {
                        not_due_memo.record(self, session.id, due_at, now);
                    }
                    _ => not_due_memo.forget(self, session.id),
                }
                if closed {
                    summary.abandonment_closed += 1;
                } else {
                    // The close path counted `session_abandonment_kept_<reason>`
                    // already; the summary carries the same breakdown to
                    // `broker status`.
                    let reason = holder_process_table
                        .keep_reason()
                        .unwrap_or(crate::broker::AbandonmentKeepReason::Unrecorded);
                    *summary.abandonment_kept.entry(reason.code()).or_default() += 1;
                }
            }
            let idle_enough_for_landed = idle_enough_for_landed
                && matches!(
                    outcome,
                    crate::broker::AbandonedSessionCloseOutcome::NotDue
                        | crate::broker::AbandonedSessionCloseOutcome::ActiveProcess
                );
            let memo_key = if idle_enough_for_landed {
                decline_memo.key(self, &session, upstream_tip.as_deref())
            } else {
                None
            };
            let remembered = memo_key
                .as_deref()
                .and_then(|key| decline_memo.remembered(session.id, key));
            if let Some(reason) = remembered {
                // Same head, same default-branch tip, same pull-request and
                // representation evidence: the landed lane would decline
                // again for the same reason. The abandonment window below
                // still applies.
                crate::phase_telemetry::add("landed_decline_memo_skips", Duration::ZERO);
                landed_decline = Some(reason);
            } else if idle_enough_for_landed {
                landed_judged_in_pass += 1;
                summary.landed_examined += 1;
                crate::phase_telemetry::add("landed_sessions_examined", Duration::ZERO);
                let started = Instant::now();
                #[cfg(test)]
                let forced_budget_exhausted =
                    LAP_LANDED_PROOF_EXHAUSTS_BUDGET.with(std::cell::Cell::get);
                #[cfg(not(test))]
                let forced_budget_exhausted = false;
                outcome = if forced_budget_exhausted {
                    crate::broker::AbandonedSessionCloseOutcome::LandedDeclined(
                        crate::broker::LandedSessionDecline::BudgetExhausted,
                    )
                } else {
                    self.try_close_landed_session(
                        session.id,
                        now,
                        landed_window_ms,
                        &mut holder_process_table,
                        allow_snapshot,
                        deadline,
                    )?
                };
                let elapsed = started.elapsed();
                crate::phase_telemetry::add("landed_session_retirement", elapsed);
                summary.landed_elapsed_ms = summary
                    .landed_elapsed_ms
                    .saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
                landed_decline = crate::broker::LandedSessionDecline::from_outcome(outcome);
                if landed_decline.is_none() {
                    summary.landed_closed += 1;
                    crate::phase_telemetry::add("landed_sessions_closed", elapsed);
                }
                decline_memo.record(self, session.id, memo_key.as_deref(), landed_decline);
                // Leave the cursor on this session so the next pass, usually
                // the background worker with its larger budget, resumes here;
                // unless this one session alone used a long pass, which must
                // not hold every later session back.
                let held_alone_too_long = landed_judged_in_pass == 1
                    && pass_started.elapsed() >= LANDED_SEARCH_HOLD_LIMIT;
                // A pass whose landed lap starts with little budget left (the
                // likely-landed queue spent the rest) never holds one session
                // for the hold limit, so that test alone pinned the cursor on
                // it pass after pass. The cursor is left on a session for one
                // pass only: the next pass judges it once more and moves on.
                let budget_exhausted =
                    landed_decline == Some(crate::broker::LandedSessionDecline::BudgetExhausted);
                let held_last_pass = budget_hold == Some(session.id);
                if budget_exhausted && (held_alone_too_long || held_last_pass) {
                    crate::phase_telemetry::add(
                        "retirement_cursor_budget_hold_released",
                        Duration::ZERO,
                    );
                }
                if budget_exhausted && !held_alone_too_long && !held_last_pass {
                    self.set_retirement_budget_hold(Some(session.id));
                    crate::phase_telemetry::add("retirement_cursor_budget_held", Duration::ZERO);
                    let reason = crate::broker::LandedSessionDecline::BudgetExhausted;
                    *summary.landed_declined.entry(reason.code()).or_default() += 1;
                    crate::phase_telemetry::add(reason.telemetry_name(), Duration::ZERO);
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::DeadlineExceeded {
                            after_session_id: cursor.after_session_id,
                        });
                    return Ok(summary);
                }
            }
            let fall_through = landed_decline
                .is_none_or(crate::broker::LandedSessionDecline::allows_abandonment_fallback)
                && !matches!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed);
            if fall_through && old_enough_for_abandonment && !abandonment_tried && !memo_skipped {
                outcome = self.try_close_abandoned_session(
                    session.id,
                    now,
                    window_ms,
                    &mut holder_process_table,
                    allow_snapshot,
                    deadline,
                )?;
            }
            // A session not yet past the full window may still close by the
            // no-work window; the not-due memo never hides it from that lane.
            let mut closed_no_work = false;
            let old_enough_for_no_work = no_work_window_ms > 0
                && crate::decide_abandonment(
                    &crate::SessionActivity {
                        session_id: session.id,
                        last_activity_at: session.last_activity_at,
                        created_at: session.created_at,
                        agent_alive: None,
                        closed: false,
                    },
                    now,
                    no_work_window_ms,
                )
                .is_abandoned();
            if fall_through && !old_enough_for_abandonment && old_enough_for_no_work {
                let no_work = self.try_close_no_work_session(
                    session.id,
                    now,
                    no_work_window_ms,
                    &mut holder_process_table,
                    allow_snapshot,
                    deadline,
                )?;
                // A keep here is counted by the lane itself and leaves the
                // pass's outcome for this session as it was.
                if no_work == crate::broker::AbandonedSessionCloseOutcome::Closed {
                    outcome = no_work;
                    closed_no_work = true;
                }
            }
            if let Some(reason) = landed_decline
                && !matches!(outcome, crate::broker::AbandonedSessionCloseOutcome::Closed)
            {
                *summary.landed_declined.entry(reason.code()).or_default() += 1;
                crate::phase_telemetry::add(reason.telemetry_name(), Duration::ZERO);
            }
            match outcome {
                crate::broker::AbandonedSessionCloseOutcome::Closed => {
                    summary.retired += 1;
                    // The cursor lane closes by the landed window or the
                    // abandonment window; merged-pull-request closes happen
                    // in the likely-landed queue.
                    let reason = if closed_no_work {
                        crate::session_auto_close::CLOSE_REASON_INACTIVE_NO_WORK
                    } else if landed_decline.is_none() && idle_enough_for_landed {
                        crate::session_auto_close::CLOSE_REASON_LANDED
                    } else {
                        "abandoned"
                    };
                    summary.record_close(reason);
                    summary.record_holder_evidence(self.closed_holder_evidence(session.id));
                }
                crate::broker::AbandonedSessionCloseOutcome::HolderUnavailable => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::HolderLivenessUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::LeaseUnavailable
                | crate::broker::AbandonedSessionCloseOutcome::LeaseHeld => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::LeaseLivenessUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::WorktreeActivityUnavailable => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::WorktreeActivityUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::LifecycleBusy => {
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::LifecycleBusy {
                            session_id: session.id,
                        });
                }
                crate::broker::AbandonedSessionCloseOutcome::OperationGuardBusy => {
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::OperationGuardBusy {
                            session_id: session.id,
                        });
                }
                crate::broker::AbandonedSessionCloseOutcome::OperationActive => {
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::OperationActive {
                            session_id: session.id,
                        });
                }
                crate::broker::AbandonedSessionCloseOutcome::OperationEvidenceUnavailable => {
                    summary.deferred += 1;
                    summary.reasons.push(
                        SessionRetirementDeferredReason::OperationEvidenceUnavailable {
                            session_id: session.id,
                        },
                    );
                }
                crate::broker::AbandonedSessionCloseOutcome::LegacyArtifactMarkers(markers) => {
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::LegacyArtifactMarkers {
                            session_id: session.id,
                            markers,
                        });
                }
                crate::broker::AbandonedSessionCloseOutcome::DatabaseBusy => {
                    summary.deferred += 1;
                    summary
                        .reasons
                        .push(SessionRetirementDeferredReason::DatabaseBusy {
                            session_id: session.id,
                        });
                    // Keep the cursor on this row. Do not perform another DB
                    // write while the competing writer still holds the lock.
                    return Ok(summary);
                }
                crate::broker::AbandonedSessionCloseOutcome::NotDue
                | crate::broker::AbandonedSessionCloseOutcome::Changed
                | crate::broker::AbandonedSessionCloseOutcome::ActiveProcess
                | crate::broker::AbandonedSessionCloseOutcome::LandedDeclined(_) => {}
            }
            cursor.after_session_id = session.id;
            if !self.persist_session_retirement_cursor(&cursor, &mut expected_raw)? {
                summary.deferred += 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::CursorBusy);
                return Ok(summary);
            }
            summary.record_cursor(&cursor);
            if budget_hold.is_some_and(|held| held <= session.id) {
                self.set_retirement_budget_hold(None);
                budget_hold = None;
            }
        }

        if deadline_expired(deadline) {
            summary.deferred += 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::DeadlineExceeded {
                    after_session_id: cursor.after_session_id,
                });
            return Ok(summary);
        }
        let more = self.store().open_session_page(
            cursor.after_session_id,
            cursor.high_water_session_id,
            1,
        )?;
        if more.is_empty() {
            cursor.after_session_id = cursor.high_water_session_id;
            if self.persist_session_retirement_cursor(&cursor, &mut expected_raw)? {
                summary.record_cursor(&cursor);
            } else {
                summary.deferred += 1;
                summary
                    .reasons
                    .push(SessionRetirementDeferredReason::CursorBusy);
            }
        } else {
            summary.deferred += 1;
            summary
                .reasons
                .push(SessionRetirementDeferredReason::MoreSessionsRemain {
                    after_session_id: cursor.after_session_id,
                    high_water_session_id: cursor.high_water_session_id,
                });
        }
        Ok(summary)
    }

    /// Offer the landed lane, first, the open sessions idle past the landed
    /// window that already show landing evidence: a merged pull request in
    /// the cached provider data, or a branch tip (with commits of its own)
    /// that is an ancestor of the fetched default branch, read with one
    /// `for-each-ref --merged` listing. Every proof and re-check is
    /// `try_close_landed_session`'s, unchanged; only the order differs from
    /// the oldest-first cursor. Returns the sessions it judged, which the
    /// cursor lane does not judge again in this pass.
    fn retire_likely_landed_sessions(
        &mut self,
        landed_window_ms: i64,
        merged_window_ms: i64,
        deadline: Option<Instant>,
        upstream_tip: Option<&str>,
        decline_memo: &mut LandedDeclineMemo,
        summary: &mut SessionRetirementSummary,
    ) -> Result<std::collections::BTreeSet<i64>, BrokerOpError> {
        let _span = crate::phase_telemetry::span("landed_likely_queue");
        let mut judged = std::collections::BTreeSet::new();
        if deadline_expired(deadline) {
            return Ok(judged);
        }
        let now = now_ms();
        let merged_pull_requests = self
            .store_ref()
            .open_sessions_with_merged_pull_request()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let follow_ups = self
            .store_ref()
            .open_adopted_sessions_on_spawned_checkouts()
            .unwrap_or_default();
        // One `for-each-ref --merged` walk over every local branch: bounded by
        // the lane deadline and timed, since it runs before any session.
        let (merged_branches, tips) = {
            let _span = crate::phase_telemetry::span("landed_likely_branch_listing");
            let _git_deadline = deadline.map(crate::git::limit_git_until);
            (
                upstream_tip
                    .and_then(|tip| self.repo_handle().local_branches_merged_into(tip))
                    .unwrap_or_default(),
                self.repo_handle().local_branch_tips().unwrap_or_default(),
            )
        };
        let after = self
            .store_ref()
            .meta_get(LIKELY_LANDED_CURSOR_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let live = self.store_ref().live_sessions()?;
        // Streaks and unprovable marks of sessions no longer open go once per
        // pass, bounded by the stored entries.
        self.prune_landed_proof_timeouts(
            &live
                .iter()
                .filter(|session| session.cleanup_state == SessionCleanupState::Open)
                .map(|session| session.id)
                .collect(),
        );
        let mut likely = live
            .into_iter()
            .filter(|session| {
                if session.cleanup_state != SessionCleanupState::Open {
                    return false;
                }
                let branch_ref = format!("refs/heads/{}", session.branch);
                let on_default = merged_branches.contains(&branch_ref)
                    && tips
                        .get(&branch_ref)
                        .is_some_and(|tip| session.diff_base.as_deref() != Some(tip.as_str()));
                let idle_past = |window_ms: i64| {
                    window_ms > 0
                        && crate::decide_abandonment(
                            &crate::SessionActivity {
                                session_id: session.id,
                                last_activity_at: session.last_activity_at,
                                created_at: session.created_at,
                                agent_alive: None,
                                closed: false,
                            },
                            now,
                            window_ms,
                        )
                        .is_abandoned()
                };
                ((merged_pull_requests.contains(&session.id) || on_default)
                    && idle_past(landed_window_ms))
                    || (merged_pull_request_lane(
                        session,
                        &merged_pull_requests,
                        &follow_ups,
                        merged_window_ms,
                    ) && idle_past(merged_window_ms))
            })
            .collect::<Vec<_>>();
        // Sessions with a merged pull request first: they are the ones that
        // usually close. Branch ancestry alone follows, then the rotation.
        likely.sort_by_key(|session| likely_landed_order(session.id, &merged_pull_requests, after));
        likely.truncate(LIKELY_LANDED_PASS_LIMIT);
        if likely.is_empty() {
            return Ok(judged);
        }
        let ids = likely.iter().map(|session| session.id).collect::<Vec<_>>();
        // The session that has been timing out longest is judged last, with
        // whatever is left of the lane's share: every session is eventually
        // judged conclusively, however slow its checkout.
        // The session that has been timing out longest gets whatever is left
        // of the lane's share: last, after the normal queue, and first every
        // fourth such pass, so it is reached even when the queue alone would
        // spend the share. A session that runs out of time even when it went
        // first, with the whole share, is reported.
        let grant = self.oldest_timed_out_session(&ids);
        let mut grant_first = false;
        if let Some(index) = grant.and_then(|grant| likely.iter().position(|s| s.id == grant)) {
            let granted = likely.remove(index);
            if self.landed_proof_grant_goes_first() {
                crate::phase_telemetry::add("landed_proof_long_budget_first", Duration::ZERO);
                grant_first = true;
                likely.insert(0, granted);
            } else {
                likely.push(granted);
            }
        }
        let mut process_table = crate::session_holder::RecordedHolderProcessTable::default();
        process_table.prime(self.store_ref(), &ids, deadline);
        for session in likely {
            if deadline_expired(deadline) {
                break;
            }
            let key = decline_memo.key(self, &session, upstream_tip);
            if key
                .as_deref()
                .and_then(|key| decline_memo.remembered(session.id, key))
                .is_some()
            {
                continue;
            }
            let _grant =
                (grant == Some(session.id)).then(|| crate::broker::long_proof_grant(session.id));
            judged.insert(session.id);
            let allow_snapshot = match deadline {
                Some(deadline) => {
                    deadline.saturating_duration_since(Instant::now())
                        > crate::session_holder::PROCESS_TABLE_TIMEOUT + Duration::from_millis(100)
                }
                None => true,
            };
            summary.landed_examined += 1;
            crate::phase_telemetry::add("landed_sessions_examined", Duration::ZERO);
            crate::phase_telemetry::add("landed_likely_examined", Duration::ZERO);
            let started = Instant::now();
            // A spawned session whose every pull request merged takes the
            // shorter merged window and its stricter running-work checks; any
            // other session keeps the landed window.
            let merged_lane = merged_pull_request_lane(
                &session,
                &merged_pull_requests,
                &follow_ups,
                merged_window_ms,
            );
            let outcome = if merged_lane {
                summary.merged_examined += 1;
                crate::phase_telemetry::add("landed_merged_pr_examined", Duration::ZERO);
                self.try_close_merged_pull_request_session(
                    session.id,
                    now_ms(),
                    merged_window_ms,
                    &mut process_table,
                    allow_snapshot,
                    deadline,
                )?
            } else {
                self.try_close_landed_session(
                    session.id,
                    now_ms(),
                    landed_window_ms,
                    &mut process_table,
                    allow_snapshot,
                    deadline,
                )?
            };
            // Evidence the merged lane cannot read (an unknown gate, operation
            // or artifact inventory) used to wait only for the landed window;
            // past that window the landed lane decides, as it did before.
            let merged_unknown = merged_lane
                && matches!(
                    crate::broker::LandedSessionDecline::from_outcome(outcome),
                    Some(crate::broker::LandedSessionDecline::OperationUnknown)
                );
            let past_landed_window = landed_window_ms > 0
                && crate::decide_abandonment(
                    &crate::SessionActivity {
                        session_id: session.id,
                        last_activity_at: session.last_activity_at,
                        created_at: session.created_at,
                        agent_alive: None,
                        closed: false,
                    },
                    now_ms(),
                    landed_window_ms,
                )
                .is_abandoned();
            let fell_back = merged_unknown && past_landed_window;
            let outcome = if fell_back {
                crate::phase_telemetry::add("landed_merged_pr_fallback_to_landed", Duration::ZERO);
                self.try_close_landed_session(
                    session.id,
                    now_ms(),
                    landed_window_ms,
                    &mut process_table,
                    allow_snapshot,
                    deadline,
                )?
            } else {
                outcome
            };
            let merged_lane = merged_lane && !fell_back;
            let elapsed = started.elapsed();
            crate::phase_telemetry::add("landed_session_retirement", elapsed);
            summary.landed_elapsed_ms = summary
                .landed_elapsed_ms
                .saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
            crate::warn_unrecorded(
                "advance the likely-landed queue",
                self.store_ref()
                    .meta_set(LIKELY_LANDED_CURSOR_KEY, &session.id.to_string()),
            );
            let decline = crate::broker::LandedSessionDecline::from_outcome(outcome);
            if grant_first
                && grant == Some(session.id)
                && decline == Some(crate::broker::LandedSessionDecline::ProofTimedOut)
            {
                self.mark_landed_proof_unprovable(session.id);
            }
            decline_memo.record(self, session.id, key.as_deref(), decline);
            match decline {
                None => {
                    summary.landed_closed += 1;
                    summary.retired += 1;
                    summary.record_holder_evidence(self.closed_holder_evidence(session.id));
                    summary.record_close(if merged_lane {
                        crate::session_auto_close::CLOSE_REASON_MERGED_PULL_REQUEST
                    } else {
                        crate::session_auto_close::CLOSE_REASON_LANDED
                    });
                    crate::phase_telemetry::add("landed_sessions_closed", elapsed);
                    crate::phase_telemetry::add("landed_likely_closed", elapsed);
                    if merged_lane {
                        summary.merged_closed += 1;
                        crate::phase_telemetry::add("landed_merged_pr_closed", elapsed);
                    }
                }
                Some(reason) => {
                    *summary.landed_declined.entry(reason.code()).or_default() += 1;
                    crate::phase_telemetry::add(reason.telemetry_name(), Duration::ZERO);
                    if matches!(
                        reason,
                        crate::broker::LandedSessionDecline::BudgetExhausted
                            | crate::broker::LandedSessionDecline::Busy
                    ) {
                        break;
                    }
                }
            }
        }
        Ok(judged)
    }

    /// Run one bounded session-retirement page without planning or applying
    /// whole-checkout cleanup. The detached retirement lane uses this while a
    /// closed checkout is still inside its independent cleanup grace period.
    pub(crate) fn retire_abandoned_sessions_for_recovery(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let retirement = self.retire_abandoned_sessions(deadline)?;
        let (enabled, config_error) = match AutoCleanupPolicy::load(self.main_root()) {
            Ok(policy) => (policy.auto_remove, None),
            Err(error) => (false, Some(error)),
        };
        let mut report = AutoCleanupReport {
            enabled,
            config_error,
            evaluated_at_ms: now_ms(),
            ..AutoCleanupReport::default()
        };
        report.absorb_landed_retirement(&retirement);
        report.deferred = retirement.deferred;
        report.sessions_retired = retirement.retired;
        report.retirement_deferred_reasons = retirement.reasons;
        // Closed sessions whose checkout is already gone are finished in the
        // same worker, with what is left of the pass.
        if !deadline_expired(deadline) {
            let missing = self.finish_closed_sessions_with_missing_checkout(deadline)?;
            report.missing_checkout_examined = missing.examined;
            report.missing_checkout_finished = missing.finished;
            report.missing_checkout_kept = missing
                .kept
                .into_iter()
                .map(|(code, count)| (code.to_string(), count))
                .collect();
        }
        Ok(report)
    }

    /// Run one auto-cleanup pass within `deadline`: select, then revalidate
    /// and remove. The caller holds the GC lock for the whole pass.
    pub(crate) fn auto_remove_disposable_checkouts(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let pass_started = Instant::now();
        let capacity_pressure = self.checkout_capacity_pressure(deadline);
        // Retirement only prepares capacity (a closed checkout still waits
        // out its grace); removal frees it. Retirement gets a bounded share
        // and removal the rest of the pass, including whatever retirement
        // leaves unused, so a heavy retirement lap can no longer starve the
        // removal lane of every pass.
        let retirement_deadline =
            lane_deadline(pass_started, deadline, retirement_share(capacity_pressure));
        let mut lanes = AutoCleanupLanes {
            capacity_pressure,
            retirement_budget_ms: duration_ms(
                retirement_deadline.map(|end| end.saturating_duration_since(pass_started)),
            ),
            ..AutoCleanupLanes::default()
        };
        let retirement = {
            let _span = crate::phase_telemetry::span("auto_cleanup_retirement_lane");
            self.retire_abandoned_sessions(retirement_deadline)?
        };
        let removal_started = Instant::now();
        lanes.retirement_elapsed_ms = duration_ms(Some(removal_started - pass_started));
        lanes.removal_budget_ms =
            duration_ms(deadline.map(|end| end.saturating_duration_since(removal_started)));
        if retirement.reasons.iter().any(|reason| {
            matches!(
                reason,
                SessionRetirementDeferredReason::DatabaseBusy { .. }
                    | SessionRetirementDeferredReason::CursorBusy
            )
        }) {
            // Database contention is a reason to defer this pass as a whole.
            // Skip checkout planning and its cursor writes while the other
            // database writer may still be active.
            let (enabled, config_error) = match AutoCleanupPolicy::load(self.main_root()) {
                Ok(policy) => (policy.auto_remove, None),
                Err(error) => (false, Some(error.to_string())),
            };
            let mut report = AutoCleanupReport {
                enabled,
                config_error,
                evaluated_at_ms: now_ms(),
                ..AutoCleanupReport::default()
            };
            report.absorb_landed_retirement(&retirement);
            report.deferred = retirement.deferred;
            report.sessions_retired = retirement.retired;
            report.retirement_deferred_reasons = retirement.reasons;
            report.lanes = Some(lanes);
            return Ok(report);
        }
        let selection_deadline = lane_deadline(removal_started, deadline, REMOVAL_SELECTION_SHARE);
        lanes.removal_apply_reserve_ms = duration_ms(
            deadline
                .zip(selection_deadline)
                .map(|(end, selection_end)| end.saturating_duration_since(selection_end)),
        );
        let mut plan = {
            let _span = crate::phase_telemetry::span("auto_cleanup_removal_lane");
            self.auto_cleanup_plan(selection_deadline)?
        };
        // Revalidation and removal may use the whole lane.
        plan.deadline = deadline;
        lanes.removal_selected = plan.selected.len();
        plan.report.absorb_landed_retirement(&retirement);
        plan.report.sessions_retired = retirement.retired;
        plan.report.deferred += retirement.deferred;
        plan.report.retirement_deferred_reasons = retirement.reasons;
        let planned = plan.report.lanes.unwrap_or_default();
        lanes.removal_examined = planned.removal_examined;
        lanes.removal_memo_skips = planned.removal_memo_skips;
        lanes.removal_priority_examined = planned.removal_priority_examined;
        lanes.removal_priority_selected = planned.removal_priority_selected;
        lanes.removal_scan_ms = planned.removal_scan_ms;
        lanes.removal_inventory_unavailable = planned.removal_inventory_unavailable;
        lanes.removal_probe_ms = planned.removal_probe_ms;
        lanes.removal_inventory_wait = planned.removal_inventory_wait;
        lanes.removal_gate_checks = planned.removal_gate_checks;
        lanes.removal_priority_memo_skips = planned.removal_priority_memo_skips;
        lanes.removal_db_in_grace = planned.removal_db_in_grace;
        lanes.removal_db_session_open = planned.removal_db_session_open;
        lanes.removal_priority_db_skips = planned.removal_priority_db_skips;
        lanes.removal_memo_hits = planned.removal_memo_hits;
        lanes.removal_check_timings = planned.removal_check_timings;
        lanes.removal_elapsed_ms = duration_ms(Some(removal_started.elapsed()));
        plan.report.lanes = Some(lanes);
        let report = self.auto_cleanup_apply(plan)?;
        record_lane_telemetry(&report);
        Ok(report)
    }

    /// Whether registered linked checkouts are at or above three quarters of
    /// `[worktrees] max_checkouts`. One `git worktree list`; skipped (false)
    /// when the pass is too short to afford it or the limit is disabled.
    ///
    /// A pass too short to list checkouts reads the count `broker start` or
    /// the background worker last recorded instead, while it is recent;
    /// without one it reports no pressure, as before.
    pub(crate) fn checkout_capacity_pressure(&self, deadline: Option<Instant>) -> bool {
        if deadline.is_some_and(|end| {
            end.saturating_duration_since(Instant::now()) < CAPACITY_PROBE_MIN_BUDGET
        }) {
            let recorded = self.recorded_checkout_usage(now_ms());
            crate::phase_telemetry::add(
                if recorded.is_some() {
                    "auto_cleanup_capacity_recorded"
                } else {
                    "auto_cleanup_capacity_unknown"
                },
                Duration::ZERO,
            );
            return recorded.is_some_and(|(count, maximum)| usage_under_pressure(count, maximum));
        }
        // A count the worker or `start` recorded moments ago, against the
        // same configured limit, answers without touching the filesystem.
        if let Some((count, maximum)) = self.fresh_recorded_checkout_usage(now_ms()) {
            crate::phase_telemetry::add("auto_cleanup_capacity_recorded", Duration::ZERO);
            return usage_under_pressure(count, maximum);
        }
        // Recording is left to `start` and the background worker: a pass
        // here may run while another writer holds the database and must not
        // wait on it. The count is one directory read, never a full listing.
        matches!(
            self.worktree_checkout_usage(),
            Ok(Some((count, maximum))) if usage_under_pressure(count, maximum)
        )
    }

    /// The recorded checkout count when it is at most
    /// [`CHECKOUT_USAGE_REUSE_MS`] old and was taken against the limit the
    /// configuration sets now.
    fn fresh_recorded_checkout_usage(&self, now: i64) -> Option<(usize, u32)> {
        let raw = self
            .store_ref()
            .meta_get(CHECKOUT_USAGE_KEY)
            .ok()
            .flatten()?;
        let usage = serde_json::from_str::<RecordedCheckoutUsage>(&raw).ok()?;
        let maximum = match crate::worktree_location::WorktreeLocationConfig::load(self.main_root())
        {
            Ok(Some(config)) => config.max_checkouts(),
            Ok(None) | Err(_) => {
                crate::worktree_location::WorktreeLocationConfig::default().max_checkouts()
            }
        };
        (usage.maximum == maximum
            && (0..=CHECKOUT_USAGE_REUSE_MS).contains(&now.saturating_sub(usage.at_ms)))
        .then_some((usage.count, usage.maximum))
    }

    /// The linked-checkout count from this repository's registrations alone
    /// (one directory read, never `git worktree list`), against the
    /// configured limit: `Some(None)` when the limit is disabled, `None`
    /// when the registrations cannot be read.
    pub(crate) fn registered_checkout_usage(&self) -> Option<Option<(usize, u32)>> {
        let maximum = self.configured_max_checkouts();
        if maximum == 0 {
            return Some(None);
        }
        self.registered_linked_checkout_count()
            .map(|count| Some((count, maximum)))
    }

    /// Remember a linked-checkout count for passes too short to list them.
    pub(crate) fn record_checkout_usage(&self, count: usize, maximum: u32) {
        let usage = RecordedCheckoutUsage {
            count,
            maximum,
            at_ms: now_ms(),
        };
        let text = serde_json::to_string(&usage).expect("serializing integers is infallible");
        crate::warn_unrecorded(
            "record the linked-checkout count",
            self.store_ref().meta_set(CHECKOUT_USAGE_KEY, &text),
        );
    }

    /// The recorded linked-checkout count and limit, when recent.
    fn recorded_checkout_usage(&self, now: i64) -> Option<(usize, u32)> {
        let raw = self
            .store_ref()
            .meta_get(CHECKOUT_USAGE_KEY)
            .ok()
            .flatten()?;
        let usage = serde_json::from_str::<RecordedCheckoutUsage>(&raw).ok()?;
        let age = now.saturating_sub(usage.at_ms);
        (0..=CHECKOUT_USAGE_MAX_AGE_MS)
            .contains(&age)
            .then_some((usage.count, usage.maximum))
    }

    /// `[worktrees] max_checkouts` as configured now (the default when the
    /// configuration is missing or invalid); 0 when the limit is disabled.
    pub(crate) fn configured_max_checkouts(&self) -> u32 {
        match crate::worktree_location::WorktreeLocationConfig::load(self.main_root()) {
            Ok(Some(config)) => config.max_checkouts(),
            Ok(None) | Err(_) => {
                crate::worktree_location::WorktreeLocationConfig::default().max_checkouts()
            }
        }
    }

    /// Whether the recorded count shows capacity pressure against the limit
    /// configured now: a count recorded against another limit, or with the
    /// limit since disabled, is not pressure. The worker's removal trigger
    /// uses it, so a stale count never labels a slice `pressure`.
    pub(crate) fn recorded_checkout_capacity_pressure_at_current_limit(&self) -> bool {
        let configured = self.configured_max_checkouts();
        self.recorded_checkout_usage(now_ms())
            .is_some_and(|(count, maximum)| {
                maximum == configured && usage_under_pressure(count, maximum)
            })
    }

    /// Whether the recorded count shows capacity pressure, without listing
    /// checkouts: cheap enough for a broker open deciding whether to wake
    /// the background worker.
    pub(crate) fn recorded_checkout_capacity_pressure(&self) -> bool {
        self.recorded_checkout_usage(now_ms())
            .is_some_and(|(count, maximum)| usage_under_pressure(count, maximum))
    }

    /// Select the checkouts every proof currently allows removing. Removes
    /// nothing; [`Self::auto_cleanup_apply`] re-checks each before acting.
    pub(crate) fn auto_cleanup_plan(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<AutoCleanupPlan, BrokerOpError> {
        self.auto_cleanup_plan_limited(deadline, AUTO_CLEANUP_MAX_SELECTED_PER_PASS)
    }

    /// [`Self::auto_cleanup_plan`] that stops once `max_selected` checkouts
    /// are selected, leaving the cursor on the last one judged.
    pub(crate) fn auto_cleanup_plan_limited(
        &mut self,
        deadline: Option<Instant>,
        max_selected: usize,
    ) -> Result<AutoCleanupPlan, BrokerOpError> {
        self.auto_cleanup_plan_selecting(deadline, max_selected, RemovalSelection::Probe)
    }

    /// [`Self::auto_cleanup_plan_limited`] running the lanes `selection`
    /// names. Only the order and number of groups judged differ; every
    /// judged group goes through the same proofs, except that a group the
    /// database or the keep memo keeps is kept without them.
    pub(crate) fn auto_cleanup_plan_selecting(
        &mut self,
        deadline: Option<Instant>,
        max_selected: usize,
        selection: RemovalSelection,
    ) -> Result<AutoCleanupPlan, BrokerOpError> {
        let _span = crate::phase_telemetry::span("auto_cleanup_plan");
        self.auto_cleanup_plan_with_group_inspector_limited(
            deadline,
            max_selected,
            selection,
            true,
            |broker, group, live, live_worktrees, deadline| {
                broker.auto_cleanup_candidate_for_group(group, live, live_worktrees, deadline)
            },
        )
    }

    /// Planner seam used to verify that cursor rotation happens before
    /// per-group Git and ownership work. The injected inspection judges every
    /// group: no database or memo keep runs ahead of it.
    #[cfg(test)]
    fn auto_cleanup_plan_with_group_inspector<F>(
        &mut self,
        deadline: Option<Instant>,
        inspect_group: F,
    ) -> Result<AutoCleanupPlan, BrokerOpError>
    where
        F: FnMut(
            &mut Self,
            &AutoCleanupCheckoutGroup,
            &[Session],
            &[ResolvedWorktreePath],
            Option<Instant>,
        ) -> Result<AutoCleanupGroupInspection, BrokerOpError>,
    {
        self.auto_cleanup_plan_with_group_inspector_limited(
            deadline,
            AUTO_CLEANUP_MAX_SELECTED_PER_PASS,
            RemovalSelection::Probe,
            false,
            inspect_group,
        )
    }

    /// The closed-checkout planner. With `cheap_keeps`, a group the database
    /// decides ([`fast_judging::database_keep`]) or the keep memo still
    /// proves kept ([`fast_judging::keep_memo_entry_holds`]) is kept before
    /// `inspect_group` and the verdict run, and costs none of the pass's
    /// judged shares. Full judgments alternate between the priority lane and
    /// the lap, priority first.
    fn auto_cleanup_plan_with_group_inspector_limited<F>(
        &mut self,
        deadline: Option<Instant>,
        max_selected: usize,
        selection: RemovalSelection,
        cheap_keeps: bool,
        mut inspect_group: F,
    ) -> Result<AutoCleanupPlan, BrokerOpError>
    where
        F: FnMut(
            &mut Self,
            &AutoCleanupCheckoutGroup,
            &[Session],
            &[ResolvedWorktreePath],
            Option<Instant>,
        ) -> Result<AutoCleanupGroupInspection, BrokerOpError>,
    {
        let mut report = AutoCleanupReport {
            evaluated_at_ms: now_ms(),
            ..AutoCleanupReport::default()
        };
        let policy = match AutoCleanupPolicy::load(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                report.config_error = Some(error);
                return Ok(AutoCleanupPlan::nothing(report));
            }
        };
        report.enabled = policy.auto_remove;
        if !policy.auto_remove {
            return Ok(AutoCleanupPlan::nothing(report));
        }
        let retention = match crate::load_retention_policy(self.main_root()) {
            Ok(policy) => policy,
            Err(error) => {
                report.config_error = Some(format!("retention policy unreadable: {error}"));
                return Ok(AutoCleanupPlan::nothing(report));
            }
        };
        let cleanup_grace_hours = retention.closed_worktree_grace_hours;
        // Check timings an earlier caller left on this thread are not this
        // pass's.
        let _ = fast_judging::take_check_timings();
        let (mut cursor, mut expected_cursor) = self.auto_cleanup_cursor()?;
        let memo_raw = self.store().meta_get(KEEP_MEMO_KEY)?;
        let mut memo = decode_keep_memo(memo_raw.as_deref());
        let memo_policy = keep_memo_policy(&policy);
        let mut memo_changed = false;
        let mut upstream_checked = false;
        let mut upstream: Option<(String, String)> = None;
        let mut selected = Vec::new();
        let mut lease_process_table = LeaseProcessTableSnapshot::default();
        let mut gate_witnesses = GateWitnessSnapshot::default();
        let mut alias_refresh_generation = None;
        let mut examined = 0_usize;
        let mut memo_skips = 0_usize;
        let mut memo_hits = AutoCleanupMemoHits::default();
        let mut db_in_grace = 0_usize;
        let mut db_session_open = 0_usize;
        let mut priority_db_skips = 0_usize;
        let mut priority_examined = 0_usize;
        let mut priority_selected = 0_usize;
        // Full judgments per lane in this pass, which each lane's share
        // counts; database and memo keeps cost none.
        let mut priority_judged = 0_usize;
        let mut lap_judged = 0_usize;
        let mut priority_memo =
            PriorityMemo::decode(self.store().meta_get(PRIORITY_MEMO_KEY)?.as_deref());
        let mut priority_memo_changed = false;
        let mut priority_memo_skips = 0_usize;
        // Groups the priority lane judged in this pass, skipped by every lap
        // page of the same pass.
        let mut priority_paths = std::collections::BTreeSet::<PathBuf>::new();
        // Under capacity pressure a bounded pass keeps going page by page
        // while time remains; otherwise each pass keeps its one-page tranche
        // and the background worker paces the lap. The worker's steady
        // removal (no pressure) runs the priority lane and a short stretch of
        // the lap without probing.
        let probe_started = Instant::now();
        let (priority_lane, multi_page, lap_limit) = match selection {
            RemovalSelection::Probe => {
                let pressure = deadline.is_some() && self.checkout_capacity_pressure(deadline);
                (pressure, pressure, None)
            }
            RemovalSelection::Steady => (true, false, Some(STEADY_LAP_GROUPS_PER_PASS)),
        };
        let probe_elapsed = probe_started.elapsed();
        if selection == RemovalSelection::Probe {
            crate::phase_telemetry::add("auto_cleanup_capacity_probe", probe_elapsed);
        }
        let mut first_page = true;
        let mut scan_elapsed = Duration::ZERO;
        loop {
            let cursor_before_scan = cursor;
            let scan_started = Instant::now();
            let mut waiting_on = None;
            let scan = self.auto_cleanup_checkout_groups(&mut cursor, deadline, &mut waiting_on)?;
            scan_elapsed += scan_started.elapsed();
            let Some(scan) = scan else {
                if first_page {
                    report.deferred += 1;
                    crate::phase_telemetry::add("auto_cleanup_removal_scan", scan_elapsed);
                    crate::phase_telemetry::add(
                        "auto_cleanup_removal_inventory_unavailable",
                        Duration::ZERO,
                    );
                    report.lanes = Some(AutoCleanupLanes {
                        removal_scan_ms: duration_ms(Some(scan_elapsed)),
                        removal_inventory_unavailable: true,
                        removal_probe_ms: duration_ms(Some(probe_elapsed)),
                        // Time the probe used up before the scan could start
                        // is what the scan was really waiting on.
                        removal_inventory_wait: Some(
                            if deadline_expired_at(deadline, scan_started) {
                                InventoryWait::CapacityProbe
                            } else {
                                waiting_on.unwrap_or(InventoryWait::CohortIndex)
                            },
                        ),
                        ..AutoCleanupLanes::default()
                    });
                    return Ok(AutoCleanupPlan::nothing(report));
                }
                break;
            };
            if scan.unresolved_rows > 0 {
                report.kept.push(AutoKeptCheckout {
                    worktree: "<session checkout inventory>".into(),
                    sessions: Vec::new(),
                    reason: format!(
                        "{} session checkout path(s) could not be resolved; cleanup remains blocked",
                        scan.unresolved_rows
                    ),
                    ..Default::default()
                });
                return Ok(AutoCleanupPlan::nothing(report));
            }
            if cursor != cursor_before_scan {
                self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
            }
            // Older installed routers do not write the close outbox atomically.
            // Reconcile only this rotating, bounded cohort page so a closure made
            // by one of those clients eventually receives its original grace
            // without turning routine maintenance into a full-history query.
            let legacy_closed_spawned_ids = scan
                .groups
                .iter()
                .filter(|group| !policy.is_pinned(&group.worktree))
                .flat_map(|group| group.sessions.iter())
                .filter(|session| {
                    session.origin == SessionOrigin::Spawned
                        && session.status.is_closed()
                        && session.cleanup_state == SessionCleanupState::Closed
                })
                .map(|session| session.id)
                .collect::<Vec<_>>();
            self.store().request_closed_spawned_checkout_cleanups(
                &legacy_closed_spawned_ids,
                cleanup_grace_hours,
                now_ms(),
            )?;
            if alias_refresh_generation
                .is_some_and(|generation| generation != scan.alias_refresh_generation)
            {
                // The path index moved under this pass; the next pass starts a
                // fresh selection against the new lap.
                break;
            }
            alias_refresh_generation = Some(scan.alias_refresh_generation);
            let AutoCleanupGroupScan {
                groups: lap_groups,
                live,
                live_worktrees,
                ..
            } = scan;

            // Under capacity pressure, and in the worker's steady removal,
            // judge what most likely became removable first: a merged pull
            // request, then a due cleanup request. The lap still visits
            // every group in session order, so nothing is starved.
            let mut priority_queue = std::collections::VecDeque::new();
            if first_page && priority_lane {
                let _span = crate::phase_telemetry::span("auto_cleanup_priority_selection");
                let (groups, skips) = self.auto_cleanup_priority_groups(
                    &priority_memo,
                    now_ms(),
                    cleanup_grace_hours,
                    deadline,
                )?;
                priority_memo_skips += skips;
                for _ in 0..skips.min(4096) {
                    crate::phase_telemetry::add("auto_cleanup_priority_memo_skip", Duration::ZERO);
                }
                for (group, merged) in groups {
                    // A group the database keeps (inside its grace, or an
                    // open session uses it) is no priority: the lap reports
                    // it, and its cleanup request wakes removal once its
                    // grace ends.
                    if cheap_keeps
                        && timed(CheckKind::Database, || {
                            fast_judging::database_keep(
                                &group,
                                &live,
                                &live_worktrees,
                                &retention,
                                now_ms(),
                            )
                        })
                        .is_some()
                    {
                        priority_db_skips += 1;
                        crate::phase_telemetry::add(
                            "auto_cleanup_priority_db_skip",
                            Duration::ZERO,
                        );
                        continue;
                    }
                    priority_queue.push_back((group, merged));
                }
            }
            let mut lap_queue = std::collections::VecDeque::from(lap_groups);
            let mut stop_pass = false;
            // Full judgments alternate between the lanes, priority first, so
            // the lap gets every other one and moves on every pass however
            // many priority candidates there are.
            let mut lap_turn = false;
            // The lap's share is spent and its next group needs a full
            // judgment.
            let mut lap_closed = false;
            loop {
                if deadline_expired(deadline) {
                    stop_pass = true;
                    break;
                }
                let priority_open =
                    priority_judged < PRIORITY_GROUPS_PER_PASS && !priority_queue.is_empty();
                let lap_open = !lap_closed && !lap_queue.is_empty();
                let take_lap = match (priority_open, lap_open) {
                    (false, false) => break,
                    (true, true) => lap_turn,
                    (_, lap_open) => lap_open,
                };
                let (priority, group) = if take_lap {
                    let Some(group) = lap_queue.pop_front() else {
                        break;
                    };
                    (None, group)
                } else {
                    let Some((group, merged)) = priority_queue.pop_front() else {
                        break;
                    };
                    (Some(merged), group)
                };
                let cursor_session_id = checkout_group_cursor_session_id(&group);
                if priority.is_none() && priority_paths.contains(&group.worktree) {
                    // Already judged by the priority lane in this pass.
                    cursor.after_session_id = cursor_session_id;
                    self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
                    continue;
                }
                if priority.is_some() {
                    priority_paths.insert(group.worktree.clone());
                }
                // A lap group still queued for the priority lane is judged
                // here, once, and remembered as the priority lane would.
                let queued_merged = if priority.is_none() {
                    priority_queue
                        .iter()
                        .position(|(queued, _)| queued.worktree == group.worktree)
                        .and_then(|index| priority_queue.remove(index))
                        .map(|(_, merged)| merged)
                } else {
                    None
                };
                let worktree_key = group.worktree.to_string_lossy().into_owned();
                if cheap_keeps {
                    // Decided by the database alone: no Git process, no walk.
                    if let Some(kept) = timed(CheckKind::Database, || {
                        fast_judging::database_keep(
                            &group,
                            &live,
                            &live_worktrees,
                            &retention,
                            now_ms(),
                        )
                    }) {
                        if kept.code == "in_grace" {
                            db_in_grace += 1;
                            crate::phase_telemetry::add(
                                "auto_cleanup_db_kept_in_grace",
                                Duration::ZERO,
                            );
                        } else {
                            db_session_open += 1;
                            crate::phase_telemetry::add(
                                "auto_cleanup_db_kept_session_open",
                                Duration::ZERO,
                            );
                        }
                        report.kept.push(kept);
                        if priority.is_none() {
                            cursor.after_session_id = cursor_session_id;
                            self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
                        }
                        continue;
                    }
                    // Remembered: kept for a reason only a change lifts, and
                    // nothing it depended on changed.
                    if !upstream_checked
                        && memo
                            .entries
                            .get(&worktree_key)
                            .is_some_and(|entry| entry.code == fast_judging::MEMO_NOT_CONTAINED)
                    {
                        upstream_checked = true;
                        upstream = timed(CheckKind::Upstream, || {
                            let _git_deadline = deadline.map(crate::git::limit_git_until);
                            self.remote_default_ref_and_tip()
                        });
                    }
                    let session_ids = group
                        .sessions
                        .iter()
                        .map(|session| session.id)
                        .collect::<Vec<_>>();
                    let remembered = timed(CheckKind::Memo, || {
                        memo.entries
                            .get(&worktree_key)
                            .filter(|entry| {
                                fast_judging::keep_memo_entry_holds(
                                    entry,
                                    &fast_judging::MemoContext {
                                        worktree: &group.worktree,
                                        session_ids: &session_ids,
                                        policy: &memo_policy,
                                        upstream_tip: upstream
                                            .as_ref()
                                            .map(|(_, tip)| tip.as_str()),
                                        removal_attempt_recorded: entry.code
                                            == fast_judging::MEMO_UNMARKED_DELETIONS
                                            && self.removal_attempt_any(&group.worktree),
                                        now_ms: now_ms(),
                                    },
                                )
                            })
                            .map(|entry| fast_judging::remembered_kept(&worktree_key, entry))
                    });
                    if let Some(kept) = remembered {
                        // Nothing the verdict depended on changed: keep it
                        // without judging it again.
                        memo_skips += 1;
                        memo_hits.count(&kept.code);
                        examined += 1;
                        report.kept.push(kept);
                        if priority.is_some() || queued_merged.is_some() {
                            priority_examined += 1;
                            crate::phase_telemetry::add(
                                "auto_cleanup_priority_examined",
                                Duration::ZERO,
                            );
                        }
                        // As after a full judgment: the lap moves past every
                        // group it reached, a queued priority one included
                        // (otherwise the lap's last group needs a pass more).
                        if priority.is_none() {
                            cursor.after_session_id = cursor_session_id;
                            self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
                        }
                        if let Some(merged) = priority.or(queued_merged) {
                            priority_memo.remember_with(
                                worktree_key,
                                merged,
                                PriorityOutcome::Kept,
                                None,
                                now_ms(),
                            );
                            priority_memo_changed = true;
                        }
                        continue;
                    }
                }
                if priority.is_none() && lap_limit.is_some_and(|limit| lap_judged >= limit) {
                    // The steady lap's share of this pass is spent; the cursor
                    // stays on the last group it passed, and a queued priority
                    // group goes back to the priority lane.
                    if let Some(merged) = queued_merged {
                        priority_queue.push_front((group.clone(), merged));
                    }
                    lap_queue.push_front(group);
                    lap_closed = true;
                    continue;
                }
                examined += 1;
                if priority.is_none() {
                    lap_judged += 1;
                } else {
                    priority_judged += 1;
                }
                // The other lane judges next.
                lap_turn = priority.is_some();
                // What the checkout's files said before the judgment: a keep
                // is remembered only if they still say it after.
                let signal_before = if cheap_keeps {
                    fast_judging::checkout_signal(&group.worktree)
                } else {
                    None
                };
                let judged_from_ms = now_ms();
                let deferred_before = report.deferred;
                let selected_before = selected.len();
                let kept_before = report.kept.len();
                let inspection = timed(CheckKind::Inspect, || {
                    inspect_group(self, &group, &live, &live_worktrees, deadline)
                })?;
                let mut stop_for_deadline = false;
                let inspection = if deadline_expired(deadline) {
                    stop_for_deadline = true;
                    AutoCleanupGroupInspection::Deferred
                } else {
                    inspection
                };
                match inspection {
                    AutoCleanupGroupInspection::Candidate(candidate) => {
                        let candidate = *candidate;
                        if !upstream_checked {
                            upstream_checked = true;
                            upstream = timed(CheckKind::Upstream, || {
                                let _git_deadline = deadline.map(crate::git::limit_git_until);
                                self.remote_default_ref_and_tip()
                            });
                        }
                        if deadline_expired(deadline) {
                            report.deferred += 1;
                            stop_for_deadline = true;
                        } else if let Some(upstream) = upstream.as_ref() {
                            // One deep verdict: ancestry first, then the
                            // landing search while time remains. (A shallow
                            // verdict that deferred to a deep one ran every
                            // other proof twice.) Selection reuses the pass's
                            // open-session index; apply re-reads it.
                            let verdict = self.auto_cleanup_verdict(
                                &candidate,
                                &policy,
                                upstream,
                                AutoCleanupVerdictOptions {
                                    deep: true,
                                    deadline,
                                    selected: None,
                                    lease_process_table: &mut lease_process_table,
                                    gate_witnesses: &mut gate_witnesses,
                                    live_index: Some((live.as_slice(), live_worktrees.as_slice())),
                                },
                            );
                            if deadline_expired(deadline) {
                                report.deferred += 1;
                                stop_for_deadline = true;
                            } else {
                                match verdict {
                                    Ok(proof) => {
                                        memo_changed |=
                                            memo.entries.remove(&worktree_key).is_some();
                                        selected.push((candidate, proof));
                                    }
                                    Err(NotRemoved::Defer) => report.deferred += 1,
                                    Err(NotRemoved::Keep(reason)) => {
                                        let code = kept_reason_code(&reason);
                                        let candidate_ids = candidate.session_ids();
                                        let mut ignored_blockers = Vec::new();
                                        if code == "ignored_content" {
                                            // The verdict stops at the first
                                            // blocker; list them all once so the
                                            // report can say what to configure.
                                            if let Ok(blockers) =
                                                timed(CheckKind::BlockerListing, || {
                                                    ignored_content_blockers(
                                                        &candidate.worktree,
                                                        &policy,
                                                        IGNORED_BLOCKER_LIMIT,
                                                    )
                                                })
                                            {
                                                ignored_blockers = blockers
                                                    .into_iter()
                                                    .map(|(relative, _)| relative)
                                                    .collect();
                                            }
                                            if !ignored_blockers.is_empty()
                                                && let Some(head) =
                                                    fast_judging::checkout_head_from_files(
                                                        &candidate.worktree,
                                                    )
                                            {
                                                memo.entries.insert(
                                                    worktree_key.clone(),
                                                    KeepMemoEntry {
                                                        head,
                                                        policy: memo_policy.clone(),
                                                        fingerprint: keep_memo_fingerprint(
                                                            &candidate.worktree,
                                                            &ignored_blockers,
                                                        ),
                                                        blockers: ignored_blockers.clone(),
                                                        sessions: candidate_ids.clone(),
                                                        reason: reason.clone(),
                                                        recorded_at_ms: now_ms(),
                                                        code: fast_judging::MEMO_IGNORED
                                                            .to_string(),
                                                        upstream: None,
                                                    },
                                                );
                                                memo_changed = true;
                                            }
                                        } else {
                                            // Uncommitted changes, unmarked
                                            // deletions and commits not
                                            // contained are remembered with the
                                            // signal that would lift them.
                                            let remembered = if cheap_keeps {
                                                fast_judging::keep_memo_entry_for(
                                                    code,
                                                    &reason,
                                                    &fast_judging::MemoContext {
                                                        worktree: &candidate.worktree,
                                                        session_ids: &candidate_ids,
                                                        policy: &memo_policy,
                                                        upstream_tip: Some(upstream.1.as_str()),
                                                        removal_attempt_recorded: code
                                                            == fast_judging::MEMO_UNMARKED_DELETIONS
                                                            && self.removal_attempt_any(
                                                                &candidate.worktree,
                                                            ),
                                                        now_ms: now_ms(),
                                                    },
                                                    signal_before.as_ref(),
                                                    judged_from_ms,
                                                )
                                            } else {
                                                None
                                            };
                                            match remembered {
                                                Some(entry) => {
                                                    memo.entries
                                                        .insert(worktree_key.clone(), entry);
                                                    memo_changed = true;
                                                }
                                                None => {
                                                    memo_changed |= memo
                                                        .entries
                                                        .remove(&worktree_key)
                                                        .is_some();
                                                }
                                            }
                                        }
                                        let ownership_reproof = (code
                                            == "ownership_reproof_refused")
                                            .then(|| OWNERSHIP_REPROOF_DEVICE_RENUMBER.to_string());
                                        let gate_keep = (code == "gate_running")
                                            .then(|| gate_keep_kind(&reason))
                                            .flatten()
                                            .map(str::to_string);
                                        report.kept.push(AutoKeptCheckout {
                                            worktree: worktree_key.clone(),
                                            sessions: candidate_ids,
                                            reason,
                                            code: code.to_string(),
                                            ignored_blockers,
                                            ownership_reproof,
                                            gate_keep,
                                        })
                                    }
                                }
                            }
                        } else {
                            report.kept.push(AutoKeptCheckout {
                                worktree: candidate.worktree.to_string_lossy().into_owned(),
                                sessions: candidate.session_ids(),
                                reason: "no fetched remote default branch (origin/HEAD) to prove containment against".into(),
                                ..Default::default()
                            });
                        }
                    }
                    AutoCleanupGroupInspection::Kept(kept) => report.kept.push(kept),
                    AutoCleanupGroupInspection::Deferred => {
                        report.deferred += 1;
                        // A selected-group deferral means its proof exhausted
                        // this tranche. Checkpoint it below, then let the next
                        // pass resume at the following complete group.
                        stop_for_deadline = true;
                    }
                }
                if priority.is_some() || queued_merged.is_some() {
                    priority_examined += 1;
                    crate::phase_telemetry::add("auto_cleanup_priority_examined", Duration::ZERO);
                }
                if priority.is_none() {
                    cursor.after_session_id = cursor_session_id;
                    self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor)?;
                }
                if let Some(merged) = priority.or(queued_merged) {
                    // A priority group never moves the lap cursor. Remember
                    // every judged one, selected and deferred included (and a
                    // queued one the lap judged), so the next pass judges
                    // others and the lap moves on.
                    let outcome = if selected.len() > selected_before {
                        priority_selected += 1;
                        PriorityOutcome::Selected
                    } else if stop_for_deadline || report.deferred > deferred_before {
                        PriorityOutcome::Deferred
                    } else {
                        PriorityOutcome::Kept
                    };
                    // A keep only a state change can lift is remembered with
                    // that state, so later passes skip it until it changes.
                    let stable = match report.kept.get(kept_before..) {
                        Some([kept]) if outcome == PriorityOutcome::Kept => {
                            let code = if kept.code.is_empty() {
                                kept_reason_code(&kept.reason)
                            } else {
                                kept.code.as_str()
                            };
                            StablePriorityKeep::for_kept_group(&group, code, &kept.ignored_blockers)
                        }
                        _ => None,
                    };
                    priority_memo.remember_with(worktree_key, merged, outcome, stable, now_ms());
                    priority_memo_changed = true;
                }
                if stop_for_deadline || selected.len() >= max_selected.max(1) {
                    stop_pass = true;
                    break;
                }
            }
            if lap_limit.is_some_and(|limit| lap_judged >= limit) {
                stop_pass = true;
            }
            first_page = false;
            // Under capacity pressure, a bounded pass moves on to the next page
            // of closed checkouts while time remains, up to the end of the
            // current lap.
            if stop_pass
                || !multi_page
                || selected.len() >= max_selected.max(1)
                || deadline_expired(deadline)
                || self.store().cleanup_cohort_group_count(
                    crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                    cursor.after_session_id,
                    cursor.high_water_session_id,
                )? == 0
            {
                break;
            }
        }
        if memo_changed {
            memo.entries
                .retain(|worktree, _| Path::new(worktree).is_dir());
            if memo.entries.len() > KEEP_MEMO_MAX_ENTRIES {
                let mut ages = memo
                    .entries
                    .iter()
                    .map(|(worktree, entry)| (entry.recorded_at_ms, worktree.clone()))
                    .collect::<Vec<_>>();
                ages.sort();
                let excess = memo.entries.len() - KEEP_MEMO_MAX_ENTRIES;
                for (_, worktree) in ages.into_iter().take(excess) {
                    memo.entries.remove(&worktree);
                }
            }
            let text =
                serde_json::to_string(&memo).expect("serializing the keep memo is infallible");
            self.store().meta_set(KEEP_MEMO_KEY, &text)?;
        }
        if priority_memo_changed {
            priority_memo.prune(now_ms());
            let text = serde_json::to_string(&priority_memo)
                .expect("serializing the priority memo is infallible");
            self.store().meta_set(PRIORITY_MEMO_KEY, &text)?;
        }
        report.blocked_by_ignored = blocked_by_ignored_from_memo(&memo);
        crate::phase_telemetry::add("auto_cleanup_removal_scan", scan_elapsed);
        report.lanes = Some(AutoCleanupLanes {
            removal_examined: examined,
            removal_memo_skips: memo_skips,
            removal_scan_ms: duration_ms(Some(scan_elapsed)),
            removal_priority_examined: priority_examined,
            removal_priority_selected: priority_selected,
            removal_priority_memo_skips: priority_memo_skips,
            removal_probe_ms: duration_ms(Some(probe_elapsed)),
            removal_gate_checks: gate_witnesses.stats,
            removal_db_in_grace: db_in_grace,
            removal_db_session_open: db_session_open,
            removal_priority_db_skips: priority_db_skips,
            removal_memo_hits: memo_hits,
            removal_check_timings: fast_judging::take_check_timings(),
            ..AutoCleanupLanes::default()
        });
        // The current deferred group, if any, was counted above and the cursor
        // persisted through it. On an early deadline break it still points to
        // the prior group, so this adds the unvisited suffix in either case.
        report.deferred += self.store().cleanup_cohort_group_count(
            crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
            cursor.after_session_id,
            cursor.high_water_session_id,
        )?;
        #[cfg(test)]
        if SELECTION_EXHAUSTS_ITS_SHARE.with(std::cell::Cell::get)
            && let Some(deadline) = deadline
        {
            // Stand-in for a selection whose proofs use every millisecond
            // it is given.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        Ok(AutoCleanupPlan {
            report,
            policy,
            upstream,
            selected,
            deadline,
            alias_refresh_generation,
            bound_revalidation: false,
        })
    }

    /// Revalidate every proof for each selected checkout, under the lock and
    /// immediately before removal, then remove the ones that still pass.
    pub(crate) fn auto_cleanup_apply(
        &mut self,
        plan: AutoCleanupPlan,
    ) -> Result<AutoCleanupReport, BrokerOpError> {
        let _span = crate::phase_telemetry::span("auto_cleanup_apply");
        let AutoCleanupPlan {
            mut report,
            policy,
            upstream,
            selected,
            deadline,
            alias_refresh_generation,
            bound_revalidation,
        } = plan;
        if let Some(expected_alias_generation) = alias_refresh_generation {
            let alias_snapshot_current = self
                .store()
                .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)?
                .is_some_and(|state| {
                    state.complete
                        && state.unresolved_rows == 0
                        && state.alias_refresh_generation == expected_alias_generation
                });
            if !alias_snapshot_current {
                report.deferred = report.deferred.saturating_add(selected.len());
                report.assign_kept_codes();
                self.record_auto_cleanup_report(&report)?;
                return Ok(report);
            }
        }
        let Some(upstream) = upstream.filter(|_| !selected.is_empty()) else {
            report.assign_kept_codes();
            self.record_auto_cleanup_report(&report)?;
            return Ok(report);
        };
        let _lifecycle_claim = match self.try_claim_worktree_lifecycle("auto_cleanup") {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                report.deferred = report.deferred.saturating_add(selected.len());
                report.assign_kept_codes();
                self.record_auto_cleanup_report(&report)?;
                return Ok(report);
            }
            Err(error) => {
                report.deferred = report.deferred.saturating_add(selected.len());
                report.kept.push(AutoKeptCheckout {
                    worktree: "<session checkout inventory>".into(),
                    sessions: Vec::new(),
                    reason: format!("could not claim the worktree lifecycle: {error}"),
                    ..Default::default()
                });
                report.assign_kept_codes();
                self.record_auto_cleanup_report(&report)?;
                return Ok(report);
            }
        };
        // One process snapshot for the whole batch, taken after selection so
        // it is as close to removal as it can be. No snapshot keeps all.
        let open_file_snapshot = crate::reclaim::open_file_snapshot_under(Path::new("/"));
        let records = crate::measurement::load_size_records(self.main_root());
        if let Some(alias_generation) = alias_refresh_generation
            && !self.cleanup_cohort_filesystem_aliases_match(alias_generation, deadline)?
        {
            report.deferred = report.deferred.saturating_add(selected.len());
            // Running out of time is not a changed path: say which, so a
            // pass starved of its apply time is visible as such.
            let reason = if deadline_expired(deadline) {
                crate::phase_telemetry::add(
                    "auto_cleanup_alias_verification_deadline",
                    Duration::ZERO,
                );
                "registered checkout paths could not be verified before the deadline; selected checkouts wait for the next pass"
            } else {
                "registered checkout paths changed during filesystem alias verification; cleanup requires a fresh pass"
            };
            report.kept.push(AutoKeptCheckout {
                worktree: "<session checkout inventory>".into(),
                sessions: Vec::new(),
                reason: reason.into(),
                ..Default::default()
            });
            report.assign_kept_codes();
            self.record_auto_cleanup_report(&report)?;
            return Ok(report);
        }
        let mut attempted_any = false;
        let mut apply_gate_checks = AutoCleanupGateChecks::default();
        // Checkouts whose removal the deletion gate put off.
        let mut deletion_put_off = Vec::new();
        for (candidate, selected_proof) in selected {
            // The first selected checkout always goes, so a pass whose
            // selection spent the budget still converges; the rest wait for
            // a later pass rather than stretching a broker open (#460).
            if attempted_any && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                report.deferred += 1;
                continue;
            }
            attempted_any = true;
            let worktree_text = candidate.worktree.to_string_lossy().into_owned();
            let keep_it = |report: &mut AutoCleanupReport, reason: String| {
                report.kept.push(AutoKeptCheckout {
                    worktree: worktree_text.clone(),
                    sessions: candidate.session_ids(),
                    reason,
                    ..Default::default()
                });
            };
            let artifact_use_reclaimer =
                match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                    self.main_root(),
                    &candidate.worktree,
                ) {
                    Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(
                        reclaimer,
                    )) => reclaimer,
                    Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(block)) => {
                        let reason = match block {
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended => {
                                "checkout has an active artifact producer; kept the worktree"
                            }
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                                ..
                            } => "checkout has unresolved artifact-use markers; kept the worktree",
                            crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable => {
                                "artifact-use protection is unavailable; kept the worktree"
                            }
                        };
                        keep_it(&mut report, reason.into());
                        continue;
                    }
                    Err(error) => {
                        keep_it(
                            &mut report,
                            format!("artifact-use inventory failed; kept the worktree: {error}"),
                        );
                        continue;
                    }
                };
            let _artifact_use_reclaimer = artifact_use_reclaimer;
            if let Some(reason) = crate::reclaim::open_file_snapshot_blocker_for_worktree(
                &open_file_snapshot,
                &candidate.worktree,
            ) {
                keep_it(&mut report, reason);
                continue;
            }
            // Selection is not authority to keep using an old cleanup policy.
            // Recheck under the lifecycle lock for each candidate, including
            // opt-out, newly pinned paths and withdrawn regenerable globs.
            match AutoCleanupPolicy::load(self.main_root()) {
                Ok(current) if current == policy => {}
                Ok(current) => {
                    report.enabled = current.auto_remove;
                    keep_it(
                        &mut report,
                        "cleanup policy changed before removal; create a fresh plan".into(),
                    );
                    continue;
                }
                Err(error) => {
                    report.enabled = false;
                    report.config_error = Some(error.clone());
                    keep_it(
                        &mut report,
                        format!("cleanup policy unreadable before removal: {error}"),
                    );
                    continue;
                }
            }
            // Revalidate every proof now, under the lock, right before removal.
            let mut lease_process_table = LeaseProcessTableSnapshot::default();
            // A fresh gate read for each checkout, under the lifecycle claim.
            let mut gate_witnesses = GateWitnessSnapshot::default();
            let verdict = self.auto_cleanup_verdict(
                &candidate,
                &policy,
                &upstream,
                AutoCleanupVerdictOptions {
                    deep: true,
                    deadline: deadline.filter(|_| bound_revalidation),
                    selected: Some(&selected_proof),
                    lease_process_table: &mut lease_process_table,
                    gate_witnesses: &mut gate_witnesses,
                    // Re-read under the lifecycle claim, never reused.
                    live_index: None,
                },
            );
            apply_gate_checks.absorb(&gate_witnesses.stats);
            let proof = match verdict {
                Ok(proof) => proof,
                Err(NotRemoved::Keep(reason)) => {
                    keep_it(&mut report, format!("changed before removal: {reason}"));
                    continue;
                }
                Err(NotRemoved::Defer) => {
                    report.deferred += 1;
                    continue;
                }
            };
            // Unattended removal takes the machine's single-deleter slot only
            // now, every proof re-read, and checks memory pressure once it
            // holds it. A slot another repository's worker keeps through the
            // bounded wait, or memory pressure, puts this removal off, and
            // the next pass judges the group again: its priority memo entry
            // is dropped (the priority lane takes it first if it is a
            // priority candidate) and the lap cursor is moved back before it
            // (the lap reaches it first if it is not). Foreground callers
            // have no pacer and pass.
            if crate::gc::unattended_pacing::enter_unattended_deletion()
                != crate::gc::unattended_pacing::DeletionGate::Proceed
            {
                report.deferred += 1;
                deletion_put_off.push((
                    worktree_text.clone(),
                    candidate
                        .sessions
                        .iter()
                        .map(|session| session.id)
                        .min()
                        .unwrap_or_default(),
                ));
                continue;
            }
            match self.remove_auto_cleanup_candidate(&candidate, &proof) {
                Ok(()) => {
                    let removed = AutoRemovedCheckout {
                        worktree: worktree_text.clone(),
                        sessions: candidate.session_ids(),
                        branch: Some(candidate.branch().to_string()),
                        head: proof.head.clone(),
                        contained_in: upstream.0.clone(),
                        containing_commit: proof.containing_commit.clone(),
                        proof: proof.proof.clone(),
                        bytes: records
                            .get(&worktree_text)
                            .map(|record| record.bytes)
                            .or_else(|| {
                                candidate.sessions.iter().find_map(|session| {
                                    records.get(&session.worktree_path).map(|r| r.bytes)
                                })
                            }),
                        resumed_interrupted_removal: proof.interrupted_removal_deletions,
                        ownership_reproof: proof.ownership_reproof.map(str::to_string),
                    };
                    let payload = serde_json::to_string(&removed).unwrap_or_default();
                    self.store().append_event(
                        crate::events::BROKER_CLEANUP_AUTO_REMOVED,
                        candidate.sessions.last().map(|session| session.id),
                        Some(&payload),
                    )?;
                    report.removed.push(removed);
                }
                Err(error) => keep_it(&mut report, format!("removal failed: {error}")),
            }
        }
        let apply_check_timings = fast_judging::take_check_timings();
        if let Some(lanes) = report.lanes.as_mut() {
            lanes.removal_gate_checks.absorb(&apply_gate_checks);
            lanes.removal_check_timings.absorb(&apply_check_timings);
        }
        report.assign_kept_codes();
        self.note_priority_apply_keeps(&report)?;
        // The report first: it names checkouts already removed, and nothing
        // about ordering the next pass may lose it.
        self.record_auto_cleanup_report(&report)?;
        self.judge_put_off_groups_first(&deletion_put_off)?;
        Ok(report)
    }

    /// Make the next pass judge again the groups (worktree, cursor session
    /// id) whose removal was put off at the last step for a reason that does
    /// not concern the checkout: drop their priority memo entries, and move
    /// the lap cursor back before the first of them the lap already passed.
    fn judge_put_off_groups_first(
        &mut self,
        groups: &[(String, i64)],
    ) -> Result<(), BrokerOpError> {
        if groups.is_empty() {
            return Ok(());
        }
        let (mut cursor, mut expected_cursor) = self.auto_cleanup_cursor()?;
        #[cfg(test)]
        if PUT_OFF_CURSOR_RACE.with(std::cell::Cell::get) {
            // Another pass moves the cursor between the read and the write.
            let mut moved = cursor;
            moved.high_water_session_id = moved.high_water_session_id.saturating_add(1);
            let mut raw = expected_cursor.clone();
            self.persist_auto_cleanup_cursor(&moved, &mut raw)?;
        }
        if let Some(first) = groups
            .iter()
            .map(|(_, cursor_session_id)| *cursor_session_id)
            .filter(|id| *id > 0 && *id <= cursor.after_session_id)
            .min()
        {
            cursor.after_session_id = first - 1;
            // Only a hint for ordering: if another pass moved the cursor
            // meanwhile, leave its cursor and count the conflict.
            match self.persist_auto_cleanup_cursor(&cursor, &mut expected_cursor) {
                Ok(()) => {}
                Err(BrokerOpError::AutoCleanupCursorChanged) => {
                    crate::phase_telemetry::add(
                        "auto_cleanup_put_off_cursor_conflict",
                        Duration::ZERO,
                    );
                }
                Err(error) => return Err(error),
            }
        }
        let raw = self.store().meta_get(PRIORITY_MEMO_KEY)?;
        let mut memo = PriorityMemo::decode(raw.as_deref());
        let before = memo.entries.len();
        for (worktree, _) in groups {
            memo.entries.remove(worktree);
        }
        if memo.entries.len() != before {
            let text =
                serde_json::to_string(&memo).expect("serializing the priority memo is infallible");
            self.store().meta_set(PRIORITY_MEMO_KEY, &text)?;
        }
        Ok(())
    }

    /// Record, on the priority memo entry of each checkout apply kept, the
    /// keep code it gave, so the record says why a selected priority group was
    /// not removed. The entry already keeps it from being picked first again.
    fn note_priority_apply_keeps(
        &mut self,
        report: &AutoCleanupReport,
    ) -> Result<(), BrokerOpError> {
        if report.kept.is_empty() {
            return Ok(());
        }
        let raw = self.store().meta_get(PRIORITY_MEMO_KEY)?;
        let mut memo = PriorityMemo::decode(raw.as_deref());
        let mut changed = false;
        for kept in &report.kept {
            if let Some(entry) = memo.entries.get_mut(&kept.worktree)
                && entry.outcome == "selected"
                && entry.apply_code.as_deref() != Some(kept.code.as_str())
            {
                entry.apply_code = Some(kept.code.clone());
                changed = true;
            }
        }
        if changed {
            let text =
                serde_json::to_string(&memo).expect("serializing the priority memo is infallible");
            self.store().meta_set(PRIORITY_MEMO_KEY, &text)?;
        }
        Ok(())
    }

    fn remove_auto_cleanup_candidate(
        &mut self,
        candidate: &AutoCleanupCandidate,
        proof: &ContainmentProof,
    ) -> Result<(), BrokerOpError> {
        let first = candidate
            .sessions
            .first()
            .map(|session| session.id)
            .unwrap_or_default();
        // A checkout the retirement lane kept on purpose or already moved
        // into the trash is that lane's: this lane never acts on it too.
        if candidate
            .sessions
            .iter()
            .any(|session| self.checkout_retirement_owns(session.id))
        {
            return Err(crate::BrokerError::Io {
                path: candidate.worktree.clone(),
                source: std::io::Error::other("owned by the checkout retirement lane"),
            }
            .into());
        }
        self.refuse_live_checkout(first, &candidate.worktree)?;
        // Durable before Git deletes anything: only this record lets a later
        // pass finish a removal that stopped part-way.
        self.record_removal_attempt(&candidate.worktree, candidate.spawned_owner.id, &proof.head)?;
        // Forced: the proofs above are stricter than Git's own check, and the
        // only untracked file they let through is Finder's `.DS_Store`, which
        // a plain `worktree remove` refuses.
        let git_removal_started = Instant::now();
        #[cfg(test)]
        if GIT_REMOVAL_FAILS_PART_WAY.with(std::cell::Cell::get) {
            note_git_removal_time(Duration::from_millis(1));
            return Err(crate::BrokerError::Io {
                path: candidate.worktree.clone(),
                source: std::io::Error::other("simulated removal stopped part-way"),
            }
            .into());
        }
        let git_removal =
            self.remove_checkout_through_trash(&candidate.spawned_owner, &candidate.worktree, true);
        note_git_removal_time(git_removal_started.elapsed());
        git_removal?;
        ensure_auto_cleanup_path_absent(&candidate.worktree)?;
        let branch = candidate.branch();
        if self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
            .as_deref()
            == Some(proof.head.as_str())
        {
            // Only the exact proved head is deleted; a branch that moved is
            // somebody's and stays. The checked delete refuses a race.
            self.repo_handle()
                .delete_branch_ref_checked(branch, &proof.head)?;
        }
        for session in &candidate.sessions {
            self.store()
                .set_session_status(session.id, crate::SessionStatus::Cleaned, None)?;
        }
        self.clear_removal_attempt(&candidate.worktree);
        Ok(())
    }

    /// Record, before Git starts deleting, that automatic removal began on
    /// `worktree` for `owner_session_id` at `head`.
    pub(crate) fn record_removal_attempt(
        &self,
        worktree: &Path,
        owner_session_id: i64,
        head: &str,
    ) -> Result<(), BrokerOpError> {
        let attempt = RemovalAttempt {
            owner_session_id,
            head: head.to_string(),
            at_ms: now_ms(),
        };
        let text = serde_json::to_string(&attempt).expect("serializing the attempt is infallible");
        self.store_ref()
            .meta_set(&removal_attempt_key(worktree), &text)?;
        Ok(())
    }

    fn clear_removal_attempt(&self, worktree: &Path) {
        crate::warn_unrecorded(
            "clear a finished checkout removal record",
            self.store_ref()
                .meta_set(&removal_attempt_key(worktree), ""),
        );
    }

    /// Whether any automatic removal attempt is recorded for `worktree`,
    /// whatever its owner or HEAD; an unreadable record counts as one. The
    /// keep memo never remembers or reuses an unmarked-deletions keep while
    /// one is, since a record can make the deletions resumable.
    fn removal_attempt_any(&self, worktree: &Path) -> bool {
        match self.store_ref().meta_get(&removal_attempt_key(worktree)) {
            Ok(Some(text)) => !text.is_empty(),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    /// Whether automatic removal recorded starting on `worktree` for this
    /// owner at this HEAD. A checkout someone later re-created at the same
    /// path has another owner session.
    fn removal_attempt_recorded(&self, worktree: &Path, owner_session_id: i64, head: &str) -> bool {
        self.store_ref()
            .meta_get(&removal_attempt_key(worktree))
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<RemovalAttempt>(&text).ok())
            .is_some_and(|attempt| {
                attempt.owner_session_id == owner_session_id && attempt.head == head
            })
    }

    pub(crate) fn record_auto_cleanup_report(
        &mut self,
        report: &AutoCleanupReport,
    ) -> Result<(), BrokerOpError> {
        let mut stored = report.clone();
        stored.assign_kept_codes();
        // The capacity slice keeps its own key; the report only shows it.
        stored.capacity_removal = None;
        // A pass whose removal lane judged no checkout (a broker open's
        // inline sliver, the retirement-only worker) must not erase the last
        // removal verdicts: carry them so `status` and `gc plan` keep
        // showing what holds each closed checkout.
        let removal_judged = stored.lanes.is_some_and(|lanes| lanes.removal_examined > 0)
            || !stored.removed.is_empty()
            || !stored.kept.is_empty();
        let retirement_judged = stored.landed_sessions_examined > 0
            || stored.sessions_retired > 0
            || stored.abandonment_sessions_examined > 0;
        if (!removal_judged || !retirement_judged)
            && let Some(previous) = self.last_auto_cleanup_report()
        {
            let previous_has_counts = previous.landed_sessions_examined > 0
                || previous.abandonment_sessions_examined > 0
                || !previous.sessions_closed_by_reason.is_empty();
            if !retirement_judged && previous_has_counts {
                stored.carry_retirement_counts(&previous);
            }
            if !removal_judged {
                stored.kept = previous.kept;
            }
        }
        if stored.blocked_by_ignored.is_empty() {
            let memo = self.store().meta_get(KEEP_MEMO_KEY)?;
            stored.blocked_by_ignored = blocked_by_ignored_from_meta(memo.as_deref());
        }
        let text = serde_json::to_string(&stored).unwrap_or_default();
        self.store().meta_set(AUTO_CLEANUP_LAST_REPORT_KEY, &text)?;
        Ok(())
    }

    /// The last auto-cleanup report, for `status` and `gc plan`, with the
    /// background worker's last capacity removal slice attached.
    pub fn last_auto_cleanup_report(&mut self) -> Option<AutoCleanupReport> {
        let mut report: AutoCleanupReport = self
            .store()
            .meta_get(AUTO_CLEANUP_LAST_REPORT_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())?;
        report.capacity_removal = self.last_capacity_removal_slice();
        Some(report)
    }

    /// The background worker's last capacity removal slice, from its own
    /// meta key.
    pub fn last_capacity_removal_slice(&mut self) -> Option<CapacityRemovalSlice> {
        self.store()
            .meta_get(CAPACITY_REMOVAL_SLICE_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    pub(crate) fn record_capacity_removal_slice(
        &mut self,
        slice: &CapacityRemovalSlice,
    ) -> Result<(), BrokerOpError> {
        let text = serde_json::to_string(slice).expect("serializing the slice is infallible");
        self.store().meta_set(CAPACITY_REMOVAL_SLICE_KEY, &text)?;
        Ok(())
    }

    /// Where the closed-checkout planner stands; reads only the database.
    pub(crate) fn capacity_removal_lap_position(
        &mut self,
    ) -> Result<CapacityRemovalLapPosition, BrokerOpError> {
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let index_state = self.store().cleanup_cohort_scan_state(namespace)?;
        let index_complete = index_state.as_ref().is_some_and(|state| state.complete);
        let (index_after_session_id, index_high_water_session_id) =
            index_state.as_ref().map_or((0, 0), |state| {
                (state.after_session_id, state.high_water_session_id)
            });
        let (cursor, _) = self.auto_cleanup_cursor()?;
        let maximum_group_id = self
            .store()
            .cleanup_cohort_group_max_session_id(namespace)?;
        let lap_remaining = self.store().cleanup_cohort_group_count(
            namespace,
            cursor.after_session_id,
            cursor.high_water_session_id.max(maximum_group_id),
        )?;
        Ok(CapacityRemovalLapPosition {
            index_complete,
            lap_remaining,
            after_session_id: cursor.after_session_id,
            high_water_session_id: cursor.high_water_session_id,
            index_after_session_id,
            index_high_water_session_id,
        })
    }

    /// Select under the GC lock, without removing anything.
    pub fn auto_cleanup_plan_now(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<AutoCleanupPlan>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_cleanup_plan(deadline).map(Some)
    }

    /// Revalidate and remove under the GC lock.
    pub fn auto_cleanup_apply_now(
        &mut self,
        plan: AutoCleanupPlan,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_cleanup_apply(plan).map(Some)
    }

    /// The removal lane alone, for `broker start` at the linked-checkout
    /// limit: no retirement (it frees no slot now), the whole `budget` for
    /// selecting and removing closed checkouts. Waits up to `lock_wait` for
    /// a GC lock another maintenance pass holds instead of giving up at
    /// once; `None` when it stays held.
    /// `max_removals` caps the selection (`None`: the usual per-pass cap).
    /// `bound_revalidation` makes revalidation honor the budget too (see
    /// [`AutoCleanupPlan`]); Git's removal of a checkout already started is
    /// never interrupted. `selection` names the lanes selection runs: the
    /// background worker's steady removal (no capacity pressure) passes
    /// [`RemovalSelection::Steady`]; every other caller lets the capacity
    /// probe decide.
    pub(crate) fn auto_cleanup_for_checkout_capacity(
        &mut self,
        lock_wait: Duration,
        budget: Duration,
        max_removals: Option<usize>,
        bound_revalidation: bool,
        selection: RemovalSelection,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let waiting_since = Instant::now();
        // While it waits it says so: the background worker yields the GC
        // lock to a waiting foreground command instead of taking it again.
        let _lock = match crate::gc::GcLock::acquire_waiting(self.main_root(), lock_wait) {
            Ok(lock) => lock,
            Err(BrokerOpError::GcLocked { .. }) => {
                crate::phase_telemetry::add(
                    "checkout_capacity_cleanup_lock_busy",
                    waiting_since.elapsed(),
                );
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let started = Instant::now();
        let deadline = started + budget;
        let selection_deadline = lane_deadline(started, Some(deadline), REMOVAL_SELECTION_SHARE);
        let mut plan = {
            let _span = crate::phase_telemetry::span("auto_cleanup_removal_lane");
            self.auto_cleanup_plan_selecting(
                selection_deadline,
                max_removals.unwrap_or(AUTO_CLEANUP_MAX_SELECTED_PER_PASS),
                selection,
            )?
        };
        // Revalidation and removal may use the whole budget.
        plan.deadline = Some(deadline);
        plan.bound_revalidation = bound_revalidation;
        let planned = plan.report.lanes.unwrap_or_default();
        plan.report.lanes = Some(AutoCleanupLanes {
            capacity_pressure: selection == RemovalSelection::Probe,
            removal_budget_ms: duration_ms(Some(budget)),
            removal_elapsed_ms: duration_ms(Some(started.elapsed())),
            removal_selected: plan.selected.len(),
            removal_apply_reserve_ms: duration_ms(
                selection_deadline.map(|end| deadline.saturating_duration_since(end)),
            ),
            ..planned
        });
        let report = self.auto_cleanup_apply(plan)?;
        record_lane_telemetry(&report);
        Ok(Some(report))
    }

    /// Run an auto-cleanup pass now, outside the sweep's cadence. Takes the
    /// GC lock; returns `None` when another GC holds it.
    pub fn auto_cleanup_now(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<AutoCleanupReport>, BrokerOpError> {
        let Ok(_lock) = crate::gc::GcLock::acquire(self.main_root()) else {
            return Ok(None);
        };
        self.auto_remove_disposable_checkouts(deadline).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTO_CLEANUP_CURSOR_KEY, AutoCleanupCursor, AutoCleanupGroupInspection, AutoCleanupPolicy,
        LiveOverlapEvidence, ResolvedWorktreePath, SESSION_RETIREMENT_CURSOR_VERSION,
        SessionCleanupState, SessionRetirementCursor, SessionRetirementDeferredReason,
        auto_cleanup_lap_range, classify_ignored, decode_auto_cleanup_cursor,
        decode_session_retirement_cursor, first_live_worktree_overlap,
        group_closed_checkout_sessions, ignored_content_reason, resolve_worktree_paths_until,
    };
    #[cfg(unix)]
    use super::{
        AutoCleanupCandidate, AutoCleanupPlan, AutoCleanupReport, ContainmentProof,
        proves_removed_historical_instance,
    };
    use super::{SESSION_RETIREMENT_CURSOR_KEY, now_ms};
    #[cfg(unix)]
    use crate::git::GitRepo;
    #[cfg(unix)]
    use crate::{Session, SessionOrigin};
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    fn classify(root: &Path, relative: &str, policy: &AutoCleanupPolicy) -> Result<(), String> {
        classify_ignored(root, relative, policy)
    }

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    /// A not-due time is at most one window ahead, however far in the
    /// future the checkout's Git metadata is dated.
    #[test]
    fn a_not_due_time_is_clamped_to_one_window() {
        let window = 72 * 3_600_000;
        assert_eq!(super::clamp_not_due(1_000 + 5, 1_000, window), 1_005);
        assert_eq!(
            super::clamp_not_due(i64::MAX, 1_000, window),
            1_000 + window
        );
        assert_eq!(super::clamp_not_due(5_000, 1_000, -1), 1_000);
    }

    #[test]
    fn partial_live_path_index_is_discarded_when_deadline_expires_mid_scan() {
        let temporary = tempfile::tempdir().unwrap();
        let first_path = temporary.path().join("first");
        let second_path = temporary.path().join("second");
        std::fs::create_dir_all(&first_path).unwrap();
        std::fs::create_dir_all(&second_path).unwrap();
        let paths = [first_path.as_path(), second_path.as_path()];
        let resolved_count = std::cell::Cell::new(0);

        let resolved = resolve_worktree_paths_until(
            paths,
            || resolved_count.get() == 1,
            |path| {
                resolved_count.set(resolved_count.get() + 1);
                ResolvedWorktreePath::resolve(path)
            },
        );

        assert!(resolved.is_none(), "a partial index cannot be used");
        assert_eq!(resolved_count.get(), 1, "expiry stops before the next path");
    }

    #[test]
    fn live_overlap_index_preserves_component_and_missing_path_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("checkout");
        std::fs::create_dir_all(&candidate_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let missing_child = ResolvedWorktreePath::resolve(&candidate_path.join("not-created"));
        let sibling = ResolvedWorktreePath::resolve(&temporary.path().join("checkout-old"));

        assert_eq!(
            first_live_worktree_overlap(&candidate, &[missing_child]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "a genuinely missing live path keeps the existing raw-path fallback"
        );
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[sibling]),
            None,
            "path-prefix checks compare components rather than string prefixes"
        );
    }

    #[test]
    fn a_gate_keeps_a_checkout_only_when_it_could_be_using_it() {
        use super::{GateAttribution, attribute_gate_witness};
        use crate::gates::{GateWitnessCheckout, RunningGateWitness};

        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("foo");
        let sibling_path = temporary.path().join("foo-2");
        std::fs::create_dir_all(candidate_path.join("sub")).unwrap();
        std::fs::create_dir_all(&sibling_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let witness = |session_id: Option<i64>, checkout: Option<&Path>| RunningGateWitness {
            evidence: "gate pidfile x.pid names running process 1".into(),
            session_id,
            checkout: checkout.map_or(GateWitnessCheckout::Unknown, |path| {
                GateWitnessCheckout::Recorded(path.to_path_buf())
            }),
        };
        let attribute = |witness: &RunningGateWitness| {
            let resolved = match &witness.checkout {
                GateWitnessCheckout::Recorded(path) => Some(ResolvedWorktreePath::resolve(path)),
                GateWitnessCheckout::Unknown => None,
            };
            attribute_gate_witness(witness, resolved.as_ref(), &candidate, &[7, 9])
        };

        assert_eq!(
            attribute(&witness(Some(9), Some(&sibling_path))),
            GateAttribution::OwnSession,
            "one of the checkout's own sessions keeps it wherever its gate runs"
        );
        assert_eq!(
            attribute(&witness(Some(8), Some(&candidate_path))),
            GateAttribution::Path
        );
        assert_eq!(
            attribute(&witness(None, Some(&candidate_path.join("sub")))),
            GateAttribution::Path
        );
        assert_eq!(
            attribute(&witness(None, Some(temporary.path()))),
            GateAttribution::Path,
            "a gate in a directory containing the checkout can walk into it"
        );
        assert_eq!(
            attribute(&witness(Some(8), Some(&sibling_path))),
            GateAttribution::Elsewhere,
            "a string prefix of another checkout is not this checkout"
        );
        assert_eq!(
            attribute(&witness(Some(8), None)),
            GateAttribution::Unknown,
            "a record naming no checkout keeps every checkout"
        );
        assert_eq!(attribute(&witness(None, None)), GateAttribution::Unknown);
        #[cfg(unix)]
        {
            let alias = temporary.path().join("alias");
            std::os::unix::fs::symlink(&candidate_path, &alias).unwrap();
            assert_eq!(
                attribute(&witness(Some(8), Some(&alias))),
                GateAttribution::Path,
                "a symlinked spelling of the checkout is still the checkout"
            );
            let canonical = std::fs::canonicalize(&candidate_path).unwrap();
            let canonical_candidate = ResolvedWorktreePath::resolve(&canonical);
            let raw = witness(Some(8), Some(&candidate_path));
            let resolved = ResolvedWorktreePath::resolve(&candidate_path);
            assert_eq!(
                attribute_gate_witness(&raw, Some(&resolved), &canonical_candidate, &[7]),
                GateAttribution::Path,
                "a /private-style alias of the candidate still matches"
            );
        }
    }

    #[test]
    fn the_gate_check_reads_once_per_snapshot_and_keeps_on_unknown_owners() {
        use super::{
            GATE_RUNNING_OWN_SESSION, GATE_RUNNING_PATH, GATE_RUNNING_UNKNOWN_OWNER,
            GateWitnessSnapshot, gate_keep_kind, kept_reason_code,
        };
        use crate::gates::{GatePidRecord, encode_gate_checkout, running_dir};

        let root = tempfile::tempdir().unwrap();
        let candidate_path = root.path().join("checkout");
        let elsewhere_path = root.path().join("elsewhere");
        std::fs::create_dir_all(&candidate_path).unwrap();
        std::fs::create_dir_all(&elsewhere_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let run_dir = running_dir(root.path());
        std::fs::create_dir_all(&run_dir).unwrap();
        let me = std::process::id();
        let pidfile = |name: &str, checkout: Option<&Path>| {
            let checkout = checkout.map_or_else(|| "-".to_string(), encode_gate_checkout);
            std::fs::write(run_dir.join(name), format!("{me} tree {me} - - {checkout}")).unwrap();
        };

        // Only a gate in another checkout: nothing keeps the candidate.
        pidfile("41-unit.pid", Some(&elsewhere_path));
        assert!(
            GatePidRecord::parse(&std::fs::read_to_string(run_dir.join("41-unit.pid")).unwrap())
                .unwrap()
                .checkout
                .is_some()
        );
        let mut snapshot = GateWitnessSnapshot::default();
        assert_eq!(snapshot.keep_reason(root.path(), &candidate, &[7]), None);
        assert_eq!(snapshot.keep_reason(root.path(), &candidate, &[7]), None);
        assert_eq!(snapshot.stats.checks, 2);
        assert_eq!(snapshot.stats.scans, 1, "one read serves the whole pass");
        assert_eq!(snapshot.stats.gates_seen, 2);
        assert_eq!(snapshot.stats.attributed_elsewhere, 2);
        assert_eq!(snapshot.stats.attributed_here, 0);
        assert_eq!(snapshot.stats.unknown_owner, 0);

        // The checkout's own session.
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot
            .keep_reason(root.path(), &candidate, &[41])
            .unwrap();
        assert!(reason.starts_with(GATE_RUNNING_OWN_SESSION), "{reason}");
        assert_eq!(gate_keep_kind(&reason), Some("own_session"));
        assert_eq!(kept_reason_code(&reason), "gate_running");

        // A gate recorded in the checkout itself.
        pidfile("42-unit.pid", Some(&candidate_path));
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_PATH), "{reason}");
        assert_eq!(gate_keep_kind(&reason), Some("path"));
        assert_eq!(kept_reason_code(&reason), "gate_running");
        std::fs::remove_file(run_dir.join("42-unit.pid")).unwrap();

        // A record that names no checkout, then a malformed one.
        pidfile("43-unit.pid", None);
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_UNKNOWN_OWNER), "{reason}");
        assert_eq!(gate_keep_kind(&reason), Some("unknown_owner"));
        assert_eq!(kept_reason_code(&reason), "gate_running");
        assert_eq!(snapshot.stats.unknown_owner, 1);
        std::fs::remove_file(run_dir.join("43-unit.pid")).unwrap();
        std::fs::write(run_dir.join("44-unit.pid"), "malformed").unwrap();
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_UNKNOWN_OWNER), "{reason}");
        std::fs::remove_file(run_dir.join("44-unit.pid")).unwrap();

        // An unverifiable record of the checkout's own session is unknown.
        std::fs::write(run_dir.join("7-unit.pid"), "malformed").unwrap();
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(
            reason.starts_with(GATE_RUNNING_OWN_SESSION)
                || reason.starts_with(GATE_RUNNING_UNKNOWN_OWNER),
            "{reason}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_recorded_checkout_that_cannot_be_resolved_is_owner_unknown() {
        use super::{GateAttribution, attribute_gate_witness};
        use crate::gates::{GateWitnessCheckout, RunningGateWitness};

        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("checkout");
        std::fs::create_dir_all(&candidate_path).unwrap();
        let file = temporary.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        // A path through a regular file fails with ENOTDIR, not NotFound.
        let unresolvable = file.join("checkout");
        let witness = RunningGateWitness {
            evidence: "gate pidfile 8-unit.pid names running process 1".into(),
            session_id: Some(8),
            checkout: GateWitnessCheckout::Recorded(unresolvable.clone()),
        };
        let resolved = ResolvedWorktreePath::resolve(&unresolvable);
        assert_eq!(
            attribute_gate_witness(
                &witness,
                Some(&resolved),
                &ResolvedWorktreePath::resolve(&candidate_path),
                &[7],
            ),
            GateAttribution::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_own_session_inventory_decides_when_no_witness_does() {
        use super::{GATE_RUNNING_OWN_SESSION, GATE_RUNNING_UNKNOWN_OWNER, GateWitnessSnapshot};
        use crate::gates::running_dir;

        let root = tempfile::tempdir().unwrap();
        let candidate_path = root.path().join("checkout");
        std::fs::create_dir_all(&candidate_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let run_dir = running_dir(root.path());
        std::fs::create_dir_all(&run_dir).unwrap();
        // A leader that has exited, in a process group that is still alive
        // (this test's own): no witness sees it running, the strict inventory
        // does.
        let mut child = Command::new("true").spawn().unwrap();
        let dead_leader = child.id();
        child.wait().unwrap();
        // SAFETY: getpgrp takes no arguments and cannot fail.
        let group = unsafe { libc::getpgrp() };
        assert!(group > 1);
        std::fs::write(
            run_dir.join("7-unit.pid"),
            format!("{group} tree {dead_leader} - -"),
        )
        .unwrap();
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(
            reason.starts_with(&format!(
                "{GATE_RUNNING_OWN_SESSION}: session 7 is running gate unit"
            )),
            "{reason}"
        );
        assert_eq!(snapshot.stats.gates_seen, 0, "no witness decided it");
        assert_eq!(
            snapshot.keep_reason(root.path(), &candidate, &[8]),
            None,
            "another session's gate record does not keep this checkout"
        );
        std::fs::remove_file(run_dir.join("7-unit.pid")).unwrap();

        // A record of the checkout's own session the inventory cannot verify
        // (no gate name) and no witness sees running: unknown keeps.
        std::fs::write(
            run_dir.join("7-.pid"),
            format!("{dead_leader} tree {dead_leader} - -"),
        )
        .unwrap();
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_UNKNOWN_OWNER), "{reason}");
        assert_eq!(snapshot.stats.gates_seen, 0, "no witness decided it");
        assert_eq!(snapshot.stats.inventory_unknown, 1);
        assert_eq!(snapshot.keep_reason(root.path(), &candidate, &[8]), None);
        assert_eq!(
            snapshot.stats.inventory_unknown, 1,
            "only the checkout whose own session is unverifiable counts"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_held_owner_lock_keeps_only_the_checkout_its_line_names() {
        use super::{GATE_RUNNING_PATH, GATE_RUNNING_UNKNOWN_OWNER, GateWitnessSnapshot};
        use crate::gates::hold_gate_owner_lock_for_tests;

        let root = tempfile::tempdir().unwrap();
        let candidate_path = root.path().join("checkout");
        let elsewhere_path = root.path().join("checkout-2");
        std::fs::create_dir_all(&candidate_path).unwrap();
        std::fs::create_dir_all(&elsewhere_path).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);

        let held =
            hold_gate_owner_lock_for_tests(root.path(), "unit", "s41-unit", Some(&elsewhere_path));
        let mut snapshot = GateWitnessSnapshot::default();
        assert_eq!(snapshot.keep_reason(root.path(), &candidate, &[7]), None);
        assert_eq!(snapshot.stats.attributed_elsewhere, 1);
        drop(held);

        let held =
            hold_gate_owner_lock_for_tests(root.path(), "unit", "p77-unit", Some(&candidate_path));
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_PATH), "{reason}");
        drop(held);

        let held = hold_gate_owner_lock_for_tests(root.path(), "unit", "p77-unit", None);
        let mut snapshot = GateWitnessSnapshot::default();
        let reason = snapshot.keep_reason(root.path(), &candidate, &[7]).unwrap();
        assert!(reason.starts_with(GATE_RUNNING_UNKNOWN_OWNER), "{reason}");
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn live_overlap_index_preserves_canonical_alias_and_git_directory_identity() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&real, &alias).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&real);
        let alias_child = ResolvedWorktreePath::resolve(&alias.join("child"));
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[alias_child]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "canonical aliases of nested live paths remain protected"
        );

        let git_dir = temporary.path().join("shared-git-dir");
        std::fs::create_dir_all(&git_dir).unwrap();
        let candidate_path = temporary.path().join("checkout-one");
        let live_path = temporary.path().join("checkout-two");
        for worktree in [&candidate_path, &live_path] {
            std::fs::create_dir_all(worktree).unwrap();
            std::fs::write(
                worktree.join(".git"),
                format!("gitdir: {}\n", git_dir.display()),
            )
            .unwrap();
        }
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let live = ResolvedWorktreePath::resolve(&live_path);
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[live]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "different recorded paths sharing a Git directory remain protected"
        );
    }

    #[test]
    fn unknown_live_path_identity_blocks_instead_of_authorizing_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("candidate");
        let live_path = temporary.path().join("live");
        std::fs::create_dir_all(candidate_path.join(".git")).unwrap();
        std::fs::create_dir_all(&live_path).unwrap();
        std::fs::write(live_path.join(".git"), "not a gitdir pointer\n").unwrap();

        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let unknown_live = ResolvedWorktreePath::resolve(&live_path);
        assert_eq!(
            first_live_worktree_overlap(&candidate, &[unknown_live]),
            Some((0, LiveOverlapEvidence::Unknown)),
            "unreadable or malformed identity is not treated as a complete disjoint index"
        );
    }

    #[test]
    fn live_overlap_index_keeps_the_first_matching_session_order() {
        let temporary = tempfile::tempdir().unwrap();
        let candidate_path = temporary.path().join("checkout");
        std::fs::create_dir_all(candidate_path.join("one")).unwrap();
        std::fs::create_dir_all(candidate_path.join("two")).unwrap();
        let candidate = ResolvedWorktreePath::resolve(&candidate_path);
        let first = ResolvedWorktreePath::resolve(&candidate_path.join("one"));
        let second = ResolvedWorktreePath::resolve(&candidate_path.join("two"));

        assert_eq!(
            first_live_worktree_overlap(&candidate, &[first, second]),
            Some((0, LiveOverlapEvidence::Overlap)),
            "the index returns the first overlapping live row in store order"
        );
    }

    #[cfg(unix)]
    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A proof-3 listing stops at the thread's Git deadline instead of
    /// running past it (a spent deadline starts no Git at all, and the
    /// cleanliness verdict is then an error, which every caller keeps), and
    /// is timed as a Git child. Without a deadline it answers as before.
    #[cfg(unix)]
    #[test]
    fn a_proof_listing_stops_at_the_active_git_deadline() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        let policy = AutoCleanupPolicy::default();

        let recorder = crate::phase_telemetry::Recorder::default();
        let (spent_listing, spent_verdict) = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            crate::git::with_git_deadline(std::time::Duration::ZERO, || {
                (
                    super::git(dir.path(), &["status", "--porcelain"]),
                    super::uncommitted_reason(dir.path(), None, dir.path(), &policy),
                )
            })
        };
        let error = spent_listing.expect_err("a spent deadline starts no listing");
        assert!(error.contains("time budget"), "{error}");
        assert!(
            spent_verdict.is_err(),
            "a stopped listing is no verdict: {spent_verdict:?}"
        );
        let (_, counts) = recorder.totals();
        assert!(
            counts
                .get("proof_git_deadline_before_start")
                .copied()
                .unwrap_or(0)
                >= 2,
            "{counts:?}"
        );
        assert!(!counts.contains_key("subprocess_git"), "{counts:?}");

        let recorder = crate::phase_telemetry::Recorder::default();
        let verdict = {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            crate::git::with_git_deadline(std::time::Duration::from_secs(600), || {
                super::uncommitted_reason(dir.path(), None, dir.path(), &policy)
            })
        };
        assert_eq!(verdict, Ok(None), "an empty repository is clean");
        let (_, counts) = recorder.totals();
        assert!(counts.contains_key("subprocess_git"), "{counts:?}");
        assert_eq!(
            counts.get("proof_uncommitted_check"),
            Some(&1),
            "{counts:?}"
        );
        assert_eq!(
            super::git(dir.path(), &["status", "--porcelain"]),
            Ok(String::new())
        );
    }

    /// A shell script a proof-3 listing runs instead of Git (see
    /// [`super::proof_git_seam_for_test`]); `release` is a file whose
    /// creation lets a blocked script finish.
    #[cfg(unix)]
    fn proof_git_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }

    /// Whether `pid` still runs (a zombie waiting for its reaper does not).
    #[cfg(unix)]
    fn process_running(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(')')
                .is_none_or(|(_, rest)| !rest.trim_start().starts_with('Z')),
            Err(_) => true,
        }
    }

    /// The records of a `status -z` listing: a rename's source field is
    /// consumed with it, ignored entries and Finder metadata do not count,
    /// and a record cut off mid-listing is not read.
    #[test]
    fn status_records_read_whole_records_only() {
        let listing = "R  new.txt\0old.txt\0!! target/\0?? .DS_Store\0 M src/lib.rs\0?? half";
        assert_eq!(
            super::status_records(listing),
            vec![
                ("R ", "new.txt"),
                ("!!", "target/"),
                ("??", ".DS_Store"),
                (" M", "src/lib.rs"),
            ]
        );
        assert_eq!(
            super::first_counting_record("!! target/\0?? .DS_Store\0 M a.rs\0", false),
            Some("a.rs".to_string())
        );
        assert_eq!(
            super::first_counting_record("!! target/\0?? .DS_Store\0", true),
            Some(".DS_Store".to_string())
        );
        assert_eq!(
            super::first_counting_record("!! target/\0?? x.txt", false),
            None,
            "a record not yet terminated is not read"
        );
    }

    /// The streamed early-stop check reads each byte once: a field cut off
    /// mid-read is read when it completes, ignored entries and Finder
    /// metadata never stop the listing, and a rename does.
    #[test]
    fn the_streamed_check_reads_each_field_once() {
        let mut scan = super::CountingRecordScan::new(false);
        let mut stdout = Vec::new();
        for chunk in [&b"!! tar"[..], b"get/\0?? .DS_", b"Store\0!! a.log\0"] {
            stdout.extend_from_slice(chunk);
            assert!(!scan.holds_counting_record(&stdout), "{stdout:?}");
        }
        assert_eq!(
            scan.scanned,
            stdout.len(),
            "every complete field was read exactly once"
        );
        let consumed = scan.scanned;
        stdout.extend_from_slice(b"?? notes");
        assert!(!scan.holds_counting_record(&stdout));
        assert_eq!(scan.scanned, consumed, "an unterminated field waits");
        stdout.extend_from_slice(b".txt\0");
        assert!(scan.holds_counting_record(&stdout));

        let mut strict = super::CountingRecordScan::new(true);
        assert!(strict.holds_counting_record(b"?? .DS_Store\0"));
        let mut renamed = super::CountingRecordScan::new(false);
        assert!(renamed.holds_counting_record(b"R  new.txt\0old.txt\0"));
    }

    /// A dirty checkout answers at its first record: the listing is stopped
    /// there instead of being waited for (the script never finishes on its
    /// own), and the reason says "at least 1".
    #[cfg(unix)]
    #[test]
    fn a_dirty_listing_answers_at_its_first_record() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let script = proof_git_script(
            dir.path(),
            "dirty.sh",
            &format!(
                "case \" $* \" in *\" status \"*) printf '?? notes.txt\\0'; while [ ! -e '{}' ]; do sleep 0.05; done;; *) exit 0;; esac",
                release.display()
            ),
        );
        let _seam = super::proof_git_seam_for_test(Some(script), None);
        let policy = AutoCleanupPolicy::default();
        // The deadline only turns a regression into a failure, not a hang.
        let (uncommitted, unclean) =
            crate::git::with_git_deadline(std::time::Duration::from_secs(600), || {
                (
                    super::uncommitted_reason(dir.path(), None, dir.path(), &policy),
                    super::unclean_reason(dir.path(), None, dir.path(), &policy),
                )
            });
        std::fs::write(&release, "").unwrap();
        for reason in [uncommitted, unclean] {
            assert_eq!(
                reason,
                Ok(Some(
                    "at least 1 uncommitted or untracked path(s), first: notes.txt".into()
                ))
            );
        }
    }

    /// A stopped listing takes its whole process group with it: a child the
    /// listing started (as a submodule `git status` would be) is not left
    /// running.
    #[cfg(unix)]
    #[test]
    fn a_stopped_listing_takes_its_process_group_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let pid_file = dir.path().join("child.pid");
        let script = proof_git_script(
            dir.path(),
            "group.sh",
            &format!(
                "sh -c 'while [ ! -e \"{release}\" ]; do sleep 0.05; done' &\necho $! > '{pid}.tmp' && mv '{pid}.tmp' '{pid}'\nprintf '?? notes.txt\\0'\nwhile [ ! -e '{release}' ]; do sleep 0.05; done",
                release = release.display(),
                pid = pid_file.display(),
            ),
        );
        let _seam = super::proof_git_seam_for_test(Some(script), None);
        let policy = AutoCleanupPolicy::default();
        let reason = crate::git::with_git_deadline(std::time::Duration::from_secs(600), || {
            super::uncommitted_reason(dir.path(), None, dir.path(), &policy)
        });
        let child: i32 = std::fs::read_to_string(&pid_file)
            .expect("the listing started its child before printing")
            .trim()
            .parse()
            .unwrap();
        // SAFETY: signal 0 only checks that the process exists.
        let gone = unsafe { libc::kill(child, 0) } != 0;
        let running_where_visible = std::path::Path::new("/proc/self/stat")
            .exists()
            .then(|| process_running(child));
        std::fs::write(&release, "").unwrap();
        // Whether the group cleanup could prove the group gone or not, the
        // answer is never "clean".
        assert_ne!(reason, Ok(None));
        if reason.is_ok() {
            // An answer means the cleanup proved the group absent (`ESRCH`
            // for the group): its member was killed and reaped.
            assert!(gone, "the listing's child outlived it");
        } else if let Some(running) = running_where_visible {
            // An error may only mean a killed member still waited for its
            // reaper; where `/proc` shows that, it must not be running.
            assert!(!running, "the listing's child outlived it");
        }
    }

    /// A listing stopped at its deadline and one past its output limit are
    /// errors, never "clean": every caller keeps on an error.
    #[cfg(unix)]
    #[test]
    fn stopped_and_overlong_listings_are_no_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let blocking = proof_git_script(
            dir.path(),
            "blocking.sh",
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.05; done",
                release.display()
            ),
        );
        let policy = AutoCleanupPolicy::default();
        let stopped = {
            let _seam = super::proof_git_seam_for_test(Some(blocking), None);
            crate::git::with_git_deadline(std::time::Duration::from_millis(200), || {
                super::unclean_reason(dir.path(), None, dir.path(), &policy)
            })
        };
        std::fs::write(&release, "").unwrap();
        let error = stopped.expect_err("a listing that never finished is no verdict");
        assert!(error.contains("time budget"), "{error}");

        let overlong = proof_git_script(
            dir.path(),
            "overlong.sh",
            "i=0; while [ $i -lt 64 ]; do printf ' M file-%04d.txt\\0' $i; i=$((i+1)); done",
        );
        let _seam = super::proof_git_seam_for_test(Some(overlong), Some(64));
        let error = super::git(dir.path(), &["status"]).expect_err("past the output limit");
        assert!(error.contains("output exceeded"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_ignored_listing_keeps_existing_whole_checkout_policy() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Auto cleanup test"]);
        git(
            root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(
            root.join(".gitignore"),
            ".aethyme/\nopaque/\nprivate.sqlite\n",
        )
        .unwrap();
        git(root, &["add", ".gitignore"]);
        git(root, &["commit", "-qm", "ignore disposable output"]);
        touch(root, "opaque/nested/user-data.json");
        touch(root, ".aethyme/run/worker.json");

        let policy = AutoCleanupPolicy::default();
        let reason = ignored_content_reason(root, &policy).unwrap().unwrap();
        assert!(
            reason.contains("opaque") || reason.contains(".aethyme"),
            "{reason}"
        );
        assert!(root.join("opaque/nested/user-data.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_pattern_releases_and_protected_refusals_are_counted() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Auto cleanup test"]);
        git(
            root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "reports/*\n!reports/README.md\n").unwrap();
        touch(root, "reports/README.md");
        git(root, &["add", ".gitignore", "reports/README.md"]);
        git(root, &["commit", "-qm", "ignore generated reports"]);
        touch(root, "reports/quality-1.json");
        let policy = AutoCleanupPolicy {
            regenerable: vec!["reports/*.json".into(), "reports/*.pem".into()],
            ..Default::default()
        };

        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            assert_eq!(
                super::ignored_content_blockers(root, &policy, 16).unwrap(),
                Vec::<(String, String)>::new()
            );
            touch(root, "reports/signing.pem");
            let blockers = super::ignored_content_blockers(root, &policy, 16).unwrap();
            assert_eq!(blockers.len(), 1, "{blockers:?}");
            assert_eq!(blockers[0].0, "reports/signing.pem");
            assert!(blockers[0].1.contains("protected name"), "{blockers:?}");
        }
        let (_, counts) = recorder.totals();
        assert_eq!(
            counts.get("cleanup_checkout_released_by_file_pattern"),
            Some(&1),
            "{counts:?}"
        );
        assert_eq!(
            counts.get("cleanup_regenerable_protected_refused"),
            Some(&1),
            "{counts:?}"
        );
    }

    #[cfg(unix)]
    fn closed_dirty_checkout(broker: &mut crate::Broker, label: &str) -> (i64, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "cursor test backlog owner")
            .unwrap();
        std::fs::write(worktree.join("cursor-dirty.txt"), "retain this checkout\n").unwrap();
        (session.id, worktree)
    }

    #[cfg(unix)]
    fn retirement_fixture() -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
        crate::Session,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let session = broker.start_worktree("retirement fixture", None).unwrap();
        (owner, storage, broker, session)
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    struct ArtifactUseReconcileFixture {
        _owner: tempfile::TempDir,
        _storage: crate::test_storage::TestStorage,
        broker: crate::Broker,
        session: crate::Session,
        primary_root: PathBuf,
        checkout_root: PathBuf,
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    impl ArtifactUseReconcileFixture {
        fn new() -> Self {
            let (owner, storage, broker, session) = retirement_fixture();
            let primary_root = broker.main_root().to_path_buf();
            let checkout_root = PathBuf::from(&session.worktree_path);
            Self {
                _owner: owner,
                _storage: storage,
                broker,
                session,
                primary_root,
                checkout_root,
            }
        }

        fn start_running_marker(&self, invocation_id: &str) -> std::process::Child {
            use std::os::unix::process::CommandExt;

            let admission = crate::file_lock::artifact_use::begin_artifact_use(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                invocation_id,
            )
            .unwrap();
            let crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(mut producer) =
                admission
            else {
                panic!("artifact-use protocol should be available for this test");
            };

            let mut command = Command::new("sh");
            command.args(["-c", "exec sleep 2"]);
            command.process_group(0);
            let child = command.spawn().unwrap();
            let process_group = i32::try_from(child.id()).unwrap();
            producer
                .mark_process_started(&child, process_group)
                .unwrap();
            drop(producer);
            child
        }

        #[cfg(target_os = "macos")]
        fn start_waiting_running_marker(&self, invocation_id: &str) -> std::process::Child {
            use std::os::unix::process::CommandExt;

            let admission = crate::file_lock::artifact_use::begin_artifact_use(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                invocation_id,
            )
            .unwrap();
            let crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(mut producer) =
                admission
            else {
                panic!("artifact-use protocol should be available for this test");
            };

            let mut command = Command::new("sh");
            command.args(["-c", "read -r release_marker"]);
            command.stdin(std::process::Stdio::piped());
            command.process_group(0);
            let child = command.spawn().unwrap();
            let process_group = i32::try_from(child.id()).unwrap();
            producer
                .mark_process_started(&child, process_group)
                .unwrap();
            drop(producer);
            child
        }

        fn start_predispatch_marker(&self, invocation_id: &str) {
            let admission = crate::file_lock::artifact_use::begin_artifact_use(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                invocation_id,
            )
            .unwrap();
            let crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer) =
                admission
            else {
                panic!("artifact-use protocol should be available for this test");
            };
            drop(producer);
        }

        fn close_and_bind(
            &mut self,
        ) -> (
            super::ClosedArtifactCheckoutCohort,
            crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
        ) {
            self.broker
                .close_abandoning_unpushed(
                    self.session.id,
                    "explicit artifact-use reconciliation fixture",
                )
                .unwrap();

            let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
            let deadline = Instant::now() + Duration::from_secs(15);
            for _ in 0..32 {
                let state = self
                    .broker
                    .store()
                    .cleanup_cohort_scan_state(namespace)
                    .unwrap();
                if state.as_ref().is_some_and(|state| {
                    state.complete && state.unresolved_rows == 0 && state.pending_dirty_rows == 0
                }) {
                    break;
                }
                self.broker
                    .advance_cleanup_cohort_scan(Some(deadline))
                    .unwrap();
            }
            let cohort = match self
                .broker
                .closed_artifact_checkout_cohort_for_session_readonly(
                    self.session.id,
                    &self.checkout_root,
                    Some(deadline),
                )
                .unwrap()
            {
                super::ClosedArtifactCheckoutCohortResolution::Available(cohort) => cohort,
                super::ClosedArtifactCheckoutCohortResolution::Unavailable(reason) => {
                    panic!("fixture cohort refused: {}", reason.code())
                }
            };

            let repository = GitRepo::discover(&self.primary_root).unwrap();
            let git_common_dir = repository.git_common_dir().unwrap();
            let repository_key =
                crate::host_state::repository_key(&self.primary_root, Some(&git_common_dir));
            let host_database = self.broker.host_operation_database_path().unwrap();
            let registry = crate::worktree_registry::artifact_use_primary_root_witness_guard(
                &host_database,
                &self.primary_root,
                &repository_key,
            )
            .unwrap();
            (cohort, registry)
        }

        fn marker_path(&self, invocation_id: &str) -> PathBuf {
            let runtime = self.primary_root.join(".aethyme/run/artifact-use");
            for entry in std::fs::read_dir(runtime).unwrap() {
                let entry = entry.unwrap();
                if !entry.file_type().unwrap().is_dir()
                    || !entry.file_name().to_string_lossy().ends_with(".markers")
                {
                    continue;
                }
                for marker_entry in std::fs::read_dir(entry.path()).unwrap() {
                    let marker_entry = marker_entry.unwrap();
                    let bytes = std::fs::read(marker_entry.path()).unwrap();
                    let marker: crate::file_lock::artifact_use::ArtifactUseMarker =
                        serde_json::from_slice(&bytes).unwrap();
                    if marker.session_id == self.session.id && marker.invocation_id == invocation_id
                    {
                        return marker_entry.path();
                    }
                }
            }
            panic!("fixture artifact-use marker was not found");
        }

        fn plan(
            &self,
            cohort: &super::ClosedArtifactCheckoutCohort,
            registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
        ) -> crate::file_lock::artifact_use::ArtifactUseReconcilePlan {
            crate::file_lock::artifact_use::plan_artifact_use_marker_reconciliation(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                cohort,
                registry,
            )
            .unwrap()
        }

        fn apply(
            &self,
            cohort: &super::ClosedArtifactCheckoutCohort,
            registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
            digest: &str,
        ) -> std::io::Result<crate::file_lock::artifact_use::ArtifactUseReconcileReport> {
            crate::file_lock::artifact_use::apply_artifact_use_marker_reconciliation(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                cohort,
                registry,
                digest,
            )
        }

        fn apply_with_after_unlink<F>(
            &self,
            cohort: &super::ClosedArtifactCheckoutCohort,
            registry: &crate::worktree_registry::ArtifactUsePrimaryRootWitnessGuard,
            digest: &str,
            after_unlink: F,
        ) -> std::io::Result<crate::file_lock::artifact_use::ArtifactUseReconcileReport>
        where
            F: FnMut(&Path, &str),
        {
            crate::file_lock::artifact_use::apply_artifact_use_marker_reconciliation_with_after_unlink(
                &self.primary_root,
                &self.checkout_root,
                self.session.id,
                cohort,
                registry,
                digest,
                after_unlink,
            )
        }
    }

    #[cfg(unix)]
    #[test]
    fn readonly_artifact_cohort_lookup_does_not_advance_the_index() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        let worktree = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "read-only cohort fixture")
            .unwrap();

        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let before = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .map(|state| {
                (
                    state.revision,
                    state.after_session_id,
                    state.complete,
                    state.alias_refresh_generation,
                )
            });
        let result = broker
            .closed_artifact_checkout_cohort_for_session_readonly(session.id, &worktree, None)
            .unwrap();
        if before.as_ref().map(|state| state.2) != Some(true) {
            let expected = if before.is_none() {
                super::ClosedArtifactCheckoutCohortUnavailableReason::IndexMissing
            } else {
                super::ClosedArtifactCheckoutCohortUnavailableReason::IndexIncomplete
            };
            assert!(matches!(
                result,
                super::ClosedArtifactCheckoutCohortResolution::Unavailable(reason)
                    if reason == expected
            ));
        }
        let after = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .map(|state| {
                (
                    state.revision,
                    state.after_session_id,
                    state.complete,
                    state.alias_refresh_generation,
                )
            });
        assert_eq!(after, before);
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_plan_then_apply_removes_only_proven_quiescent_marker() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-positive-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);

        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 1);
        assert!(matches!(
            &plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
        ));

        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
        assert_eq!(report.plan_digest, plan.digest);
        assert_eq!(report.outcomes.len(), 1);
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Synced
            }
        ));
        assert!(report.stopped.is_none());
        assert!(!marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    impl ArtifactUseReconcileFixture {
        /// Rewrite the session's witness as one saved without volume UUIDs
        /// before a mount renumbering, and its marker as a legacy marker.
        /// A remount renumbers every object on the volume, so both the
        /// repository's `.git` and the checkout root get new device numbers.
        /// A zero marker device means "the number the witness saw".
        fn age_witness_and_marker(
            &mut self,
            marker_path: &Path,
            primary_marker_device: u64,
            checkout_marker_device: u64,
        ) -> u64 {
            let mut saved = self
                .broker
                .store()
                .spawned_worktree_instance_witness(self.session.id)
                .unwrap()
                .expect("spawn records a witness");
            saved.schema_version = 2;
            saved.volume_uuids = None;
            saved.git_admin_device += 1;
            saved.git_common_device += 1;
            saved.worktree_device += 1;
            saved.git_pointer_device += 1;
            let witnessed_device = saved.git_common_device;
            let witnessed_checkout_device = saved.worktree_device;
            let connection = rusqlite::Connection::open(self.broker.store().db_path()).unwrap();
            connection
                .execute(
                    "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                    rusqlite::params![self.session.id, serde_json::to_string(&saved).unwrap()],
                )
                .unwrap();

            let bytes = std::fs::read(marker_path).unwrap();
            let mut marker: crate::file_lock::artifact_use::ArtifactUseMarker =
                serde_json::from_slice(&bytes).unwrap();
            marker.schema_version = 1;
            marker.primary_volume_uuid = None;
            marker.checkout_volume_uuid = None;
            marker.primary_device = if primary_marker_device == 0 {
                witnessed_device
            } else {
                primary_marker_device
            };
            marker.checkout_device = if checkout_marker_device == 0 {
                witnessed_checkout_device
            } else {
                checkout_marker_device
            };
            std::fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
            witnessed_device
        }
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_survives_a_renumbered_mount_that_reclaim_only_reproves() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-renumbered-mount-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let marker_path = fixture.marker_path(invocation_id);
        fixture.age_witness_and_marker(&marker_path, 0, 0);

        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 1);
        assert!(
            matches!(
                &plan.markers[0].disposition,
                crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
            ),
            "{:?}",
            plan.markers[0].disposition
        );

        let session_id = fixture.session.id;
        let checkout_root = fixture.checkout_root.clone();
        // Reclaim never accepts the renumbered witness as exact: this closed,
        // registered checkout on its branch is re-proven through Git, and the
        // cohort records the re-proof it needed.
        match fixture
            .broker
            .closed_artifact_checkout_cohort_for_session_detailed(session_id, &checkout_root, None)
            .unwrap()
        {
            super::ClosedArtifactCheckoutCohortResolution::Available(reclaim) => assert_eq!(
                reclaim.ownership_reproof(),
                Some(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER)
            ),
            other => panic!("reclaim must re-prove the renumbered checkout: {other:?}"),
        }

        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed { .. }
        ));
        assert!(!marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_retains_a_legacy_device_number_nothing_witnessed() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-unwitnessed-device-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let marker_path = fixture.marker_path(invocation_id);
        fixture.age_witness_and_marker(&marker_path, u64::MAX, 0);

        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        assert!(matches!(
            &plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Retain { reason }
                if reason.contains("no validated registry witness")
        ));
        assert!(marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    impl ArtifactUseReconcileFixture {
        /// Insert another session row the way `broker start` does; the real
        /// schema trigger queues its dirty row and bumps the cleanup revision.
        fn start_other_session(&mut self, worktree: &Path) {
            let at = now_ms();
            rusqlite::Connection::open(self.broker.store().db_path())
                .unwrap()
                .execute(
                    "INSERT INTO sessions (worktree_path, branch, origin, status,
                     cleanup_state, created_at, updated_at, last_activity_at)
                     VALUES (?1, 'agent/other', 'spawned', 'active', 'open', ?2, ?2, ?2)",
                    rusqlite::params![worktree.to_string_lossy(), at],
                )
                .unwrap();
        }

        fn index_snapshot(&mut self) -> (i64, i64, bool, i64) {
            let state = self
                .broker
                .store()
                .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)
                .unwrap()
                .unwrap();
            (
                state.revision,
                state.after_session_id,
                state.complete,
                state.pending_dirty_rows,
            )
        }

        fn review_cohort(&mut self) -> super::ClosedArtifactCheckoutCohortResolution {
            let session_id = self.session.id;
            let checkout_root = self.checkout_root.clone();
            self.broker
                .closed_artifact_checkout_cohort_for_session_readonly(
                    session_id,
                    &checkout_root,
                    None,
                )
                .unwrap()
        }
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_tolerates_unrelated_session_churn_without_writing_the_index() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-unrelated-churn-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        fixture.close_and_bind();

        let sibling = fixture
            .checkout_root
            .with_file_name("unrelated-sibling-checkout");
        std::fs::create_dir_all(&sibling).unwrap();
        fixture.start_other_session(&sibling);
        let before = fixture.index_snapshot();
        assert!(!before.2, "a new session must leave the index incomplete");
        assert_eq!(before.3, 1);

        let cohort = match fixture.review_cohort() {
            super::ClosedArtifactCheckoutCohortResolution::Available(cohort) => cohort,
            super::ClosedArtifactCheckoutCohortResolution::Unavailable(reason) => {
                panic!("unrelated churn refused marker review: {}", reason.code())
            }
        };
        assert_eq!(
            fixture.index_snapshot(),
            before,
            "review must not write the index"
        );
        assert_eq!(cohort.session_ids(), vec![fixture.session.id]);
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_refuses_pending_churn_that_touches_the_checkout() {
        for (label, nested) in [("same path", false), ("nested path", true)] {
            let mut fixture = ArtifactUseReconcileFixture::new();
            let invocation_id = "reconcile-touching-churn-fixture";
            let mut child = fixture.start_running_marker(invocation_id);
            child.wait().unwrap();
            fixture.close_and_bind();

            let touching = if nested {
                fixture.checkout_root.join("nested-checkout")
            } else {
                fixture.checkout_root.clone()
            };
            fixture.start_other_session(&touching);
            let before = fixture.index_snapshot();
            assert!(
                matches!(
                    fixture.review_cohort(),
                    super::ClosedArtifactCheckoutCohortResolution::Unavailable(
                        super::ClosedArtifactCheckoutCohortUnavailableReason::IndexIncomplete
                    )
                ),
                "{label}"
            );
            assert_eq!(fixture.index_snapshot(), before, "{label}");
        }
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    impl ArtifactUseReconcileFixture {
        /// Leave a quiescent marker from the current session, clean that
        /// session up, and start a fresh spawn at the same path, the way a
        /// reused task name does. Returns the predecessor's marker path.
        fn retire_into_successor_at_same_path(&mut self, invocation_id: &str) -> PathBuf {
            let mut child = self.start_running_marker(invocation_id);
            child.wait().unwrap();
            let marker_path = self.marker_path(invocation_id);
            // Cleanup refuses an unsettled marker; set it aside so the
            // predecessor is cleaned, then restore it as the stale leftover.
            let leftover = std::fs::read(&marker_path).unwrap();
            std::fs::remove_file(&marker_path).unwrap();
            let report = self.broker.finish(self.session.id).unwrap();
            assert!(report.cleanup.completed && report.cleanup.worktree_removed);
            let handoff = self
                .broker
                .latest_handoff_for_session(self.session.id)
                .unwrap();

            let successor = self
                .broker
                .start_worktree("retirement fixture", None)
                .unwrap();
            assert_eq!(successor.worktree_path, self.session.worktree_path);
            let created_at = successor.created_at.max(handoff.recorded_at + 1);
            rusqlite::Connection::open(self.broker.store().db_path())
                .unwrap()
                .execute(
                    "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                    rusqlite::params![created_at, successor.id],
                )
                .unwrap();
            self.session = self.broker.store().session(successor.id).unwrap();
            std::fs::create_dir_all(marker_path.parent().unwrap()).unwrap();
            std::fs::write(&marker_path, leftover).unwrap();
            marker_path
        }

        /// Age one session's saved witness to a pre-remount epoch (schema 2,
        /// every device renumbered) and give its worktree a declared inode,
        /// so the proof does not depend on the inode allocator.
        fn age_saved_witness(&mut self, session_id: i64, worktree_inode: Option<u64>) {
            let mut saved = self
                .broker
                .store()
                .spawned_worktree_instance_witness(session_id)
                .unwrap()
                .expect("spawn records a witness");
            saved.schema_version = 2;
            saved.volume_uuids = None;
            saved.git_admin_device += 1;
            saved.git_common_device += 1;
            saved.worktree_device += 1;
            saved.git_pointer_device += 1;
            if let Some(inode) = worktree_inode {
                saved.worktree_inode = inode;
            }
            rusqlite::Connection::open(self.broker.store().db_path())
                .unwrap()
                .execute(
                    "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                    rusqlite::params![session_id, serde_json::to_string(&saved).unwrap()],
                )
                .unwrap();
        }

        /// Rewrite a marker as legacy schema 1 naming the given checkout
        /// object and the aged `.git` device number.
        fn age_marker_naming(&mut self, marker_path: &Path, session_id: i64, inode: Option<u64>) {
            let witness = self
                .broker
                .store()
                .spawned_worktree_instance_witness(session_id)
                .unwrap()
                .unwrap();
            let mut marker: crate::file_lock::artifact_use::ArtifactUseMarker =
                serde_json::from_slice(&std::fs::read(marker_path).unwrap()).unwrap();
            marker.schema_version = 1;
            marker.primary_volume_uuid = None;
            marker.checkout_volume_uuid = None;
            marker.primary_device = witness.git_common_device;
            marker.checkout_device = witness.worktree_device;
            if let Some(inode) = inode {
                marker.checkout_inode = inode;
            }
            std::fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        }
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_retires_a_removed_predecessors_markers_after_a_renumbered_mount() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let predecessor_id = fixture.session.id;
        let leftover = fixture.retire_into_successor_at_same_path("reconcile-predecessor");
        let successor_marker = "reconcile-successor";
        let mut child = fixture.start_running_marker(successor_marker);
        child.wait().unwrap();
        let successor_path = fixture.marker_path(successor_marker);

        // Both spawns were witnessed before a remount renumbered every device.
        // The predecessor's checkout object had its own inode; its leftover
        // marker names exactly that object.
        let live_inode = fixture
            .broker
            .store()
            .spawned_worktree_instance_witness(fixture.session.id)
            .unwrap()
            .unwrap()
            .worktree_inode;
        let retired_inode = live_inode ^ (1 << 40);
        fixture.age_saved_witness(predecessor_id, Some(retired_inode));
        fixture.age_saved_witness(fixture.session.id, None);
        fixture.age_marker_naming(&leftover, predecessor_id, Some(retired_inode));
        fixture.age_marker_naming(&successor_path, fixture.session.id, None);

        let (cohort, registry) = fixture.close_and_bind();
        assert_eq!(cohort.session_ids(), vec![fixture.session.id]);
        assert!(cohort.retired_predecessor_witness(predecessor_id).is_some());
        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 2);
        for marker in &plan.markers {
            assert!(
                matches!(
                    &marker.disposition,
                    crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
                ),
                "session {}: {:?}",
                marker.session_id,
                marker.disposition
            );
        }
        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
        assert!(report.stopped.is_none());
        assert!(!leftover.exists());
        assert!(!successor_path.exists());
    }

    /// How the replacement link is evidenced in a handover fixture.
    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[derive(Clone, Copy, Debug)]
    enum HandoverEvidence {
        /// The `session.replaced` event this binary writes.
        Recorded,
        /// No event, as an earlier binary left it: only the shared timestamp.
        LegacyTimestamp,
        /// No event and no shared timestamp: nothing links the two.
        None,
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    impl ArtifactUseReconcileFixture {
        /// The spawned owner closes; an adopted session then works in the
        /// same checkout, leaves a killed producer's marker, and is replaced
        /// by `start --adopt --replace-stale`. Returns the replaced session's
        /// id and its marker; `self.session` becomes the open successor.
        fn hand_over_by_replace_stale(&mut self, evidence: HandoverEvidence) -> (i64, PathBuf) {
            let owner_id = self.session.id;
            self.broker
                .close_abandoning_unpushed(owner_id, "spawned owner closed before adoption")
                .unwrap();
            let checkout = self.checkout_root.clone();
            let stale = self
                .broker
                .adopt(&checkout, Some("stale adopted work"))
                .unwrap();
            self.session = stale.clone();
            let invocation_id = "handover-killed-producer";
            let mut child = self.start_running_marker(invocation_id);
            child.wait().unwrap();
            let marker = self.marker_path(invocation_id);

            let replaced = self
                .broker
                .adopt_with(
                    &checkout,
                    Some("successor"),
                    crate::AdoptMode::ReplaceStale,
                    None,
                )
                .unwrap();
            assert_eq!(replaced.outcome, crate::AdoptOutcome::Replaced);
            self.session = replaced.session;
            let db = rusqlite::Connection::open(self.broker.store().db_path()).unwrap();
            match evidence {
                HandoverEvidence::Recorded => {}
                HandoverEvidence::LegacyTimestamp | HandoverEvidence::None => {
                    db.execute(
                        "DELETE FROM events WHERE kind = ?1",
                        [crate::events::SESSION_REPLACED],
                    )
                    .unwrap();
                }
            }
            if matches!(evidence, HandoverEvidence::None) {
                db.execute(
                    "UPDATE sessions SET created_at = created_at + 1 WHERE id = ?1",
                    [self.session.id],
                )
                .unwrap();
            }
            (stale.id, marker)
        }

        /// Close the current session, let the index catch up, and review.
        fn close_and_review(&mut self) -> super::ClosedArtifactCheckoutCohortResolution {
            self.broker
                .close_abandoning_unpushed(self.session.id, "handover fixture")
                .unwrap();
            let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
            let deadline = Instant::now() + Duration::from_secs(15);
            for _ in 0..32 {
                let state = self
                    .broker
                    .store()
                    .cleanup_cohort_scan_state(namespace)
                    .unwrap();
                if state.as_ref().is_some_and(|state| {
                    state.complete && state.unresolved_rows == 0 && state.pending_dirty_rows == 0
                }) {
                    break;
                }
                self.broker
                    .advance_cleanup_cohort_scan(Some(deadline))
                    .unwrap();
            }
            self.review_cohort()
        }
    }

    /// A session `--replace-stale` retired never owned a checkout of its
    /// own: its successor kept working in it. Its alias must not block the
    /// cohort, and its killed producer's marker must be reconcilable, whether
    /// the link was recorded or only left by an earlier binary's transaction.
    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_settles_a_marker_left_by_a_session_replace_stale_retired() {
        for evidence in [
            HandoverEvidence::Recorded,
            HandoverEvidence::LegacyTimestamp,
        ] {
            let mut fixture = ArtifactUseReconcileFixture::new();
            let owner_id = fixture.session.id;
            let (replaced_id, marker) = fixture.hand_over_by_replace_stale(evidence);
            assert_eq!(
                fixture
                    .broker
                    .store()
                    .session_replacement_successor(replaced_id)
                    .unwrap(),
                Some(fixture.session.id),
                "{evidence:?}"
            );

            let (cohort, registry) = fixture.close_and_bind();
            let mut members = cohort.session_ids();
            members.sort_unstable();
            assert_eq!(members, vec![owner_id, fixture.session.id], "{evidence:?}");
            assert!(cohort.worked_in_live_checkout(replaced_id), "{evidence:?}");
            let plan = fixture.plan(&cohort, &registry);
            assert_eq!(plan.markers.len(), 1, "{evidence:?}");
            assert!(
                matches!(
                    &plan.markers[0].disposition,
                    crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
                ),
                "{evidence:?}: {:?}",
                plan.markers[0].disposition
            );
            let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
            assert!(report.stopped.is_none(), "{evidence:?}");
            assert!(!marker.exists(), "{evidence:?}");
        }
    }

    /// Without replacement evidence, a cleaned session recorded at the same
    /// path is an outside occupant the cohort cannot account for.
    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_still_refuses_a_same_path_cleaned_alias_without_a_handover() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let (replaced_id, marker) = fixture.hand_over_by_replace_stale(HandoverEvidence::None);
        assert_eq!(
            fixture
                .broker
                .store()
                .session_replacement_successor(replaced_id)
                .unwrap(),
            None
        );
        assert!(matches!(
            fixture.close_and_review(),
            super::ClosedArtifactCheckoutCohortResolution::Unavailable(
                super::ClosedArtifactCheckoutCohortUnavailableReason::AliasOverlapOutsideCohort
            )
        ));
        assert!(marker.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_retains_a_predecessor_marker_naming_another_checkout_object() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let predecessor_id = fixture.session.id;
        let leftover = fixture.retire_into_successor_at_same_path("reconcile-predecessor-mismatch");
        let live_inode = fixture
            .broker
            .store()
            .spawned_worktree_instance_witness(fixture.session.id)
            .unwrap()
            .unwrap()
            .worktree_inode;
        let retired_inode = live_inode ^ (1 << 40);
        fixture.age_saved_witness(predecessor_id, Some(retired_inode));
        fixture.age_saved_witness(fixture.session.id, None);
        // The marker names an object the predecessor's witness never saw.
        fixture.age_marker_naming(&leftover, predecessor_id, Some(retired_inode ^ 1));

        let (cohort, registry) = fixture.close_and_bind();
        assert!(cohort.retired_predecessor_witness(predecessor_id).is_some());
        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 1);
        assert!(matches!(
            &plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Retain { reason }
                if reason.contains("checkout root inode")
        ));
        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
        assert!(report.stopped.is_none());
        assert!(leftover.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_retains_a_checkout_device_number_nothing_witnessed() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-unwitnessed-checkout-device-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let marker_path = fixture.marker_path(invocation_id);
        fixture.age_witness_and_marker(&marker_path, 0, u64::MAX);

        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        assert!(
            matches!(
                &plan.markers[0].disposition,
                crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Retain { reason }
                    if reason == "artifact-use marker checkout root device does not match"
            ),
            "{:?}",
            plan.markers[0].disposition
        );
        assert!(marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn marker_review_still_refuses_a_live_process_after_a_renumbered_mount() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-renumbered-live-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        let marker_path = fixture.marker_path(invocation_id);
        fixture.age_witness_and_marker(&marker_path, 0, 0);

        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        let disposition = plan.markers[0].disposition.clone();
        child.wait().unwrap();
        assert!(
            !matches!(
                disposition,
                crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
            ),
            "{disposition:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn explicit_artifact_use_v2_reconcile_uses_current_registry_and_requires_quiescence() {
        use std::io::Write;

        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-v2-remounted-primary-fixture";
        let mut child = fixture.start_waiting_running_marker(invocation_id);
        let marker_path = fixture.marker_path(invocation_id);
        let bytes = std::fs::read(&marker_path).unwrap();
        let mut marker: crate::file_lock::artifact_use::ArtifactUseMarker =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(marker.schema_version, 2);
        assert!(marker.primary_volume_uuid.is_some());
        assert!(marker.checkout_volume_uuid.is_some());
        assert_ne!(marker.primary_device, u64::MAX);
        marker.primary_device = u64::MAX;
        let live_marker = marker.clone();
        std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

        let (cohort, registry) = fixture.close_and_bind();
        let live_plan = fixture.plan(&cohort, &registry);
        assert_eq!(live_plan.markers.len(), 1);
        assert!(matches!(
            &live_plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Retain { reason }
                if reason.contains("PID and start token are still present")
        ));
        let live_report = fixture
            .apply(&cohort, &registry, &live_plan.digest)
            .unwrap();
        assert!(matches!(
            &live_report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Retained { reason }
                if reason.contains("PID and start token are still present")
        ));
        assert!(marker_path.exists());

        let mut unknown_marker = live_marker.clone();
        unknown_marker.process.as_mut().unwrap().start_time = None;
        std::fs::write(&marker_path, serde_json::to_vec(&unknown_marker).unwrap()).unwrap();
        let unknown_plan = fixture.plan(&cohort, &registry);
        assert!(matches!(
            &unknown_plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Unknown { reason }
                if reason.contains("start token")
        ));
        let unknown_report = fixture
            .apply(&cohort, &registry, &unknown_plan.digest)
            .unwrap();
        assert!(matches!(
            &unknown_report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Unknown { reason }
                if reason.contains("start token")
        ));
        assert!(marker_path.exists());

        std::fs::write(&marker_path, serde_json::to_vec(&live_marker).unwrap()).unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        child.wait().unwrap();

        let quiescent_plan = fixture.plan(&cohort, &registry);
        assert!(matches!(
            &quiescent_plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
        ));
        let quiescent_report = fixture
            .apply(&cohort, &registry, &quiescent_plan.digest)
            .unwrap();
        assert!(matches!(
            &quiescent_report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Synced
            }
        ));
        assert!(!marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_refuses_atomic_marker_replacement() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-marker-replacement-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);
        let plan = fixture.plan(&cohort, &registry);

        let mut replacement = std::fs::read(&marker_path).unwrap();
        replacement.push(b'\n');
        let replacement_path = marker_path.with_extension("json-replacement");
        std::fs::write(&replacement_path, &replacement).unwrap();
        std::fs::rename(&replacement_path, &marker_path).unwrap();

        assert!(fixture.apply(&cohort, &registry, &plan.digest).is_err());
        assert_eq!(std::fs::read(&marker_path).unwrap(), replacement);
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_refuses_changed_session_alias_set() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-alias-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);
        let plan = fixture.plan(&cohort, &registry);

        let mut changed_aliases = cohort.clone();
        let mut alias = changed_aliases.sessions[0].clone();
        alias.id = fixture.session.id + 1;
        changed_aliases.sessions.push(alias);

        assert!(
            fixture
                .apply(&changed_aliases, &registry, &plan.digest)
                .is_err()
        );
        assert!(marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_refuses_changed_closed_cohort_snapshot() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-cohort-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);
        let plan = fixture.plan(&cohort, &registry);

        let mut changed_cohort = cohort.clone();
        changed_cohort.sessions[0]
            .branch
            .push_str("-changed-after-review");

        assert!(
            fixture
                .apply(&changed_cohort, &registry, &plan.digest)
                .is_err()
        );
        assert!(marker_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_retains_marker_with_unavailable_start_token() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-unknown-process-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);
        let bytes = std::fs::read(&marker_path).unwrap();
        let mut marker: crate::file_lock::artifact_use::ArtifactUseMarker =
            serde_json::from_slice(&bytes).unwrap();
        marker.process.as_mut().unwrap().start_time = None;
        std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

        let plan = fixture.plan(&cohort, &registry);
        assert!(matches!(
            &plan.markers[0].disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Unknown { reason }
                if reason.contains("start token")
        ));
        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();
        assert_eq!(report.outcomes.len(), 1);
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Unknown { reason }
                if reason.contains("start token")
        ));
        assert!(report.stopped.is_none());
        assert!(marker_path.exists());
        child.wait().unwrap();
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_returns_prior_progress_when_later_marker_changes() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let mut first_child = fixture.start_running_marker("reconcile-mid-sequence-first");
        first_child.wait().unwrap();
        let mut later_child = fixture.start_running_marker("reconcile-mid-sequence-later");
        later_child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 2);
        assert!(plan.markers.iter().all(|marker| matches!(
            &marker.disposition,
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed
        )));

        let first_filename = plan.markers[0].filename.clone();
        let later_filename = plan.markers[1].filename.clone();
        let first_path = fixture.marker_path(&plan.markers[0].invocation_id);
        let later_path = fixture.marker_path(&plan.markers[1].invocation_id);
        let mut changed_later_marker = false;
        let report = fixture
            .apply_with_after_unlink(&cohort, &registry, &plan.digest, |_, removed_filename| {
                if removed_filename == first_filename {
                    let mut replacement = std::fs::read(&later_path).unwrap();
                    replacement.push(b'\n');
                    let replacement_path = later_path.with_extension("json-replacement");
                    std::fs::write(&replacement_path, replacement).unwrap();
                    std::fs::rename(replacement_path, &later_path).unwrap();
                    changed_later_marker = true;
                }
            })
            .unwrap();

        assert!(changed_later_marker);
        assert_eq!(report.outcomes.len(), 2);
        assert_eq!(report.outcomes[0].filename, first_filename);
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Synced
            }
        ));
        assert_eq!(report.outcomes[1].filename, later_filename);
        assert!(matches!(
            &report.outcomes[1].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Unknown { .. }
        ));
        let stop = report.stopped.unwrap();
        assert!(matches!(
            stop.kind,
            crate::file_lock::artifact_use::ArtifactUseReconcileApplyStopKind::InventoryChanged
        ));
        assert_eq!(stop.marker_filename, later_filename);
        assert!(!first_path.exists());
        assert!(later_path.exists());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_reports_unconfirmed_unlink_when_directory_sync_fails() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-directory-sync-fixture";
        let mut child = fixture.start_running_marker(invocation_id);
        child.wait().unwrap();
        let (cohort, registry) = fixture.close_and_bind();
        let plan = fixture.plan(&cohort, &registry);
        assert_eq!(plan.markers.len(), 1);
        let marker_path = fixture.marker_path(invocation_id);
        let moved_directory = marker_path
            .parent()
            .unwrap()
            .with_extension("markers-moved");

        let report = fixture
            .apply_with_after_unlink(&cohort, &registry, &plan.digest, |marker_dir, filename| {
                if filename == plan.markers[0].filename {
                    std::fs::rename(marker_dir, &moved_directory).unwrap();
                }
            })
            .unwrap();

        assert_eq!(report.outcomes.len(), 1);
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Unconfirmed
            }
        ));
        let stop = report.stopped.unwrap();
        assert!(matches!(
            stop.kind,
            crate::file_lock::artifact_use::ArtifactUseReconcileApplyStopKind::MarkerDirectorySyncFailed
        ));
        assert!(!marker_path.exists());
        assert!(moved_directory.is_dir());
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn explicit_artifact_use_apply_reports_known_retained_marker() {
        let mut fixture = ArtifactUseReconcileFixture::new();
        let invocation_id = "reconcile-predispatch-fixture";
        fixture.start_predispatch_marker(invocation_id);
        let (cohort, registry) = fixture.close_and_bind();
        let marker_path = fixture.marker_path(invocation_id);
        let plan = fixture.plan(&cohort, &registry);

        let report = fixture.apply(&cohort, &registry, &plan.digest).unwrap();

        assert_eq!(report.outcomes.len(), 1);
        assert!(matches!(
            &report.outcomes[0].outcome,
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Retained { reason }
                if reason.contains("pre-dispatch")
        ));
        assert!(report.stopped.is_none());
        assert!(marker_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn removed_spawn_history_does_not_block_a_fresh_instance_at_the_same_path() {
        let (_owner, _storage, mut broker, first) = retirement_fixture();
        let old_witness = broker
            .store()
            .spawned_worktree_instance_witness(first.id)
            .unwrap()
            .unwrap();
        let report = broker.finish(first.id).unwrap();
        assert!(report.cleanup.completed && report.cleanup.worktree_removed);
        let alias = broker.store().session(first.id).unwrap();
        let handoff = broker.latest_handoff_for_session(first.id).unwrap();
        let mut current = broker.start_worktree("retirement fixture", None).unwrap();
        assert_eq!(current.worktree_path, first.worktree_path);
        // Declare strict temporal ordering without relying on clock resolution.
        // Real handoffs at a tied timestamp conservatively remain blockers.
        current.created_at = current.created_at.max(handoff.recorded_at + 1);
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![current.created_at, current.id],
            )
            .unwrap();
        broker
            .close_abandoning_unpushed(current.id, "path reuse regression fixture")
            .unwrap();
        let path = PathBuf::from(&current.worktree_path);
        let fresh = GitRepo::discover(&path)
            .unwrap()
            .spawned_worktree_instance_witness()
            .unwrap();
        // Recycled filesystem identities must remain blocked. Do not make
        // this regression depend on a filesystem's inode allocator.
        let reused_all_identities = (old_witness.worktree_device, old_witness.worktree_inode)
            == (fresh.worktree_device, fresh.worktree_inode)
            && (
                old_witness.git_pointer_device,
                old_witness.git_pointer_inode,
            ) == (fresh.git_pointer_device, fresh.git_pointer_inode)
            && (old_witness.git_admin_device, old_witness.git_admin_inode)
                == (fresh.git_admin_device, fresh.git_admin_inode);
        broker.refresh_cleanup_cohort_aliases(None).unwrap();
        let cohort = broker
            .closed_artifact_checkout_cohort_for_session(current.id, &path, None)
            .unwrap();
        assert_eq!(cohort.is_some(), !reused_all_identities);
        if let Some(cohort) = cohort {
            assert_eq!(cohort.session_ids(), vec![current.id]);
            assert!(cohort.matches_checkout_instance(&GitRepo::discover(&path).unwrap()));
        }
        // Declare a distinct old inode for the pure proof matrix, independent
        // of the allocator behavior exercised by the integration fixture.
        let mut old_witness = old_witness;
        old_witness.worktree_inode = fresh.worktree_inode ^ 1;
        assert!(proves_removed_historical_instance(
            &alias,
            &current,
            &old_witness,
            &fresh,
            &handoff
        ));

        let rejects = |old: &Session,
                       new: &Session,
                       previous: &crate::events::SpawnedWorktreeInstanceWitness,
                       fresh: &crate::events::SpawnedWorktreeInstanceWitness,
                       proof: &crate::SessionHandoffReport| {
            assert!(!proves_removed_historical_instance(
                old, new, previous, fresh, proof
            ));
        };
        let mut bad = alias.clone();
        bad.cleanup_state = SessionCleanupState::Closed;
        rejects(&bad, &current, &old_witness, &fresh, &handoff);
        let mut bad = alias.clone();
        bad.origin = SessionOrigin::Adopted;
        rejects(&bad, &current, &old_witness, &fresh, &handoff);
        let mut bad = current.clone();
        bad.created_at = handoff.recorded_at;
        rejects(&alias, &bad, &old_witness, &fresh, &handoff);
        let mut bad = handoff.clone();
        bad.handoff.cleanup.worktree_removed = false;
        rejects(&alias, &current, &old_witness, &fresh, &bad);
        let mut bad = handoff.clone();
        bad.handoff.cleanup.completed = false;
        rejects(&alias, &current, &old_witness, &fresh, &bad);
        let mut bad = handoff.clone();
        bad.handoff.cleanup.kept = true;
        rejects(&alias, &current, &old_witness, &fresh, &bad);
        let mut bad = handoff.clone();
        bad.handoff.cleanup.recovery_action = Some("inspect residue".into());
        rejects(&alias, &current, &old_witness, &fresh, &bad);
        let mut bad = old_witness.clone();
        bad.schema_version = 1;
        rejects(&alias, &current, &bad, &fresh, &handoff);
        let mut bad = old_witness.clone();
        bad.root_generation_sha256 = Some("A".repeat(64));
        rejects(&alias, &current, &bad, &fresh, &handoff);
        let mut bad = old_witness.clone();
        bad.root_generation_sha256 = None;
        rejects(&alias, &current, &bad, &fresh, &handoff);
        let mut bad = old_witness.clone();
        bad.root_generation_sha256 = fresh.root_generation_sha256.clone();
        rejects(&alias, &current, &bad, &fresh, &handoff);
        let mut bad = old_witness.clone();
        bad.git_common_inode += 1;
        rejects(&alias, &current, &bad, &fresh, &handoff);
        let mut bad = old_witness.clone();
        bad.git_admin_inode = fresh.git_admin_inode;
        bad.git_pointer_inode = fresh.git_pointer_inode;
        bad.worktree_inode = fresh.worktree_inode;
        rejects(&alias, &current, &bad, &fresh, &handoff);
        bad.worktree_device = fresh.worktree_device + 1;
        if bad.volume_uuids.is_some() {
            rejects(&alias, &current, &bad, &fresh, &handoff);
            bad.worktree_inode = fresh.worktree_inode + 1;
        }
        assert!(
            proves_removed_historical_instance(&alias, &current, &bad, &fresh, &handoff),
            "a different object is required; a saved UUID makes mount-number drift insufficient"
        );
    }

    #[cfg(unix)]
    #[test]
    fn indexed_alias_lap_reuses_900_history_and_drains_removed_checkout_delta() {
        let (_owner, _storage, mut broker, first) = retirement_fixture();
        let first_path = PathBuf::from(&first.worktree_path);
        broker
            .close_abandoning_unpushed(first.id, "bounded cohort fixture")
            .unwrap();
        let second = broker
            .start_worktree("second bounded cohort fixture", None)
            .unwrap();
        let second_path = PathBuf::from(&second.worktree_path);
        broker
            .close_abandoning_unpushed(second.id, "bounded cohort fixture")
            .unwrap();

        for session_id in [first.id, second.id] {
            assert!(
                broker
                    .store()
                    .spawned_worktree_instance_witness(session_id)
                    .unwrap()
                    .is_some(),
                "the two real worktrees must retain their spawn-time witnesses"
            );
        }
        let mut connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE sessions SET status = 'cleaned' WHERE id = ?1",
                [first.id],
            )
            .unwrap();

        let legacy_path = broker.main_root().parent().unwrap().join("legacy-history");
        std::fs::create_dir_all(&legacy_path).unwrap();
        let tx = connection.transaction().unwrap();
        for index in 0..900 {
            tx.execute(
                "INSERT INTO sessions (
                     worktree_path, branch, origin, status, cleanup_state, closed_at,
                     created_at, updated_at, last_activity_at
                 ) VALUES (?1, ?2, 'adopted', 'cleaned', 'closed', 1, 1, 1, 1)",
                rusqlite::params![
                    legacy_path.to_string_lossy(),
                    format!("agent/legacy-{index}")
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        // Model rows that predate the index: the first lap must walk the
        // complete high-water set rather than treating trigger dirties as it.
        connection
            .execute("DELETE FROM cleanup_cohort_dirty_sessions", [])
            .unwrap();

        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let initial = broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap();
        assert!(initial.complete);
        assert_eq!(initial.high_water_session_id - initial.after_session_id, 0);
        assert_eq!(initial.alias_refresh_generation, 1);
        let indexed_row_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cleanup_cohort_scan_rows WHERE namespace = ?1",
                [namespace],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed_row_count, 902,
            "900 history rows plus two live fixtures"
        );
        assert_eq!(
            broker
                .store()
                .cleanup_cohort_scan_state(namespace)
                .unwrap()
                .unwrap()
                .pending_dirty_rows,
            0
        );

        let first_cohort = broker
            .closed_artifact_checkout_cohort_for_session(first.id, &first_path, None)
            .unwrap()
            .unwrap();
        assert_eq!(first_cohort.session_ids(), vec![first.id]);
        let second_cohort = broker
            .closed_artifact_checkout_cohort_for_session(second.id, &second_path, None)
            .unwrap()
            .unwrap();
        assert_eq!(second_cohort.session_ids(), vec![second.id]);
        let before_delta = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .unwrap();
        assert_eq!(
            before_delta.alias_refresh_generation, initial.alias_refresh_generation,
            "per-candidate proofs consume the same completed alias lap"
        );

        connection
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'cleaned'
                 WHERE id = ?1",
                [first.id],
            )
            .unwrap();
        let after_removal = broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap();
        assert!(after_removal.complete);
        assert_eq!(
            after_removal.generation, initial.generation,
            "a successful removal refreshes only dirty rows, not 900 historical rows"
        );
        assert_eq!(
            after_removal.alias_refresh_generation,
            initial.alias_refresh_generation
        );
        assert!(after_removal.progress_sequence > before_delta.progress_sequence);
        let still_retained = broker
            .closed_artifact_checkout_cohort_for_session(second.id, &second_path, None)
            .unwrap()
            .unwrap();
        assert_eq!(still_retained.session_ids(), vec![second.id]);
        assert!(still_retained.owner_witness().is_some());
    }

    fn renumber_witnesses() -> (
        crate::events::SpawnedWorktreeInstanceWitness,
        crate::events::SpawnedWorktreeInstanceWitness,
    ) {
        let saved = crate::events::SpawnedWorktreeInstanceWitness {
            schema_version: 2,
            worktree_path: "/worktrees/task".into(),
            worktree_device: 5,
            worktree_inode: 101,
            git_pointer_device: 5,
            git_pointer_inode: 102,
            git_admin_path: "/repository/.git/worktrees/task".into(),
            git_admin_device: 6,
            git_admin_inode: 201,
            git_common_path: "/repository/.git".into(),
            git_common_device: 6,
            git_common_inode: 202,
            root_generation_sha256: Some("c".repeat(64)),
            volume_uuids: None,
        };
        let mut current = saved.clone();
        current.worktree_device += 10;
        current.git_pointer_device += 10;
        current.git_admin_device += 10;
        current.git_common_device += 10;
        (saved, current)
    }

    fn proven_renumber_evidence() -> super::RenumberReproofEvidence {
        super::RenumberReproofEvidence {
            origin_spawned: true,
            closed: true,
            session_path_matches: true,
            under_broker_root: true,
            common_dir_is_this_repository: true,
            registered: true,
            checkout_branch: Some("agent/task".into()),
            checkout_head: Some("1".repeat(40)),
            branch_tip: Some("1".repeat(40)),
            recorded_head: None,
        }
    }

    /// The re-proof refuses with `expected` after `mutate` breaks exactly one
    /// condition of an otherwise proven renumber.
    fn assert_renumber_refused(
        mutate: impl FnOnce(&mut super::RenumberReproofEvidence),
        expected: &str,
    ) {
        let (saved, current) = renumber_witnesses();
        let mut evidence = proven_renumber_evidence();
        assert_eq!(
            super::renumber_reproof_refusal(&saved, &current, "agent/task", &evidence),
            None,
            "the unbroken evidence must prove the renumber"
        );
        mutate(&mut evidence);
        let refusal = super::renumber_reproof_refusal(&saved, &current, "agent/task", &evidence);
        assert_eq!(refusal, Some(expected));
        let reason = format!(
            "closed checkout cohort or ownership witness changed: spawn-time worktree identity \
             device renumber re-proof refused: {expected}"
        );
        assert_eq!(
            super::kept_reason_code(&reason),
            "ownership_reproof_refused"
        );
    }

    #[test]
    fn renumber_reproof_accepts_only_unchanged_objects() {
        let (saved, mut current) = renumber_witnesses();
        let evidence = proven_renumber_evidence();
        assert_eq!(
            super::renumber_reproof_refusal(&saved, &current, "agent/task", &evidence),
            None
        );
        current.worktree_inode += 1;
        assert_eq!(
            super::renumber_reproof_refusal(&saved, &current, "agent/task", &evidence),
            Some(crate::events::IDENTITY_INODE_CHANGED)
        );
    }

    #[test]
    fn renumber_reproof_requires_a_spawned_session() {
        assert_renumber_refused(
            |evidence| evidence.origin_spawned = false,
            "the session was not spawned by the broker",
        );
    }

    #[test]
    fn renumber_reproof_requires_a_closed_session() {
        assert_renumber_refused(
            |evidence| evidence.closed = false,
            "the session is not closed",
        );
    }

    #[test]
    fn renumber_reproof_requires_the_session_row_to_name_the_checkout() {
        assert_renumber_refused(
            |evidence| evidence.session_path_matches = false,
            "the session row names another path",
        );
    }

    #[test]
    fn renumber_reproof_requires_the_broker_worktree_root() {
        assert_renumber_refused(
            |evidence| evidence.under_broker_root = false,
            "the checkout is not under the broker worktree root",
        );
    }

    #[test]
    fn renumber_reproof_requires_the_git_pointer_into_this_repository() {
        assert_renumber_refused(
            |evidence| evidence.common_dir_is_this_repository = false,
            "the checkout's .git does not point into this repository",
        );
    }

    #[test]
    fn renumber_reproof_requires_this_repository_to_register_the_checkout() {
        assert_renumber_refused(
            |evidence| evidence.registered = false,
            "this repository does not register the checkout",
        );
    }

    #[test]
    fn renumber_reproof_requires_the_checkout_on_the_session_branch() {
        assert_renumber_refused(
            |evidence| evidence.checkout_branch = None,
            "the checkout is not on the session branch",
        );
        assert_renumber_refused(
            |evidence| evidence.checkout_branch = Some("agent/other".into()),
            "the checkout is not on the session branch",
        );
    }

    #[test]
    fn renumber_reproof_requires_head_at_the_session_branch_tip() {
        assert_renumber_refused(
            |evidence| evidence.checkout_head = Some("2".repeat(40)),
            "the checkout HEAD is not the session branch tip",
        );
        assert_renumber_refused(
            |evidence| evidence.branch_tip = None,
            "the checkout HEAD is not the session branch tip",
        );
    }

    #[test]
    fn ownership_reproof_names_the_reproof_that_held() {
        use crate::events::{IDENTITY_VOLUME_UUID_DIFFERS, IDENTITY_VOLUME_UUID_UNAVAILABLE};
        let decide = |saved: &crate::events::SpawnedWorktreeInstanceWitness,
                      current: &crate::events::SpawnedWorktreeInstanceWitness,
                      evidence: &super::RenumberReproofEvidence| {
            super::ownership_reproof_decision(saved, current, "agent/task", evidence)
        };
        let (saved, current) = renumber_witnesses();
        let evidence = proven_renumber_evidence();
        assert_eq!(
            decide(&saved, &current, &evidence),
            Ok(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER)
        );
        // Recorded volume UUIDs that still match after every object's device
        // was renumbered.
        let mut saved_uuid = saved.clone();
        saved_uuid.schema_version = 3;
        saved_uuid.volume_uuids = Some(crate::events::SpawnedWorktreeVolumeUuids {
            worktree: [1; 16],
            git_pointer: [1; 16],
            git_admin: [2; 16],
            git_common: [2; 16],
        });
        let mut current_uuid = current.clone();
        current_uuid.schema_version = 3;
        current_uuid.volume_uuids = saved_uuid.volume_uuids.clone();
        assert_eq!(
            decide(&saved_uuid, &current_uuid, &evidence),
            Ok(super::OWNERSHIP_REPROOF_VOLUME_UUID)
        );
        // A UUID match is not authority on its own: the Git half applies.
        let mut detached = evidence.clone();
        detached.checkout_branch = None;
        assert_eq!(
            decide(&saved_uuid, &current_uuid, &detached),
            Err("the checkout is not on the session branch")
        );
        let mut moved = evidence.clone();
        moved.branch_tip = Some("2".repeat(40));
        assert_eq!(
            decide(&saved_uuid, &current_uuid, &moved),
            Err("the checkout HEAD is not the session branch tip")
        );
        let mut unregistered = evidence.clone();
        unregistered.registered = false;
        assert_eq!(
            decide(&saved_uuid, &current_uuid, &unregistered),
            Err(super::REGISTRATION_REFUSAL)
        );
        let mut other_volume = current_uuid.clone();
        other_volume.volume_uuids.as_mut().unwrap().worktree = [9; 16];
        assert_eq!(
            decide(&saved_uuid, &other_volume, &evidence),
            Err(IDENTITY_VOLUME_UUID_DIFFERS)
        );
        // Today's observation without UUIDs (an unknown platform).
        assert_eq!(
            decide(&saved_uuid, &current, &evidence),
            Err(IDENTITY_VOLUME_UUID_UNAVAILABLE)
        );
        assert_eq!(
            decide(&saved, &saved, &evidence),
            Err(super::REPROOF_NOTHING_TO_REPROVE)
        );
    }

    /// In production `HEAD` is a symbolic ref to the session branch, so the
    /// checkout head and the branch tip always agree; only a recorded commit
    /// can notice the branch moved since the broker recorded it.
    #[test]
    fn ownership_reproof_binds_head_to_the_recorded_commit() {
        let (saved, current) = renumber_witnesses();
        let mut evidence = proven_renumber_evidence();
        evidence.checkout_head = Some("3".repeat(40));
        evidence.branch_tip = Some("3".repeat(40));
        assert_eq!(
            super::ownership_reproof_decision(&saved, &current, "agent/task", &evidence),
            Ok(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER),
            "without a recorded commit the branch condition alone applies"
        );
        evidence.recorded_head = Some("3".repeat(40));
        assert_eq!(
            super::ownership_reproof_decision(&saved, &current, "agent/task", &evidence),
            Ok(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER)
        );
        evidence.recorded_head = Some("1".repeat(40));
        assert_eq!(
            super::ownership_reproof_decision(&saved, &current, "agent/task", &evidence),
            Err(super::REPROOF_HEAD_MOVED),
            "the branch and HEAD moved together past the recorded commit"
        );
    }

    #[test]
    fn the_final_guard_accepts_only_the_cohorts_own_reproof() {
        use super::{
            OWNERSHIP_REPROOF_DEVICE_RENUMBER, OWNERSHIP_REPROOF_VOLUME_UUID,
            cohort_identity_still_holds,
        };
        let (saved, current) = renumber_witnesses();
        assert!(cohort_identity_still_holds(None, &saved, &saved));
        assert!(cohort_identity_still_holds(
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER),
            &saved,
            &current
        ));
        // A cohort proven exactly, then a renumber before unlink.
        assert!(!cohort_identity_still_holds(None, &saved, &current));
        // A re-proven cohort, then another object at the path.
        let mut replaced = current.clone();
        replaced.worktree_inode += 1;
        assert!(!cohort_identity_still_holds(
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER),
            &saved,
            &replaced
        ));
        // A device-renumber cohort against a volume-UUID observation, and
        // the reverse.
        let uuids = crate::events::SpawnedWorktreeVolumeUuids {
            worktree: [1; 16],
            git_pointer: [1; 16],
            git_admin: [2; 16],
            git_common: [2; 16],
        };
        let mut saved_uuid = saved.clone();
        saved_uuid.schema_version = 3;
        saved_uuid.volume_uuids = Some(uuids.clone());
        let mut current_uuid = current.clone();
        current_uuid.schema_version = 3;
        current_uuid.volume_uuids = Some(uuids);
        assert_eq!(
            saved_uuid.compare_checkout_identity(&current_uuid),
            crate::events::CheckoutIdentityMatch::VolumeUuid
        );
        assert!(!cohort_identity_still_holds(
            Some(OWNERSHIP_REPROOF_VOLUME_UUID),
            &saved,
            &current
        ));
        // A schema-3 witness matches by UUID exactly, so the volume-UUID
        // kind is checked through a legacy-shaped record here.
        let mut legacy_saved = saved_uuid.clone();
        legacy_saved.schema_version = 2;
        legacy_saved.volume_uuids = None;
        assert!(!cohort_identity_still_holds(
            Some(OWNERSHIP_REPROOF_VOLUME_UUID),
            &legacy_saved,
            &current_uuid
        ));
        assert!(cohort_identity_still_holds(
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER),
            &legacy_saved,
            &current_uuid
        ));
    }

    #[test]
    fn every_ownership_reproof_refusal_has_its_own_counter() {
        let table = super::OWNERSHIP_REPROOF_REFUSAL_COUNTERS;
        let reasons = table
            .iter()
            .map(|(reason, _)| *reason)
            .collect::<std::collections::BTreeSet<_>>();
        let counters = table
            .iter()
            .map(|(_, counter)| *counter)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(reasons.len(), table.len(), "reasons are distinct");
        assert_eq!(counters.len(), table.len(), "counters are distinct");
        for (reason, counter) in table {
            assert_eq!(super::ownership_reproof_refusal_counter(reason), *counter);
            assert!(
                counter.starts_with("ownership_reproof_refused_"),
                "{counter}"
            );
            assert_ne!(*counter, "ownership_reproof_refused_other");
        }
        assert_eq!(
            super::ownership_reproof_refusal_counter("an unlisted reason"),
            "ownership_reproof_refused_other"
        );
    }

    #[test]
    fn ownership_keep_reasons_have_their_own_codes() {
        for (reason, code) in [
            (
                "closed checkout cohort or ownership witness changed: spawn-time worktree \
                 instance witness was never recorded for session 7; inspect it",
                "ownership_witness_missing",
            ),
            (
                "closed checkout cohort or ownership witness changed: spawn-time worktree \
                 instance witness is a legacy record without a spawn generation (schema 1)",
                "ownership_witness_legacy",
            ),
            (
                "no matching spawn-time worktree instance witness: only adopted sessions use \
                 this checkout, so the broker cannot prove it created it",
                "ownership_adopted_only",
            ),
            (
                "not a readable broker-owned worktree: the checkout directory no longer exists",
                "checkout_missing",
            ),
            (
                "closed checkout cohort or ownership witness changed: cohort unavailable \
                 (index_changed)",
                "cohort_unavailable",
            ),
            (
                "no matching spawn-time worktree instance witness",
                "ownership_unproven",
            ),
        ] {
            assert_eq!(super::kept_reason_code(reason), code, "{reason}");
        }
    }

    #[cfg(unix)]
    fn rewrite_spawn_witness(
        broker: &mut crate::Broker,
        session_id: i64,
        witness: &crate::events::SpawnedWorktreeInstanceWitness,
    ) {
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                rusqlite::params![session_id, serde_json::to_string(witness).unwrap()],
            )
            .unwrap();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap()
            .complete
        {}
    }

    #[cfg(unix)]
    #[test]
    fn checkout_removal_reproves_a_schema_two_witness_after_a_device_renumber() {
        use super::{
            AutoCleanupCheckoutGroup, AutoCleanupVerdictOptions,
            ClosedArtifactCheckoutCohortResolution, GateWitnessSnapshot, LeaseProcessTableSnapshot,
            NotRemoved, OWNERSHIP_REPROOF_DEVICE_RENUMBER,
        };

        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "device renumber fixture")
            .unwrap();
        let path = PathBuf::from(&session.worktree_path);
        let mut saved = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("spawn captures a witness");
        // What a pre-UUID spawn saved, seen after a restart renumbered every
        // mount: same objects and generation, new device numbers.
        saved.schema_version = 2;
        saved.volume_uuids = None;
        saved.worktree_device += 1000;
        saved.git_pointer_device += 1000;
        saved.git_admin_device += 1000;
        saved.git_common_device += 1000;
        rewrite_spawn_witness(&mut broker, session.id, &saved);

        let reclaim = broker
            .closed_artifact_checkout_cohort_for_session(session.id, &path, None)
            .unwrap()
            .expect("GC reclaim shares the automatic lane's re-proof");
        assert_eq!(
            reclaim.ownership_reproof(),
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER)
        );
        assert!(
            reclaim.matches_checkout_instance(&GitRepo::discover(&path).unwrap()),
            "the final unlink guard accepts the re-proven instance"
        );
        let cohort = match broker
            .closed_checkout_cohort_for_removal(session.id, &path, None)
            .unwrap()
        {
            ClosedArtifactCheckoutCohortResolution::Available(cohort) => cohort,
            other => panic!("the renumbered checkout must be re-proven: {other:?}"),
        };
        assert_eq!(
            cohort.ownership_reproof(),
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER)
        );

        std::fs::write(
            broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let group = AutoCleanupCheckoutGroup {
            worktree: path.clone(),
            sessions: vec![closed],
            complete: true,
            cursor_session_id: session.id,
        };
        let candidate = match broker
            .auto_cleanup_candidate_for_group(&group, &[], &[], None)
            .unwrap()
        {
            AutoCleanupGroupInspection::Candidate(candidate) => candidate,
            _ => panic!("the renumbered checkout must remain a cleanup candidate"),
        };
        let head = GitRepo::discover(&path).unwrap().head_commit().unwrap();
        let upstream = ("main".into(), head.clone());
        let selected = ContainmentProof {
            head: head.clone(),
            containing_commit: head,
            proof: "fixture unchanged HEAD".into(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        };
        let verdict_for = |broker: &mut crate::Broker| {
            let mut lease_process_table = LeaseProcessTableSnapshot::default();
            broker.auto_cleanup_verdict(
                &candidate,
                &AutoCleanupPolicy::default(),
                &upstream,
                AutoCleanupVerdictOptions {
                    deep: false,
                    deadline: None,
                    selected: Some(&selected),
                    lease_process_table: &mut lease_process_table,
                    gate_witnesses: &mut GateWitnessSnapshot::default(),
                    live_index: None,
                },
            )
        };
        let proof = verdict_for(&mut broker).expect("the re-proof authorizes the verdict");
        assert_eq!(
            proof.ownership_reproof,
            Some(OWNERSHIP_REPROOF_DEVICE_RENUMBER),
            "the verdict records the re-proof it used"
        );

        // A detached HEAD fails the branch condition: kept, and countable.
        git(&path, &["checkout", "-q", "--detach"]);
        match verdict_for(&mut broker) {
            Err(NotRemoved::Keep(reason)) => {
                assert_eq!(
                    super::kept_reason_code(&reason),
                    "ownership_reproof_refused",
                    "{reason}"
                );
                assert!(
                    reason.contains("the checkout is not on the session branch"),
                    "{reason}"
                );
            }
            other => panic!("a detached checkout must be kept: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn checkout_removal_refuses_a_renumbered_witness_whose_root_was_replaced() {
        use super::ClosedArtifactCheckoutCohortResolution;

        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "replaced root fixture")
            .unwrap();
        let path = PathBuf::from(&session.worktree_path);
        let mut saved = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("spawn captures a witness");
        saved.schema_version = 2;
        saved.volume_uuids = None;
        saved.worktree_device += 1000;
        saved.git_pointer_device += 1000;
        saved.git_admin_device += 1000;
        saved.git_common_device += 1000;
        // Another root generation: not the directory the broker spawned.
        saved.root_generation_sha256 = Some("d".repeat(64));
        rewrite_spawn_witness(&mut broker, session.id, &saved);
        assert!(matches!(
            broker
                .closed_checkout_cohort_for_removal(session.id, &path, None)
                .unwrap(),
            ClosedArtifactCheckoutCohortResolution::Unavailable(
                super::ClosedArtifactCheckoutCohortUnavailableReason::SpawnedWitnessMismatch
            )
        ));
    }

    /// GC reclaim and compaction share the automatic lane's re-proof, and
    /// keep each refusal apart: the Git half (a detached checkout) and the
    /// filesystem half (another inode) have their own cohort codes.
    #[cfg(unix)]
    #[test]
    fn gc_reclaim_reproves_a_renumbered_witness_and_reports_each_refusal() {
        use super::{
            ClosedArtifactCheckoutCohortResolution as Resolution,
            ClosedArtifactCheckoutCohortUnavailableReason as Reason,
        };

        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "gc reclaim renumber fixture")
            .unwrap();
        let path = PathBuf::from(&session.worktree_path);
        let mut saved = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("spawn captures a witness");
        saved.schema_version = 2;
        saved.volume_uuids = None;
        saved.worktree_device += 1000;
        saved.git_pointer_device += 1000;
        saved.git_admin_device += 1000;
        saved.git_common_device += 1000;
        rewrite_spawn_witness(&mut broker, session.id, &saved);
        let reclaim = |broker: &mut crate::Broker| {
            broker
                .closed_artifact_checkout_cohort_for_session_detailed(session.id, &path, None)
                .unwrap()
        };
        match reclaim(&mut broker) {
            Resolution::Available(cohort) => assert_eq!(
                cohort.ownership_reproof(),
                Some(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER)
            ),
            other => panic!("GC reclaim must re-prove the renumbered checkout: {other:?}"),
        }

        git(&path, &["checkout", "-q", "--detach"]);
        assert_eq!(
            match reclaim(&mut broker) {
                Resolution::Unavailable(reason) => reason,
                Resolution::Available(_) => panic!("a detached checkout must not be re-proven"),
            },
            Reason::OwnershipReproofRefused
        );
        git(&path, &["checkout", "-q", session.branch.as_str()]);

        saved.worktree_inode ^= 1 << 40;
        rewrite_spawn_witness(&mut broker, session.id, &saved);
        assert_eq!(
            match reclaim(&mut broker) {
                Resolution::Unavailable(reason) => reason,
                Resolution::Available(_) => panic!("another inode must not be re-proven"),
            },
            Reason::SpawnedWitnessMismatch
        );
    }

    /// The retirement rechecks a compaction runs build the GC cohort again,
    /// so they must build the same cohort, but their re-proof decisions are
    /// counted in their own lane, never under `gc_*`.
    #[cfg(unix)]
    #[test]
    fn retirement_rechecks_count_their_reproof_apart_from_gc() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "retirement reproof lane fixture")
            .unwrap();
        let path = PathBuf::from(&session.worktree_path);
        let mut saved = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("spawn captures a witness");
        saved.schema_version = 2;
        saved.volume_uuids = None;
        saved.worktree_device += 1000;
        saved.git_pointer_device += 1000;
        saved.git_admin_device += 1000;
        saved.git_common_device += 1000;
        rewrite_spawn_witness(&mut broker, session.id, &saved);

        let counts = |broker: &mut crate::Broker, retirement: bool| {
            let recorder = crate::phase_telemetry::Recorder::default();
            let cohort = {
                let _installed = crate::phase_telemetry::install(recorder.clone());
                let cohort = if retirement {
                    broker.closed_artifact_checkout_cohort_for_retirement(session.id, &path, None)
                } else {
                    broker.closed_artifact_checkout_cohort_for_session(session.id, &path, None)
                };
                cohort
                    .unwrap()
                    .expect("the renumbered checkout is re-proven")
            };
            (cohort, recorder.totals().1)
        };
        let (gc_cohort, gc_counts) = counts(&mut broker, false);
        let (retirement_cohort, retirement_counts) = counts(&mut broker, true);
        assert_eq!(
            retirement_cohort, gc_cohort,
            "a retirement recheck must compare equal to the cohort GC chose"
        );
        assert_eq!(
            retirement_cohort.ownership_reproof(),
            Some(super::OWNERSHIP_REPROOF_DEVICE_RENUMBER)
        );
        assert_eq!(
            gc_counts.get("gc_ownership_reproved"),
            Some(&1),
            "{gc_counts:?}"
        );
        assert_eq!(
            retirement_counts.get("retirement_ownership_reproved"),
            Some(&1),
            "{retirement_counts:?}"
        );
        assert!(
            !retirement_counts
                .keys()
                .any(|name| name.starts_with("gc_ownership")),
            "{retirement_counts:?}"
        );

        git(&path, &["checkout", "-q", "--detach"]);
        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            assert!(
                broker
                    .closed_artifact_checkout_cohort_for_retirement(session.id, &path, None)
                    .unwrap()
                    .is_none(),
                "a detached checkout must not be re-proven"
            );
        }
        let (_, refused) = recorder.totals();
        assert_eq!(
            refused.get("retirement_ownership_reproof_refused"),
            Some(&1),
            "{refused:?}"
        );
        assert!(
            !refused.keys().any(|name| name.starts_with("gc_ownership")),
            "{refused:?}"
        );
    }

    /// A closed spawned checkout whose saved witness is a schema-2 record of
    /// the same objects under renumbered devices, with today's observation.
    #[cfg(unix)]
    struct RenumberFixture {
        _owner: tempfile::TempDir,
        _storage: crate::test_storage::TestStorage,
        broker: crate::Broker,
        session: crate::Session,
        path: PathBuf,
        saved: crate::events::SpawnedWorktreeInstanceWitness,
        current: crate::events::SpawnedWorktreeInstanceWitness,
    }

    #[cfg(unix)]
    impl RenumberFixture {
        fn new() -> Self {
            let (owner, storage, mut broker, session) = retirement_fixture();
            broker
                .close_abandoning_unpushed(session.id, "renumber evidence fixture")
                .unwrap();
            let path = PathBuf::from(&session.worktree_path);
            let current = GitRepo::discover(&path)
                .unwrap()
                .spawned_worktree_instance_witness()
                .expect("the live checkout has a witness");
            let mut saved = current.clone();
            saved.schema_version = 2;
            saved.volume_uuids = None;
            saved.worktree_device += 1000;
            saved.git_pointer_device += 1000;
            saved.git_admin_device += 1000;
            saved.git_common_device += 1000;
            let session = broker.store().session(session.id).unwrap();
            Self {
                _owner: owner,
                _storage: storage,
                broker,
                session,
                path,
                saved,
                current,
            }
        }

        fn reproof(&self, session: &crate::Session) -> Result<(), super::ReproofFailure> {
            self.broker.renumbered_witness_reproof(
                session,
                &self.path,
                &self.saved,
                &self.current,
                None,
            )
        }

        /// The re-proof holds as built, and refuses naming `expected` after
        /// `change` (undone by `restore`) breaks one gathered condition.
        fn assert_refused_by(
            &self,
            session: &crate::Session,
            change: impl FnOnce(),
            restore: impl FnOnce(),
            expected: &str,
        ) {
            assert_eq!(self.reproof(&self.session), Ok(()));
            change();
            let result = self.reproof(session);
            restore();
            match result {
                Err(super::ReproofFailure::Refused(reason)) => {
                    assert!(reason.contains(expected), "{reason}");
                    assert_eq!(
                        super::kept_reason_code(&reason),
                        "ownership_reproof_refused"
                    );
                }
                other => panic!("expected a refusal naming {expected:?}: {other:?}"),
            }
            assert_eq!(self.reproof(&self.session), Ok(()), "restored");
        }
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_a_closed_session_row() {
        let fixture = RenumberFixture::new();
        let mut open = fixture.session.clone();
        open.cleanup_state = SessionCleanupState::Open;
        fixture.assert_refused_by(&open, || {}, || {}, "the session is not closed");
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_the_session_row_to_name_the_checkout() {
        let fixture = RenumberFixture::new();
        let mut elsewhere = fixture.session.clone();
        elsewhere.worktree_path = fixture.broker.main_root().to_string_lossy().into_owned();
        fixture.assert_refused_by(
            &elsewhere,
            || {},
            || {},
            "the session row names another path",
        );
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_the_broker_worktree_root() {
        let fixture = RenumberFixture::new();
        let marker = fixture
            .path
            .parent()
            .unwrap()
            .join(crate::broker::WORKTREE_ROOT_MARKER);
        let original = std::fs::read(&marker).expect("the fixture root carries its marker");
        fixture.assert_refused_by(
            &fixture.session,
            || std::fs::write(&marker, b"{}").unwrap(),
            || std::fs::write(&marker, &original).unwrap(),
            "the checkout is not under the broker worktree root",
        );
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_this_repositorys_common_directory() {
        let mut fixture = RenumberFixture::new();
        let other = fixture.broker.main_root().join(".aethyme");
        let other = std::fs::canonicalize(other).unwrap();
        fixture.saved.git_common_path = other.to_string_lossy().into_owned();
        fixture.current.git_common_path = fixture.saved.git_common_path.clone();
        match fixture.reproof(&fixture.session) {
            Err(super::ReproofFailure::Refused(reason)) => assert!(
                reason.contains("does not point into this repository"),
                "{reason}"
            ),
            other => panic!("another common directory must be refused: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_a_registration_naming_the_checkout() {
        let fixture = RenumberFixture::new();
        let gitdir = Path::new(&fixture.current.git_admin_path).join("gitdir");
        let original = std::fs::read(&gitdir).unwrap();
        // Prunable: the registration names a `.git` that no longer exists.
        fixture.assert_refused_by(
            &fixture.session,
            || std::fs::write(&gitdir, "/nonexistent/checkout/.git\n").unwrap(),
            || std::fs::write(&gitdir, &original).unwrap(),
            "this repository does not register the checkout",
        );
        // Registered to another checkout's `.git` file.
        let other = fixture
            .broker
            .main_root()
            .parent()
            .unwrap()
            .join("other-checkout");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(".git"), "gitdir: /nowhere\n").unwrap();
        let other_pointer = other.join(".git");
        fixture.assert_refused_by(
            &fixture.session,
            || {
                std::fs::write(&gitdir, format!("{}\n", other_pointer.display())).unwrap();
            },
            || std::fs::write(&gitdir, &original).unwrap(),
            "this repository does not register the checkout",
        );
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_the_checkout_on_the_session_branch() {
        let fixture = RenumberFixture::new();
        let mut renamed = fixture.session.clone();
        renamed.branch = "agent/some-other-branch".into();
        fixture.assert_refused_by(
            &renamed,
            || {},
            || {},
            "the checkout is not on the session branch",
        );
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_requires_head_at_the_session_branch_tip() {
        let fixture = RenumberFixture::new();
        // On the branch, but the branch ref no longer resolves.
        let loose = Path::new(&fixture.current.git_common_path)
            .join("refs/heads")
            .join(&fixture.session.branch);
        let aside = loose.with_extension("aside");
        assert!(loose.is_file(), "the fixture branch is a loose ref");
        fixture.assert_refused_by(
            &fixture.session,
            || std::fs::rename(&loose, &aside).unwrap(),
            || std::fs::rename(&aside, &loose).unwrap(),
            "the checkout HEAD is not the session branch tip",
        );
    }

    #[cfg(unix)]
    #[test]
    fn renumber_evidence_out_of_time_is_unavailable_not_refused() {
        let fixture = RenumberFixture::new();
        let expired = Some(Instant::now() - Duration::from_millis(1));
        assert!(matches!(
            fixture.broker.renumbered_witness_reproof(
                &fixture.session,
                &fixture.path,
                &fixture.saved,
                &fixture.current,
                expired,
            ),
            Err(super::ReproofFailure::Unavailable(_))
        ));
    }

    /// The verdict re-makes the re-proof against a fresh observation after
    /// building the cohort: a registration broken in between keeps the
    /// checkout even though the cohort accepted it.
    #[cfg(unix)]
    #[test]
    fn the_verdict_recheck_refuses_a_checkout_changed_after_the_cohort() {
        use super::{
            AutoCleanupCheckoutGroup, AutoCleanupVerdictOptions, GateWitnessSnapshot,
            LeaseProcessTableSnapshot, NotRemoved,
        };

        let mut fixture = RenumberFixture::new();
        let session_id = fixture.session.id;
        let saved = fixture.saved.clone();
        rewrite_spawn_witness(&mut fixture.broker, session_id, &saved);
        std::fs::write(
            fixture.broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        let closed = fixture.broker.store().session(session_id).unwrap();
        let group = AutoCleanupCheckoutGroup {
            worktree: fixture.path.clone(),
            sessions: vec![closed],
            complete: true,
            cursor_session_id: session_id,
        };
        let candidate = match fixture
            .broker
            .auto_cleanup_candidate_for_group(&group, &[], &[], None)
            .unwrap()
        {
            AutoCleanupGroupInspection::Candidate(candidate) => candidate,
            _ => panic!("the renumbered checkout must be a cleanup candidate"),
        };
        let head = GitRepo::discover(&fixture.path)
            .unwrap()
            .head_commit()
            .unwrap();
        let upstream = ("main".into(), head.clone());
        let selected = ContainmentProof {
            head: head.clone(),
            containing_commit: head,
            proof: "fixture unchanged HEAD".into(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        };
        let gitdir = Path::new(&fixture.current.git_admin_path).join("gitdir");
        let original = std::fs::read(&gitdir).unwrap();
        let broken = gitdir.clone();
        super::AFTER_REMOVAL_COHORT_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |_: &Path| {
                std::fs::write(&broken, "/nonexistent/checkout/.git\n").unwrap();
            }));
        });
        let mut lease_process_table = LeaseProcessTableSnapshot::default();
        let verdict = fixture.broker.auto_cleanup_verdict(
            &candidate,
            &AutoCleanupPolicy::default(),
            &upstream,
            AutoCleanupVerdictOptions {
                deep: false,
                deadline: None,
                selected: Some(&selected),
                lease_process_table: &mut lease_process_table,
                gate_witnesses: &mut GateWitnessSnapshot::default(),
                live_index: None,
            },
        );
        super::AFTER_REMOVAL_COHORT_HOOK.with(|hook| *hook.borrow_mut() = None);
        std::fs::write(&gitdir, &original).unwrap();
        match verdict {
            Err(NotRemoved::Keep(reason)) => assert!(
                reason.contains("worktree instance no longer matches its spawn record"),
                "{reason}"
            ),
            other => panic!("the fresh re-check must keep it: {other:?}"),
        }
    }

    /// A closed checkout whose directory is gone is judged by one stat,
    /// under its own code, before any Git process.
    #[cfg(unix)]
    #[test]
    fn a_missing_checkout_directory_is_kept_as_checkout_missing() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "missing directory fixture")
            .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let gone = broker.main_root().parent().unwrap().join("gone-checkout");
        let group = super::AutoCleanupCheckoutGroup {
            worktree: gone,
            sessions: vec![closed],
            complete: true,
            cursor_session_id: session.id,
        };
        match broker
            .auto_cleanup_candidate_for_group(&group, &[], &[], None)
            .unwrap()
        {
            AutoCleanupGroupInspection::Kept(kept) => {
                assert_eq!(
                    super::kept_reason_code(&kept.reason),
                    "checkout_missing",
                    "{}",
                    kept.reason
                );
            }
            _ => panic!("a missing directory must be kept"),
        }
    }

    /// Every judged priority group is remembered, selected and deferred ones
    /// included, so it is not picked first on the next pass; a deferred one
    /// comes back sooner, and a changed merged state at once.
    #[test]
    fn the_priority_memo_remembers_selected_kept_and_deferred_groups() {
        use super::{PRIORITY_DEFER_MS, PRIORITY_REJUDGE_MS, PriorityMemo, PriorityOutcome};

        let now = 1_000_000;
        let mut memo = PriorityMemo::default();
        memo.remember("/selected".into(), true, PriorityOutcome::Selected, now);
        memo.remember("/kept".into(), true, PriorityOutcome::Kept, now);
        memo.remember("/deferred".into(), false, PriorityOutcome::Deferred, now);
        for path in ["/selected", "/kept"] {
            assert!(
                memo.recently_judged(path, true, now + PRIORITY_REJUDGE_MS - 1),
                "{path}"
            );
            assert!(
                !memo.recently_judged(path, true, now + PRIORITY_REJUDGE_MS),
                "{path}"
            );
        }
        assert!(memo.recently_judged("/deferred", false, now + PRIORITY_DEFER_MS - 1));
        assert!(!memo.recently_judged("/deferred", false, now + PRIORITY_DEFER_MS));
        assert!(
            !memo.recently_judged("/deferred", true, now),
            "a pull request merging since is judged at once"
        );
        memo.prune(now + PRIORITY_DEFER_MS);
        assert!(!memo.entries.contains_key("/deferred"));
        assert!(memo.entries.contains_key("/selected"));
    }

    /// A merged closed checkout that selection picks but apply keeps (an
    /// artifact-use marker only apply checks) must not hold the removal lane:
    /// the next landed checkout is still removed by the following passes.
    #[cfg(unix)]
    #[test]
    fn a_merged_checkout_kept_at_apply_does_not_block_the_next_removal() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (blocked_session, blocked) = closed_landed_checkout(&mut broker, "kept at apply");
        let (_session, landed) = closed_landed_checkout(&mut broker, "removable next");
        record_merged_pull_request(&mut broker, blocked_session.id, 77);
        let admission = crate::file_lock::artifact_use::begin_artifact_use(
            broker.main_root(),
            &blocked,
            blocked_session.id,
            "apply-only blocker",
        )
        .unwrap();
        let crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer) = admission
        else {
            panic!("artifact-use protocol should be available for this test");
        };
        // The marker stays after the producer handle is dropped.
        drop(producer);
        let root = broker.main_root().to_path_buf();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();
        for _ in 0..3 {
            if !landed.exists() {
                break;
            }
            let _ = broker.run_capacity_removal_slice(Instant::now()).unwrap();
        }
        assert!(!landed.exists(), "the next landed checkout must be removed");
        assert!(blocked.exists(), "the marked checkout stays");
        let memo = super::PriorityMemo::decode(
            broker
                .store()
                .meta_get(super::PRIORITY_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        let key = std::fs::canonicalize(&blocked).unwrap();
        let entry = memo
            .entries
            .get(key.to_string_lossy().as_ref())
            .expect("the selected priority group is remembered");
        assert_eq!(entry.outcome, "selected");
        assert_eq!(entry.apply_code.as_deref(), Some("artifact_use"));
    }

    /// `count` closed spawned checkouts with no checkout cleanup request, and
    /// a complete path index.
    #[cfg(unix)]
    fn closed_priority_fixture(
        count: usize,
    ) -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
        Vec<crate::Session>,
    ) {
        let (owner, storage, mut broker, first) = retirement_fixture();
        let mut sessions = vec![first];
        for index in 1..count {
            sessions.push(
                broker
                    .start_worktree(&format!("priority fixture {index}"), None)
                    .unwrap(),
            );
        }
        for session in &sessions {
            broker
                .close_abandoning_unpushed(session.id, "priority fixture")
                .unwrap();
        }
        broker
            .store()
            .connection()
            .execute("DELETE FROM closed_checkout_cleanup_requests", [])
            .unwrap();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap()
            .complete
        {}
        (owner, storage, broker, sessions)
    }

    #[cfg(unix)]
    #[test]
    fn priority_groups_order_merged_pull_requests_then_due_requests() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(3);
        let (plain, due, merged) = (&sessions[0], &sessions[1], &sessions[2]);
        record_merged_pull_request(&mut broker, merged.id, 41);
        let insert_request = |session_id: i64, not_before_ms: i64| {
            broker
                .store_ref()
                .connection()
                .execute(
                    "INSERT INTO closed_checkout_cleanup_requests (
                         session_id, requested_at_ms, not_before_ms, last_progress_at_ms
                     ) VALUES (?1, 1, ?2, 1)",
                    rusqlite::params![session_id, not_before_ms],
                )
                .unwrap();
        };
        insert_request(due.id, 1);
        insert_request(plain.id, i64::MAX);
        let order = broker
            .store()
            .cleanup_cohort_priority_groups(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                now_ms(),
                i64::MAX,
                16,
            )
            .unwrap()
            .into_iter()
            .map(|(_, first_session_id, merged)| (first_session_id, merged))
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![(merged.id, true), (due.id, false)],
            "a merged pull request first, then a due request; a request still in grace is not a priority"
        );
    }

    #[cfg(unix)]
    #[test]
    fn under_capacity_pressure_a_merged_checkout_is_judged_before_the_lap() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(3);
        let merged = sessions[2].id;
        record_merged_pull_request(&mut broker, merged, 42);
        broker.record_checkout_usage(900, 900);
        let judge = |broker: &mut crate::Broker| {
            let mut order = Vec::new();
            broker
                .auto_cleanup_plan_with_group_inspector(
                    Some(Instant::now() + Duration::from_millis(900)),
                    |_, group, _, _, _| {
                        order.push(group.cursor_session_id);
                        Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                            worktree: group.worktree.to_string_lossy().into_owned(),
                            sessions: group.sessions.iter().map(|session| session.id).collect(),
                            reason: "injected planner inspection".into(),
                            ..Default::default()
                        }))
                    },
                )
                .unwrap();
            order
        };
        let first = judge(&mut broker);
        assert_eq!(first.first(), Some(&merged), "{first:?}");
        assert_eq!(
            first.iter().filter(|id| **id == merged).count(),
            1,
            "the lap does not judge it again in the same pass: {first:?}"
        );
        let memo = super::PriorityMemo::decode(
            broker
                .store()
                .meta_get(super::PRIORITY_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        let merged_path = std::fs::canonicalize(&sessions[2].worktree_path).unwrap();
        assert!(memo.recently_judged(&merged_path.to_string_lossy(), true, now_ms()));
        // Kept, it yields to the rest of the lap instead of being judged
        // first on every pass.
        let second = judge(&mut broker);
        assert_ne!(second.first(), Some(&merged), "{second:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn closed_cohort_accepts_remounted_git_volume_with_spawn_time_uuid() {
        use super::{
            AutoCleanupCheckoutGroup, AutoCleanupVerdictOptions, GateWitnessSnapshot,
            LeaseProcessTableSnapshot,
        };

        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "remount fixture")
            .unwrap();
        let mut saved = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("macOS spawn captures a volume witness");
        assert_eq!(saved.schema_version, 3);
        saved.git_admin_device += 1;
        saved.git_common_device += 1;
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                rusqlite::params![session.id, serde_json::to_string(&saved).unwrap()],
            )
            .unwrap();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap()
            .complete
        {}
        let path = PathBuf::from(&session.worktree_path);
        let cohort = broker
            .closed_artifact_checkout_cohort_for_session(session.id, &path, None)
            .unwrap()
            .expect("stable saved UUID permits a changed mount device number");
        assert_eq!(cohort.owner_session_id(), session.id);
        let checkout = GitRepo::discover(&path).unwrap();
        assert!(
            cohort.matches_checkout_instance(&checkout),
            "final mutation guard accepts the same stable instance"
        );

        std::fs::write(
            broker.main_root().join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let group = AutoCleanupCheckoutGroup {
            worktree: path.clone(),
            sessions: vec![closed],
            complete: true,
            cursor_session_id: session.id,
        };
        let candidate = match broker
            .auto_cleanup_candidate_for_group(&group, &[], &[], None)
            .unwrap()
        {
            AutoCleanupGroupInspection::Candidate(candidate) => candidate,
            _ => panic!("same stable instance must remain a cleanup candidate"),
        };
        assert!(
            candidate
                .spawned_instance_witness
                .as_ref()
                .unwrap()
                .matches_current_instance(&checkout.spawned_worktree_instance_witness().unwrap())
        );
        let head = checkout.head_commit().unwrap();
        let upstream = ("main".into(), head.clone());
        let proof = ContainmentProof {
            head: head.clone(),
            containing_commit: head,
            proof: "fixture unchanged HEAD".into(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        };
        let mut lease_process_table = LeaseProcessTableSnapshot::default();
        let verdict = broker.auto_cleanup_verdict(
            &candidate,
            &AutoCleanupPolicy::default(),
            &upstream,
            AutoCleanupVerdictOptions {
                deep: false,
                deadline: None,
                selected: Some(&proof),
                lease_process_table: &mut lease_process_table,
                gate_witnesses: &mut GateWitnessSnapshot::default(),
                live_index: None,
            },
        );
        assert!(
            verdict.is_ok(),
            "final ownership proof must accept remount: {verdict:?}"
        );

        saved.volume_uuids.as_mut().unwrap().git_common = [0x7f; 16];
        connection
            .execute(
                "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                rusqlite::params![session.id, serde_json::to_string(&saved).unwrap()],
            )
            .unwrap();
        assert!(
            broker
                .closed_artifact_checkout_cohort_for_session(session.id, &path, None)
                .unwrap()
                .is_none(),
            "another volume must remain protected"
        );
        saved.schema_version = 2;
        saved.volume_uuids = None;
        connection
            .execute(
                "UPDATE spawned_worktree_instances SET witness_json = ?2 WHERE session_id = ?1",
                rusqlite::params![session.id, serde_json::to_string(&saved).unwrap()],
            )
            .unwrap();
        assert!(
            broker
                .closed_artifact_checkout_cohort_for_session(session.id, &path, None)
                .unwrap()
                .is_none(),
            "legacy device drift cannot be upgraded from current observations"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_raw_checkout_path_retargeted_to_candidate_without_db_change() {
        use std::os::unix::fs::symlink;

        let (_owner, _storage, mut broker, session) = retirement_fixture();
        let candidate_path = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "filesystem alias fixture")
            .unwrap();
        let unrelated_path = broker
            .main_root()
            .parent()
            .unwrap()
            .join("ordinary-checkout");
        std::fs::create_dir_all(&unrelated_path).unwrap();
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "INSERT INTO sessions (
                     worktree_path, branch, origin, status, cleanup_state, closed_at,
                     created_at, updated_at, last_activity_at
                 ) VALUES (?1, 'agent/unrelated', 'adopted', 'cleaned', 'closed', 1, 1, 1, 1)",
                [unrelated_path.to_string_lossy().as_ref()],
            )
            .unwrap();
        let unrelated_id = connection.last_insert_rowid();
        let initial = broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap();
        assert!(initial.complete);
        let indexed_identity = broker
            .store()
            .cleanup_cohort_indexed_rows_page(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                0,
                32,
            )
            .unwrap();
        let unrelated_text = unrelated_path.to_string_lossy();
        assert!(indexed_identity.iter().any(|row| {
            row.raw_path.as_str() == unrelated_text.as_ref()
                && row.canonical_path.as_str() == unrelated_text.as_ref()
        }));

        let moved_path = unrelated_path.with_file_name("ordinary-checkout-moved");
        std::fs::rename(&unrelated_path, &moved_path).unwrap();
        symlink(&candidate_path, &unrelated_path).unwrap();

        let owner = broker.store().session(session.id).unwrap();
        let witness = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap();
        let candidate = AutoCleanupCandidate {
            worktree: candidate_path.clone(),
            spawned_owner: owner.clone(),
            spawned_instance_witness: witness,
            sessions: vec![owner],
        };
        let plan = AutoCleanupPlan {
            report: AutoCleanupReport {
                enabled: true,
                ..AutoCleanupReport::default()
            },
            policy: AutoCleanupPolicy::default(),
            upstream: Some(("origin/main".into(), "fixture-head".into())),
            selected: vec![(
                candidate,
                ContainmentProof {
                    head: "fixture-head".into(),
                    containing_commit: "fixture-head".into(),
                    proof: "test proof, never reached".into(),
                    interrupted_removal_deletions: 0,
                    ownership_reproof: None,
                },
            )],
            deadline: None,
            alias_refresh_generation: Some(initial.alias_refresh_generation),
            bound_revalidation: false,
        };
        let report = broker.auto_cleanup_apply(plan).unwrap();
        assert_eq!(report.removed.len(), 0);
        assert_eq!(report.deferred, 1);
        assert!(
            candidate_path.is_dir(),
            "unexpected alias must keep the checkout"
        );
        assert!(
            report
                .kept
                .iter()
                .any(|kept| kept.reason.contains("registered checkout paths changed")),
            "the apply report should explain why the selected checkout was retained"
        );
        let invalidated = broker
            .store()
            .cleanup_cohort_scan_state(crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE)
            .unwrap()
            .unwrap();
        assert!(!invalidated.complete);
        assert_eq!(
            invalidated.pending_dirty_rows, 1,
            "a real filesystem mismatch must durably dirty the affected row so a later pass can converge"
        );

        let refreshed = broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap();
        assert!(refreshed.complete);
        assert_eq!(refreshed.generation, initial.generation);
        let refreshed_alias = broker
            .store()
            .cleanup_cohort_indexed_rows_page(
                crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                unrelated_id - 1,
                1,
            )
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(refreshed_alias.session_id, unrelated_id);
        assert_eq!(
            refreshed_alias.canonical_path,
            candidate_path.to_string_lossy()
        );
        let current_cohort = broker
            .closed_artifact_checkout_cohort_for_session(session.id, &candidate_path, None)
            .unwrap()
            .unwrap();
        let mut current_ids = current_cohort.session_ids();
        current_ids.sort_unstable();
        assert_eq!(current_ids, vec![session.id, unrelated_id]);
    }

    #[cfg(unix)]
    #[test]
    fn checked_worktree_activity_uses_the_gitdir_pointer_not_checkout_basename() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        let original = PathBuf::from(&session.worktree_path);
        let renamed = original.with_file_name("checkout-with-different-name");
        let original_git_dir = crate::git::checked_worktree_git_dir(&original)
            .unwrap()
            .expect("the spawned checkout has a gitdir pointer");
        std::fs::rename(&original, &renamed).unwrap();
        let connection = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        connection
            .execute(
                "UPDATE sessions SET worktree_path = ?1 WHERE id = ?2",
                rusqlite::params![renamed.to_string_lossy(), session.id],
            )
            .unwrap();

        let current = broker.store().session(session.id).unwrap();
        let resolved_git_dir = crate::git::checked_worktree_git_dir(&renamed)
            .unwrap()
            .unwrap();
        assert_eq!(resolved_git_dir, original_git_dir);
        assert_ne!(
            resolved_git_dir.file_name(),
            renamed.file_name(),
            "the admin directory can have a name independent of the checkout"
        );
        assert!(
            crate::broker::checked_worktree_activity_ms(broker.main_root(), &current)
                .unwrap()
                .is_some()
        );
    }

    #[cfg(unix)]
    fn age_open_session(broker: &mut crate::Broker, session: &crate::Session) {
        let db_path = broker.store().db_path().to_path_buf();
        let connection = rusqlite::Connection::open(db_path).unwrap();
        let old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            - 80 * 60 * 60 * 1_000;
        let missing_worktree = PathBuf::from(&session.worktree_path)
            .with_file_name(format!("missing-retirement-worktree-{}", session.id))
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1,
                 last_activity_at = ?1, worktree_path = ?2 WHERE id = ?3",
                rusqlite::params![old, missing_worktree, session.id],
            )
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automatic_cleanup_retains_dirty_submodule_files_under_ignore_all_configuration() {
        for untracked in [false, true] {
            let case = if untracked { "untracked" } else { "modified" };
            let owner = tempfile::tempdir().unwrap();
            let root = owner.path().join("repository");
            let source = owner.path().join("submodule-source");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&source).unwrap();

            git(&source, &["init", "-q", "-b", "main"]);
            git(&source, &["config", "user.name", "Auto cleanup test"]);
            git(
                &source,
                &["config", "user.email", "auto-cleanup@example.invalid"],
            );
            std::fs::write(source.join("tracked.txt"), "initial child content\n").unwrap();
            git(&source, &["add", "tracked.txt"]);
            git(&source, &["commit", "-qm", "initial child commit"]);

            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["config", "user.name", "Auto cleanup test"]);
            git(
                &root,
                &["config", "user.email", "auto-cleanup@example.invalid"],
            );
            std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
            std::fs::write(root.join("README.md"), "fixture\n").unwrap();
            git(&root, &["add", ".gitignore", "README.md"]);
            git(&root, &["commit", "-qm", "superproject base"]);
            let source_path = source.to_str().unwrap();
            git(
                &root,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    source_path,
                    "sub",
                ],
            );
            git(
                &root,
                &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
            );
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-qm", "add child submodule"]);
            git(&root, &["config", "diff.ignoreSubmodules", "all"]);
            git(&root, &["config", "submodule.sub.ignore", "all"]);
            git(&root, &["remote", "add", "origin", "."]);
            git(
                &root,
                &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
            );
            git(&root, &["remote", "set-head", "origin", "main"]);

            std::fs::create_dir_all(root.join(".aethyme")).unwrap();
            std::fs::write(
                root.join(".aethyme/config.toml"),
                "[cleanup]\nauto_remove = true\n",
            )
            .unwrap();
            std::fs::write(
                root.join(".aethyme/broker.toml"),
                "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
            )
            .unwrap();

            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&root).unwrap();
            let session = broker
                .start_worktree("dirty child submodule", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path);
            assert_eq!(
                GitRepo::discover(worktree).unwrap().head_commit().unwrap(),
                GitRepo::discover(&root).unwrap().head_commit().unwrap(),
                "the superproject commit is already the parent checkout HEAD"
            );
            git(
                worktree,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--quiet",
                    "--",
                    "sub",
                ],
            );
            broker
                .close_abandoning_unpushed(session.id, "automatic cleanup regression")
                .unwrap();

            let child = worktree.join("sub");
            let dirty_path = if untracked {
                let path = child.join("new.txt");
                std::fs::write(&path, "new child content\n").unwrap();
                path
            } else {
                let path = child.join("tracked.txt");
                std::fs::write(&path, "changed child content\n").unwrap();
                path
            };

            let expected_path = worktree.to_string_lossy().into_owned();
            let report = broker.auto_remove_disposable_checkouts(None).unwrap();
            assert!(report.removed.is_empty(), "{case}");
            assert!(
                report.kept.iter().any(|kept| {
                    kept.worktree == expected_path
                        && kept.reason.contains("uncommitted or untracked path(s)")
                }),
                "{case}: {:?}",
                report.kept
            );
            assert!(worktree.is_dir(), "{case} worktree was removed");
            assert!(dirty_path.is_file(), "{case} child data was removed");
        }
    }

    #[cfg(unix)]
    #[test]
    fn planner_resumes_wraps_and_rechecks_changed_checkout_eligibility() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
        std::fs::write(root.join("README.md"), "fixture\n").unwrap();
        git(&root, &["add", ".gitignore", "README.md"]);
        git(&root, &["commit", "-qm", "fixture"]);
        git(&root, &["remote", "add", "origin", "."]);
        git(
            &root,
            &["fetch", "-q", "origin", "main:refs/remotes/origin/main"],
        );
        git(&root, &["remote", "set-head", "origin", "main"]);

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nauto_remove = true\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nclosed_worktrees_days = 1\nclosed_worktree_grace_hours = 0\nauto_cleanup_worktrees_on_finish = false\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\nartifact_sweep_interval_hours = 1\n",
        )
        .unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (first_id, first_path) = closed_dirty_checkout(&mut broker, "cursor first candidate");
        let (second_id, second_path) =
            closed_dirty_checkout(&mut broker, "cursor second candidate");
        assert!(first_id < second_id);
        assert!(first_path.is_dir());
        assert!(second_path.is_dir());
        for session_id in [first_id, second_id] {
            assert!(
                broker
                    .store()
                    .spawned_worktree_instance_witness(session_id)
                    .unwrap()
                    .is_some(),
                "Unix fixture sessions must carry valid spawn-time worktree witnesses"
            );
        }

        let alias_path = root.join("first-checkout-alias");
        std::os::unix::fs::symlink(&first_path, &alias_path).unwrap();
        let first_record = broker.store().session(first_id).unwrap();
        let mut aliased_second_record = broker.store().session(second_id).unwrap();
        aliased_second_record.worktree_path = alias_path.to_string_lossy().into_owned();
        let groups =
            group_closed_checkout_sessions(vec![first_record, aliased_second_record], None)
                .unwrap();
        assert_eq!(groups.len(), 1, "canonical aliases share a complete group");
        let mut grouped_ids = groups[0]
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        grouped_ids.sort_unstable();
        assert_eq!(
            grouped_ids,
            vec![first_id, second_id],
            "all retained rows in a canonical alias cohort stay together"
        );

        let seed = AutoCleanupCursor {
            version: super::AUTO_CLEANUP_CURSOR_VERSION,
            cohort_generation: 1,
            after_session_id: first_id,
            high_water_session_id: second_id,
        };
        let seed_json = serde_json::to_string(&seed).unwrap();
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &seed_json)
            .unwrap();

        let expired_deadline = Instant::now() - Duration::from_millis(1);
        let expired_pass = broker.auto_cleanup_plan(Some(expired_deadline)).unwrap();
        assert!(expired_pass.selected_worktrees().is_empty());
        assert!(expired_pass.report.kept.is_empty());
        assert_eq!(expired_pass.report.deferred, 1);
        assert_eq!(
            broker
                .store()
                .meta_get(AUTO_CLEANUP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
            Some(seed_json.as_str()),
            "an incomplete discovery pass does not advance the cursor"
        );

        let mut inspected_groups = Vec::new();
        let inspected_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                inspected_groups.push(
                    group
                        .sessions
                        .iter()
                        .map(|session| session.id)
                        .collect::<Vec<_>>(),
                );
                Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                    worktree: group.worktree.to_string_lossy().into_owned(),
                    sessions: group.sessions.iter().map(|session| session.id).collect(),
                    reason: "injected planner inspection".into(),
                    ..Default::default()
                }))
            })
            .unwrap();
        assert_eq!(
            inspected_groups,
            vec![vec![second_id]],
            "only the cursor-selected group receives per-group proof work"
        );
        assert_eq!(inspected_pass.report.deferred, 0);
        let persisted = broker
            .store()
            .meta_get(AUTO_CLEANUP_CURSOR_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_auto_cleanup_cursor(Some(&persisted)).after_session_id,
            second_id,
            "the selected group is checkpointed after its inspection"
        );
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &seed_json)
            .unwrap();

        let first_pass = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(
            first_pass
                .report
                .kept
                .iter()
                .map(|kept| kept.sessions.clone())
                .collect::<Vec<_>>(),
            vec![vec![second_id]],
            "the persisted cursor resumes after the first candidate"
        );

        let wrapped_pass = broker.auto_cleanup_plan(None).unwrap();
        let mut wrapped_ids = wrapped_pass
            .report
            .kept
            .iter()
            .flat_map(|kept| kept.sessions.iter().copied())
            .collect::<Vec<_>>();
        wrapped_ids.sort_unstable();
        assert_eq!(
            wrapped_ids,
            vec![first_id, second_id],
            "finishing a lap wraps around and revisits earlier candidates"
        );

        std::fs::remove_file(first_path.join("cursor-dirty.txt")).unwrap();
        let changed_eligibility_pass = broker.auto_cleanup_plan(None).unwrap();
        let selected = changed_eligibility_pass.selected_worktrees();
        assert!(
            selected.contains(&first_path.to_string_lossy().into_owned()),
            "a candidate that becomes clean is checked again on a later lap"
        );
        assert!(
            !selected.contains(&second_path.to_string_lossy().into_owned()),
            "the still-dirty candidate remains protected"
        );

        let (third_id, _) = closed_dirty_checkout(&mut broker, "cursor third candidate");
        assert!(second_id < third_id);
        let deferred_cursor = AutoCleanupCursor {
            version: super::AUTO_CLEANUP_CURSOR_VERSION,
            cohort_generation: 1,
            after_session_id: first_id,
            high_water_session_id: third_id,
        };
        let deferred_cursor_json = serde_json::to_string(&deferred_cursor).unwrap();
        broker
            .store()
            .meta_set(AUTO_CLEANUP_CURSOR_KEY, &deferred_cursor_json)
            .unwrap();

        let mut deferred_inspections = Vec::new();
        let deferred_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                let session_id = group.sessions[0].id;
                deferred_inspections.push(session_id);
                if session_id == second_id {
                    Ok(AutoCleanupGroupInspection::Deferred)
                } else {
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: vec![session_id],
                        reason: "injected resumed inspection".into(),
                        ..Default::default()
                    }))
                }
            })
            .unwrap();
        assert_eq!(deferred_inspections, vec![second_id]);
        assert_eq!(deferred_pass.report.deferred, 2);
        let checkpoint = broker
            .store()
            .meta_get(AUTO_CLEANUP_CURSOR_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_auto_cleanup_cursor(Some(&checkpoint)).after_session_id,
            second_id,
            "a deferred selected group is checkpointed before the tranche stops"
        );

        let mut resumed_inspections = Vec::new();
        let resumed_pass = broker
            .auto_cleanup_plan_with_group_inspector(None, |_, group, _, _, _| {
                let session_id = group.sessions[0].id;
                resumed_inspections.push(session_id);
                Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                    worktree: group.worktree.to_string_lossy().into_owned(),
                    sessions: vec![session_id],
                    reason: "injected resumed inspection".into(),
                    ..Default::default()
                }))
            })
            .unwrap();
        assert_eq!(
            resumed_inspections,
            vec![third_id],
            "a fresh pass resumes at the next uninspected complete group"
        );
        assert_eq!(resumed_pass.report.deferred, 0);

        broker
            .store()
            .set_session_status(first_id, crate::types::SessionStatus::Active, None)
            .unwrap();
        let reactivated_pass = broker.auto_cleanup_plan(None).unwrap();
        assert!(
            !reactivated_pass
                .selected_worktrees()
                .contains(&first_path.to_string_lossy().into_owned()),
            "a reactivated live owner makes its previously closed checkout ineligible"
        );
        assert!(
            first_path.is_dir(),
            "reactivation must preserve the checkout"
        );
    }

    #[test]
    fn cleanup_cursor_bounds_laps_and_revisits_new_arrivals() {
        let mut cursor = AutoCleanupCursor::default();
        let mut first_lap = vec![1_i64, 2, 3];
        let range = auto_cleanup_lap_range(&mut first_lap, &mut cursor, |id| *id);
        assert_eq!(&first_lap[range], &[1, 2, 3]);
        assert_eq!(cursor.high_water_session_id, 3);

        cursor.after_session_id = 2;
        let mut with_arrivals = vec![1_i64, 2, 3, 4, 5];
        let range = auto_cleanup_lap_range(&mut with_arrivals, &mut cursor, |id| *id);
        assert_eq!(&with_arrivals[range], &[3]);
        assert_eq!(cursor.high_water_session_id, 3);

        // Once the lap reaches its captured high-water mark, later IDs join
        // the next lap instead of extending this one.
        cursor.after_session_id = 3;
        let mut next_lap = vec![1_i64, 2, 3, 4, 5];
        let range = auto_cleanup_lap_range(&mut next_lap, &mut cursor, |id| *id);
        assert_eq!(&next_lap[range], &[1, 2, 3, 4, 5]);
        assert_eq!(cursor.after_session_id, 0);
        assert_eq!(cursor.high_water_session_id, 5);
    }

    #[test]
    fn unknown_session_retirement_cursor_version_resets_scheduling_state() {
        let cursor = decode_session_retirement_cursor(Some(
            r#"{"version":9,"after_session_id":7,"high_water_session_id":12}"#,
        ));
        assert_eq!(cursor, SessionRetirementCursor::default());

        let checkpoint = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 7,
            high_water_session_id: 12,
        };
        let encoded = serde_json::to_string(&checkpoint).unwrap();
        assert_eq!(decode_session_retirement_cursor(Some(&encoded)), checkpoint);
    }

    #[test]
    fn session_retirement_report_fields_are_optional_for_legacy_json() {
        let legacy = r#"{"enabled":true,"config_error":null,"evaluated_at_ms":1,"removed":[],"kept":[],"deferred":0}"#;
        let report: super::AutoCleanupReport = serde_json::from_str(legacy).unwrap();
        assert_eq!(report.sessions_retired, 0);
        assert!(report.retirement_deferred_reasons.is_empty());
        let serialized = serde_json::to_value(report).unwrap();
        assert!(serialized.get("sessions_retired").is_none());
        assert!(serialized.get("retirement_deferred_reasons").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn open_session_pages_advance_by_id_without_materializing_the_inventory() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let first = broker.start_worktree("retirement page one", None).unwrap();
        let second = broker.start_worktree("retirement page two", None).unwrap();
        let third = broker
            .start_worktree("retirement page three", None)
            .unwrap();
        let high_water = broker.store().open_session_max_id().unwrap();

        let first_page = broker.store().open_session_page(0, high_water, 1).unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].id, first.id);
        let second_page = broker
            .store()
            .open_session_page(first_page[0].id, high_water, 1)
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].id, second.id);
        let tail = broker
            .store()
            .open_session_page(second_page[0].id, high_water, 128)
            .unwrap();
        assert_eq!(
            tail.iter().map(|session| session.id).collect::<Vec<_>>(),
            vec![third.id]
        );
        assert!(
            broker
                .store()
                .open_session_page(third.id, high_water, 1)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn landed_retirement_sweep_keeps_dirty_work_and_honors_opt_out() {
        for (dirty, disabled) in [(false, false), (true, false), (false, true)] {
            let (_owner, _storage, mut broker, session) = retirement_fixture();
            let root = broker.main_root().to_path_buf();
            let checkout = PathBuf::from(&session.worktree_path);
            let head = GitRepo::discover(&checkout).unwrap().head_commit().unwrap();
            git(&root, &["update-ref", "refs/remotes/origin/main", &head]);
            git(
                &root,
                &[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ],
            );
            broker
                .store()
                .record_session_representation(&crate::types::NewSessionRepresentation {
                    session_id: session.id,
                    session_head: head.clone(),
                    representing_commit: Some(head),
                    representing_ref: "refs/remotes/origin/main".into(),
                    discovery: crate::types::RepresentationDiscovery::MergeTime,
                    pr_number: None,
                    paths_json: "[]".into(),
                    evidence: "fixture".into(),
                })
                .unwrap();
            let holder = crate::session_holder::AgentProcess {
                pid: i64::MAX,
                started: "Mon Jan 1 00:00:00 2024".into(),
                command: "fixture-agent".into(),
            };
            broker
                .store()
                .append_event(
                    crate::events::SESSION_HOLDER_BOUND,
                    Some(session.id),
                    Some(&crate::events::session_holder_bound_payload(
                        &holder, "fixture", None,
                    )),
                )
                .unwrap();
            if disabled {
                std::fs::write(root.join(".aethyme/broker.toml"), "[retention]\nsession_abandoned_after_hours = 72\nsession_landed_after_hours = 0\n").unwrap();
            }
            if dirty {
                std::fs::write(checkout.join("untracked-work"), "preserve").unwrap();
            }
            let old_time = std::time::SystemTime::now() - Duration::from_secs(30 * 60 * 60);
            let old_ms = old_time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            let git_dir = crate::git::checked_worktree_git_dir(&checkout)
                .unwrap()
                .unwrap();
            for name in ["index", "HEAD"] {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(git_dir.join(name))
                    .unwrap()
                    .set_modified(old_time)
                    .unwrap();
            }
            broker.store().connection().execute("UPDATE sessions SET created_at = ?1, updated_at = ?1, last_activity_at = ?1 WHERE id = ?2", rusqlite::params![old_ms, session.id]).unwrap();
            let summary = broker.retire_abandoned_sessions(None).unwrap();
            assert_eq!(summary.retired, usize::from(!dirty && !disabled));
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state
                    == SessionCleanupState::Closed,
                !dirty && !disabled
            );
            if dirty {
                assert!(checkout.join("untracked-work").exists());
            }
        }
    }

    /// A repository whose default branch is on a local `origin`, with the
    /// default 24-hour landed window, a 72-hour abandonment window and no
    /// closed-checkout grace, so one pass can close and remove.
    #[cfg(unix)]
    fn landed_lane_fixture(
        cleanup: Option<&str>,
    ) -> (
        tempfile::TempDir,
        crate::test_storage::TestStorage,
        crate::Broker,
    ) {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git(&root, &["add", ".gitignore", "base.txt"]);
        git(&root, &["commit", "-qm", "fixture"]);
        aethyme_testkit::ensure_origin(&root);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        if let Some(cleanup) = cleanup {
            std::fs::write(root.join(".aethyme/config.toml"), cleanup).unwrap();
        }
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        (owner, storage, broker)
    }

    /// Start a session and commit one file in it; land it by pull request
    /// with `style`, or leave it unlanded.
    #[cfg(unix)]
    fn landed_lane_session(
        broker: &mut crate::Broker,
        label: &str,
        style: Option<aethyme_testkit::MergeStyle>,
    ) -> (crate::Session, PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let file = format!("work-{}.txt", session.id);
        std::fs::write(worktree.join(&file), format!("{label}\n")).unwrap();
        git(&worktree, &["add", &file]);
        git(&worktree, &["commit", "-qm", label]);
        if let Some(style) = style {
            let root = broker.main_root().to_path_buf();
            aethyme_testkit::land_as_pull_request(&root, &session.branch, style);
            if style == aethyme_testkit::MergeStyle::Squash {
                aethyme_testkit::delete_remote_branch(&root, &session.branch);
            }
        }
        (session, worktree)
    }

    /// Bind a holder process that no longer exists and push the session's
    /// recorded and Git-visible activity `hours` into the past.
    #[cfg(unix)]
    fn quiet_with_gone_holder(broker: &mut crate::Broker, session: &crate::Session, hours: u64) {
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder, "fixture", None,
                )),
            )
            .unwrap();
        let old_time = std::time::SystemTime::now() - Duration::from_secs(hours * 60 * 60);
        let old_ms = old_time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let checkout = PathBuf::from(&session.worktree_path);
        let git_dir = crate::git::checked_worktree_git_dir(&checkout)
            .unwrap()
            .unwrap();
        for name in ["index", "HEAD"] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(git_dir.join(name))
                .unwrap()
                .set_modified(old_time)
                .unwrap();
        }
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1, last_activity_at = ?1 WHERE id = ?2",
                rusqlite::params![old_ms, session.id],
            )
            .unwrap();
    }

    #[cfg(unix)]
    fn record_merged_pull_request(broker: &mut crate::Broker, session_id: i64, pr_number: i64) {
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', ?1, ?2, 1, 'watch')",
                rusqlite::params![pr_number, session_id],
            )
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_milestones (repository, pr_number, merged_at, first_seen_at, updated_at) VALUES ('fixture', ?1, 1, 1, 1)",
                [pr_number],
            )
            .unwrap();
    }

    /// The busiest repository's backlog: a session whose pull request merged
    /// and whose agent moved on is closed and its checkout removed by
    /// unattended maintenance, by merge commit and by squash, with no
    /// `finish`.
    #[cfg(unix)]
    #[test]
    fn maintenance_closes_and_removes_a_landed_session_without_finish() {
        for style in [
            aethyme_testkit::MergeStyle::MergeCommit,
            aethyme_testkit::MergeStyle::Squash,
        ] {
            let (_owner, _storage, mut broker) = landed_lane_fixture(None);
            let (session, worktree) =
                landed_lane_session(&mut broker, "landed by pull request", Some(style));
            record_merged_pull_request(&mut broker, session.id, 11);
            // Past the landed window, well inside the abandonment window.
            quiet_with_gone_holder(&mut broker, &session, 30);

            let report = broker.auto_cleanup_now(None).unwrap().unwrap();
            assert_eq!(report.landed_sessions_examined, 1, "{style:?}: {report:#?}");
            assert_eq!(report.landed_sessions_closed, 1, "{style:?}: {report:#?}");
            assert_eq!(report.sessions_retired, 1, "{style:?}: {report:#?}");
            assert!(report.landed_sessions_declined.is_empty(), "{report:#?}");
            assert_ne!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Open,
                "{style:?}"
            );
            // Removal is the existing whole-checkout lane; a later pass may be
            // the one that reaches the newly closed checkout.
            for _ in 0..2 {
                if !worktree.exists() {
                    break;
                }
                broker.auto_cleanup_now(None).unwrap().unwrap();
            }
            assert!(
                !worktree.exists(),
                "{style:?}: the landed checkout is removed without finish"
            );
        }
    }

    /// Every proof the landed lane cannot make keeps the session and its
    /// checkout, and is counted under its own reason.
    #[cfg(unix)]
    #[test]
    fn maintenance_keeps_landed_lane_sessions_it_cannot_prove_and_says_why() {
        type Case = (
            &'static str,
            Option<&'static str>,
            fn(&mut crate::Broker) -> (crate::Session, PathBuf),
        );
        fn dirty(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            let (session, worktree) = landed_lane_session(
                broker,
                "dirty after landing",
                Some(aethyme_testkit::MergeStyle::MergeCommit),
            );
            std::fs::write(worktree.join("unsaved.txt"), "keep me\n").unwrap();
            (session, worktree)
        }
        fn unmerged(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            let (session, worktree) = landed_lane_session(
                broker,
                "open pull request",
                Some(aethyme_testkit::MergeStyle::MergeCommit),
            );
            broker
                .store()
                .connection()
                .execute(
                    "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', 21, ?1, 1, 'watch')",
                    [session.id],
                )
                .unwrap();
            (session, worktree)
        }
        fn never_landed(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            landed_lane_session(broker, "never landed", None)
        }
        fn follow_up_after_squash(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            let (session, worktree) = landed_lane_session(
                broker,
                "squashed then continued",
                Some(aethyme_testkit::MergeStyle::Squash),
            );
            record_merged_pull_request(broker, session.id, 31);
            std::fs::write(worktree.join("follow-up.txt"), "only here\n").unwrap();
            git(&worktree, &["add", "follow-up.txt"]);
            git(&worktree, &["commit", "-qm", "follow-up after the squash"]);
            (session, worktree)
        }
        fn landed(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            landed_lane_session(
                broker,
                "landed but removal disabled",
                Some(aethyme_testkit::MergeStyle::MergeCommit),
            )
        }
        fn spawned_process_alive(broker: &mut crate::Broker) -> (crate::Session, PathBuf) {
            let (session, worktree) = landed_lane_session(
                broker,
                "landed with its process running",
                Some(aethyme_testkit::MergeStyle::MergeCommit),
            );
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET pid = ?1 WHERE id = ?2",
                    rusqlite::params![i64::from(std::process::id()), session.id],
                )
                .unwrap();
            (session, worktree)
        }
        let cases: [Case; 6] = [
            ("worktree_unclean", None, dirty),
            ("pull_request_unmerged", None, unmerged),
            ("no_landing_evidence", None, never_landed),
            ("not_landed", None, follow_up_after_squash),
            (
                "auto_remove_disabled",
                Some("[cleanup]\nauto_remove = false\n"),
                landed,
            ),
            ("process_alive", None, spawned_process_alive),
        ];
        for (reason, cleanup, make) in cases {
            let (_owner, _storage, mut broker) = landed_lane_fixture(cleanup);
            let (session, worktree) = make(&mut broker);
            quiet_with_gone_holder(&mut broker, &session, 30);

            let report = broker.auto_cleanup_now(None).unwrap().unwrap();

            assert_eq!(report.landed_sessions_examined, 1, "{reason}: {report:#?}");
            assert_eq!(report.landed_sessions_closed, 0, "{reason}: {report:#?}");
            assert_eq!(
                report.landed_sessions_declined.get(reason),
                Some(&1),
                "{reason}: {report:#?}"
            );
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Open,
                "{reason}"
            );
            assert!(worktree.exists(), "{reason}: the checkout is kept");
        }
    }

    /// A tool cache Git ignores is the repository's data until `[cleanup]
    /// regenerable` names it: unlisted it keeps the landed session, listed
    /// it is disposable output and the session closes.
    #[cfg(unix)]
    #[test]
    fn a_configured_regenerable_directory_does_not_keep_a_landed_session() {
        for (cleanup, closes) in [
            (None, false),
            (Some("[cleanup]\nregenerable = [\"tool-cache\"]\n"), true),
        ] {
            let (_owner, _storage, mut broker) = landed_lane_fixture(cleanup);
            let root = broker.main_root().to_path_buf();
            std::fs::write(root.join(".git/info/exclude"), "tool-cache/\n").unwrap();
            let (session, worktree) = landed_lane_session(
                &mut broker,
                "landed with a tool cache",
                Some(aethyme_testkit::MergeStyle::MergeCommit),
            );
            std::fs::create_dir_all(worktree.join("tool-cache")).unwrap();
            std::fs::write(worktree.join("tool-cache/entry.bin"), "cached\n").unwrap();
            quiet_with_gone_holder(&mut broker, &session, 30);

            let report = broker.auto_cleanup_now(None).unwrap().unwrap();

            assert_eq!(
                report.landed_sessions_closed,
                usize::from(closes),
                "{report:#?}"
            );
            if !closes {
                assert_eq!(
                    report.landed_sessions_declined.get("worktree_unclean"),
                    Some(&1),
                    "{report:#?}"
                );
                assert!(worktree.join("tool-cache/entry.bin").exists());
            }
        }
    }

    /// A closed session whose checkout holds no commit of its own: contained
    /// in the fetched default branch by ancestry and past its (zero) grace.
    #[cfg(unix)]
    fn closed_landed_checkout(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, PathBuf) {
        // Containment is proved against the fetched `origin/HEAD`.
        let root = broker.main_root().to_path_buf();
        git(&root, &["remote", "set-head", "origin", "main"]);
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        broker
            .close_abandoning_unpushed(session.id, "closed landed checkout fixture")
            .unwrap();
        (session, worktree)
    }

    /// The busiest repository's starvation: a retirement lap that uses every
    /// millisecond it is given no longer leaves the removal lane nothing.
    /// Before the lanes had shares, retirement ran to the pass deadline and
    /// the planner deferred with `removed: []` and `kept: []`.
    #[cfg(unix)]
    #[test]
    fn removal_keeps_its_share_when_retirement_exhausts_its_own() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) =
            closed_landed_checkout(&mut broker, "retirement exhausts its share");
        super::RETIREMENT_EXHAUSTS_ITS_SHARE.with(|flag| flag.set(true));
        let report =
            broker.auto_remove_disposable_checkouts(Some(Instant::now() + Duration::from_secs(30)));
        super::RETIREMENT_EXHAUSTS_ITS_SHARE.with(|flag| flag.set(false));
        let report = report.unwrap();
        assert!(
            report
                .retirement_deferred_reasons
                .iter()
                .any(|reason| matches!(
                    reason,
                    SessionRetirementDeferredReason::DeadlineExceeded { .. }
                )),
            "{report:#?}"
        );
        let lanes = report.lanes.expect("a maintenance pass reports its lanes");
        assert!(!lanes.capacity_pressure, "{lanes:?}");
        assert!(
            (14_000..=15_000).contains(&lanes.retirement_budget_ms),
            "retirement gets half of the pass: {lanes:?}"
        );
        assert!(lanes.removal_budget_ms > 0, "{lanes:?}");
        assert!(lanes.removal_examined >= 1, "{lanes:?}");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!worktree.exists(), "the closed landed checkout is removed");
    }

    /// A selection that runs to its deadline still leaves the apply step
    /// time: before the removal lane held part of its budget back, apply's
    /// path verification found the deadline already expired and the pass
    /// deferred every checkout it had selected, removing none.
    #[cfg(unix)]
    #[test]
    fn a_selection_that_uses_its_whole_share_still_removes() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) =
            closed_landed_checkout(&mut broker, "selection exhausts its share");
        super::SELECTION_EXHAUSTS_ITS_SHARE.with(|flag| flag.set(true));
        let report =
            broker.auto_remove_disposable_checkouts(Some(Instant::now() + Duration::from_secs(8)));
        super::SELECTION_EXHAUSTS_ITS_SHARE.with(|flag| flag.set(false));
        let report = report.unwrap();
        let lanes = report.lanes.expect("a maintenance pass reports its lanes");
        assert_eq!(lanes.removal_selected, 1, "{report:#?}");
        assert!(lanes.removal_apply_reserve_ms > 0, "{lanes:?}");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!worktree.exists(), "the selected checkout is removed");
    }

    /// The HEAD of a checkout, for a removal-attempt record.
    #[cfg(unix)]
    fn checkout_head_of(worktree: &Path) -> String {
        GitRepo::discover(worktree).unwrap().head_commit().unwrap()
    }

    /// Kept codes the removal lane reports for `worktree` over up to three
    /// passes, and whether it is gone afterwards.
    #[cfg(unix)]
    fn kept_codes_over_passes(
        broker: &mut crate::Broker,
        worktree: &Path,
    ) -> (Vec<String>, Vec<super::AutoRemovedCheckout>) {
        let text = std::fs::canonicalize(worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut codes = Vec::new();
        let mut removed = Vec::new();
        for _ in 0..3 {
            let report = broker.auto_cleanup_now(None).unwrap().unwrap();
            removed.extend(
                report
                    .removed
                    .iter()
                    .filter(|r| r.worktree == text)
                    .cloned(),
            );
            codes.extend(
                report
                    .kept
                    .iter()
                    .filter(|kept| kept.worktree == text)
                    .map(|kept| kept.code.clone()),
            );
            if !worktree.exists() || !codes.is_empty() {
                break;
            }
        }
        (codes, removed)
    }

    /// A pure tracked deletion is somebody's change unless the broker had
    /// recorded starting to remove that checkout: without the record it is
    /// kept under its own code, as an agent's uncommitted `rm -rf` must be;
    /// with the record for this owner and HEAD the removal is finished. An
    /// untracked file still keeps a recorded one.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_removal_of_a_closed_landed_checkout_is_resumed() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (session, deleted) = closed_landed_checkout(&mut broker, "hand-deleted file");
        std::fs::remove_file(deleted.join("base.txt")).unwrap();

        let (codes, removed) = kept_codes_over_passes(&mut broker, &deleted);
        assert!(removed.is_empty(), "{removed:#?}");
        assert!(deleted.exists(), "no removal record: the deletion is kept");
        assert!(
            codes
                .iter()
                .any(|code| code == "unmarked_tracked_deletions"),
            "{codes:?}"
        );

        let canonical = std::fs::canonicalize(&deleted).unwrap();
        broker
            .record_removal_attempt(&canonical, session.id, &checkout_head_of(&deleted))
            .unwrap();
        let (codes, removed) = kept_codes_over_passes(&mut broker, &deleted);
        assert!(!deleted.exists(), "{codes:?}");
        assert_eq!(removed.len(), 1, "{removed:#?}");
        assert_eq!(removed[0].resumed_interrupted_removal, 1, "{removed:#?}");

        let (notes_session, with_notes) =
            closed_landed_checkout(&mut broker, "interrupted removal with notes");
        std::fs::remove_file(with_notes.join("base.txt")).unwrap();
        std::fs::write(with_notes.join("notes.txt"), "somebody's notes\n").unwrap();
        broker
            .record_removal_attempt(
                &std::fs::canonicalize(&with_notes).unwrap(),
                notes_session.id,
                &checkout_head_of(&with_notes),
            )
            .unwrap();
        let (codes, removed) = kept_codes_over_passes(&mut broker, &with_notes);
        assert!(removed.is_empty(), "{removed:#?}");
        assert!(with_notes.join("notes.txt").is_file());
        assert!(
            codes.iter().any(|code| code == "uncommitted_changes"),
            "{codes:?}"
        );
    }

    /// Strict cleanup removes only pristine checkouts, even one the broker
    /// recorded starting to remove.
    #[cfg(unix)]
    #[test]
    fn strict_cleanup_keeps_a_recorded_interrupted_removal() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[cleanup]\nstrict = true\n"));
        let (session, deleted) = closed_landed_checkout(&mut broker, "strict interrupted");
        std::fs::remove_file(deleted.join("base.txt")).unwrap();
        broker
            .record_removal_attempt(
                &std::fs::canonicalize(&deleted).unwrap(),
                session.id,
                &checkout_head_of(&deleted),
            )
            .unwrap();
        let (codes, removed) = kept_codes_over_passes(&mut broker, &deleted);
        assert!(removed.is_empty(), "{removed:#?}");
        assert!(deleted.exists());
        assert!(
            codes.iter().any(|code| code == "strict_tracked_deletions"),
            "{codes:?}"
        );
    }

    /// A record for another owner or another HEAD does not count.
    #[cfg(unix)]
    #[test]
    fn a_removal_record_counts_only_for_its_owner_and_head() {
        let (_owner, _storage, broker) = landed_lane_fixture(None);
        let path = Path::new("/nonexistent/checkout");
        broker.record_removal_attempt(path, 7, "abc").unwrap();
        assert!(broker.removal_attempt_recorded(path, 7, "abc"));
        assert!(!broker.removal_attempt_recorded(path, 8, "abc"));
        assert!(!broker.removal_attempt_recorded(path, 7, "abd"));
        broker.clear_removal_attempt(path);
        assert!(!broker.removal_attempt_recorded(path, 7, "abc"));
    }

    #[test]
    fn only_a_pure_tracked_deletion_reads_as_an_interrupted_removal() {
        use super::interrupted_removal_deletions as deletions;
        // Worktree deletions of most tracked paths, Finder metadata aside.
        assert_eq!(
            deletions(" D a\0 D b/c\0?? b/.DS_Store\0", 4, false),
            Some(2)
        );
        // Strict mode counts Finder metadata as somebody's file.
        assert_eq!(deletions(" D a\0?? .DS_Store\0", 1, true), None);
        // A deletion of under a quarter of the tracked paths is a change.
        assert_eq!(deletions(" D a\0", 5, false), None);
        // Anything else keeps the checkout: a staged deletion or change, a
        // modification, a rename, an untracked file, a type change.
        for other in [
            "D  a\0",
            "M  a\0",
            " M a\0",
            "R  b\0a\0",
            "?? notes.txt\0",
            " T a\0",
            "MD a\0",
        ] {
            assert_eq!(
                deletions(&format!(" D z\0{other}"), 2, false),
                None,
                "{other:?}"
            );
        }
        assert_eq!(deletions("", 3, false), None, "a clean listing is not one");
    }

    /// A checkout tracking a submodule is never an interrupted removal: the
    /// submodule's own history could go with it.
    #[test]
    fn a_tracked_submodule_is_never_an_interrupted_removal() {
        use super::interrupted_removal_against_index as against;
        let blob = "100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\ta";
        let gitlink = "160000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tsub";
        assert_eq!(against(" D a\0", &format!("{blob}\0"), false), Some(1));
        assert_eq!(
            against(" D a\0", &format!("{blob}\0{gitlink}\0"), false),
            None
        );
    }

    /// A long pass's pressure probe answers from a count recorded moments
    /// ago against the same limit, instead of counting again; a count taken
    /// against another limit, or a stale one, is recounted.
    #[cfg(unix)]
    #[test]
    fn a_long_pass_pressure_probe_reuses_a_fresh_recorded_count_for_the_same_limit() {
        let (_owner, _storage, broker, _session) = retirement_fixture();
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 100\n",
        )
        .unwrap();
        let long = || Some(Instant::now() + Duration::from_secs(30));
        assert!(
            !broker.checkout_capacity_pressure(long()),
            "one checkout of 100"
        );
        broker.record_checkout_usage(99, 100);
        assert!(
            broker.checkout_capacity_pressure(long()),
            "a fresh count for the same limit is reused"
        );
        broker.record_checkout_usage(99, 200);
        assert!(
            !broker.checkout_capacity_pressure(long()),
            "a count against another limit is not"
        );
        let stale = serde_json::json!({
            "count": 99,
            "maximum": 100,
            "at_ms": super::now_ms() - super::CHECKOUT_USAGE_REUSE_MS - 1,
        })
        .to_string();
        broker
            .store_ref()
            .meta_set(super::CHECKOUT_USAGE_KEY, &stale)
            .unwrap();
        assert!(
            !broker.checkout_capacity_pressure(long()),
            "a stale count is recounted"
        );
    }

    /// The checkout count is one read of this repository's registrations, the
    /// records `git worktree list` enumerates, not the listing itself.
    #[cfg(unix)]
    #[test]
    fn the_checkout_count_reads_this_repositorys_registrations() {
        let (_owner, _storage, broker, _session) = retirement_fixture();
        let listed = broker.repo_handle().worktree_inventory().unwrap().len() - 1;
        assert_eq!(broker.registered_linked_checkout_count(), Some(listed));
        let common = broker.repo_handle().git_common_dir().unwrap();
        std::fs::create_dir_all(common.join("worktrees/registration-only")).unwrap();
        assert_eq!(broker.registered_linked_checkout_count(), Some(listed + 1));
        assert_eq!(
            broker
                .worktree_checkout_usage()
                .unwrap()
                .map(|(count, _)| count),
            Some(listed + 1)
        );
    }

    /// An inventory that is not ready says what it waited on, and the path
    /// index work one pass did is resumed by the next instead of restarting,
    /// so it converges over passes.
    #[cfg(unix)]
    #[test]
    fn an_unavailable_inventory_says_what_it_waited_on_and_resumes_its_progress() {
        let (_owner, _storage, mut broker, _first) = retirement_fixture();
        for index in 0..3 {
            broker
                .start_worktree(&format!("index lap {index}"), None)
                .unwrap();
        }
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .unwrap()
            .complete
        {}
        broker
            .store()
            .begin_cleanup_cohort_alias_refresh(namespace, super::now_ms())
            .unwrap();
        super::COHORT_SCAN_LIMITS.with(|limits| limits.set(Some((1, 1))));
        let first = broker.auto_cleanup_plan(None);
        let after_first = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .unwrap();
        let expired = broker.auto_cleanup_plan(Some(Instant::now() - Duration::from_millis(1)));
        let second = broker.auto_cleanup_plan(None);
        let after_second = broker
            .store()
            .cleanup_cohort_scan_state(namespace)
            .unwrap()
            .unwrap();
        super::COHORT_SCAN_LIMITS.with(|limits| limits.set(None));
        let lanes = first.unwrap().report.lanes.unwrap();
        assert!(lanes.removal_inventory_unavailable, "{lanes:?}");
        assert_eq!(
            lanes.removal_inventory_wait,
            Some(super::InventoryWait::CohortIndex)
        );
        assert_eq!(
            expired
                .unwrap()
                .report
                .lanes
                .unwrap()
                .removal_inventory_wait,
            Some(super::InventoryWait::CapacityProbe),
            "a deadline gone before the scan began names the probe"
        );
        second.unwrap();
        assert!(
            after_second.complete || after_second.after_session_id > after_first.after_session_id,
            "the next pass resumes the lap: {after_first:?} then {after_second:?}"
        );
    }

    /// Removal reads the lock reason from the checkout's own registration
    /// instead of listing every worktree; a checkout whose `.git` names no
    /// registration of this repository is not read that way.
    #[cfg(unix)]
    #[test]
    fn the_lock_reason_comes_from_the_checkouts_own_registration() {
        let (_owner, _storage, broker, session) = retirement_fixture();
        let path = PathBuf::from(&session.worktree_path);
        let root = broker.main_root().to_path_buf();
        assert_eq!(
            broker.repo_handle().registration_lock_reason(&path),
            Ok(None)
        );
        git(
            &root,
            &[
                "worktree",
                "lock",
                "--reason",
                "fixture lock reason",
                path.to_str().unwrap(),
            ],
        );
        assert_eq!(
            broker.repo_handle().registration_lock_reason(&path),
            Ok(Some("fixture lock reason".into()))
        );
        git(&root, &["worktree", "unlock", path.to_str().unwrap()]);
        let stray = root.parent().unwrap().join("stray-checkout");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join(".git"), "gitdir: /nowhere\n").unwrap();
        assert_eq!(
            broker.repo_handle().registration_lock_reason(&stray),
            Err(())
        );
    }

    /// A pass too short to list checkouts reads the count `start` recorded:
    /// at 861 of 900 it is under pressure, which it used to report as none.
    #[cfg(unix)]
    #[test]
    fn a_short_pass_sees_capacity_pressure_from_the_recorded_count() {
        let (_owner, _storage, broker) = landed_lane_fixture(None);
        let short = || Some(Instant::now() + Duration::from_millis(200));
        assert!(
            !broker.checkout_capacity_pressure(short()),
            "nothing recorded"
        );
        broker.record_checkout_usage(861, 900);
        assert!(broker.checkout_capacity_pressure(short()));
        assert!(broker.recorded_checkout_capacity_pressure());
        broker.record_checkout_usage(600, 900);
        assert!(!broker.checkout_capacity_pressure(short()));
        // A stale count is not evidence.
        let stale = serde_json::json!({
            "count": 861,
            "maximum": 900,
            "at_ms": super::now_ms() - super::CHECKOUT_USAGE_MAX_AGE_MS - 1,
        })
        .to_string();
        broker
            .store_ref()
            .meta_set(super::CHECKOUT_USAGE_KEY, &stale)
            .unwrap();
        assert!(!broker.checkout_capacity_pressure(short()));
        assert!(!broker.recorded_checkout_capacity_pressure());
    }

    /// The background worker's own removal slice frees a slot under capacity
    /// pressure without any cleanup request, then backs off once only kept
    /// checkouts remain instead of re-judging them every run.
    #[cfg(unix)]
    #[test]
    fn the_worker_capacity_slice_removes_and_then_backs_off() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "worker frees a slot");
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "worker keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        let root = broker.main_root().to_path_buf();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();

        let first = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("two checkouts against a limit of one is pressure");
        assert_eq!(first.removed.len(), 1, "{first:#?}");
        assert!(!landed.exists());
        assert!(first.lanes.is_some_and(|lanes| lanes.capacity_pressure));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "pressure", "{slice:#?}");
        assert_eq!(
            slice.removal_cap, 12,
            "pressure keeps its limits: {slice:#?}"
        );

        let second = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("one kept checkout against a limit of one is still pressure");
        assert!(second.removed.is_empty(), "{second:#?}");
        assert!(kept.join("notes.txt").is_file());
        assert!(
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .is_none(),
            "a slice that removed nothing backs off"
        );
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    /// Limits of a test slice: the worker's, with a smaller allowance.
    #[cfg(unix)]
    fn slice_allowing(removals: usize) -> crate::gc::CapacitySliceLimits {
        crate::gc::CapacitySliceLimits {
            removals,
            ..crate::gc::CapacitySliceLimits::WORKER
        }
    }

    #[cfg(unix)]
    fn limit_checkouts_to_one(broker: &crate::Broker) {
        std::fs::write(
            broker.main_root().join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();
    }

    /// A slice stops at its removal allowance, one removal at a time, so a
    /// long backlog cannot turn into a burst of forced removals, and says
    /// so in its record.
    #[cfg(unix)]
    #[test]
    fn a_worker_capacity_slice_caps_its_removals() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..5)
            .map(|index| closed_landed_checkout(&mut broker, &format!("backlog {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let report = broker
            .run_capacity_removal_slice_with(Instant::now(), slice_allowing(4))
            .unwrap()
            .expect("five checkouts against a limit of one is pressure");
        assert_eq!(report.removed.len(), 4, "{report:#?}");
        assert_eq!(worktrees.iter().filter(|path| path.exists()).count(), 1);
        let slice = broker
            .last_capacity_removal_slice()
            .expect("a slice records itself");
        assert_eq!(slice.stop_reason.as_deref(), Some("cap"), "{slice:#?}");
        assert_eq!(slice.removal_cap, 4, "{slice:#?}");
        assert_eq!(slice.passes, 4, "one removal per pass: {slice:#?}");
        assert_eq!(
            slice.retry_in_ms, None,
            "a slice that removed runs again at once"
        );
        assert!(
            slice.lock_held_ms > 0 && slice.lock_wait_ms == 0,
            "{slice:#?}"
        );
        assert!(
            slice.lock_yield_ms >= 3 * 400,
            "without a pacer, the lock is still left free between passes: {slice:#?}"
        );
    }

    /// The worker's own allowance lets one slice clear a backlog the old
    /// cap of four left for later runs.
    #[cfg(unix)]
    #[test]
    fn the_worker_slice_removes_more_than_four_while_the_backlog_lasts() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..6)
            .map(|index| closed_landed_checkout(&mut broker, &format!("long backlog {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("six checkouts against a limit of one is pressure");
        assert_eq!(report.removed.len(), 6, "{report:#?}");
        assert!(worktrees.iter().all(|path| !path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("nothing_selected"),
            "{slice:#?}"
        );
    }

    /// A checkout judged and kept costs none of the removal allowance: a
    /// slice allowed two removals removes two even when it kept one first.
    #[cfg(unix)]
    #[test]
    fn a_kept_checkout_does_not_consume_the_removal_allowance() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_first, first) = closed_landed_checkout(&mut broker, "allowance first");
        let (_second, second) = closed_landed_checkout(&mut broker, "allowance second");
        let (kept_session, kept) = closed_landed_checkout(&mut broker, "allowance keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        // Judged first: a merged pull request puts it ahead of the others.
        record_merged_pull_request(&mut broker, kept_session.id, 91);
        limit_checkouts_to_one(&broker);
        let report = broker
            .run_capacity_removal_slice_with(Instant::now(), slice_allowing(2))
            .unwrap()
            .expect("three checkouts against a limit of one is pressure");
        assert_eq!(report.removed.len(), 2, "{report:#?}");
        assert!(!first.exists() && !second.exists());
        assert!(kept.join("notes.txt").is_file());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert!(slice.kept >= 1, "the kept checkout was judged: {slice:#?}");
        assert_eq!(slice.stop_reason.as_deref(), Some("cap"), "{slice:#?}");
    }

    /// A foreground command waiting for the GC lock (`start` at the limit)
    /// is yielded to before the slice takes the lock: nothing is removed,
    /// the slice says why, and it comes back after the short lock back-off.
    #[cfg(unix)]
    #[test]
    fn a_worker_capacity_slice_yields_to_a_waiting_foreground_command() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "yield to start");
        limit_checkouts_to_one(&broker);
        let root = broker.main_root().to_path_buf();
        let mut waiter = Command::new("sleep").arg("60").spawn().unwrap();
        let waiters = root.join(".aethyme/run/gc-lock-waiters");
        std::fs::create_dir_all(&waiters).unwrap();
        std::fs::write(waiters.join(waiter.id().to_string()), b"").unwrap();

        let report = broker.run_capacity_removal_slice(Instant::now());
        let _ = waiter.kill();
        let _ = waiter.wait();
        assert!(report.unwrap().is_none(), "no removal pass ran");
        assert!(
            landed.exists(),
            "the waiting command removes it, not the worker"
        );
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("yielded_to_foreground"),
            "{slice:#?}"
        );
        assert_eq!(slice.outcome, "lock_busy", "{slice:#?}");
        assert_eq!(slice.passes, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    /// Install a pacer on a fake clock whose pressure signal replays
    /// `samples`, repeating the last.
    #[cfg(unix)]
    fn install_scripted_pacer(
        samples: Vec<crate::gc::unattended_pacing::PressureSample>,
    ) -> (
        crate::gc::unattended_pacing::UnattendedPacingScope,
        crate::gc::unattended_pacing::FakeClock,
    ) {
        install_scripted_pacer_at(
            crate::gc::unattended_pacing::UnlinkRate::Limited(1_000),
            samples,
        )
    }

    /// [`install_scripted_pacer`] at `rate`.
    #[cfg(unix)]
    fn install_scripted_pacer_at(
        rate: crate::gc::unattended_pacing::UnlinkRate,
        samples: Vec<crate::gc::unattended_pacing::PressureSample>,
    ) -> (
        crate::gc::unattended_pacing::UnattendedPacingScope,
        crate::gc::unattended_pacing::FakeClock,
    ) {
        use crate::gc::unattended_pacing::{self, FakeClock, Scripted, UnattendedPacer};
        let clock = FakeClock::new();
        let scope = unattended_pacing::install(UnattendedPacer::new(
            rate,
            Box::new(clock.clone()),
            Box::new(Scripted {
                samples,
                reads: Default::default(),
            }),
        ));
        (scope, clock)
    }

    #[cfg(unix)]
    fn memory_pressure(elevated: bool) -> crate::gc::unattended_pacing::PressureSample {
        crate::gc::unattended_pacing::PressureSample {
            memory_elevated: Some(elevated),
            memory_warning: None,
            load_elevated: Some(false),
        }
    }

    /// No removal starts under memory pressure: the slice checks the pacer
    /// before its first pass and stops, backs off ten minutes, and the next
    /// slice of the same worker run does not remove either. The record says
    /// memory kept it from selection.
    #[cfg(unix)]
    #[test]
    fn a_worker_capacity_slice_removes_nothing_under_pressure() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("gated {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let (scope, clock) = install_scripted_pacer(vec![memory_pressure(true)]);
        let worker_started = Instant::now();
        let first = broker.run_capacity_removal_slice(worker_started);
        let second = broker.run_capacity_removal_slice(worker_started);
        drop(scope);
        assert!(first.unwrap().is_none(), "no pass ran");
        assert!(second.unwrap().is_none(), "the next slice is skipped");
        assert!(worktrees.iter().all(|path| path.exists()));
        assert!(clock.slept.get() > Duration::ZERO, "the pacer paused first");
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_backoff"),
            "{slice:#?}"
        );
        assert_eq!(slice.passes, 0, "{slice:#?}");
        assert_eq!(slice.outcome, "pressure_before_selection", "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("memory"), "{slice:#?}");
        assert!(slice.memory_wait_ms > 0, "{slice:#?}");
        assert!(!slice.throttled, "{slice:#?}");
        assert!(slice.budget_consumed_by.is_none(), "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    /// Pressure that starts while a removal is paid for ends the slice after
    /// that removal and backs off ten minutes even though it removed one, so
    /// the next slice cannot remove again with the pacer's pause budget
    /// spent.
    #[cfg(unix)]
    #[test]
    fn pressure_after_a_removal_backs_off_instead_of_retrying_at_once() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("pressured {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        // Calm for the gate before the first pass and for the check right
        // before Git deletes, then pressure for good.
        let (scope, _clock) = install_scripted_pacer(vec![
            memory_pressure(false),
            memory_pressure(false),
            memory_pressure(true),
        ]);
        let worker_started = Instant::now();
        let first = broker.run_capacity_removal_slice(worker_started);
        let second = broker.run_capacity_removal_slice(worker_started);
        drop(scope);
        let first = first.unwrap().expect("the first slice ran a pass");
        assert_eq!(first.removed.len(), 1, "{first:#?}");
        assert!(second.unwrap().is_none(), "the next slice is skipped");
        assert_eq!(worktrees.iter().filter(|path| path.exists()).count(), 1);
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_backoff"),
            "{slice:#?}"
        );
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
    }

    /// With production ordering (a real clock, a slice deadline shorter than
    /// the pacer's pause budget) sustained memory pressure still stops the
    /// slice as `memory_backoff` with its ten-minute back-off, not as
    /// `budget` with a one-minute retry.
    #[cfg(unix)]
    #[test]
    fn pressure_that_outlasts_a_short_slice_still_backs_off_ten_minutes() {
        use crate::gc::unattended_pacing::{
            self, Scripted, SystemClock, UnattendedPacer, UnlinkRate,
        };
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "short pressured slice");
        limit_checkouts_to_one(&broker);
        let scope = unattended_pacing::install(UnattendedPacer::new(
            UnlinkRate::Limited(1_000),
            Box::new(SystemClock),
            Box::new(Scripted {
                samples: vec![memory_pressure(true)],
                reads: Default::default(),
            }),
        ));
        let limits = crate::gc::CapacitySliceLimits {
            budget: Duration::from_secs(6),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        let report = broker.run_capacity_removal_slice_with(Instant::now(), limits);
        drop(scope);
        assert!(report.unwrap().is_none(), "no pass ran");
        assert!(landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_backoff"),
            "{slice:#?}"
        );
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
        assert!(slice.pressure_wait_ms >= 1_000, "{slice:#?}");
        assert!(slice.memory_wait_ms >= 1_000, "{slice:#?}");
        assert_eq!(slice.load_wait_ms, 0, "{slice:#?}");
    }

    #[cfg(unix)]
    fn load_pressure(elevated: bool) -> crate::gc::unattended_pacing::PressureSample {
        crate::gc::unattended_pacing::PressureSample {
            memory_elevated: Some(false),
            memory_warning: None,
            load_elevated: Some(elevated),
        }
    }

    /// [`install_scripted_pacer`] with the unlink limiter off, so a removal's
    /// charge costs no fake time beyond its pressure checks.
    #[cfg(unix)]
    fn install_unlimited_scripted_pacer(
        samples: Vec<crate::gc::unattended_pacing::PressureSample>,
    ) -> (
        crate::gc::unattended_pacing::UnattendedPacingScope,
        crate::gc::unattended_pacing::FakeClock,
    ) {
        install_scripted_pacer_at(crate::gc::unattended_pacing::UnlinkRate::Unlimited, samples)
    }

    /// A busy machine's normal state, load far above the core count with
    /// memory fine, throttles steady removal and never stops it: after a
    /// bounded wait the slice removes one landed checkout, says it ran
    /// throttled because of load, keeps the idle streak, and the next slice
    /// is due at the normal minute rather than a ten-minute back-off.
    #[cfg(unix)]
    #[test]
    fn sustained_load_still_removes_a_landed_checkout_per_steady_slice() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let landed = (0..2)
            .map(|index| {
                let (session, worktree) =
                    closed_landed_checkout(&mut broker, &format!("loaded {index}"));
                record_merged_pull_request(&mut broker, session.id, 400 + index);
                worktree
            })
            .collect::<Vec<_>>();
        let (scope, clock) = install_unlimited_scripted_pacer(vec![load_pressure(true)]);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("load never stops a steady slice");
        assert_eq!(
            report.removed.len(),
            1,
            "one removal under load: {report:#?}"
        );
        assert_eq!(landed.iter().filter(|path| path.exists()).count(), 1);
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(slice.outcome, "removed", "{slice:#?}");
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("load_throttled"),
            "{slice:#?}"
        );
        assert!(slice.throttled, "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("load"), "{slice:#?}");
        assert_eq!(slice.memory_wait_ms, 0, "{slice:#?}");
        assert!(
            slice.load_wait_ms >= 5_000,
            "the bounded gate wait: {slice:#?}"
        );
        assert!(
            clock.slept.get() < Duration::from_secs(10),
            "bounded waits, not the slice: {slice:#?}"
        );
        assert_eq!(slice.idle_streak, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
        // No hard back-off: once the minute passes the next slice is due and
        // removes the other landed checkout, still under load.
        broker.end_steady_removal_back_off_for_test();
        assert!(broker.capacity_removal_due(super::now_ms()).unwrap());
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("the next slice runs");
        drop(scope);
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(landed.iter().all(|path| !path.exists()));
    }

    /// Sustained memory pressure stops a steady slice before any pass: no
    /// removal, the longer ten-minute retry that a landing does not shorten,
    /// and the idle streak is left as it was, not counted as an idle lap.
    #[cfg(unix)]
    #[test]
    fn sustained_memory_pressure_backs_off_without_growing_the_idle_streak() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "memory keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs");
        assert_eq!(broker.last_capacity_removal_slice().unwrap().idle_streak, 1);
        let (session, landed) = closed_landed_checkout(&mut broker, "memory landed");
        record_merged_pull_request(&mut broker, session.id, 410);
        broker.end_steady_removal_back_off_for_test();

        let (scope, _clock) = install_unlimited_scripted_pacer(vec![memory_pressure(true)]);
        let report = broker.run_capacity_removal_slice(Instant::now());
        drop(scope);
        assert!(report.unwrap().is_none(), "no pass ran");
        assert!(landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_backoff"),
            "{slice:#?}"
        );
        assert_eq!(slice.outcome, "pressure_before_selection", "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("memory"), "{slice:#?}");
        assert_eq!(slice.passes, 0, "{slice:#?}");
        assert_eq!(slice.idle_streak, 1, "not an idle lap: {slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
        broker.end_steady_removal_back_off_for_test();
        assert!(
            !broker.capacity_removal_due(super::now_ms()).unwrap(),
            "the memory back-off is a hard retry"
        );
    }

    /// Memory pressure at the warning level slows a steady slice and never
    /// stops it: after a bounded wait it removes exactly one landed
    /// checkout, stops `memory_warning_throttled`, and the next slice waits
    /// the two-minute gap as a hard retry that the steady minute does not end.
    #[cfg(unix)]
    #[test]
    fn a_memory_warning_allows_one_removal_then_a_hard_two_minute_gap() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let landed = (0..2)
            .map(|index| {
                let (session, worktree) =
                    closed_landed_checkout(&mut broker, &format!("warning {index}"));
                record_merged_pull_request(&mut broker, session.id, 440 + index);
                worktree
            })
            .collect::<Vec<_>>();
        let warning = crate::gc::unattended_pacing::PressureSample {
            memory_elevated: Some(false),
            memory_warning: Some(true),
            load_elevated: Some(false),
        };
        let (scope, clock) = install_unlimited_scripted_pacer(vec![warning]);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a memory warning never stops a steady slice");
        drop(scope);
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert_eq!(landed.iter().filter(|path| path.exists()).count(), 1);
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_warning_throttled"),
            "{slice:#?}"
        );
        assert!(slice.throttled, "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("memory"), "{slice:#?}");
        assert_eq!(slice.memory_wait_ms, 0, "no critical pause: {slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(120_000), "{slice:#?}");
        assert!(
            clock.slept.get() < Duration::from_secs(10),
            "bounded waits, not the slice: {slice:#?}"
        );
        broker.end_steady_removal_back_off_for_test();
        assert!(
            !broker.capacity_removal_due(super::now_ms()).unwrap(),
            "the warning gap is a hard retry"
        );
    }

    /// Load that clears during the bounded wait leaves a normal slice: it
    /// is not throttled and removes every landed checkout its allowance
    /// covers, while still reporting the load it waited for.
    #[cfg(unix)]
    #[test]
    fn load_that_clears_mid_wait_leaves_a_normal_steady_slice() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let landed = (0..2)
            .map(|index| {
                let (session, worktree) =
                    closed_landed_checkout(&mut broker, &format!("clearing {index}"));
                record_merged_pull_request(&mut broker, session.id, 420 + index);
                worktree
            })
            .collect::<Vec<_>>();
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![
            load_pressure(true),
            load_pressure(true),
            load_pressure(false),
        ]);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs");
        drop(scope);
        assert_eq!(report.removed.len(), 2, "{report:#?}");
        assert!(landed.iter().all(|path| !path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert!(!slice.throttled, "{slice:#?}");
        assert_ne!(slice.stop_reason.as_deref(), Some("load_throttled"));
        assert_eq!(slice.pressure_seen.as_deref(), Some("load"), "{slice:#?}");
        assert_eq!(slice.load_wait_ms, 2_000, "{slice:#?}");
    }

    /// At the checkout limit, sustained load cannot starve removal and
    /// cannot run it back to back either: each pressure slice removes one
    /// checkout after its bounded wait, then the next slice waits the
    /// 30-second throttle gap (not the ten-minute back-off, and not at once,
    /// even within the same worker run), and once it passes the next slice
    /// removes the next checkout. The records count the run's slices and
    /// removals and its time split.
    #[cfg(unix)]
    #[test]
    fn capacity_pressure_makes_progress_under_sustained_load() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..3)
            .map(|index| closed_landed_checkout(&mut broker, &format!("at limit {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![load_pressure(true)]);
        let worker_started = Instant::now();
        for slice_number in 1..=2 {
            let report = broker
                .run_capacity_removal_slice(worker_started)
                .unwrap()
                .expect("load never stops a pressure slice");
            assert_eq!(report.removed.len(), 1, "{report:#?}");
            let slice = broker.last_capacity_removal_slice().unwrap();
            assert_eq!(slice.trigger, "pressure", "{slice:#?}");
            assert_eq!(
                slice.stop_reason.as_deref(),
                Some("load_throttled"),
                "{slice:#?}"
            );
            assert!(slice.throttled, "{slice:#?}");
            assert_eq!(slice.retry_in_ms, Some(30_000), "{slice:#?}");
            assert_eq!(slice.throttle_gap_ms, 30_000, "{slice:#?}");
            assert_eq!(slice.run_slices, slice_number, "{slice:#?}");
            assert_eq!(slice.run_removed, slice_number, "{slice:#?}");
            assert!(slice.run_elapsed_ms >= slice.elapsed_ms, "{slice:#?}");
            assert!(
                slice.run_deleting_ms + slice.run_waiting_ms <= slice.run_elapsed_ms + 1,
                "{slice:#?}"
            );
            assert!(
                slice.run_deleting_pct + slice.run_waiting_pct <= 100,
                "{slice:#?}"
            );
            assert_eq!(
                worktrees.iter().filter(|path| path.exists()).count(),
                3 - slice_number
            );
            // The gap holds within the worker run and across broker opens.
            assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
            assert!(
                broker
                    .run_capacity_removal_slice(worker_started)
                    .unwrap()
                    .is_none(),
                "no slice inside the throttle gap"
            );
            assert_eq!(
                worktrees.iter().filter(|path| path.exists()).count(),
                3 - slice_number
            );
            broker.end_capacity_removal_back_off_for_test();
            assert!(broker.capacity_removal_due(super::now_ms()).unwrap());
        }
        drop(scope);
    }

    /// Without load a pressure slice that removed still lets the next one
    /// run at once: the throttle gap is load's alone.
    #[cfg(unix)]
    #[test]
    fn without_load_a_pressure_slice_that_removed_applies_no_throttle_gap() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..3)
            .map(|index| closed_landed_checkout(&mut broker, &format!("calm {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![load_pressure(false)]);
        let report = broker
            .run_capacity_removal_slice_with(Instant::now(), slice_allowing(1))
            .unwrap()
            .expect("pressure runs a slice");
        drop(scope);
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert_eq!(worktrees.iter().filter(|path| path.exists()).count(), 2);
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.stop_reason.as_deref(), Some("cap"), "{slice:#?}");
        assert!(!slice.throttled, "{slice:#?}");
        assert_eq!(slice.throttle_gap_ms, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, None, "{slice:#?}");
        assert!(broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    #[cfg(unix)]
    fn memory_and_load_pressure() -> crate::gc::unattended_pacing::PressureSample {
        crate::gc::unattended_pacing::PressureSample {
            memory_elevated: Some(true),
            memory_warning: None,
            load_elevated: Some(true),
        }
    }

    /// Memory pressure and elevated load in the same sample stop a slice as
    /// memory pressure: no pass, `memory_backoff`, the ten-minute retry,
    /// and no load wait or throttle recorded.
    #[cfg(unix)]
    #[test]
    fn memory_and_load_in_one_sample_stop_a_slice_as_memory_pressure() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("both {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![memory_and_load_pressure()]);
        let report = broker.run_capacity_removal_slice(Instant::now());
        drop(scope);
        assert!(report.unwrap().is_none(), "no pass ran");
        assert!(worktrees.iter().all(|path| path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("memory_backoff"),
            "{slice:#?}"
        );
        assert_eq!(slice.outcome, "pressure_before_selection", "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("memory"), "{slice:#?}");
        assert!(slice.memory_wait_ms > 0, "{slice:#?}");
        assert_eq!(slice.load_wait_ms, 0, "{slice:#?}");
        assert!(!slice.throttled, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    /// The first removal a load-throttled steady slice makes resets even a
    /// long stale idle streak to 0, so the next slice comes a minute later
    /// instead of after the idle ladder's longest wait.
    #[cfg(unix)]
    #[test]
    fn a_throttled_removal_resets_a_stale_idle_streak() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let (session, landed) = closed_landed_checkout(&mut broker, "stale streak");
        record_merged_pull_request(&mut broker, session.id, 430);
        broker.set_steady_removal_idle_streak_for_test(26);
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![load_pressure(true)]);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs under load");
        drop(scope);
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("load_throttled"),
            "{slice:#?}"
        );
        assert_eq!(slice.idle_streak, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
        assert_eq!(
            slice.throttle_gap_ms, 0,
            "the steady minute already exceeds the gap: {slice:#?}"
        );
    }

    /// Under sustained load a slice whose budget ran out before its first
    /// pressure check (counting and the other pre-selection work) is out of
    /// time, not pressure-blocked: it names what used the budget and its
    /// retries escalate (1, then 2 minutes) instead of staying at one.
    #[cfg(unix)]
    #[test]
    fn under_sustained_load_a_slice_out_of_time_before_selection_keeps_its_streak() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("short {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        // Exactly the minimum slice: the slice starts, and whatever counting
        // takes leaves less than a minimum slice for the first pass.
        let limits = crate::gc::CapacitySliceLimits {
            budget: Duration::from_secs(5),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![load_pressure(true)]);
        for expected_retry in [60_000, 120_000] {
            let report = broker.run_capacity_removal_slice_with(Instant::now(), limits);
            assert!(report.unwrap().is_none(), "no pass ran");
            let slice = broker.last_capacity_removal_slice().unwrap();
            assert_eq!(slice.stop_reason.as_deref(), Some("budget"), "{slice:#?}");
            assert_eq!(slice.passes, 0, "{slice:#?}");
            assert_eq!(slice.outcome, "out_of_time_before_selection", "{slice:#?}");
            assert!(slice.budget_consumed_by.is_some(), "{slice:#?}");
            assert_eq!(slice.retry_in_ms, Some(expected_retry), "{slice:#?}");
            broker.end_capacity_removal_back_off_for_test();
        }
        drop(scope);
        assert!(worktrees.iter().all(|path| path.exists()));
    }

    /// Under sustained load a removal pass that fails before it removes
    /// anything stays a failure: stop `failed`, the out-of-time outcome
    /// with its escalating retry, never `pressure_before_selection` because
    /// the slice also waited for load.
    #[cfg(unix)]
    #[test]
    fn under_sustained_load_a_failed_pass_is_not_pressure_blocked() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("failing {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        // A directory where the GC lock file belongs, too old to be a lock
        // being written: taking the GC lock fails with an I/O error.
        let lock = broker.main_root().join(".aethyme/gc.lock");
        std::fs::create_dir(&lock).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(60);
        std::fs::File::open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let (scope, _clock) = install_unlimited_scripted_pacer(vec![load_pressure(true)]);
        for expected_retry in [60_000, 120_000] {
            let report = broker.run_capacity_removal_slice(Instant::now());
            assert!(report.is_err(), "the pass failed: {report:?}");
            let slice = broker.last_capacity_removal_slice().unwrap();
            assert_eq!(slice.stop_reason.as_deref(), Some("failed"), "{slice:#?}");
            assert_eq!(slice.passes, 0, "{slice:#?}");
            assert!(slice.throttled, "the slice did see load: {slice:#?}");
            assert_eq!(slice.outcome, "out_of_time_before_selection", "{slice:#?}");
            assert!(slice.budget_consumed_by.is_some(), "{slice:#?}");
            assert_eq!(slice.retry_in_ms, Some(expected_retry), "{slice:#?}");
            broker.end_capacity_removal_back_off_for_test();
        }
        drop(scope);
        assert!(worktrees.iter().all(|path| path.exists()));
    }

    /// A slice that reached its pressure check with at least the minimum
    /// slice left but whose bounded load wait (on a real clock) left too
    /// little for a pass is genuinely blocked by load: outcome
    /// `pressure_before_selection`, a one-minute retry.
    #[cfg(unix)]
    #[test]
    fn a_slice_the_load_wait_left_without_a_pass_is_pressure_blocked() {
        use crate::gc::unattended_pacing::{
            self, Scripted, SystemClock, UnattendedPacer, UnlinkRate,
        };
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "load blocked");
        limit_checkouts_to_one(&broker);
        let scope = unattended_pacing::install(UnattendedPacer::new(
            UnlinkRate::Unlimited,
            Box::new(SystemClock),
            Box::new(Scripted {
                samples: vec![load_pressure(true)],
                reads: Default::default(),
            }),
        ));
        // Seven seconds: the check is reached with more than the 5-second
        // minimum slice left (counting takes far less than two seconds),
        // and its 5-second load wait leaves less.
        let limits = crate::gc::CapacitySliceLimits {
            budget: Duration::from_secs(7),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        let report = broker.run_capacity_removal_slice_with(Instant::now(), limits);
        drop(scope);
        assert!(report.unwrap().is_none(), "no pass ran");
        assert!(landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.stop_reason.as_deref(), Some("budget"), "{slice:#?}");
        assert_eq!(slice.passes, 0, "{slice:#?}");
        assert_eq!(slice.outcome, "pressure_before_selection", "{slice:#?}");
        assert_eq!(slice.pressure_seen.as_deref(), Some("load"), "{slice:#?}");
        assert!(slice.load_wait_ms >= 1_000, "{slice:#?}");
        assert!(slice.budget_consumed_by.is_none(), "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
    }

    /// A pacer wait the slice's end cuts short stops the slice as
    /// `slice_deadline`, and the next slice waits at least what is owed.
    #[cfg(unix)]
    #[test]
    fn a_pacer_wait_cut_by_the_slice_end_is_owed_by_the_next_slice() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..2)
            .map(|index| closed_landed_checkout(&mut broker, &format!("owed {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        let (scope, _clock) = install_scripted_pacer(vec![memory_pressure(false)]);
        let limits = crate::gc::CapacitySliceLimits {
            budget: Duration::from_secs(6),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        let report = broker.run_capacity_removal_slice_with(Instant::now(), limits);
        drop(scope);
        let report = report.unwrap().expect("a pass ran");
        // An unmeasured checkout is charged 10,000 entries: 10 s at the
        // default rate, longer than the 6 s slice.
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert_eq!(worktrees.iter().filter(|path| path.exists()).count(), 1);
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("slice_deadline"),
            "{slice:#?}"
        );
        assert!(slice.pacer_unpaid_ms > 0, "{slice:#?}");
        assert!(
            slice
                .retry_in_ms
                .is_some_and(|wait| wait >= i64::try_from(slice.pacer_unpaid_ms).unwrap()),
            "{slice:#?}"
        );
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
    }

    /// Passes whose selected checkout is kept at the last step are bounded:
    /// the slice stops after four of them, holds the lock at most about half
    /// the time, never judges the same checkout twice, and waits a minute.
    #[cfg(unix)]
    #[test]
    fn kept_only_passes_are_bounded_and_never_repeat_a_checkout() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let mut blocked = Vec::new();
        for index in 0..5 {
            let (session, worktree) =
                closed_landed_checkout(&mut broker, &format!("kept at apply {index}"));
            record_merged_pull_request(&mut broker, session.id, 200 + index);
            let admission = crate::file_lock::artifact_use::begin_artifact_use(
                broker.main_root(),
                &worktree,
                session.id,
                "apply-only blocker",
            )
            .unwrap();
            let crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer) =
                admission
            else {
                panic!("artifact-use protocol should be available for this test");
            };
            drop(producer);
            blocked.push(worktree);
        }
        limit_checkouts_to_one(&broker);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("pressure runs a slice");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(blocked.iter().all(|path| path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("kept_only_limit"),
            "{slice:#?}"
        );
        assert_eq!(slice.passes, 4, "{slice:#?}");
        assert_eq!(slice.kept_only_passes, 4, "{slice:#?}");
        let mut kept = report
            .kept
            .iter()
            .map(|kept| kept.worktree.clone())
            .collect::<Vec<_>>();
        let judged = kept.len();
        kept.sort();
        kept.dedup();
        assert_eq!(kept.len(), judged, "no checkout judged twice: {report:#?}");
        assert!(
            slice.retry_in_ms.is_some_and(|wait| wait >= 60_000),
            "{slice:#?}"
        );
    }

    /// A pass's selection and revalidation honor the pass budget, not the
    /// slice's: with a pass budget too short to revalidate, nothing is
    /// removed although the slice itself has time.
    #[cfg(unix)]
    #[test]
    fn a_capacity_pass_is_bounded_by_the_pass_budget() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "pass budget");
        limit_checkouts_to_one(&broker);
        let limits = crate::gc::CapacitySliceLimits {
            pass_budget: Duration::from_millis(1),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        let report = broker
            .run_capacity_removal_slice_with(Instant::now(), limits)
            .unwrap();
        assert!(
            report.is_none_or(|report| report.removed.is_empty()),
            "nothing is removed inside a 1 ms pass"
        );
        assert!(landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.pass_budget_ms, 1, "{slice:#?}");
    }

    /// Without capacity pressure the worker still removes a closed checkout
    /// whose pull request merged: removal follows merges, not pressure. The
    /// slice runs with the steady limits, the priority lane names the
    /// checkout, and the next steady slice waits a minute.
    #[cfg(unix)]
    #[test]
    fn without_pressure_the_worker_removes_a_merged_checkout() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (session, landed) = closed_landed_checkout(&mut broker, "steady merged");
        record_merged_pull_request(&mut broker, session.id, 301);
        assert!(
            !broker.checkout_capacity_pressure(Some(Instant::now() + Duration::from_secs(30))),
            "one checkout of the default limit is no pressure"
        );
        assert_eq!(
            broker
                .checkout_removal_trigger_due(super::now_ms())
                .unwrap(),
            Some(crate::gc::CheckoutRemovalTrigger::Steady),
            "landed removal is due without pressure"
        );

        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs without pressure");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        assert!(
            report.lanes.is_some_and(|lanes| !lanes.capacity_pressure),
            "{report:#?}"
        );
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(slice.outcome, "removed", "{slice:#?}");
        assert_eq!(slice.removal_cap, 4, "the steady allowance: {slice:#?}");
        assert!(slice.budget_ms <= 60_000, "{slice:#?}");
        assert!(slice.priority_examined >= 1, "{slice:#?}");
        assert_eq!(slice.priority_removed, 1, "{slice:#?}");
        assert_eq!(slice.lap_removed, 0, "{slice:#?}");
        assert_eq!(slice.idle_streak, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
        assert!(
            !broker.capacity_removal_due(super::now_ms()).unwrap(),
            "the next steady slice waits its minute"
        );
    }

    /// Without pressure a closed checkout whose work never landed is judged
    /// and kept: the steady trigger changes when removal runs, never what
    /// is removable.
    #[cfg(unix)]
    #[test]
    fn without_pressure_a_checkout_whose_work_did_not_land_is_kept() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        git(&root, &["remote", "set-head", "origin", "main"]);
        let (session, worktree) = landed_lane_session(&mut broker, "steady unlanded", None);
        broker
            .close_abandoning_unpushed(session.id, "closed unlanded checkout fixture")
            .unwrap();
        // Even a recorded merge does not stand in for the landing proof.
        record_merged_pull_request(&mut broker, session.id, 302);

        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs without pressure");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(worktree.join(format!("work-{}.txt", session.id)).is_file());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(slice.removed, 0, "{slice:#?}");
        assert!(slice.examined >= 1, "the checkout was judged: {slice:#?}");
        assert!(
            slice
                .kept_by_code
                .get("not_contained")
                .is_some_and(|kept| *kept >= 1),
            "kept because its commit is not on the default branch: {slice:#?}"
        );
    }

    /// A steady slice that finds nothing removable backs off, and each such
    /// slice in a row waits longer (5, then 10 minutes), so an idle
    /// repository costs a worker run at most every half hour. A removal
    /// resets the wait to a minute.
    #[cfg(unix)]
    #[test]
    fn steady_removal_backs_off_while_nothing_is_removable() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "steady keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();

        let first = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs without pressure");
        assert!(first.removed.is_empty(), "{first:#?}");
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(slice.idle_streak, 1, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(5 * 60_000), "{slice:#?}");
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
        assert!(
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .is_none(),
            "no slice runs before the back-off ends"
        );

        // Past the back-off: the next empty slice waits twice as long.
        broker.end_steady_removal_back_off_for_test();
        assert!(broker.capacity_removal_due(super::now_ms()).unwrap());
        broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("the back-off ended");
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.idle_streak, 2, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(10 * 60_000), "{slice:#?}");
        assert!(kept.join("notes.txt").is_file());

        // A landed checkout ends the idle run.
        let (session, landed) = closed_landed_checkout(&mut broker, "steady after idle");
        record_merged_pull_request(&mut broker, session.id, 303);
        broker.end_steady_removal_back_off_for_test();
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("the back-off ended");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.idle_streak, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
    }

    /// `[cleanup] auto_remove = false` turns the steady removal off too.
    #[cfg(unix)]
    #[test]
    fn steady_removal_honors_auto_remove_off() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[cleanup]\nauto_remove = false\n"));
        let (session, landed) = closed_landed_checkout(&mut broker, "steady disabled");
        record_merged_pull_request(&mut broker, session.id, 304);
        assert_eq!(
            broker
                .checkout_removal_trigger_due(super::now_ms())
                .unwrap(),
            None
        );
        assert!(
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .is_none()
        );
        assert!(landed.exists());
    }

    /// Git failing to remove checkouts after it started deleting stops the
    /// slice at the second failure, with the ten-minute back-off, even
    /// though removal allowance and passes remain.
    #[cfg(unix)]
    #[test]
    fn a_slice_stops_after_two_failed_removals() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..4)
            .map(|index| closed_landed_checkout(&mut broker, &format!("fails {index}")).1)
            .collect::<Vec<_>>();
        limit_checkouts_to_one(&broker);
        super::GIT_REMOVAL_FAILS_PART_WAY.with(|flag| flag.set(true));
        let report = broker.run_capacity_removal_slice(Instant::now());
        super::GIT_REMOVAL_FAILS_PART_WAY.with(|flag| flag.set(false));
        let report = report.unwrap().expect("pressure runs a slice");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(worktrees.iter().all(|path| path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "pressure", "{slice:#?}");
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("removal_failures"),
            "{slice:#?}"
        );
        assert_eq!(slice.removal_failures, 2, "{slice:#?}");
        assert_eq!(slice.passes, 2, "{slice:#?}");
        assert_eq!(
            slice.kept_by_code.get("removal_failed"),
            Some(&2),
            "{slice:#?}"
        );
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
    }

    /// Steady removal never touches an open session's checkout: it judges
    /// closed checkouts only.
    #[cfg(unix)]
    #[test]
    fn steady_removal_leaves_an_open_sessions_checkout() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let open = broker.start_worktree("steady open", None).unwrap();
        let open_path = PathBuf::from(&open.worktree_path);
        let (session, landed) = closed_landed_checkout(&mut broker, "steady closed");
        record_merged_pull_request(&mut broker, session.id, 305);
        record_merged_pull_request(&mut broker, open.id, 306);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a closed checkout makes steady removal due");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        assert!(open_path.is_dir(), "the open session keeps its checkout");
        assert!(
            report
                .removed
                .iter()
                .all(|removed| !removed.sessions.contains(&open.id)),
            "{report:#?}"
        );
    }

    /// A landed checkout still inside `closed_worktree_grace_hours` is kept
    /// (`in_grace`) by the steady slice and removed by a later one once the
    /// grace no longer covers it.
    #[cfg(unix)]
    #[test]
    fn steady_removal_waits_for_the_closed_worktree_grace() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 1\n",
        )
        .unwrap();
        let (session, landed) = closed_landed_checkout(&mut broker, "steady in grace");
        record_merged_pull_request(&mut broker, session.id, 307);
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a closed checkout makes steady removal due");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.kept_by_code.get("in_grace"), Some(&1), "{slice:#?}");

        // The grace no longer covers it (here: the grace is set to zero).
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 0\n",
        )
        .unwrap();
        broker.end_steady_removal_back_off_for_test();
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("the back-off ended");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
    }

    /// One steady selection judges at most 16 groups of the lap, and the
    /// lap cursor stops on the last group it judged, so the next steady
    /// pass resumes there.
    #[cfg(unix)]
    #[test]
    fn a_steady_selection_judges_at_most_sixteen_lap_groups() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        // Cleanup requests not yet due: only the lap names these groups.
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 72\nclosed_worktree_grace_hours = 72\n",
        )
        .unwrap();
        git(&root, &["remote", "set-head", "origin", "main"]);
        let mut ids = Vec::new();
        for index in 0..18 {
            let session = broker
                .start_worktree(&format!("steady lap {index}"), None)
                .unwrap();
            broker
                .close_abandoning_unpushed(session.id, "steady lap fixture")
                .unwrap();
            ids.push(session.id);
        }
        ids.sort_unstable();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .is_some_and(|state| state.complete)
        {}
        let mut judged = Vec::new();
        let plan = broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Steady,
                false,
                |_, group, _, _, _| {
                    judged.push(group.cursor_session_id);
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected steady inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        let lanes = plan.report.lanes.unwrap();
        assert_eq!(lanes.removal_priority_examined, 0, "{lanes:?}");
        assert_eq!(
            judged.len(),
            super::STEADY_LAP_GROUPS_PER_PASS,
            "{judged:?}"
        );
        assert_eq!(lanes.removal_examined, super::STEADY_LAP_GROUPS_PER_PASS);
        assert_eq!(judged, ids[..16].to_vec(), "oldest first");
        let cursor = decode_auto_cleanup_cursor(
            broker
                .store()
                .meta_get(AUTO_CLEANUP_CURSOR_KEY)
                .unwrap()
                .as_deref(),
        );
        assert_eq!(
            cursor.after_session_id, ids[15],
            "the cursor stops on the last group judged"
        );
    }

    /// A closed checkout the removal proofs keep: its notes are uncommitted.
    #[cfg(unix)]
    fn closed_checkout_with_notes(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, PathBuf) {
        let (session, worktree) = closed_landed_checkout(broker, label);
        std::fs::write(worktree.join("notes.txt"), "somebody's notes\n").unwrap();
        (session, worktree)
    }

    /// The busiest repository's stall: a steady pass that selects nothing
    /// used to end the slice although most of the lap was never judged. Here
    /// the first pass judges eight kept merged checkouts and sixteen kept lap
    /// groups and selects nothing; the slice goes on and its next pass
    /// reaches and removes the landed checkout.
    #[cfg(unix)]
    #[test]
    fn an_unfinished_steady_lap_reaches_a_landed_checkout_in_the_same_slice() {
        // No checkout limit: two dozen closed checkouts never make pressure.
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let mut kept = Vec::new();
        for index in 0..8 {
            let (session, worktree) =
                closed_checkout_with_notes(&mut broker, &format!("merged kept {index}"));
            record_merged_pull_request(&mut broker, session.id, 400 + index);
            kept.push(worktree);
        }
        for index in 0..super::STEADY_LAP_GROUPS_PER_PASS {
            kept.push(closed_checkout_with_notes(&mut broker, &format!("lap kept {index}")).1);
        }
        let (_session, landed) = closed_landed_checkout(&mut broker, "landed late in the lap");

        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs without pressure");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(
            !landed.exists(),
            "the landed checkout is removed in this slice"
        );
        assert!(kept.iter().all(|path| path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert!(
            slice.passes >= 2,
            "the slice went on past its first pass: {slice:#?}"
        );
        assert!(
            slice.priority_memo_skips >= 8,
            "the kept merged checkouts are not judged again: {slice:#?}"
        );
        assert_eq!(slice.idle_streak, 0, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
    }

    /// A steady slice that removes nothing while the lap is unfinished is
    /// not idle: the next one is due a minute later and the back-off does
    /// not grow. Only the slice that reaches the end of the lap with nothing
    /// removed starts the back-off (5 minutes).
    #[cfg(unix)]
    #[test]
    fn steady_removal_backs_off_only_after_a_full_empty_lap() {
        // No checkout limit: thirty closed checkouts never make pressure.
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let kept = (0..30)
            .map(|index| closed_checkout_with_notes(&mut broker, &format!("kept {index}")).1)
            .collect::<Vec<_>>();
        // Room for one pass only: the lock gap after it (at least half a
        // second) leaves less than the five-second minimum slice.
        let one_pass = crate::gc::CapacitySliceLimits {
            budget: Duration::from_millis(5_400),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        broker
            .run_capacity_removal_slice_with(Instant::now(), one_pass)
            .unwrap();
        let first = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(first.trigger, "steady", "{first:#?}");
        assert!(
            !first.lap_completed,
            "thirty groups need more than one pass: {first:#?}"
        );
        assert_eq!(first.outcome, "lap_unfinished", "{first:#?}");
        assert_eq!(
            first.idle_streak, 0,
            "no back-off before a full lap: {first:#?}"
        );
        assert_eq!(first.retry_in_ms, Some(60_000), "{first:#?}");

        let mut completed = None;
        for _ in 0..4 {
            broker.end_steady_removal_back_off_for_test();
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .expect("a steady slice runs");
            let slice = broker.last_capacity_removal_slice().unwrap();
            assert_eq!(slice.removed, 0, "{slice:#?}");
            if slice.lap_completed {
                completed = Some(slice);
                break;
            }
            assert_eq!(slice.idle_streak, 0, "{slice:#?}");
            assert_eq!(slice.retry_in_ms, Some(60_000), "{slice:#?}");
        }
        let completed = completed.expect("the lap ends within a few slices");
        assert_eq!(
            completed.stop_reason.as_deref(),
            Some("lap_complete"),
            "{completed:#?}"
        );
        assert_eq!(completed.outcome, "nothing_removable", "{completed:#?}");
        assert_eq!(completed.idle_streak, 1, "{completed:#?}");
        assert_eq!(completed.retry_in_ms, Some(5 * 60_000), "{completed:#?}");
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
        assert!(kept.iter().all(|path| path.exists()));
    }

    /// A priority group kept for a reason only a state change can lift is
    /// not judged ahead of the lap again while its state is unchanged; a
    /// newly merged checkout is, and so is a kept one whose checkout changed.
    #[cfg(unix)]
    #[test]
    fn stable_kept_priority_groups_are_skipped_until_their_state_changes() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(10);
        for (index, session) in sessions.iter().take(8).enumerate() {
            record_merged_pull_request(&mut broker, session.id, 600 + index as i64);
        }
        let pass = |broker: &mut crate::Broker| {
            broker
                .auto_cleanup_plan_with_group_inspector_limited(
                    None,
                    1,
                    super::RemovalSelection::Steady,
                    false,
                    |_, group, _, _, _| {
                        Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                            worktree: group.worktree.to_string_lossy().into_owned(),
                            sessions: group.sessions.iter().map(|session| session.id).collect(),
                            reason: super::OWNERSHIP_ADOPTED_ONLY.to_string(),
                            ..Default::default()
                        }))
                    },
                )
                .unwrap()
                .report
                .lanes
                .unwrap()
        };
        let memo = |broker: &mut crate::Broker| {
            super::PriorityMemo::decode(
                broker
                    .store()
                    .meta_get(super::PRIORITY_MEMO_KEY)
                    .unwrap()
                    .as_deref(),
            )
        };
        let key = |session: &crate::Session| {
            std::fs::canonicalize(&session.worktree_path)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let first = pass(&mut broker);
        assert_eq!(first.removal_priority_examined, 8, "{first:?}");
        let before = memo(&mut broker);
        for session in sessions.iter().take(8) {
            let entry = &before.entries[&key(session)];
            assert_eq!(
                entry.stable.as_ref().map(|stable| stable.code.as_str()),
                Some("ownership_adopted_only"),
                "{entry:?}"
            );
        }

        // A pull request merges, and one kept checkout's files change.
        record_merged_pull_request(&mut broker, sessions[9].id, 609);
        std::fs::write(
            PathBuf::from(&sessions[0].worktree_path).join("changed.txt"),
            "x",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let second = pass(&mut broker);
        assert_eq!(
            second.removal_priority_examined, 2,
            "the new merge and the changed checkout: {second:?}"
        );
        assert_eq!(second.removal_priority_memo_skips, 7, "{second:?}");
        let after = memo(&mut broker);
        for session in sessions.iter().take(8).skip(1) {
            assert_eq!(
                after.entries[&key(session)].judged_at_ms,
                before.entries[&key(session)].judged_at_ms,
                "an unchanged stable keep is not judged ahead of the lap again"
            );
        }
        assert!(after.entries.contains_key(&key(&sessions[9])));
        assert_ne!(
            after.entries[&key(&sessions[0])].judged_at_ms,
            before.entries[&key(&sessions[0])].judged_at_ms
        );
    }

    /// Only a keep no running process, timer or lock can lift is remembered
    /// past the usual re-judge interval; a running gate, the grace period or
    /// a held lease stays retryable after it, and a changed merged state
    /// ends either at once.
    #[test]
    fn transient_priority_keeps_stay_retryable() {
        use super::{
            AutoCleanupCheckoutGroup, KEEP_MEMO_MAX_AGE_MS, PRIORITY_REJUDGE_MS, PriorityMemo,
            PriorityOutcome, StablePriorityKeep,
        };
        let group = AutoCleanupCheckoutGroup {
            worktree: std::path::PathBuf::from("/nonexistent/priority/checkout"),
            sessions: Vec::new(),
            complete: true,
            cursor_session_id: 1,
        };
        for code in [
            "gate_running",
            "in_grace",
            "lease_held",
            "process_use",
            "artifact_use",
            "not_contained",
        ] {
            assert!(
                StablePriorityKeep::for_kept_group(&group, code, &[]).is_none(),
                "{code}"
            );
        }
        assert!(
            StablePriorityKeep::for_kept_group(&group, "ignored_content", &[]).is_none(),
            "ignored content without its blockers has nothing to fingerprint"
        );
        let stable = StablePriorityKeep::for_kept_group(&group, "checkout_missing", &[])
            .expect("a missing checkout is a stable keep");
        assert!(stable.still_holds(&group.worktree, &[]));

        let now = 1_000_000;
        let mut memo = PriorityMemo::default();
        memo.remember_with(
            "/stable".into(),
            true,
            PriorityOutcome::Kept,
            Some(stable.clone()),
            now,
        );
        memo.remember_with("/gate".into(), true, PriorityOutcome::Kept, None, now);
        assert!(memo.recently_judged("/stable", true, now + PRIORITY_REJUDGE_MS));
        assert!(!memo.recently_judged("/stable", true, now + KEEP_MEMO_MAX_AGE_MS));
        assert!(!memo.recently_judged("/gate", true, now + PRIORITY_REJUDGE_MS));
        assert!(
            !memo.recently_judged("/stable", false, now),
            "merged state changed"
        );
        memo.remember_with(
            "/selected".into(),
            true,
            PriorityOutcome::Selected,
            Some(stable),
            now,
        );
        assert!(memo.entries["/selected"].stable.is_none());
    }

    /// After a pressure episode ends (here the limit was raised) the worker
    /// switches to steady removal at once: the pressure back-off does not
    /// hold it.
    #[cfg(unix)]
    #[test]
    fn steady_removal_follows_a_pressure_episode() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "pressure keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        limit_checkouts_to_one(&broker);
        broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("pressure runs a slice");
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "pressure", "{slice:#?}");
        assert!(slice.retry_in_ms.is_some(), "{slice:#?}");

        std::fs::remove_file(broker.main_root().join(".aethyme/config.toml")).unwrap();
        let (session, landed) = closed_landed_checkout(&mut broker, "steady after pressure");
        record_merged_pull_request(&mut broker, session.id, 308);
        assert_eq!(
            broker
                .checkout_removal_trigger_due(super::now_ms())
                .unwrap(),
            Some(crate::gc::CheckoutRemovalTrigger::Steady)
        );
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("steady removal runs despite the pressure back-off");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        assert!(kept.join("notes.txt").is_file());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
    }

    /// With `max_checkouts = 0` there is never pressure, even when a count
    /// recorded against an earlier limit says otherwise: the slice runs as
    /// steady removal.
    #[cfg(unix)]
    #[test]
    fn a_disabled_limit_runs_steady_removal_despite_a_stale_count() {
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let (session, landed) = closed_landed_checkout(&mut broker, "no limit");
        record_merged_pull_request(&mut broker, session.id, 309);
        broker.record_checkout_usage(5, 1);
        assert!(broker.recorded_checkout_capacity_pressure());
        assert_eq!(
            broker
                .checkout_removal_trigger_due(super::now_ms())
                .unwrap(),
            Some(crate::gc::CheckoutRemovalTrigger::Steady)
        );
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("steady removal runs with the limit disabled");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(slice.removal_cap, 4, "{slice:#?}");
    }

    /// The failed-removal cap holds on the steady path too, and its wait is
    /// not shortened by a checkout that lands meanwhile.
    #[cfg(unix)]
    #[test]
    fn a_steady_slice_stops_after_two_failed_removals() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let worktrees = (0..3)
            .map(|index| closed_landed_checkout(&mut broker, &format!("steady fails {index}")).1)
            .collect::<Vec<_>>();
        super::GIT_REMOVAL_FAILS_PART_WAY.with(|flag| flag.set(true));
        let report = broker.run_capacity_removal_slice(Instant::now());
        super::GIT_REMOVAL_FAILS_PART_WAY.with(|flag| flag.set(false));
        let report = report.unwrap().expect("a steady slice runs");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(worktrees.iter().all(|path| path.exists()));
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.trigger, "steady", "{slice:#?}");
        assert_eq!(
            slice.stop_reason.as_deref(),
            Some("removal_failures"),
            "{slice:#?}"
        );
        assert_eq!(slice.removal_failures, 2, "{slice:#?}");
        assert_eq!(slice.retry_in_ms, Some(600_000), "{slice:#?}");
        let (_session, _landed) = closed_landed_checkout(&mut broker, "lands during the wait");
        assert!(
            !broker.steady_removal_due(super::now_ms()).unwrap(),
            "a new landing does not cut the failure back-off short"
        );
    }

    /// Steady removal needs a closed checkout to be due: a repository with
    /// none never wakes the worker for it.
    #[cfg(unix)]
    #[test]
    fn steady_removal_is_not_due_without_a_closed_checkout() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let _open = broker.start_worktree("only open work", None).unwrap();
        assert!(!broker.steady_removal_due(super::now_ms()).unwrap());
        assert_eq!(
            broker
                .checkout_removal_trigger_due(super::now_ms())
                .unwrap(),
            None
        );
        assert!(
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .is_none()
        );
        assert!(broker.last_capacity_removal_slice().is_none());
    }

    /// A cleanup request that becomes due after the last steady slice makes
    /// the next one due at once, without waiting out the idle back-off, so
    /// removal follows the landing.
    #[cfg(unix)]
    #[test]
    fn a_newly_due_cleanup_request_ends_the_idle_back_off() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "idle keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("a steady slice runs");
        let slice = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(slice.retry_in_ms, Some(5 * 60_000), "{slice:#?}");
        assert!(!broker.steady_removal_due(super::now_ms()).unwrap());

        std::thread::sleep(Duration::from_millis(5));
        let (_session, landed) = closed_landed_checkout(&mut broker, "lands after idle");
        assert!(
            broker.steady_removal_due(super::now_ms()).unwrap(),
            "its grace (zero here) ended after the last steady slice"
        );
        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("due without waiting the back-off");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
    }

    /// `broker start` at the checkout limit, waiting for a held GC lock,
    /// announces itself so the worker yields, and runs its pass once the
    /// lock is free.
    #[cfg(unix)]
    #[test]
    fn start_announces_itself_while_it_waits_for_the_gc_lock() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "start waits");
        limit_checkouts_to_one(&broker);
        let root = broker.main_root().to_path_buf();
        let held = crate::gc::GcLock::acquire(&root).unwrap();
        let notice = root
            .join(".aethyme/run/gc-lock-waiters")
            .join(std::process::id().to_string());
        let holder = std::thread::spawn(move || {
            let mut announced = false;
            for _ in 0..100 {
                if notice.is_file() {
                    announced = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(held);
            announced
        });
        let report = broker
            .auto_cleanup_for_checkout_capacity(
                Duration::from_secs(20),
                Duration::from_secs(20),
                None,
                false,
                super::RemovalSelection::Probe,
            )
            .unwrap();
        assert!(holder.join().unwrap(), "the waiter announced itself");
        let report = report.expect("the lock was released within the wait");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
    }

    /// A slice that finds the GC lock held backs off briefly instead of
    /// letting every broker open respawn the worker into the same lock.
    #[cfg(unix)]
    #[test]
    fn a_worker_capacity_slice_backs_off_when_the_gc_lock_is_held() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "lock held");
        let root = broker.main_root().to_path_buf();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();
        let held = crate::gc::GcLock::acquire(&root).unwrap();
        assert!(
            broker
                .run_capacity_removal_slice(Instant::now())
                .unwrap()
                .is_none()
        );
        assert!(!broker.capacity_removal_due(super::now_ms()).unwrap());
        drop(held);
        assert!(landed.exists());
        let slice = broker
            .last_capacity_removal_slice()
            .expect("a slice records itself");
        assert_eq!(slice.outcome, "lock_busy", "{slice:#?}");
        assert_eq!(slice.passes, 0, "{slice:#?}");
    }

    /// A slice whose checkout path index is mid-lap, with more of the lap
    /// left than one refresh call advances, finishes the lap and then judges
    /// checkouts. It used to find no inventory, examine nothing and back off
    /// for ten minutes; on a repository with thousands of registered
    /// sessions that happened whenever another lane had rewound the lap.
    #[cfg(unix)]
    #[test]
    fn a_capacity_slice_finishes_a_long_cohort_index_lap_before_selecting() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, landed) = closed_landed_checkout(&mut broker, "long lap frees a slot");
        let (_kept_session, kept) = closed_landed_checkout(&mut broker, "long lap keeps notes");
        std::fs::write(kept.join("notes.txt"), "somebody's notes\n").unwrap();
        let root = broker.main_root().to_path_buf();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();
        let namespace = crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE;
        let mut complete = false;
        for _ in 0..16 {
            complete = broker
                .refresh_cleanup_cohort_aliases(None)
                .unwrap()
                .is_some_and(|state| state.complete);
            if complete {
                break;
            }
        }
        assert!(complete, "the fixture's path index completes");
        // Another lane asks for fresh filesystem coverage: the completed lap
        // is rewound to its start.
        let rewound = broker
            .store()
            .begin_cleanup_cohort_alias_refresh(namespace, super::now_ms())
            .unwrap();
        assert!(!rewound.complete);
        assert_eq!(rewound.after_session_id, 0);

        // One session per page and one page per refresh call: the lap is
        // longer than any single call, as thousands of sessions are.
        super::COHORT_SCAN_LIMITS.with(|limits| limits.set(Some((1, 1))));
        let report = broker.run_capacity_removal_slice(Instant::now());
        super::COHORT_SCAN_LIMITS.with(|limits| limits.set(None));
        let report = report.unwrap().expect("pressure runs a slice");
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!landed.exists());
        assert!(kept.join("notes.txt").is_file());
        let slice = broker
            .last_capacity_removal_slice()
            .expect("a slice records itself");
        assert_eq!(slice.outcome, "removed", "{slice:#?}");
        assert!(slice.index_complete, "{slice:#?}");
    }

    /// The slice's own result survives a later pass's report: `gc plan` and
    /// `status` show why it removed nothing, counted per keep code, with the
    /// ignored directory that holds a checkout and the line that would
    /// release it. Nothing kept is removed.
    #[cfg(unix)]
    #[test]
    fn a_capacity_slice_that_removes_nothing_says_why_in_its_own_record() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        std::fs::write(root.join(".git/info/exclude"), "project-cache/\n").unwrap();
        let (_cached_session, cached) = closed_landed_checkout(&mut broker, "slice keeps a cache");
        touch(&cached, "project-cache/quality/index.json");
        let (_noted_session, noted) = closed_landed_checkout(&mut broker, "slice keeps notes");
        std::fs::write(noted.join("notes.txt"), "somebody's notes\n").unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[worktrees]\nmax_checkouts = 1\n",
        )
        .unwrap();

        let report = broker
            .run_capacity_removal_slice(Instant::now())
            .unwrap()
            .expect("pressure runs a slice");
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(cached.join("project-cache/quality/index.json").is_file());
        assert!(noted.join("notes.txt").is_file());
        let slice = broker
            .last_capacity_removal_slice()
            .expect("a slice records itself");
        assert_eq!(slice.outcome, "nothing_removable", "{slice:#?}");
        assert_eq!(slice.removed, 0);
        assert!(slice.examined >= 2, "{slice:#?}");
        assert_eq!(
            slice.kept_by_code.get("ignored_content"),
            Some(&1),
            "{slice:#?}"
        );
        assert_eq!(
            slice.kept_by_code.get("uncommitted_changes"),
            Some(&1),
            "{slice:#?}"
        );
        assert_eq!(
            slice.blocked_by_ignored,
            vec![super::BlockedByIgnored {
                name: "project-cache/".into(),
                checkouts: 1,
                config_line: Some("[cleanup] regenerable = [\"project-cache\"]".into()),
                config_refusal: None,
            }]
        );
        assert!(slice.index_complete, "{slice:#?}");
        assert_eq!(slice.lap_remaining, 0, "{slice:#?}");
        assert!(
            slice
                .retry_after_ms
                .is_some_and(|at| at > super::now_ms() + 5 * 60_000),
            "a finished lap with nothing removable backs off fully: {slice:#?}"
        );

        // A broker open's pass writes the shared report; the slice record
        // is not part of it and is still shown alongside it.
        broker
            .record_auto_cleanup_report(&AutoCleanupReport {
                enabled: true,
                deferred: 2,
                ..AutoCleanupReport::default()
            })
            .unwrap();
        let stored = broker
            .store()
            .meta_get(super::AUTO_CLEANUP_LAST_REPORT_KEY)
            .unwrap()
            .unwrap();
        assert!(!stored.contains("capacity_removal"), "{stored}");
        let shown = broker.last_auto_cleanup_report().unwrap();
        assert_eq!(shown.capacity_removal.as_ref(), Some(&slice));
        let status = broker.status(super::now_ms()).unwrap();
        assert_eq!(status.capacity_removal.as_ref(), Some(&slice));
    }

    /// A closed landed checkout whose only leftover is a cache its tool
    /// tagged is removed; the same cache holding a `.env` keeps its checkout,
    /// reported as `ignored_content`.
    #[cfg(unix)]
    #[test]
    fn a_closed_landed_checkout_holding_only_a_tagged_cache_is_removed() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        std::fs::write(root.join(".git/info/exclude"), "tool-cache/\n").unwrap();
        let (_tagged_session, tagged) = closed_landed_checkout(&mut broker, "tagged cache only");
        touch(&tagged, "tool-cache/objects/ab/cdef");
        std::fs::write(tagged.join("tool-cache/CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
        let (_guarded_session, guarded) =
            closed_landed_checkout(&mut broker, "tagged cache with a secret");
        touch(&guarded, "tool-cache/objects/ab/cdef");
        touch(&guarded, "tool-cache/.env");
        std::fs::write(guarded.join("tool-cache/CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
        let guarded_text = std::fs::canonicalize(&guarded)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let mut guarded_codes = Vec::new();
        for _ in 0..3 {
            let report = broker.auto_cleanup_now(None).unwrap().unwrap();
            guarded_codes.extend(
                report
                    .kept
                    .iter()
                    .filter(|kept| kept.worktree == guarded_text)
                    .map(|kept| kept.code.clone()),
            );
            if !tagged.exists() && !guarded_codes.is_empty() {
                break;
            }
        }
        assert!(!tagged.exists(), "the tagged cache alone does not keep it");
        assert!(guarded.join("tool-cache/.env").is_file());
        assert!(
            guarded_codes.iter().any(|code| code == "ignored_content"),
            "{guarded_codes:?}"
        );
    }

    /// An ignored directory nothing recognises keeps its closed checkout,
    /// is named in the report with the line that would release it, is not
    /// re-listed while nothing changes, and is released once configured.
    #[cfg(unix)]
    #[test]
    fn an_unknown_ignored_directory_is_named_remembered_and_released_by_configuration() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        std::fs::write(root.join(".git/info/exclude"), "project-cache/\n").unwrap();
        let (_session, worktree) = closed_landed_checkout(&mut broker, "unknown ignored cache");
        touch(&worktree, "project-cache/quality/index.json");
        let text = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let kept_entry = |report: &AutoCleanupReport| {
            report
                .kept
                .iter()
                .find(|kept| kept.worktree == text)
                .cloned()
                .unwrap_or_else(|| panic!("{report:#?}"))
        };

        let first = broker.auto_cleanup_now(None).unwrap().unwrap();
        let kept = kept_entry(&first);
        assert_eq!(kept.code, "ignored_content", "{kept:?}");
        assert_eq!(kept.ignored_blockers, vec!["project-cache/".to_string()]);
        assert_eq!(
            first.blocked_by_ignored,
            vec![super::BlockedByIgnored {
                name: "project-cache/".into(),
                checkouts: 1,
                config_line: Some("[cleanup] regenerable = [\"project-cache\"]".into()),
                config_refusal: None,
            }]
        );
        assert_eq!(first.lanes.unwrap().removal_memo_skips, 0);
        let memo = broker.store().meta_get(super::KEEP_MEMO_KEY).unwrap();
        assert_eq!(
            super::blocked_by_ignored_from_meta(memo.as_deref()),
            first.blocked_by_ignored,
            "status reads the same aggregate"
        );

        // Unchanged: the remembered verdict is reused, still reported.
        let second = broker.auto_cleanup_now(None).unwrap().unwrap();
        assert_eq!(second.lanes.unwrap().removal_memo_skips, 1, "{second:#?}");
        assert_eq!(kept_entry(&second).ignored_blockers, kept.ignored_blockers);

        // A change inside the blocker re-proves the checkout.
        touch(&worktree, "project-cache/new-report.json");
        let third = broker.auto_cleanup_now(None).unwrap().unwrap();
        assert_eq!(third.lanes.unwrap().removal_memo_skips, 0, "{third:#?}");
        assert_eq!(kept_entry(&third).code, "ignored_content");
        assert!(worktree.join("project-cache/quality/index.json").is_file());

        // The repository declares it regenerable: the policy change alone
        // invalidates the remembered verdict and the checkout goes.
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nregenerable = [\"project-cache\"]\n",
        )
        .unwrap();
        for _ in 0..2 {
            if !worktree.exists() {
                break;
            }
            broker.auto_cleanup_now(None).unwrap().unwrap();
        }
        assert!(
            !worktree.exists(),
            "the configured cache no longer keeps it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_standalone_ignored_file_is_named_with_its_line_and_released_by_a_file_pattern() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        std::fs::write(root.join(".git/info/exclude"), "quality-*.json\n").unwrap();
        let (_session, worktree) = closed_landed_checkout(&mut broker, "generated report file");
        touch(&worktree, "quality-report.json");
        // A link the same pattern names is unlinked; its target is not touched.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("kept.json"), "outside\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("kept.json"),
            worktree.join("quality-link.json"),
        )
        .unwrap();
        let text = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let first = broker.auto_cleanup_now(None).unwrap().unwrap();
        let kept = first
            .kept
            .iter()
            .find(|kept| kept.worktree == text)
            .cloned()
            .unwrap_or_else(|| panic!("{first:#?}"));
        assert_eq!(kept.code, "ignored_content", "{kept:?}");
        assert!(
            kept.ignored_blockers
                .contains(&"quality-report.json".to_string()),
            "{kept:?}"
        );
        let report_row = first
            .blocked_by_ignored
            .iter()
            .find(|row| row.name == "quality-report.json")
            .cloned()
            .unwrap_or_else(|| panic!("{first:#?}"));
        assert_eq!(
            report_row,
            super::BlockedByIgnored {
                name: "quality-report.json".into(),
                checkouts: 1,
                config_line: Some("[cleanup] regenerable = [\"quality-report.json\"]".into()),
                config_refusal: None,
            }
        );

        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[cleanup]\nregenerable = [\"quality-*.json\"]\n",
        )
        .unwrap();
        for _ in 0..2 {
            if !worktree.exists() {
                break;
            }
            broker.auto_cleanup_now(None).unwrap().unwrap();
        }
        assert!(
            !worktree.exists(),
            "the file pattern no longer keeps the checkout"
        );
        assert!(outside.path().join("kept.json").is_file());
    }

    #[test]
    fn lane_shares_give_removal_the_larger_part_under_capacity_pressure() {
        use std::time::{Duration, Instant};
        let started = Instant::now();
        let end = started + Duration::from_secs(40);
        assert_eq!(
            super::lane_deadline(started, Some(end), super::retirement_share(false)),
            Some(started + Duration::from_secs(20))
        );
        assert_eq!(
            super::lane_deadline(started, Some(end), super::retirement_share(true)),
            Some(started + Duration::from_secs(10))
        );
        assert_eq!(
            super::lane_deadline(started, None, super::retirement_share(true)),
            None
        );
    }

    #[test]
    fn keep_reasons_have_stable_codes() {
        for (reason, code) in [
            (
                "ignored content requires explicit cleanup: ignored path cache/ is not a recognised build-output directory",
                "ignored_content",
            ),
            (
                "changed before removal: 2 uncommitted or untracked path(s), first: a.txt",
                "uncommitted_changes",
            ),
            (
                "session 7 is within the 24 hour closed-worktree grace period",
                "in_grace",
            ),
            (
                "session 7 is still open (active) in this worktree",
                "session_open",
            ),
            (
                "commits are not contained in the remote default branch",
                "not_contained",
            ),
            (
                "no matching spawn-time worktree instance witness",
                "ownership_unproven",
            ),
            ("something new", "other"),
        ] {
            assert_eq!(super::kept_reason_code(reason), code, "{reason}");
        }
        let mut report = super::AutoCleanupReport {
            kept: vec![super::AutoKeptCheckout {
                reason: "pinned by [cleanup] keep".into(),
                ..Default::default()
            }],
            ..super::AutoCleanupReport::default()
        };
        report.assign_kept_codes();
        assert_eq!(report.kept[0].code, "pinned");
    }

    /// The busiest repository's landed lane: the oldest-first cursor sat on
    /// sessions that will never land while younger sessions with a merged
    /// pull request were never reached. A likely-landed session now closes
    /// in the first pass even while a lap over older sessions is in
    /// progress, and the older sessions' unchangeable declines are
    /// remembered instead of re-proved every lap.
    #[cfg(unix)]
    #[test]
    fn a_likely_landed_session_behind_never_landing_older_ones_closes_first() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        git(&root, &["remote", "set-head", "origin", "main"]);
        let mut older = Vec::new();
        for index in 0..5 {
            let (session, _) =
                landed_lane_session(&mut broker, &format!("never lands {index}"), None);
            quiet_with_gone_holder(&mut broker, &session, 30);
            older.push(session);
        }
        let (landed, _) = landed_lane_session(
            &mut broker,
            "landed behind older sessions",
            Some(aethyme_testkit::MergeStyle::MergeCommit),
        );
        record_merged_pull_request(&mut broker, landed.id, 21);
        quiet_with_gone_holder(&mut broker, &landed, 30);
        // A fixed-high-water lap over the older sessions is in progress: the
        // cursor alone would not reach the landed session in this pass.
        let lap = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: older.last().unwrap().id,
        };
        broker
            .store()
            .meta_set(
                SESSION_RETIREMENT_CURSOR_KEY,
                &serde_json::to_string(&lap).unwrap(),
            )
            .unwrap();

        let report = broker.auto_cleanup_now(None).unwrap().unwrap();
        assert_ne!(
            broker.store().session(landed.id).unwrap().cleanup_state,
            SessionCleanupState::Open,
            "{report:#?}"
        );
        assert!(report.landed_sessions_closed >= 1, "{report:#?}");
        for session in &older {
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Open,
                "an unlanded session stays open"
            );
        }
        let memo: std::collections::BTreeMap<i64, (String, String)> = serde_json::from_str(
            &broker
                .store()
                .meta_get(super::LANDED_DECLINE_MEMO_KEY)
                .unwrap()
                .expect("unchangeable declines are remembered"),
        )
        .unwrap();
        for session in &older {
            assert_eq!(
                memo.get(&session.id).map(|(_, code)| code.as_str()),
                Some("no_landing_evidence"),
                "{memo:?}"
            );
        }
    }

    /// The retirement cursor resumes where the last pass stopped instead of
    /// restarting from the oldest session, and the report says where it is.
    #[cfg(unix)]
    #[test]
    fn landed_lane_resumes_from_the_retirement_cursor() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let mut sessions = Vec::new();
        for label in [
            "first never landed",
            "second never landed",
            "third never landed",
        ] {
            let (session, _) = landed_lane_session(&mut broker, label, None);
            quiet_with_gone_holder(&mut broker, &session, 30);
            sessions.push(session.id);
        }
        let high_water = broker.store().open_session_max_id().unwrap();
        // A previous pass stopped after the first session of this lap.
        let cursor = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: sessions[0],
            high_water_session_id: high_water,
        };
        broker
            .store()
            .meta_set(
                SESSION_RETIREMENT_CURSOR_KEY,
                &serde_json::to_string(&cursor).unwrap(),
            )
            .unwrap();

        let resumed = broker.auto_cleanup_now(None).unwrap().unwrap();
        assert_eq!(
            resumed.landed_sessions_examined, 2,
            "the pass judges only the sessions after the cursor: {resumed:#?}"
        );
        assert_eq!(
            resumed.retirement_cursor,
            Some(super::SessionRetirementCursorPosition {
                after_session_id: high_water,
                high_water_session_id: high_water,
            })
        );

        let next_lap = broker.auto_cleanup_now(None).unwrap().unwrap();
        // The new lap starts again from the oldest session. The two
        // `no_landing_evidence` declines the last pass proved are remembered
        // (same head, tip and evidence), so only the first session is
        // proved; all three are still counted under their decline.
        assert_eq!(
            next_lap.landed_sessions_examined, 1,
            "a finished lap starts the next one from the oldest session: {next_lap:#?}"
        );
        assert_eq!(
            next_lap.landed_sessions_declined.get("no_landing_evidence"),
            Some(&3)
        );
    }

    /// `broker status` shows the last pass's landed-lane counts per reason.
    #[cfg(unix)]
    #[test]
    fn status_reports_landed_lane_counts_per_reason() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (session, _) = landed_lane_session(&mut broker, "never landed", None);
        quiet_with_gone_holder(&mut broker, &session, 30);
        let fresh = broker.status(now_ms()).unwrap();
        assert!(
            fresh.landed_session_retirement.is_none(),
            "nothing is reported before the landed lane judged a session"
        );

        broker.auto_cleanup_now(None).unwrap().unwrap();

        let view = broker.status(now_ms()).unwrap();
        let landed = view
            .landed_session_retirement
            .as_ref()
            .expect("status reports the landed lane");
        assert_eq!(landed.examined, 1);
        assert_eq!(landed.closed, 0);
        assert_eq!(landed.declined.get("no_landing_evidence"), Some(&1));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json["landed_session_retirement"]["declined"]["no_landing_evidence"],
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_closes_old_unbound_sessions_after_the_existing_window() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);

        let summary = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(summary.retired, 1);
        assert_eq!(summary.deferred, 0);
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_records_gone_lease_holder_without_status_and_starts_grace_once() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Oct 26 08:42:41 2026".into(),
            command: "/opt/codex app-server --listen local".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        broker
            .store()
            .claim_lease(session.id, "source.rs", None)
            .unwrap();
        assert!(
            broker
                .store()
                .latest_session_holder_gone_event(session.id)
                .unwrap()
                .is_none(),
            "the test intentionally does not prime gone evidence through status"
        );

        let first = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(first.retired, 0);
        assert_eq!(first.deferred, 1);
        let first_gone = broker
            .store()
            .latest_session_holder_gone_event(session.id)
            .unwrap()
            .expect("maintenance records the absent exact holder");
        assert!(
            first_gone.id
                > broker
                    .store()
                    .latest_session_holder_event(session.id)
                    .unwrap()
                    .unwrap()
                    .id
        );

        let leases = broker
            .store()
            .unreleased_unexpired_leases_for_sessions_at(&[session.id], now_ms())
            .unwrap();
        let absent = crate::session_holder::ProcessTable::default();
        let liveness = crate::lease_liveness::assess_for_abandonment(
            broker.store(),
            &leases,
            Some(&absent),
            now_ms(),
            crate::lease_liveness::LeaseLivenessPolicy::default(),
        )
        .unwrap();
        assert_eq!(liveness.len(), 1);
        assert_eq!(
            liveness[0].liveness_evidence.stale_since,
            Some(first_gone.ts)
        );
        assert!(liveness[0].liveness_evidence.holds);

        let second = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(second.retired, 0);
        let second_gone = broker
            .store()
            .latest_session_holder_gone_event(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (second_gone.id, second_gone.ts),
            (first_gone.id, first_gone.ts)
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_records_gone_task_holder_without_status_and_starts_grace_once() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Oct 26 08:42:41 2026".into(),
            command: "/opt/codex agent --session ordinary-retirement".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        broker
            .store()
            .claim_lease(session.id, "ordinary-source.rs", None)
            .unwrap();
        assert!(
            broker
                .store()
                .latest_session_holder_gone_event(session.id)
                .unwrap()
                .is_none()
        );

        let first = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(first.retired, 0);
        assert_eq!(first.deferred, 1);
        let first_gone = broker
            .store()
            .latest_session_holder_gone_event(session.id)
            .unwrap()
            .expect("maintenance records the absent task holder without status");
        assert!(
            first_gone.id
                > broker
                    .store()
                    .latest_session_holder_event(session.id)
                    .unwrap()
                    .unwrap()
                    .id
        );

        let second = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(second.retired, 0);
        assert_eq!(second.deferred, 1);
        let second_gone = broker
            .store()
            .latest_session_holder_gone_event(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (second_gone.id, second_gone.ts),
            (first_gone.id, first_gone.ts),
            "repeated maintenance preserves the first gone sighting"
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_does_not_record_gone_without_an_active_explicit_lease() {
        let (_owner, _storage, mut broker, no_lease) = retirement_fixture();
        let implicit_only = broker
            .start_worktree("retirement implicit-only lease", None)
            .unwrap();
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Oct 26 08:42:41 2026".into(),
            command: "/opt/codex agent --session fixture".into(),
        };
        for session in [&no_lease, &implicit_only] {
            age_open_session(&mut broker, session);
            broker
                .store()
                .append_event(
                    crate::events::SESSION_HOLDER_BOUND,
                    Some(session.id),
                    Some(&crate::events::session_holder_bound_payload(
                        &holder,
                        "registered",
                        None,
                    )),
                )
                .unwrap();
        }
        broker
            .store()
            .set_implicit_leases(implicit_only.id, &["src/lib.rs".into()])
            .unwrap();

        let summary = broker.retire_abandoned_sessions(None).unwrap();

        assert_eq!(summary.retired, 2);
        for session in [&no_lease, &implicit_only] {
            assert!(
                broker
                    .store()
                    .latest_session_holder_gone_event(session.id)
                    .unwrap()
                    .is_none(),
                "maintenance records durable gone evidence only for explicit lease holders"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_defers_a_busy_database_writer_without_waiting() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let mut process_table = crate::session_holder::RecordedHolderProcessTable::default();

        let started = Instant::now();
        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                now_ms(),
                72 * 3_600_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();
        assert_eq!(
            outcome,
            crate::broker::AbandonedSessionCloseOutcome::DatabaseBusy
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "a competing database writer defers retirement without delaying maintenance"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn busy_retirement_skips_checkout_planning_and_keeps_its_cursor() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Oct 26 08:42:41 2026".into(),
            command: "/opt/codex app-server --listen local".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder,
                    "registered",
                    None,
                )),
            )
            .unwrap();
        broker
            .store()
            .claim_lease(session.id, "busy-retirement.rs", None)
            .unwrap();
        let cursor = SessionRetirementCursor {
            version: SESSION_RETIREMENT_CURSOR_VERSION,
            after_session_id: 0,
            high_water_session_id: session.id,
        };
        let raw_cursor = serde_json::to_string(&cursor).unwrap();
        broker
            .store()
            .meta_set(SESSION_RETIREMENT_CURSOR_KEY, &raw_cursor)
            .unwrap();

        let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let started = Instant::now();
        let report = broker.auto_remove_disposable_checkouts(None).unwrap();

        assert!(
            started.elapsed()
                < crate::session_holder::PROCESS_TABLE_TIMEOUT + Duration::from_millis(250),
            "a busy retirement pass may take one process snapshot but does not wait on the writer"
        );
        assert_eq!(report.deferred, 1);
        assert!(matches!(
            report.retirement_deferred_reasons.as_slice(),
            [SessionRetirementDeferredReason::DatabaseBusy { session_id }]
                if *session_id == session.id
        ));
        assert_eq!(
            broker
                .store()
                .meta_get(SESSION_RETIREMENT_CURSOR_KEY)
                .unwrap(),
            Some(raw_cursor)
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn busy_first_retirement_pass_defers_without_creating_its_cursor() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let started = Instant::now();

        let report = broker.auto_remove_disposable_checkouts(None).unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "first-run cursor creation must defer instead of waiting on a writer"
        );
        assert_eq!(report.deferred, 1);
        assert!(matches!(
            report.retirement_deferred_reasons.as_slice(),
            [SessionRetirementDeferredReason::CursorBusy]
        ));
        assert_eq!(
            broker
                .store()
                .meta_get(SESSION_RETIREMENT_CURSOR_KEY)
                .unwrap(),
            None,
            "failed initialization leaves the durable cursor unchanged"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_retirement_keeps_a_session_with_unreadable_holder_evidence_open() {
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        age_open_session(&mut broker, &session);
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some("not-json"),
            )
            .unwrap();

        let summary = broker.retire_abandoned_sessions(None).unwrap();
        assert_eq!(summary.retired, 0);
        assert_eq!(summary.deferred, 1);
        assert!(matches!(
            summary.reasons.as_slice(),
            [SessionRetirementDeferredReason::HolderLivenessUnavailable { session_id }]
                if *session_id == session.id
        ));
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Open
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_retention_policy_defers_session_retirement_explicitly() {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("repository");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Auto cleanup test"]);
        git(
            &root,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        git(&root, &["commit", "--allow-empty", "-qm", "fixture"]);
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nsession_abandoned_after_hours = 'invalid'\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();

        let report = broker.auto_remove_disposable_checkouts(None).unwrap();
        assert_eq!(report.sessions_retired, 0);
        assert_eq!(report.deferred, 1);
        assert!(matches!(
            report.retirement_deferred_reasons.as_slice(),
            [SessionRetirementDeferredReason::RetentionPolicyUnavailable { .. }]
        ));
    }

    #[test]
    fn unknown_cleanup_cursor_version_resets_scheduling_state() {
        let cursor = decode_auto_cleanup_cursor(Some(
            r#"{"version":9,"after_session_id":7,"high_water_session_id":12}"#,
        ));
        assert_eq!(cursor, AutoCleanupCursor::default());
    }

    #[test]
    fn whole_checkout_names_cover_the_unattended_artifact_catalog() {
        for kind in crate::artifact_catalog::CATALOG {
            if kind.unattended {
                assert!(
                    super::BUILT_IN_REGENERABLE.contains(&kind.name),
                    "{}",
                    kind.name
                );
            }
        }
    }

    #[test]
    fn ignored_directories_require_output_evidence_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        assert!(!policy.strict);
        for path in [
            "node_modules/pkg/server.pem",
            ".pnpm-store/.env",
            "unknown-data/local.sqlite",
            ".chau7/sessions/transcript.json",
            ".aethyme/state.json",
            "assets/build/logo.svg",
            "target-notes/plan.md",
        ] {
            touch(root, path);
        }
        for path in ["node_modules/", ".pnpm-store/"] {
            assert_eq!(classify(root, path, &policy), Ok(()), "{path}");
        }
        for path in [
            "unknown-data/",
            ".chau7/",
            ".aethyme/",
            "assets/build/",
            "target-notes/",
        ] {
            assert!(classify(root, path, &policy).is_err(), "{path}");
        }
        // No recursive scan: an unresolvable package-fixture link is ignored.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("does-not-exist"),
            root.join("node_modules/dangling"),
        )
        .unwrap();
        assert_eq!(classify(root, "node_modules/", &policy), Ok(()));
    }

    const CACHEDIR_TAG: &str =
        "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag.\n";

    #[test]
    fn a_cache_directory_tag_declares_an_ignored_directory_regenerable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, "tool-cache/objects/ab/cdef");
        std::fs::write(root.join("tool-cache/CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
        assert_eq!(classify(root, "tool-cache/", &policy), Ok(()));

        // A wrong signature, a tag in a subdirectory only, or a tag that is
        // a symlink proves nothing.
        touch(root, "near-miss/data.bin");
        std::fs::write(
            root.join("near-miss/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc56\n",
        )
        .unwrap();
        assert!(classify(root, "near-miss/", &policy).is_err());
        touch(root, "mixed/reports/summary.json");
        std::fs::create_dir_all(root.join("mixed/tagged")).unwrap();
        std::fs::write(root.join("mixed/tagged/CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
        assert!(classify(root, "mixed/", &policy).is_err());
        #[cfg(unix)]
        {
            touch(root, "linked-tag/data.bin");
            std::os::unix::fs::symlink(
                root.join("tool-cache/CACHEDIR.TAG"),
                root.join("linked-tag/CACHEDIR.TAG"),
            )
            .unwrap();
            assert!(classify(root, "linked-tag/", &policy).is_err());
        }

        // Secrets, keys, databases and broker state beneath the tag keep it.
        for protected in [
            "nested/.env",
            "nested/.env.local",
            "deploy.pem",
            "state/app.sqlite",
            "x/.aethyme/state.json",
        ] {
            let name = format!("guarded-{}", protected.len());
            touch(root, &format!("{name}/{protected}"));
            std::fs::write(root.join(&name).join("CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
            assert!(
                classify(root, &format!("{name}/"), &policy).is_err(),
                "{protected}"
            );
        }
    }

    #[test]
    fn witnessed_test_tool_outputs_are_regenerable_and_lookalikes_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, ".hypothesis/examples/0a1b/2c3d");
        touch(root, ".hypothesis/.gitignore");
        assert_eq!(classify(root, ".hypothesis/", &policy), Ok(()));
        touch(root, "playwright-report/index.html");
        touch(root, "playwright-report/data/0123.png");
        assert_eq!(classify(root, "playwright-report/", &policy), Ok(()));
        std::fs::create_dir_all(root.join("test-results/spec-chromium")).unwrap();
        std::fs::write(
            root.join("test-results/.last-run.json"),
            r#"{"status":"passed","failedTests":[]}"#,
        )
        .unwrap();
        assert_eq!(classify(root, "test-results/", &policy), Ok(()));

        let other = tempfile::tempdir().unwrap();
        let root = other.path();
        // A person's notes beside the tool's entries, a report folder with
        // extra files, or a results folder no Playwright run wrote.
        touch(root, ".hypothesis/examples/0a1b/2c3d");
        touch(root, ".hypothesis/notes.md");
        assert!(classify(root, ".hypothesis/", &policy).is_err());
        touch(root, "playwright-report/index.html");
        touch(root, "playwright-report/summary.xlsx");
        assert!(classify(root, "playwright-report/", &policy).is_err());
        touch(root, "test-results/junit.xml");
        assert!(classify(root, "test-results/", &policy).is_err());
        std::fs::write(root.join("test-results/.last-run.json"), "not json").unwrap();
        assert!(classify(root, "test-results/", &policy).is_err());
    }

    /// A protected file nested two levels deep keeps each witnessed test-tool
    /// directory, however well its top level matches the tool's layout.
    #[test]
    fn witnessed_test_tool_outputs_keep_a_nested_protected_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, ".hypothesis/examples/0a1b/.env");
        assert!(classify(root, ".hypothesis/", &policy).is_err());
        touch(root, "playwright-report/index.html");
        touch(root, "playwright-report/data/attachments/deploy.pem");
        assert!(classify(root, "playwright-report/", &policy).is_err());
        std::fs::create_dir_all(root.join("test-results")).unwrap();
        std::fs::write(
            root.join("test-results/.last-run.json"),
            r#"{"status":"passed"}"#,
        )
        .unwrap();
        touch(root, "test-results/spec-chromium/state/app.sqlite");
        assert!(classify(root, "test-results/", &policy).is_err());
    }

    /// Every protected name keeps a tagged cache when it sits two levels
    /// below the tag; a public key alone does not.
    #[test]
    fn every_protected_name_keeps_a_tagged_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        for (index, name) in [
            "server.key",
            "bundle.p12",
            "bundle.pfx",
            "app.db",
            "app.sqlite3",
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            ".netrc",
            ".npmrc",
            ".pypirc",
            "credentials.json",
            "vault.kdbx",
            ".envrc",
            "app.db-wal",
            "cache.sqlite-shm",
            "infra.tfstate",
            "infra.tfstate.backup",
            ".git-credentials",
            ".pgpass",
            "release.jks",
            "debug.keystore",
        ]
        .into_iter()
        .enumerate()
        {
            let cache = format!("guarded-cache-{index}");
            touch(root, &format!("{cache}/nested/deeper/{name}"));
            std::fs::write(root.join(&cache).join("CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
            assert!(
                classify(root, &format!("{cache}/"), &policy).is_err(),
                "{name}"
            );
        }
        touch(root, "public-cache/nested/deeper/id_ed25519.pub");
        std::fs::write(root.join("public-cache/CACHEDIR.TAG"), CACHEDIR_TAG).unwrap();
        assert_eq!(classify(root, "public-cache/", &policy), Ok(()));
    }

    /// `.hypothesis` needs one of its stores, not only its ignore file, and
    /// an oversized `.last-run.json` proves nothing.
    #[test]
    fn test_tool_witnesses_require_their_store_and_a_bounded_status_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        touch(root, ".hypothesis/.gitignore");
        assert!(classify(root, ".hypothesis/", &policy).is_err());
        std::fs::create_dir_all(root.join("test-results")).unwrap();
        let padding = " ".repeat(1024 * 1024);
        std::fs::write(
            root.join("test-results/.last-run.json"),
            format!(r#"{{"status":"passed"}}{padding}"#),
        )
        .unwrap();
        assert!(classify(root, "test-results/", &policy).is_err());
    }

    #[test]
    fn standalone_ignored_files_and_invalid_paths_remain_protected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        for path in [".env", "private.sqlite", "notes.txt", "node_modules"] {
            touch(root, path);
            assert!(classify(root, path, &policy).is_err(), "{path}");
        }
        touch(root, ".DS_Store");
        assert_eq!(classify(root, ".DS_Store", &policy), Ok(()));
        touch(root, ".aethyme/generated/experience-status.md");
        assert_eq!(
            classify(root, ".aethyme/generated/experience-status.md", &policy),
            Ok(())
        );
        touch(root, ".aethyme/local-state.json");
        assert!(classify(root, ".aethyme/local-state.json", &policy).is_err());
        for path in ["../unknown-data/", "/unknown-data/", "node_modules/../.env"] {
            assert!(classify(root, path, &policy).is_err(), "{path}");
        }
    }

    #[test]
    fn strict_cleanup_requires_no_ignored_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy {
            strict: true,
            ..Default::default()
        };
        touch(root, "node_modules/pkg/index.js");
        touch(root, "unknown-data/local.sqlite");
        touch(root, ".DS_Store");
        touch(root, ".aethyme/generated/experience-status.md");
        for path in [
            "node_modules/",
            "unknown-data/",
            ".DS_Store",
            ".aethyme/generated/experience-status.md",
        ] {
            let reason = classify(root, path, &policy).unwrap_err();
            assert!(reason.contains("strict cleanup"), "{path}: {reason}");
        }
    }

    fn regenerable_policy(globs: &[&str]) -> AutoCleanupPolicy {
        AutoCleanupPolicy {
            regenerable: globs.iter().map(|glob| (*glob).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_file_pattern_releases_only_the_standalone_files_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = regenerable_policy(&["reports/quality-*.json", "apps/*/public/_routes.json"]);
        for released in ["reports/quality-1.json", "apps/web/public/_routes.json"] {
            touch(root, released);
            assert_eq!(classify(root, released, &policy), Ok(()), "{released}");
            assert!(
                classify(root, released, &AutoCleanupPolicy::default()).is_err(),
                "{released} is kept without the pattern"
            );
        }
        // `*` stays within one component, so neither a deeper file nor a
        // sibling the pattern does not name is released.
        for kept in [
            "reports/quality-1.txt",
            "reports/summary.json",
            "reports/nested/quality-2.json",
            "apps/web/nested/public/_routes.json",
        ] {
            touch(root, kept);
            let reason = classify(root, kept, &policy).unwrap_err();
            assert!(
                reason.contains("is not inside a recognised build-output directory"),
                "{kept}: {reason}"
            );
        }
    }

    #[test]
    fn a_directory_pattern_does_not_release_a_standalone_file_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = regenerable_policy(&["*-cache", "**/generated"]);
        // Directory roots keep their existing match, where `*` crosses `/`.
        touch(root, "src/tool-cache/index.bin");
        assert_eq!(classify(root, "src/tool-cache/", &policy), Ok(()));
        touch(root, "lib/generated/out.json");
        assert_eq!(classify(root, "lib/generated/", &policy), Ok(()));
        // Files those patterns would reach under the directory match stay.
        for kept in ["src/app-cache", "docs/generated"] {
            touch(root, kept);
            let reason = classify(root, kept, &policy).unwrap_err();
            assert!(
                reason.contains("is not inside a recognised build-output directory"),
                "{kept}: {reason}"
            );
        }
    }

    #[test]
    fn a_configured_pattern_never_releases_a_protected_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = regenerable_policy(&["reports/*"]);
        touch(root, "reports/report.json");
        assert_eq!(classify(root, "reports/report.json", &policy), Ok(()));
        for protected in [
            "reports/signing.pem",
            "reports/.env.ci",
            "reports/.envrc",
            "reports/.env-prod",
            "reports/.ENV",
            "reports/Signing.PEM",
            "reports/id_ed25519",
            "reports/cache.sqlite3",
            "reports/cache.sqlite-wal",
            "reports/app.db-wal",
            "reports/app.db-shm",
            "reports/app.db-journal",
            "reports/credentials.json",
            "reports/.npmrc",
            "reports/.git-credentials",
            "reports/.pgpass",
            "reports/infra.tfstate",
            "reports/infra.tfstate.backup",
            "reports/release.jks",
            "reports/debug.keystore",
        ] {
            touch(root, protected);
            let reason = classify(root, protected, &policy).unwrap_err();
            assert!(reason.contains("protected name"), "{protected}: {reason}");
        }

        // A protected directory component keeps a file the pattern names,
        // not only a protected basename. (A glob naming the directory itself
        // would release it through the directory rule, which is unchanged.)
        let nested = regenerable_policy(&["runs/*/out.json"]);
        touch(root, "runs/run-1/out.json");
        assert_eq!(classify(root, "runs/run-1/out.json", &nested), Ok(()));
        for protected in [
            "runs/.env.d/out.json",
            "runs/state.tfstate.d/out.json",
            "runs/VAULT.KDBX/out.json",
        ] {
            touch(root, protected);
            let reason = classify(root, protected, &nested).unwrap_err();
            assert!(reason.contains("protected name"), "{protected}: {reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_pattern_releases_only_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("reports")).unwrap();
        let policy = regenerable_policy(&["reports/q-*"]);
        assert!(
            Command::new("mkfifo")
                .arg(root.join("reports/q-pipe"))
                .status()
                .unwrap()
                .success()
        );
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("reports/q-sock")).unwrap();
        for special in ["reports/q-pipe", "reports/q-sock"] {
            let reason = classify(root, special, &policy).unwrap_err();
            assert!(
                reason.contains("is not inside a recognised build-output directory"),
                "{special}: {reason}"
            );
        }
        touch(root, "reports/q-file");
        assert_eq!(classify(root, "reports/q-file", &policy), Ok(()));
    }

    #[test]
    fn strict_mode_keeps_a_file_a_pattern_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy {
            strict: true,
            ..regenerable_policy(&["reports/quality-*.json"])
        };
        touch(root, "reports/quality-1.json");
        let reason = classify(root, "reports/quality-1.json", &policy).unwrap_err();
        assert!(reason.contains("strict cleanup"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_pattern_judges_a_link_by_itself_and_never_follows_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("checkout");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("reports")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_ed25519"), "private\n").unwrap();
        let policy = regenerable_policy(&["reports/quality-*.json", "reports/*.pem"]);

        // Judged by the link's own name; removing it only unlinks it.
        symlink(
            outside.join("id_ed25519"),
            root.join("reports/quality-link.json"),
        )
        .unwrap();
        assert_eq!(
            classify(&root, "reports/quality-link.json", &policy),
            Ok(())
        );
        // Following a dangling link would fail to stat it.
        symlink(
            outside.join("missing"),
            root.join("reports/quality-dangling.json"),
        )
        .unwrap();
        assert_eq!(
            classify(&root, "reports/quality-dangling.json", &policy),
            Ok(())
        );
        // A link whose own name is protected stays even when configured.
        symlink(outside.join("id_ed25519"), root.join("reports/signing.pem")).unwrap();
        let reason = classify(&root, "reports/signing.pem", &policy).unwrap_err();
        assert!(reason.contains("protected name"), "{reason}");
        assert!(outside.join("id_ed25519").is_file());
    }

    #[test]
    fn a_standalone_file_blocker_names_the_line_that_releases_exactly_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let entry = |blockers: &[&str]| super::KeepMemoEntry {
            head: "head".into(),
            policy: String::new(),
            fingerprint: Vec::new(),
            blockers: blockers
                .iter()
                .map(|blocker| (*blocker).to_string())
                .collect(),
            sessions: vec![1],
            reason: String::new(),
            recorded_at_ms: 0,
            code: String::new(),
            upstream: None,
        };
        let mut memo = super::decode_keep_memo(None);
        memo.entries.insert(
            first.to_string_lossy().into_owned(),
            entry(&[
                "reports/quality-1.json",
                "reports/[draft] v1.json",
                "keys/server.pem",
                "notes.txt",
                "project-cache/",
            ]),
        );
        memo.entries.insert(
            second.to_string_lossy().into_owned(),
            entry(&["reports/quality-1.json"]),
        );
        let rows = super::blocked_by_ignored_from_memo(&memo);
        let row = |name: &str| {
            rows.iter()
                .find(|row| row.name == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name}: {rows:#?}"))
        };

        let quality = row("reports/quality-1.json");
        assert_eq!(quality.checkouts, 2, "one row per path across checkouts");
        assert_eq!(
            quality.config_line.as_deref(),
            Some("[cleanup] regenerable = [\"reports/quality-1.json\"]")
        );
        assert_eq!(quality.config_refusal, None);
        // Each line is valid configuration that releases exactly its file.
        std::fs::create_dir_all(dir.path().join(".aethyme")).unwrap();
        touch(&first, "reports/other.json");
        for name in ["reports/quality-1.json", "reports/[draft] v1.json"] {
            let line = row(name).config_line.expect(name);
            let body = line.strip_prefix("[cleanup] ").expect(&line);
            std::fs::write(
                dir.path().join(".aethyme/config.toml"),
                format!("[cleanup]\n{body}\n"),
            )
            .unwrap();
            let policy = AutoCleanupPolicy::load(dir.path()).unwrap();
            touch(&first, name);
            assert_eq!(classify(&first, name, &policy), Ok(()), "{line}");
            assert!(
                classify(&first, "reports/other.json", &policy).is_err(),
                "{line} names only {name}"
            );
        }

        let key = row("keys/server.pem");
        assert_eq!(key.config_line, None);
        assert!(
            key.config_refusal
                .as_deref()
                .is_some_and(|refusal| refusal.contains("protected name")),
            "{key:?}"
        );
        // A root file a pattern cannot name without also naming ordinary
        // root files gets no line either.
        let notes = row("notes.txt");
        assert_eq!(notes.config_line, None);
        assert!(
            notes
                .config_refusal
                .as_deref()
                .is_some_and(|refusal| refusal.contains("secret or ordinary root-file")),
            "{notes:?}"
        );
        // A name the escaper cannot express says so, not that it is a secret.
        for unnameable in ["reports/trailing\\", "reports/two\nlines.json"] {
            let refusal = super::regenerable_file_config_line(unnameable).unwrap_err();
            assert!(
                refusal.contains("cannot be named by a [cleanup] regenerable pattern"),
                "{unnameable:?}: {refusal}"
            );
        }
        let directory = row("project-cache/");
        assert_eq!(
            directory.config_line.as_deref(),
            Some("[cleanup] regenerable = [\"project-cache\"]")
        );
        assert_eq!(directory.config_refusal, None);
    }

    /// Write Aethyme's generated bytes for `relative` under `root`.
    fn write_generated(root: &Path, relative: &str) -> String {
        let content = aethyme_enhance::deploy::generated_artifact_contents(relative)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{relative} is not a generated artifact"));
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &content).unwrap();
        content
    }

    #[test]
    fn unmodified_generated_artifacts_are_disposable_and_edits_stay_protected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let policy = AutoCleanupPolicy::default();
        let settings = write_generated(root, ".claude/settings.local.json");
        write_generated(root, ".claude/hooks/aethyme-load-context.sh");
        assert_eq!(
            classify(root, ".claude/settings.local.json", &policy),
            Ok(())
        );
        assert_eq!(classify(root, ".claude/hooks/", &policy), Ok(()));
        assert_eq!(
            classify(root, ".claude/hooks/aethyme-load-context.sh", &policy),
            Ok(())
        );
        touch(root, ".claude/hooks/.DS_Store");
        assert_eq!(classify(root, ".claude/hooks/", &policy), Ok(()));

        // One changed byte (a harness adding permissions, a person editing)
        // keeps the existing protection and its message.
        let mut edited = settings.into_bytes();
        let last = edited.len() - 2;
        edited[last] = b' ';
        std::fs::write(root.join(".claude/settings.local.json"), &edited).unwrap();
        assert_eq!(
            classify(root, ".claude/settings.local.json", &policy),
            Err("ignored path .claude/settings.local.json is not inside a recognised build-output directory".into())
        );
        // An unknown file beside the generated hook protects the directory.
        touch(root, ".claude/hooks/mine.sh");
        assert_eq!(
            classify(root, ".claude/hooks/", &policy),
            Err("ignored path .claude/hooks/ is not a recognised build-output directory".into())
        );
        // A generated file's name elsewhere, or a backup deploy made of the
        // user's settings, is not generated.
        touch(root, "docs/settings.local.json");
        assert!(classify(root, "docs/settings.local.json", &policy).is_err());
        touch(root, ".claude/settings.local.json.bak");
        assert!(classify(root, ".claude/settings.local.json.bak", &policy).is_err());

        // Strict mode still keeps every ignored leftover.
        let strict = AutoCleanupPolicy {
            strict: true,
            ..Default::default()
        };
        write_generated(root, ".codex/skills/aethyme/SKILL.md");
        let reason = classify(root, ".codex/skills/aethyme/SKILL.md", &strict).unwrap_err();
        assert!(reason.contains("strict cleanup"), "{reason}");
        assert_eq!(
            classify(root, ".codex/skills/aethyme/SKILL.md", &policy),
            Ok(())
        );
    }

    #[cfg(unix)]
    #[test]
    fn generated_artifact_links_are_not_followed_or_proven() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        write_generated(outside.path(), ".claude/settings.local.json");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join(".claude/settings.local.json"),
            root.join(".claude/settings.local.json"),
        )
        .unwrap();
        assert!(classify(root, ".claude/settings.local.json", &Default::default()).is_err());
        std::os::unix::fs::symlink(outside.path().join(".claude"), root.join(".claude/hooks"))
            .unwrap();
        assert!(classify(root, ".claude/hooks", &Default::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dependency_root_links_are_classified_without_following_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(root.join("missing"), root.join("node_modules")).unwrap();
        assert_eq!(classify(root, "node_modules", &Default::default()), Ok(()));
        // A link with an unknown name is not assumed to point at a directory.
        std::os::unix::fs::symlink(root.join("missing"), root.join("unknown-link")).unwrap();
        assert!(classify(root, "unknown-link", &Default::default()).is_err());
    }

    #[test]
    fn configured_globs_add_directories_and_never_secrets_or_root_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".aethyme")).unwrap();
        let write =
            |body: &str| std::fs::write(dir.path().join(".aethyme/config.toml"), body).unwrap();
        assert_eq!(
            AutoCleanupPolicy::load(dir.path()).unwrap(),
            AutoCleanupPolicy::default()
        );
        write(
            "[cleanup]\nauto_remove = false\nkeep = [\"*-pinned\"]\nregenerable = [\".gradle\", \"**/.gradle\"]\n",
        );
        let policy = AutoCleanupPolicy::load(dir.path()).unwrap();
        assert!(!policy.auto_remove);
        assert!(policy.is_pinned(Path::new("/w/repo/fix-pinned")));
        touch(dir.path(), ".gradle/caches/x.bin");
        assert_eq!(classify(dir.path(), ".gradle/", &policy), Ok(()));
        for rejected in [
            "\".env*\"",
            "\"*\"",
            "\"**\"",
            "\"*.db\"",
            "\"*.txt\"",
            "\".aethyme\"",
        ] {
            write(&format!("[cleanup]\nregenerable = [{rejected}]\n"));
            assert!(AutoCleanupPolicy::load(dir.path()).is_err(), "{rejected}");
        }
        write("[cleanup]\nstrict = true\n");
        assert!(AutoCleanupPolicy::load(dir.path()).unwrap().strict);
        assert!(!AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
        write("[cleanup]\nstrict = false\n");
        assert!(!AutoCleanupPolicy::load(dir.path()).unwrap().strict);
        assert!(AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
        write("[cleanup]\nstrict = 1\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
        assert!(!AutoCleanupPolicy::artifact_sweep_allowed(dir.path()));
        write("[cleanup]\nauto_remove = \"yes\"\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
        write("[cleanup]\nautoremove = true\n");
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
    }

    #[test]
    fn ignored_directory_sweep_is_opt_in_and_strict_always_disables_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".aethyme")).unwrap();
        let config = dir.path().join(".aethyme/config.toml");
        assert!(
            !AutoCleanupPolicy::load(dir.path())
                .unwrap()
                .ignored_directory_sweep
        );

        std::fs::write(&config, "[cleanup]\nignored_directory_sweep = true\n").unwrap();
        let enabled = AutoCleanupPolicy::load(dir.path()).unwrap();
        assert!(enabled.ignored_directory_sweep && !enabled.strict);

        std::fs::write(
            &config,
            "[cleanup]\nignored_directory_sweep = true\nstrict = true\n",
        )
        .unwrap();
        let strict = AutoCleanupPolicy::load(dir.path()).unwrap();
        assert!(strict.ignored_directory_sweep && strict.strict);

        std::fs::write(&config, "[cleanup]\nignored_directory_sweep = \"yes\"\n").unwrap();
        assert!(AutoCleanupPolicy::load(dir.path()).is_err());
    }

    /// Lets a test remember a keep for files it wrote a moment ago, and
    /// restores the settle time when it ends.
    #[cfg(unix)]
    struct RememberFreshFiles;

    #[cfg(unix)]
    impl RememberFreshFiles {
        fn new() -> Self {
            super::fast_judging::MEMO_SETTLE_OVERRIDE_MS.with(|cell| cell.set(Some(-60_000)));
            Self
        }
    }

    #[cfg(unix)]
    impl Drop for RememberFreshFiles {
        fn drop(&mut self) {
            super::fast_judging::MEMO_SETTLE_OVERRIDE_MS.with(|cell| cell.set(None));
        }
    }

    #[cfg(unix)]
    fn kept_for(report: &AutoCleanupReport, worktree: &str) -> super::AutoKeptCheckout {
        report
            .kept
            .iter()
            .find(|kept| kept.worktree == worktree)
            .cloned()
            .unwrap_or_else(|| panic!("{worktree} is not kept: {report:#?}"))
    }

    /// The database alone keeps a group inside its closed-worktree grace or
    /// one an open session uses, with the reasons and codes the full
    /// judgment gives; a group past its grace that no open session uses is
    /// left to the full judgment.
    #[cfg(unix)]
    #[test]
    fn the_database_alone_keeps_groups_in_grace_or_in_use() {
        use super::fast_judging::database_keep;
        let (_owner, _storage, mut broker, session) = retirement_fixture();
        broker
            .close_abandoning_unpushed(session.id, "database keep fixture")
            .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let group = super::AutoCleanupCheckoutGroup {
            worktree: std::fs::canonicalize(&closed.worktree_path).unwrap(),
            sessions: vec![closed],
            complete: true,
            cursor_session_id: session.id,
        };
        let mut retention = crate::load_retention_policy(broker.main_root()).unwrap();
        let kept = database_keep(&group, &[], &[], &retention, now_ms())
            .expect("inside the default 24 hour grace");
        assert_eq!(kept.code, "in_grace", "{kept:?}");
        assert_eq!(super::kept_reason_code(&kept.reason), "in_grace");
        retention.closed_worktree_grace_hours = 0;
        assert!(database_keep(&group, &[], &[], &retention, now_ms()).is_none());

        let open = broker.start_worktree("open user", None).unwrap();
        let live = broker.store().live_sessions().unwrap();
        let live_worktrees = super::resolve_live_worktree_paths(&live, None).unwrap();
        assert!(
            database_keep(&group, &live, &live_worktrees, &retention, now_ms()).is_none(),
            "an open session elsewhere does not keep it"
        );
        let used = super::AutoCleanupCheckoutGroup {
            worktree: std::fs::canonicalize(&open.worktree_path).unwrap(),
            sessions: group.sessions.clone(),
            complete: true,
            cursor_session_id: session.id,
        };
        let kept = database_keep(&used, &live, &live_worktrees, &retention, now_ms())
            .expect("an open session uses it");
        assert_eq!(kept.code, "session_open", "{kept:?}");
    }

    /// Groups the database keeps cost no judgment: none reaches the group
    /// inspection, the lap reports each as `in_grace`, and a merged one is
    /// left out of the priority lane instead of taking its share.
    #[cfg(unix)]
    #[test]
    fn groups_the_database_keeps_cost_no_judgment_or_priority_share() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(3);
        record_merged_pull_request(&mut broker, sessions[2].id, 501);
        let mut judged = 0_usize;
        let plan = broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Steady,
                true,
                |_, group, _, _, _| {
                    judged += 1;
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        assert_eq!(judged, 0, "every group is inside the default 24 hour grace");
        let lanes = plan.report.lanes.unwrap();
        assert_eq!(lanes.removal_examined, 0, "{lanes:?}");
        assert_eq!(lanes.removal_db_in_grace, 3, "{lanes:?}");
        assert_eq!(lanes.removal_priority_db_skips, 1, "{lanes:?}");
        assert_eq!(lanes.removal_priority_examined, 0, "{lanes:?}");
        assert_eq!(lanes.removal_check_timings.inspect.count, 0, "{lanes:?}");
        assert!(lanes.removal_check_timings.database.count >= 3, "{lanes:?}");
        assert_eq!(plan.report.kept.len(), 3, "{:#?}", plan.report);
        assert!(
            plan.report.kept.iter().all(|kept| kept.code == "in_grace"),
            "{:#?}",
            plan.report
        );
    }

    /// The priority lane orders candidates whose closed-worktree grace has
    /// passed first: one still inside it would only be kept.
    #[cfg(unix)]
    #[test]
    fn priority_candidates_past_their_grace_come_first() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(2);
        record_merged_pull_request(&mut broker, sessions[0].id, 801);
        record_merged_pull_request(&mut broker, sessions[1].id, 802);
        let now = now_ms();
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET closed_at = ?1 WHERE id = ?2",
                rusqlite::params![now - 25 * 3_600_000, sessions[0].id],
            )
            .unwrap();
        let order = |broker: &mut crate::Broker, in_grace_after_ms: i64| {
            broker
                .store()
                .cleanup_cohort_priority_groups(
                    crate::store::cleanup_cohort::CLEANUP_COHORT_NAMESPACE,
                    now,
                    in_grace_after_ms,
                    16,
                )
                .unwrap()
                .into_iter()
                .map(|(_, first_session_id, _)| first_session_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&mut broker, i64::MAX),
            vec![sessions[1].id, sessions[0].id],
            "without a grace, the most recently closed first"
        );
        assert_eq!(
            order(&mut broker, now - 24 * 3_600_000),
            vec![sessions[0].id, sessions[1].id],
            "the one past its grace first"
        );
    }

    /// However many priority candidates wait, the lap judges every other
    /// group of a pass: here each judgment takes 200 ms of a 900 ms pass and
    /// ten merged checkouts wait, and the lap's oldest group is still judged.
    #[cfg(unix)]
    #[test]
    fn the_lap_judges_in_every_pass_however_many_priority_groups_wait() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(12);
        for (index, session) in sessions.iter().enumerate().skip(2) {
            record_merged_pull_request(&mut broker, session.id, 700 + index as i64);
        }
        let mut judged = Vec::new();
        let plan = broker
            .auto_cleanup_plan_with_group_inspector_limited(
                Some(Instant::now() + Duration::from_millis(900)),
                1,
                super::RemovalSelection::Steady,
                false,
                |_, group, _, _, _| {
                    judged.push(group.cursor_session_id);
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected slow inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        let lanes = plan.report.lanes.unwrap();
        assert!(
            judged.contains(&sessions[0].id),
            "the lap's oldest group was judged: {judged:?}"
        );
        assert!(
            lanes.removal_examined > lanes.removal_priority_examined,
            "{lanes:?}"
        );
        assert!(
            sessions[2..]
                .iter()
                .any(|session| judged.first() == Some(&session.id)),
            "a priority group is judged first: {judged:?}"
        );
    }

    /// A keep for uncommitted changes is remembered and reused without
    /// inspecting the checkout again until the changed path changes; then
    /// the checkout is judged again and, clean and landed, selected.
    #[cfg(unix)]
    #[test]
    fn an_uncommitted_keep_is_remembered_until_the_checkout_changes() {
        let _fresh = RememberFreshFiles::new();
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) = closed_checkout_with_notes(&mut broker, "remembered notes");
        let key = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let first = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(kept_for(&first.report, &key).code, "uncommitted_changes");
        let memo = super::decode_keep_memo(
            broker
                .store()
                .meta_get(super::KEEP_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        assert_eq!(
            memo.entries.get(&key).map(|entry| entry.code.as_str()),
            Some("uncommitted_changes"),
            "{memo:?}"
        );

        let second = broker.auto_cleanup_plan(None).unwrap();
        assert!(second.selected.is_empty(), "{:#?}", second.report);
        assert_eq!(kept_for(&second.report, &key).code, "uncommitted_changes");
        let lanes = second.report.lanes.unwrap();
        assert_eq!(lanes.removal_memo_hits.uncommitted_changes, 1, "{lanes:?}");
        assert_eq!(
            lanes.removal_check_timings.inspect.count, 0,
            "a memo hit runs no inspection: {lanes:?}"
        );

        std::fs::remove_file(worktree.join("notes.txt")).unwrap();
        let third = broker.auto_cleanup_plan(None).unwrap();
        let lanes = third.report.lanes.unwrap();
        assert_eq!(lanes.removal_memo_hits.uncommitted_changes, 0, "{lanes:?}");
        assert_eq!(third.selected_worktrees(), vec![key]);
    }

    /// A new commit moves HEAD and ends a remembered keep; the commit is then
    /// remembered as not contained until the remote default branch tip
    /// moves, and once the work lands there the checkout is selected.
    #[cfg(unix)]
    #[test]
    fn a_new_commit_or_a_newly_landed_tip_ends_a_remembered_keep() {
        let _fresh = RememberFreshFiles::new();
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let root = broker.main_root().to_path_buf();
        let (session, worktree) = closed_checkout_with_notes(&mut broker, "committed later");
        let key = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        broker.auto_cleanup_plan(None).unwrap();
        let hit = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(
            hit.report
                .lanes
                .unwrap()
                .removal_memo_hits
                .uncommitted_changes,
            1,
            "{:#?}",
            hit.report
        );

        git(&worktree, &["add", "notes.txt"]);
        git(&worktree, &["commit", "-qm", "notes"]);
        let committed = broker.auto_cleanup_plan(None).unwrap();
        let lanes = committed.report.lanes.unwrap();
        assert_eq!(lanes.removal_memo_hits.uncommitted_changes, 0, "{lanes:?}");
        assert!(lanes.removal_check_timings.inspect.count >= 1, "{lanes:?}");
        assert_eq!(kept_for(&committed.report, &key).code, "not_contained");

        let remembered = broker.auto_cleanup_plan(None).unwrap();
        assert!(remembered.selected.is_empty(), "{:#?}", remembered.report);
        assert_eq!(
            remembered
                .report
                .lanes
                .unwrap()
                .removal_memo_hits
                .not_contained,
            1,
            "{:#?}",
            remembered.report
        );

        aethyme_testkit::land_as_pull_request(
            &root,
            &session.branch,
            aethyme_testkit::MergeStyle::MergeCommit,
        );
        let landed = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(
            landed.report.lanes.unwrap().removal_memo_hits.not_contained,
            0,
            "{:#?}",
            landed.report
        );
        assert_eq!(landed.selected_worktrees(), vec![key]);
    }

    /// A memo hit only ever keeps: a removable checkout with a matching
    /// remembered keep is kept, and selected once the memo is gone.
    #[cfg(unix)]
    #[test]
    fn a_remembered_keep_never_selects_a_checkout() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (session, worktree) = closed_landed_checkout(&mut broker, "removable, remembered");
        let canonical = std::fs::canonicalize(&worktree).unwrap();
        let key = canonical.to_string_lossy().into_owned();
        let memo_policy =
            super::keep_memo_policy(&AutoCleanupPolicy::load(broker.main_root()).unwrap());
        let ids = vec![session.id];
        let signal =
            super::fast_judging::checkout_signal(&canonical).expect("a linked checkout's files");
        let entry = super::fast_judging::keep_memo_entry_for(
            "uncommitted_changes",
            "1 uncommitted or untracked path(s), first: notes.txt",
            &super::fast_judging::MemoContext {
                worktree: &canonical,
                session_ids: &ids,
                policy: &memo_policy,
                upstream_tip: None,
                removal_attempt_recorded: false,
                now_ms: now_ms(),
            },
            Some(&signal),
            // As if judged long after every file here last changed.
            now_ms() + 3_600_000,
        )
        .expect("an unchanged checkout can be remembered");
        let mut memo = super::decode_keep_memo(None);
        memo.entries.insert(key.clone(), entry);
        broker
            .store()
            .meta_set(super::KEEP_MEMO_KEY, &serde_json::to_string(&memo).unwrap())
            .unwrap();

        let remembered = broker.auto_cleanup_plan(None).unwrap();
        assert!(remembered.selected.is_empty(), "{:#?}", remembered.report);
        assert_eq!(
            kept_for(&remembered.report, &key).code,
            "uncommitted_changes"
        );
        assert_eq!(
            remembered
                .report
                .lanes
                .unwrap()
                .removal_memo_hits
                .uncommitted_changes,
            1
        );

        broker.store().meta_set(super::KEEP_MEMO_KEY, "").unwrap();
        let judged = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(judged.selected_worktrees(), vec![key]);
    }

    /// A memo hit on a lap group still queued for the priority lane moves
    /// the lap cursor on, as a full judgment does: when it is the lap's last
    /// group, the lap ends in this pass instead of needing one more.
    #[cfg(unix)]
    #[test]
    fn a_memo_hit_on_a_queued_priority_group_moves_the_lap_on() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (first, _) = closed_checkout_with_notes(&mut broker, "judged first");
        let (last, last_worktree) = closed_checkout_with_notes(&mut broker, "remembered last");
        record_merged_pull_request(&mut broker, first.id, 901);
        record_merged_pull_request(&mut broker, last.id, 902);
        // The first group closed most recently, so the priority lane judges
        // it first; the remembered one waits behind it there, and is the
        // lap's last group.
        let now = now_ms();
        for (session_id, closed_at) in [(last.id, now - 3_600_000), (first.id, now - 60_000)] {
            broker
                .store()
                .connection()
                .execute(
                    "UPDATE sessions SET closed_at = ?1 WHERE id = ?2",
                    rusqlite::params![closed_at, session_id],
                )
                .unwrap();
        }
        let canonical = std::fs::canonicalize(&last_worktree).unwrap();
        let key = canonical.to_string_lossy().into_owned();
        let memo_policy =
            super::keep_memo_policy(&AutoCleanupPolicy::load(broker.main_root()).unwrap());
        let ids = vec![last.id];
        let signal =
            super::fast_judging::checkout_signal(&canonical).expect("a linked checkout's files");
        let entry = super::fast_judging::keep_memo_entry_for(
            "uncommitted_changes",
            "1 uncommitted or untracked path(s), first: notes.txt",
            &super::fast_judging::MemoContext {
                worktree: &canonical,
                session_ids: &ids,
                policy: &memo_policy,
                upstream_tip: None,
                removal_attempt_recorded: false,
                now_ms: now_ms(),
            },
            Some(&signal),
            // As if judged long after every file here last changed.
            now_ms() + 3_600_000,
        )
        .expect("an unchanged checkout can be remembered");
        let mut memo = super::decode_keep_memo(None);
        memo.entries.insert(key.clone(), entry);
        broker
            .store()
            .meta_set(super::KEEP_MEMO_KEY, &serde_json::to_string(&memo).unwrap())
            .unwrap();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .is_some_and(|state| state.complete)
        {}

        let mut judged = Vec::new();
        let plan = broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Steady,
                true,
                |_, group, _, _, _| {
                    judged.push(group.cursor_session_id);
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        assert_eq!(judged, vec![first.id], "{:#?}", plan.report);
        let lanes = plan.report.lanes.unwrap();
        assert_eq!(lanes.removal_memo_hits.uncommitted_changes, 1, "{lanes:?}");
        assert_eq!(kept_for(&plan.report, &key).code, "uncommitted_changes");
        let position = broker.capacity_removal_lap_position().unwrap();
        assert_eq!(position.after_session_id, last.id, "{position:?}");
        assert_eq!(position.lap_remaining, 0, "{position:?}");
    }

    /// An unmarked tracked deletion is remembered like uncommitted changes,
    /// and the memo stops holding once a removal attempt is recorded for the
    /// checkout (the record can make the deletions resumable): the checkout
    /// is judged again.
    #[cfg(unix)]
    #[test]
    fn an_unmarked_deletion_keep_is_remembered_until_a_removal_attempt_is_recorded() {
        let _fresh = RememberFreshFiles::new();
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (session, worktree) = closed_landed_checkout(&mut broker, "remembered deletion");
        std::fs::remove_file(worktree.join("base.txt")).unwrap();
        let canonical = std::fs::canonicalize(&worktree).unwrap();
        let key = canonical.to_string_lossy().into_owned();
        let first = broker.auto_cleanup_plan(None).unwrap();
        assert_eq!(
            kept_for(&first.report, &key).code,
            "unmarked_tracked_deletions"
        );
        let memo = super::decode_keep_memo(
            broker
                .store()
                .meta_get(super::KEEP_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        assert_eq!(
            memo.entries.get(&key).map(|entry| entry.code.as_str()),
            Some("unmarked_tracked_deletions"),
            "{memo:?}"
        );

        let second = broker.auto_cleanup_plan(None).unwrap();
        let lanes = second.report.lanes.unwrap();
        assert_eq!(
            lanes.removal_memo_hits.unmarked_tracked_deletions, 1,
            "{lanes:?}"
        );
        assert_eq!(lanes.removal_check_timings.inspect.count, 0, "{lanes:?}");

        broker
            .record_removal_attempt(&canonical, session.id, &checkout_head_of(&worktree))
            .unwrap();
        let third = broker.auto_cleanup_plan(None).unwrap();
        let lanes = third.report.lanes.unwrap();
        assert_eq!(
            lanes.removal_memo_hits.unmarked_tracked_deletions, 0,
            "{lanes:?}"
        );
        assert!(lanes.removal_check_timings.inspect.count >= 1, "{lanes:?}");
        assert!(
            !third
                .report
                .kept
                .iter()
                .any(|kept| kept.worktree == key && kept.code == "unmarked_tracked_deletions"),
            "a recorded removal makes the deletion resumable: {:#?}",
            third.report
        );
    }

    /// A keep is remembered only if the checkout's change signal reads the
    /// same before and after the judgment: one that moved during it (here
    /// the index, as a `git add` while the checkout was being judged)
    /// records nothing, and the next pass judges the checkout again.
    #[cfg(unix)]
    #[test]
    fn no_keep_is_remembered_when_the_checkout_changes_during_its_judgment() {
        let _fresh = RememberFreshFiles::new();
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) = closed_checkout_with_notes(&mut broker, "staged mid-judgment");
        let key = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut staged = false;
        let plan = broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Probe,
                true,
                |broker, group, live, live_worktrees, deadline| {
                    let inspection = broker.auto_cleanup_candidate_for_group(
                        group,
                        live,
                        live_worktrees,
                        deadline,
                    );
                    if !staged {
                        staged = true;
                        git(&worktree, &["add", "notes.txt"]);
                    }
                    inspection
                },
            )
            .unwrap();
        assert!(staged);
        assert_eq!(kept_for(&plan.report, &key).code, "uncommitted_changes");
        let memo = super::decode_keep_memo(
            broker
                .store()
                .meta_get(super::KEEP_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        assert!(!memo.entries.contains_key(&key), "{memo:?}");

        let next = broker.auto_cleanup_plan(None).unwrap();
        let lanes = next.report.lanes.unwrap();
        assert_eq!(lanes.removal_memo_hits.uncommitted_changes, 0, "{lanes:?}");
        assert!(lanes.removal_check_timings.inspect.count >= 1, "{lanes:?}");
        assert_eq!(kept_for(&next.report, &key).code, "uncommitted_changes");
    }

    /// Apply re-reads the open sessions under its locks: an open session
    /// that starts on a selected checkout after selection keeps it.
    #[cfg(unix)]
    #[test]
    fn apply_refuses_a_checkout_an_open_session_started_on_after_selection() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) = closed_landed_checkout(&mut broker, "opened after selection");
        let key = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
        assert_eq!(plan.selected_worktrees(), vec![key.clone()]);
        let at = now_ms();
        rusqlite::Connection::open(broker.store().db_path())
            .unwrap()
            .execute(
                "INSERT INTO sessions (worktree_path, branch, origin, status,
                 cleanup_state, created_at, updated_at, last_activity_at)
                 VALUES (?1, 'agent/late-open', 'spawned', 'active', 'open', ?2, ?2, ?2)",
                rusqlite::params![worktree.to_string_lossy(), at],
            )
            .unwrap();
        let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(worktree.exists());
        // The fresh read refuses it: kept (the open session overlaps it) or
        // deferred (its path cannot be resolved against the open one yet),
        // never removed.
        let kept = report.kept.iter().find(|kept| kept.worktree == key);
        assert!(
            kept.is_some_and(|kept| kept.reason.starts_with("changed before removal: ")
                || kept.reason.starts_with("removal failed: "))
                || (kept.is_none() && report.deferred == 1),
            "{report:#?}"
        );
    }

    /// The host deleter slot is taken only right before Git removes a
    /// checkout. A removal that finds it kept by another repository's worker
    /// waits the bounded wait and is put off (the checkout stays, the slice
    /// ends `host_deleter_busy` with a minute's retry) instead of the run
    /// queueing for minutes; with the slot free the next slice removes the
    /// checkout and gives the slot back.
    #[cfg(unix)]
    #[test]
    fn a_removal_waits_only_briefly_for_the_host_deleter_slot() {
        use crate::gc::unattended_pacing::{
            self, FakeClock, PressureSample, Scripted, UnattendedPacer, UnlinkRate,
        };
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let (_session, worktree) = closed_landed_checkout(&mut broker, "removed once it is free");
        let host = tempfile::tempdir().unwrap();
        let take = |host: &std::path::Path| match crate::gc::wait_for_host_deleter_slot(
            Some(host),
            Duration::ZERO,
            Duration::from_millis(1),
            |_| unreachable!(),
        )
        .0
        {
            crate::gc::HostDeleterSlot::Held(lock) => Some(lock),
            _ => None,
        };
        let other = take(host.path()).expect("another worker deletes");
        let clock = FakeClock::new();
        let _scope = unattended_pacing::install(
            UnattendedPacer::new(
                UnlinkRate::Unlimited,
                Box::new(clock.clone()),
                Box::new(Scripted {
                    samples: vec![PressureSample::default()],
                    reads: Default::default(),
                }),
            )
            .with_host_directory(Some(host.path().to_path_buf())),
        );

        broker.run_capacity_removal_slice(Instant::now()).unwrap();
        let busy = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(busy.trigger, "steady", "{busy:#?}");
        assert_eq!(
            busy.stop_reason.as_deref(),
            Some("host_deleter_busy"),
            "{busy:#?}"
        );
        assert_eq!(busy.outcome, "lock_busy", "{busy:#?}");
        assert_eq!(busy.removed, 0, "{busy:#?}");
        assert_eq!(busy.host_slot_wait_ms, 2_000, "{busy:#?}");
        assert_eq!(busy.host_slot_held_ms, 0, "{busy:#?}");
        assert_eq!(busy.retry_in_ms, Some(60_000), "{busy:#?}");
        assert!(worktree.exists());
        assert!(!unattended_pacing::host_deleter_slot_held());

        drop(other);
        broker.end_steady_removal_back_off_for_test();
        broker.end_steady_removal_hard_retry_for_test();
        broker.run_capacity_removal_slice(Instant::now()).unwrap();
        let removed = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(removed.removed, 1, "{removed:#?}");
        assert!(!worktree.exists());
        assert_eq!(removed.host_slot_wait_ms, 0, "{removed:#?}");
        assert!(
            !unattended_pacing::host_deleter_slot_held(),
            "given back after the removal's charge"
        );
        assert!(take(host.path()).is_some(), "free for the other worker");
    }

    /// A group whose removal a busy host deleter slot put off is judged
    /// again by the next slice: the lap cursor is never left past it, and
    /// the next slice removes it.
    #[cfg(unix)]
    #[test]
    fn a_lap_group_put_off_by_a_busy_slot_is_judged_first_again() {
        use crate::gc::unattended_pacing::{
            self, FakeClock, PressureSample, Scripted, UnattendedPacer, UnlinkRate,
        };
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let (session, worktree) = closed_landed_checkout(&mut broker, "lap group put off");
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .is_some_and(|state| state.complete)
        {}
        let host = tempfile::tempdir().unwrap();
        let other = match crate::gc::wait_for_host_deleter_slot(
            Some(host.path()),
            Duration::ZERO,
            Duration::from_millis(1),
            |_| unreachable!(),
        )
        .0
        {
            crate::gc::HostDeleterSlot::Held(lock) => lock,
            _ => panic!("another worker deletes"),
        };
        let _scope = unattended_pacing::install(
            UnattendedPacer::new(
                UnlinkRate::Unlimited,
                Box::new(FakeClock::new()),
                Box::new(Scripted {
                    samples: vec![PressureSample::default()],
                    reads: Default::default(),
                }),
            )
            .with_host_directory(Some(host.path().to_path_buf())),
        );
        broker.run_capacity_removal_slice(Instant::now()).unwrap();
        let busy = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(
            busy.stop_reason.as_deref(),
            Some("host_deleter_busy"),
            "{busy:#?}"
        );
        assert_eq!(busy.host_slot_busy, 1, "{busy:#?}");
        let position = broker.capacity_removal_lap_position().unwrap();
        assert!(
            position.after_session_id < session.id && position.lap_remaining >= 1,
            "the cursor went back before it: {position:?}"
        );
        assert!(worktree.exists());

        drop(other);
        broker.end_steady_removal_back_off_for_test();
        broker.end_steady_removal_hard_retry_for_test();
        broker.run_capacity_removal_slice(Instant::now()).unwrap();
        let removed = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(removed.removed, 1, "{removed:#?}");
        assert!(!worktree.exists());
    }

    /// A put-off group the lap had already passed has its priority memo
    /// entry dropped and the lap cursor moved back before it, so the lap
    /// reaches it again first.
    #[cfg(unix)]
    #[test]
    fn a_put_off_group_moves_the_lap_cursor_back_before_it() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(3);
        broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Probe,
                false,
                |_, group, _, _, _| {
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        let passed = broker.capacity_removal_lap_position().unwrap();
        assert_eq!(passed.after_session_id, sessions[2].id, "{passed:?}");
        let middle = std::fs::canonicalize(&sessions[1].worktree_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut memo = super::PriorityMemo::decode(None);
        memo.remember(
            middle.clone(),
            true,
            super::PriorityOutcome::Selected,
            now_ms(),
        );
        broker
            .store()
            .meta_set(
                super::PRIORITY_MEMO_KEY,
                &serde_json::to_string(&memo).unwrap(),
            )
            .unwrap();

        broker
            .judge_put_off_groups_first(&[(middle.clone(), sessions[1].id)])
            .unwrap();
        let rewound = broker.capacity_removal_lap_position().unwrap();
        assert_eq!(rewound.after_session_id, sessions[1].id - 1, "{rewound:?}");
        assert!(rewound.lap_remaining >= 2, "{rewound:?}");
        let memo = super::PriorityMemo::decode(
            broker
                .store()
                .meta_get(super::PRIORITY_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        assert!(!memo.entries.contains_key(&middle), "{memo:?}");
        // A group the lap has not reached yet leaves the cursor alone.
        broker
            .judge_put_off_groups_first(&[(String::new(), sessions[2].id)])
            .unwrap();
        assert_eq!(broker.capacity_removal_lap_position().unwrap(), rewound);
    }

    /// Moving the lap cursor back for a put-off group is only an ordering
    /// hint: when another pass moved the cursor meanwhile, the conflict is
    /// counted and not an error, the other pass's cursor stays, and the
    /// priority memo entry is still dropped.
    #[cfg(unix)]
    #[test]
    fn a_cursor_conflict_while_moving_back_is_not_an_error() {
        let (_owner, _storage, mut broker, sessions) = closed_priority_fixture(2);
        broker
            .auto_cleanup_plan_with_group_inspector_limited(
                None,
                1,
                super::RemovalSelection::Probe,
                false,
                |_, group, _, _, _| {
                    Ok(AutoCleanupGroupInspection::Kept(super::AutoKeptCheckout {
                        worktree: group.worktree.to_string_lossy().into_owned(),
                        sessions: group.sessions.iter().map(|session| session.id).collect(),
                        reason: "injected inspection".into(),
                        ..Default::default()
                    }))
                },
            )
            .unwrap();
        let passed = broker.capacity_removal_lap_position().unwrap();
        let first = std::fs::canonicalize(&sessions[0].worktree_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut memo = super::PriorityMemo::decode(None);
        memo.remember(
            first.clone(),
            true,
            super::PriorityOutcome::Selected,
            now_ms(),
        );
        broker
            .store()
            .meta_set(
                super::PRIORITY_MEMO_KEY,
                &serde_json::to_string(&memo).unwrap(),
            )
            .unwrap();
        super::PUT_OFF_CURSOR_RACE.with(|flag| flag.set(true));
        let result = broker.judge_put_off_groups_first(&[(first.clone(), sessions[0].id)]);
        super::PUT_OFF_CURSOR_RACE.with(|flag| flag.set(false));
        result.expect("a cursor conflict is not an error");
        let after = broker.capacity_removal_lap_position().unwrap();
        assert_eq!(
            after.after_session_id, passed.after_session_id,
            "not moved back over the other pass's cursor: {after:?}"
        );
        assert_eq!(
            after.high_water_session_id,
            passed.high_water_session_id + 1,
            "the other pass's write stays: {after:?}"
        );
        let memo = super::PriorityMemo::decode(
            broker
                .store()
                .meta_get(super::PRIORITY_MEMO_KEY)
                .unwrap()
                .as_deref(),
        );
        assert!(!memo.entries.contains_key(&first), "{memo:?}");
    }

    /// A hard retry set while a worker run is going (a held lock, memory
    /// pressure, failures in another lane) stops its continuation slices.
    #[cfg(unix)]
    #[test]
    fn a_hard_retry_set_mid_run_stops_continuation_slices() {
        // No checkout limit: thirty-four closed checkouts never make pressure.
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let kept = (0..34)
            .map(|index| closed_checkout_with_notes(&mut broker, &format!("hard kept {index}")).1)
            .collect::<Vec<_>>();
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .is_some_and(|state| state.complete)
        {}
        let one_pass = crate::gc::CapacitySliceLimits {
            budget: Duration::from_millis(5_400),
            pass_budget: Duration::from_millis(400),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        crate::gc::HARD_RETRY_BETWEEN_SLICES.with(|flag| flag.set(true));
        let run =
            broker.run_capacity_removal_slices(Instant::now(), Duration::from_secs(20), one_pass);
        crate::gc::HARD_RETRY_BETWEEN_SLICES.with(|flag| flag.set(false));
        run.unwrap();
        let last = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(last.outcome, "lap_unfinished", "{last:#?}");
        assert_eq!(last.run_slices, 1, "{last:#?}");
        assert!(!last.continued, "{last:#?}");
        assert!(broker.steady_removal_hard_retry_pending_for_test());
        assert!(kept.iter().all(|path| path.exists()));
    }

    /// Selection that reuses the pass's open-session index and runs one deep
    /// verdict still hands apply a checkout apply re-proves under its locks:
    /// a file written after selection keeps it.
    #[cfg(unix)]
    #[test]
    fn apply_still_refuses_a_checkout_changed_after_selection() {
        let (_owner, _storage, mut broker) = landed_lane_fixture(None);
        let (_session, worktree) = closed_landed_checkout(&mut broker, "changed after selection");
        let key = std::fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
        assert_eq!(plan.selected_worktrees(), vec![key.clone()]);
        std::fs::write(worktree.join("late.txt"), "late work\n").unwrap();
        let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(worktree.join("late.txt").is_file());
        let kept = kept_for(&report, &key);
        assert!(
            kept.reason.starts_with("changed before removal: "),
            "{kept:?}"
        );
        assert_eq!(kept.code, "uncommitted_changes", "{kept:?}");
    }

    /// A worker run takes steady slices back to back while the lap is
    /// unfinished and nothing blocks, instead of one slice per run; and a
    /// slice that leaves the lap unfinished reports no idle streak however
    /// many empty laps came before, since its retry is a minute.
    #[cfg(unix)]
    #[test]
    fn a_worker_run_keeps_taking_steady_slices_while_the_lap_is_unfinished() {
        // No checkout limit: thirty-four closed checkouts never make pressure.
        let (_owner, _storage, mut broker) =
            landed_lane_fixture(Some("[worktrees]\nmax_checkouts = 0\n"));
        let kept = (0..34)
            .map(|index| closed_checkout_with_notes(&mut broker, &format!("run kept {index}")).1)
            .collect::<Vec<_>>();
        // A complete path index, so the short passes go to judging.
        while !broker
            .refresh_cleanup_cohort_aliases(None)
            .unwrap()
            .is_some_and(|state| state.complete)
        {}
        broker.set_steady_removal_idle_streak_for_test(27);
        // Room for one pass per slice (see the full-empty-lap test), and a
        // short pass, so neither one slice nor the run's first finishes the
        // lap.
        let one_pass = crate::gc::CapacitySliceLimits {
            budget: Duration::from_millis(5_400),
            pass_budget: Duration::from_millis(400),
            ..crate::gc::CapacitySliceLimits::WORKER
        };
        broker
            .run_capacity_removal_slice_with(Instant::now(), one_pass)
            .unwrap();
        let single = broker.last_capacity_removal_slice().unwrap();
        assert_eq!(single.trigger, "steady", "{single:#?}");
        assert_eq!(single.outcome, "lap_unfinished", "{single:#?}");
        assert_eq!(single.idle_streak, 0, "{single:#?}");
        assert_eq!(single.empty_laps, 27, "{single:#?}");
        assert_eq!(single.retry_in_ms, Some(60_000), "{single:#?}");
        assert!(!single.continued, "{single:#?}");

        broker.end_steady_removal_back_off_for_test();
        broker
            .run_capacity_removal_slices(Instant::now(), Duration::from_secs(20), one_pass)
            .unwrap();
        let last = broker.last_capacity_removal_slice().unwrap();
        assert!(last.run_slices >= 2, "{last:#?}");
        assert!(last.continued, "{last:#?}");
        assert_eq!(last.removed, 0, "{last:#?}");
        assert!(kept.iter().all(|path| path.exists()));
    }
}

fn ensure_auto_cleanup_path_absent(path: &Path) -> Result<(), BrokerOpError> {
    let args = format!("remove {}", path.display());
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(BrokerOpError::Git(crate::GitError::Git {
            args,
            stderr: format!(
                "Git reported worktree removal but {} still exists; retained it because its identity is unproven",
                path.display()
            ),
        })),
        Err(error) => Err(BrokerOpError::Git(crate::GitError::Git {
            args,
            stderr: format!(
                "could not verify that {} is absent after Git worktree removal; retained it: {error}",
                path.display()
            ),
        })),
    }
}

#[cfg(test)]
mod fail_closed_removal_tests {
    use super::ensure_auto_cleanup_path_absent;
    #[cfg(unix)]
    use super::{AutoCleanupCandidate, ContainmentProof};
    #[cfg(unix)]
    use crate::{SessionCleanupState, git::with_git_deadline};
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn auto_cleanup_preserves_candidate_after_git_removal_timeout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = owner.path().join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Auto cleanup test"]);
        git(
            &repository,
            &["config", "user.email", "auto-cleanup@example.invalid"],
        );
        git(&repository, &["commit", "--allow-empty", "-qm", "initial"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("auto cleanup candidate", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve this candidate").unwrap();
        let spawned_owner = broker.store().session(session.id).unwrap();
        let spawned_instance_witness = broker
            .store()
            .spawned_worktree_instance_witness(session.id)
            .unwrap()
            .expect("the production spawn path records its instance witness");
        let candidate = AutoCleanupCandidate {
            worktree: worktree.clone(),
            spawned_owner: spawned_owner.clone(),
            spawned_instance_witness: Some(spawned_instance_witness),
            sessions: vec![spawned_owner],
        };
        let proof = ContainmentProof {
            head: "fixture-head".into(),
            containing_commit: "fixture-containing-commit".into(),
            proof: "ancestry".into(),
            interrupted_removal_deletions: 0,
            ownership_reproof: None,
        };

        let result = with_git_deadline(Duration::ZERO, || {
            broker.remove_auto_cleanup_candidate(&candidate, &proof)
        });
        assert!(result.is_err(), "expired Git budget must refuse removal");
        assert_eq!(
            std::fs::read(&evidence).unwrap(),
            b"preserve this candidate"
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed,
            "failed removal must leave the candidate available for retry"
        );
    }

    #[test]
    fn auto_cleanup_preserves_a_path_git_left_after_removal() {
        let owner = tempfile::tempdir().unwrap();
        let replacement = owner.path().join("replacement-worktree");
        std::fs::create_dir(&replacement).unwrap();
        let evidence = replacement.join("keep.txt");
        std::fs::write(&evidence, b"preserve replacement").unwrap();

        let result = ensure_auto_cleanup_path_absent(&replacement);

        assert!(result.is_err(), "a remaining path is not proof of removal");
        assert_eq!(std::fs::read(evidence).unwrap(), b"preserve replacement");
    }
}
