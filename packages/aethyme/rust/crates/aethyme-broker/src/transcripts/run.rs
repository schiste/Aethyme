//! One archiving pass: scan, link, judge, then (with `apply`) archive the
//! eligible transcripts largest first, so space comes back early. Plus the
//! status and restore entry points the CLI shares.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::agents::{self, AgentProbe, ClaudeProcess, SystemAgents};
use super::archive::{self, ArchiveContext, Hooks, NoHooks, RestorePlan, Restored};
use super::config::EnabledSource;
use super::eligibility::{self, Facts, Linkage, SkipReason};
use super::first_run::{self, FirstRunStatus, Gate};
use super::index::{ArchiveRow, Index};
use super::linkage::{self, BrokerDbs};
use super::open_files::{OpenFileProbe, OpenFiles, SystemProbe};
use super::pacing::{self, Pacer};
use super::providers::{self, Provider, Transcript};
use super::safe_fs::{self, Root};
use super::{Environment, LaneLock, TranscriptsConfig};

pub const RUN_SCHEMA: &str = "aethyme.transcripts-archive/v1";
pub const STATUS_SCHEMA: &str = "aethyme.transcripts-status/v1";
/// How long one host-wide open-file observation serves the transcripts a
/// pass judges, including the check right before an original is removed;
/// an older observation is replaced before it is used again. Each costs
/// most of a second on a busy host, so one per transcript dominated a pass.
pub(crate) const OPEN_LISTING_MAX_AGE: Duration = Duration::from_secs(30);
/// Published copies whose originals are removed together, judged against
/// one open-file observation taken after the last of them was verified;
/// a batch is also flushed once its first copy is this old, when space
/// runs out, and at the end of every pass.
pub(crate) const REMOVAL_BATCH_FILES: usize = 200;
pub(crate) const REMOVAL_BATCH_AGE: Duration = Duration::from_secs(60);
/// Wall time one background-worker pass may spend; the next pass resumes.
pub(crate) const WORKER_RUN_BUDGET: Duration = Duration::from_secs(10 * 60);
/// Every repository's detached worker runs the transcripts lane, and the
/// lane's work is host-wide. After a worker pass that found nothing to
/// archive and finished within its budget, the next worker pass on this
/// host waits this long: transcripts become eligible after days of
/// inactivity, so a quarter of an hour changes no verdict. On the busiest
/// host 189 worker passes ran in 10.7 h (median gap 113 s, 24,011 s in
/// all) and 184 of them found nothing eligible.
pub(crate) const WORKER_PASS_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// The last worker pass's outcome, in the transcripts host directory.
const LAST_WORKER_PASS_FILE: &str = "last-worker-pass.json";

/// What the next worker pass needs to know about the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
struct LastWorkerPass {
    at_ms: i64,
    /// It found nothing to archive and was not cut short by its budget.
    idle: bool,
}

/// Whether a worker pass is due: no record of the last one, the last one
/// found work or ran out of budget, the record is unreadable, or the
/// interval has passed (a clock that went back counts as passed).
fn worker_pass_due(host_dir: &Path, now_ms: i64) -> bool {
    let Some(last) = std::fs::read(host_dir.join(LAST_WORKER_PASS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LastWorkerPass>(&bytes).ok())
    else {
        return true;
    };
    let interval_ms = i64::try_from(WORKER_PASS_INTERVAL.as_millis()).unwrap_or(i64::MAX);
    !last.idle || now_ms < last.at_ms || now_ms.saturating_sub(last.at_ms) >= interval_ms
}

fn record_worker_pass(host_dir: &Path, report: &RunReport, now_ms: i64) {
    let last = LastWorkerPass {
        at_ms: now_ms,
        idle: report.first_run_gate == Gate::Open
            && report.eligible.is_empty()
            && !report.budget_exhausted,
    };
    crate::warn_unrecorded(
        "record the transcripts worker pass",
        serde_json::to_vec(&last)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(host_dir.join(LAST_WORKER_PASS_FILE), bytes)),
    );
}
/// Examples of skipped transcripts a report lists (all are counted).
const MAX_SKIPPED_EXAMPLES: usize = 50;
/// The run log rotates past this size.
const RUN_LOG_MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const RUN_LOG: &str = "runs.jsonl";

/// How one pass runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    /// Archive, rather than only report what would be archived.
    pub apply: bool,
    /// Stop starting new files after this long.
    pub budget: Option<Duration>,
}

/// One transcript in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptView {
    pub provider: Provider,
    pub path: PathBuf,
    pub destination: PathBuf,
    pub agent_session_id: Option<String>,
    pub bytes: u64,
}

impl TranscriptView {
    fn of(transcript: &Transcript) -> Self {
        Self {
            provider: transcript.provider,
            path: transcript.path.clone(),
            destination: transcript.destination.clone(),
            agent_session_id: transcript.agent_session_id.clone(),
            bytes: transcript.bytes,
        }
    }
}

/// One archived transcript in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchivedView {
    pub provider: Provider,
    pub path: PathBuf,
    pub destination: PathBuf,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub sha256: String,
    pub resumed: bool,
}

/// One skipped transcript in a report. Interactive output shows the path;
/// the run log keeps only the session id and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedView {
    pub path: PathBuf,
    pub agent_session_id: Option<String>,
    pub reason: SkipReason,
    pub detail: String,
}

/// Why a pass could not trust "unlinked": counts and flags only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct LinkageIssues {
    /// Broker databases that could not be read.
    pub unreadable_databases: u64,
    /// The worktree registry could not be read completely.
    pub registry_incomplete: bool,
    /// The transcript index could not be read.
    pub index_unreadable: bool,
    /// Live session holders or their open files could not all be listed.
    pub live_holders_incomplete: bool,
}

impl LinkageIssues {
    pub fn any(&self) -> bool {
        self.unreadable_databases > 0
            || self.registry_incomplete
            || self.index_unreadable
            || self.live_holders_incomplete
    }

    /// The kinds of issue present, for advice and status.
    pub fn kinds(&self) -> Vec<String> {
        let mut kinds = Vec::new();
        if self.unreadable_databases > 0 {
            kinds.push(format!(
                "{} unreadable broker database(s)",
                self.unreadable_databases
            ));
        }
        if self.registry_incomplete {
            kinds.push("incomplete worktree registry".to_string());
        }
        if self.index_unreadable {
            kinds.push("unreadable transcript index".to_string());
        }
        if self.live_holders_incomplete {
            kinds.push("live session holders not fully listed".to_string());
        }
        kinds
    }
}

/// Index meta keys for pass health.
const META_DEGRADED_PASSES: &str = "consecutive_degraded_passes";
const META_LAST_ISSUES: &str = "last_linkage_issues";
/// Consecutive degraded passes before `broker status` advises.
pub(crate) const DEGRADED_PASSES_FOR_ADVICE: u64 = 3;

/// Record whether this applied pass could read every source of links.
fn record_pass_health(index: &Index, report: &RunReport) {
    let previous = index
        .meta(META_DEGRADED_PASSES)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let passes = if report.linkage_degraded {
        previous + 1
    } else {
        0
    };
    let issues = serde_json::to_string(&report.linkage_issues).unwrap_or_default();
    crate::warn_unrecorded(
        "record transcript linkage health",
        index
            .set_meta(META_DEGRADED_PASSES, &passes.to_string())
            .and_then(|()| index.set_meta(META_LAST_ISSUES, &issues)),
    );
}

/// Linkage health as the index last recorded it.
fn pass_health(index: &Index) -> (u64, Option<LinkageIssues>) {
    let passes = index
        .meta(META_DEGRADED_PASSES)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let issues = index
        .meta(META_LAST_ISSUES)
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str(&value).ok());
    (passes, issues)
}

/// The `broker status` advice for linkage that stayed degraded for
/// [`DEGRADED_PASSES_FOR_ADVICE`] passes, from `index`.
pub(crate) fn linkage_advice_from(index: &Index) -> Option<crate::StatusAdvice> {
    let (passes, issues) = pass_health(index);
    if passes < DEGRADED_PASSES_FOR_ADVICE {
        return None;
    }
    let kinds = issues.unwrap_or_default().kinds();
    Some(crate::StatusAdvice {
        id: "transcripts.linkage-degraded",
        severity: crate::StatusAdviceSeverity::Warning,
        reason: "transcript archiving could not read every source of session links for several passes",
        summary: format!(
            "the last {passes} transcript archive passes skipped every unlinked transcript because session links could not all be read ({})",
            if kinds.is_empty() {
                "unknown cause".to_string()
            } else {
                kinds.join(", ")
            }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence: kinds,
        commands: vec!["aethyme transcripts status".into()],
    })
}

/// The advice for this host, when transcript archiving is enabled here
/// (explicitly or by default).
pub(crate) fn linkage_degraded_advice() -> Option<crate::StatusAdvice> {
    if crate::gc::background_process_is_suppressed() {
        return None;
    }
    let (environment, config) = super::host_settings()?;
    if !config.archive {
        return None;
    }
    let index = Index::open_existing(&environment.host_dir).ok()??;
    linkage_advice_from(&index)
}

/// What one pass found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunReport {
    pub schema: &'static str,
    pub enabled: bool,
    /// `default` (no explicit `archive` setting) or `settings`.
    pub enabled_source: EnabledSource,
    /// The first-run gate this pass saw: `open`, or for a background pass
    /// on a default-on host `notice_pending`/`waiting` (nothing was done).
    pub first_run_gate: Gate,
    pub apply: bool,
    /// `acquired`, `busy` (another run holds it; nothing was done) or
    /// `not_taken` (a dry run reads only).
    pub lock: &'static str,
    pub examined: u64,
    /// Transcript tails read for their last record's timestamp (only for
    /// transcripts whose modification time was old enough).
    pub record_tails_read: u64,
    pub eligible: Vec<TranscriptView>,
    pub eligible_bytes: u64,
    pub archived: Vec<ArchivedView>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub resumed: u64,
    pub verify_failures: u64,
    pub skipped: BTreeMap<String, u64>,
    pub skipped_examples: Vec<SkippedView>,
    pub leftover_temps_removed: u64,
    /// Index rows whose archive had been moved away (for example by
    /// `codex unarchive`), dropped so totals never overcount.
    pub reconciled_missing: u64,
    pub unreadable_entries: u64,
    pub scan_truncated: bool,
    pub broker_databases: u64,
    pub links_added: u64,
    /// True when a broker database or a live-holder listing could not be
    /// read this pass: unlinked transcripts were then all skipped.
    pub linkage_degraded: bool,
    /// What could not be read, by kind (counts and flags; no paths).
    pub linkage_issues: LinkageIssues,
    /// Host-wide open-file observations taken (one serves every provider
    /// root and every transcript judged while it is young enough).
    pub open_listings: u64,
    /// Observations taken because the previous one outgrew
    /// [`OPEN_LISTING_MAX_AGE`].
    pub open_observation_refreshes: u64,
    /// Open-state judgements served by an observation, in total and at most
    /// by one observation.
    pub open_files_judged: u64,
    pub open_files_judged_max_per_observation: u64,
    /// Batches of removals, each judged against its own observation.
    pub removal_batches: u64,
    /// Originals kept at removal, and their copies discarded: open in the
    /// batch observation (also `open_by_process`), the batch observation
    /// incomplete (also `open_check_unavailable`), or changed in size, mtime
    /// or identity since compression (also `modified_during_compression`).
    pub removals_refused_open: u64,
    pub removals_refused_unobserved: u64,
    pub removals_refused_changed: u64,
    /// Full syncs that fell back to `fsync` (macOS file systems refusing
    /// `F_FULLFSYNC`).
    pub full_sync_fallbacks: u64,
    pub paced_bytes_per_second: Option<u64>,
    pub pacing_slept_ms: u64,
    pub budget_exhausted: bool,
    /// Set when no remaining eligible transcript fits in the free space of
    /// its archive volume (`size/3 + 256 MiB`): the pass stopped.
    pub insufficient_space: bool,
    pub phases_ms: BTreeMap<String, u64>,
    pub warnings: Vec<String>,
}

impl RunReport {
    fn new(config: &TranscriptsConfig, apply: bool) -> Self {
        Self {
            schema: RUN_SCHEMA,
            enabled: config.archive,
            enabled_source: config.enabled_source,
            first_run_gate: Gate::Open,
            apply,
            lock: "not_taken",
            examined: 0,
            record_tails_read: 0,
            eligible: Vec::new(),
            eligible_bytes: 0,
            archived: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
            resumed: 0,
            verify_failures: 0,
            skipped: BTreeMap::new(),
            skipped_examples: Vec::new(),
            leftover_temps_removed: 0,
            reconciled_missing: 0,
            unreadable_entries: 0,
            scan_truncated: false,
            broker_databases: 0,
            links_added: 0,
            linkage_degraded: false,
            linkage_issues: LinkageIssues::default(),
            open_listings: 0,
            open_observation_refreshes: 0,
            open_files_judged: 0,
            open_files_judged_max_per_observation: 0,
            removal_batches: 0,
            removals_refused_open: 0,
            removals_refused_unobserved: 0,
            removals_refused_changed: 0,
            full_sync_fallbacks: 0,
            paced_bytes_per_second: None,
            pacing_slept_ms: 0,
            budget_exhausted: false,
            insufficient_space: false,
            phases_ms: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Count a skip without an example.
    fn count_skip(&mut self, reason: SkipReason) {
        *self.skipped.entry(reason.label().to_string()).or_default() += 1;
        if reason == SkipReason::VerifyFailed {
            self.verify_failures += 1;
        }
    }

    /// Count a skip and keep an example (except for the routine ones).
    fn skip(&mut self, transcript: &Transcript, reason: SkipReason, detail: &str) {
        self.count_skip(reason);
        let routine = matches!(
            reason,
            SkipReason::TooRecent
                | SkipReason::RecentRecord
                | SkipReason::RecentSessionActivity
                | SkipReason::DestinationConflictBackoff
        );
        if !routine && self.skipped_examples.len() < MAX_SKIPPED_EXAMPLES {
            self.skipped_examples.push(SkippedView {
                path: transcript.path.clone(),
                agent_session_id: transcript.agent_session_id.clone(),
                reason,
                detail: detail.to_string(),
            });
        }
    }

    fn phase(&mut self, name: &'static str, elapsed: Duration) {
        *self.phases_ms.entry(name.to_string()).or_default() += elapsed.as_millis() as u64;
        crate::phase_telemetry::add(name, elapsed);
    }

    /// The counters this pass adds to the lane's lifetime totals.
    fn counters(&self) -> BTreeMap<String, u64> {
        let mut counters = BTreeMap::new();
        counters.insert("runs".to_string(), 1);
        counters.insert("examined".to_string(), self.examined);
        counters.insert("archived".to_string(), self.archived.len() as u64);
        counters.insert("bytes_in".to_string(), self.bytes_in);
        counters.insert("bytes_out".to_string(), self.bytes_out);
        counters.insert("resumed".to_string(), self.resumed);
        counters.insert("verify_failures".to_string(), self.verify_failures);
        counters.insert(
            "leftover_temps_removed".to_string(),
            self.leftover_temps_removed,
        );
        counters.insert("reconciled_missing".to_string(), self.reconciled_missing);
        counters.insert("full_sync_fallbacks".to_string(), self.full_sync_fallbacks);
        counters.insert(
            "removals_refused_open".to_string(),
            self.removals_refused_open,
        );
        counters.insert(
            "removals_refused_unobserved".to_string(),
            self.removals_refused_unobserved,
        );
        counters.insert(
            "removals_refused_changed".to_string(),
            self.removals_refused_changed,
        );
        for (reason, count) in &self.skipped {
            counters.insert(format!("skipped_{reason}"), *count);
        }
        counters
    }
}

/// The validated roots one pass works beneath.
struct Roots {
    /// The Codex home: Codex sources and their archives.
    codex: Option<Root>,
    /// Claude Code's `projects/` directory: Claude sources.
    claude: Option<Root>,
    /// Aethyme's transcript state: Claude archives.
    state: Option<Root>,
}

/// Validate one root; a missing directory is silently absent, any other
/// failure is a path-free warning.
fn validated(path: Option<PathBuf>, what: &str, warnings: &mut Vec<String>) -> Option<Root> {
    let path = path?;
    match Root::open(&path) {
        Ok(root) => Some(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            warnings.push(format!(
                "the {what} failed validation and is skipped: {error}"
            ));
            None
        }
    }
}

impl Roots {
    fn open(
        environment: &Environment,
        layout: &providers::Layout,
        warnings: &mut Vec<String>,
    ) -> Self {
        let state = match super::ensure_private_directory(&environment.host_dir) {
            Ok(()) => validated(
                Some(environment.host_dir.clone()),
                "transcript state directory",
                warnings,
            ),
            Err(error) => {
                warnings.push(format!(
                    "cannot create the transcript state directory: {error}"
                ));
                None
            }
        };
        Self {
            codex: validated(layout.codex_home.clone(), "Codex home", warnings),
            claude: validated(
                layout.claude_projects(),
                "Claude projects directory",
                warnings,
            ),
            state,
        }
    }

    /// The source and destination roots for `provider`.
    fn for_provider(&self, provider: Provider) -> Option<(&Root, &Root)> {
        match provider {
            Provider::Codex => self.codex.as_ref().map(|root| (root, root)),
            Provider::Claude => self.claude.as_ref().zip(self.state.as_ref()),
        }
    }

    fn all(&self) -> Vec<&Root> {
        [&self.codex, &self.claude, &self.state]
            .into_iter()
            .filter_map(Option::as_ref)
            .collect()
    }
}

/// Whether an archive file is still where the index says.
fn archive_present(row: &ArchiveRow) -> bool {
    !matches!(
        std::fs::symlink_metadata(&row.archive_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Drop index rows whose archive was moved away; returns how many.
fn reconcile_index(index: &Index) -> Result<u64, String> {
    let mut dropped = 0;
    for row in index.archives()? {
        if !archive_present(&row) {
            index.remove_archive(&row.original_path)?;
            dropped += 1;
        }
    }
    Ok(dropped)
}

/// Append one line to `<host dir>/runs.jsonl`, rotating it past its cap.
/// The line holds counts, reasons, timings and session ids: never a path
/// (project directory names encode repository paths) and never content.
fn append_run_log(host_dir: &Path, report: &RunReport) {
    let path = host_dir.join(RUN_LOG);
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > RUN_LOG_MAX_BYTES) {
        super::best_effort(std::fs::rename(
            &path,
            host_dir.join(format!("{RUN_LOG}.1")),
        ));
    }
    let skipped_sessions: Vec<serde_json::Value> = report
        .skipped_examples
        .iter()
        .map(|example| {
            serde_json::json!({
                "agent_session_id": example.agent_session_id,
                "reason": example.reason,
            })
        })
        .collect();
    let line = serde_json::json!({
        "at_ms": super::now_ms(),
        "apply": report.apply,
        "enabled_source": report.enabled_source,
        "first_run_gate": report.first_run_gate,
        "examined": report.examined,
        "record_tails_read": report.record_tails_read,
        "eligible": report.eligible.len(),
        "eligible_bytes": report.eligible_bytes,
        "archived": report.archived.len(),
        "bytes_in": report.bytes_in,
        "bytes_out": report.bytes_out,
        "resumed": report.resumed,
        "verify_failures": report.verify_failures,
        "skipped": report.skipped,
        "skipped_sessions": skipped_sessions,
        "linkage_degraded": report.linkage_degraded,
        "linkage_issues": report.linkage_issues,
        "insufficient_space": report.insufficient_space,
        "full_sync_fallbacks": report.full_sync_fallbacks,
        "open_listings": report.open_listings,
        "open_observation_refreshes": report.open_observation_refreshes,
        "open_files_judged": report.open_files_judged,
        "open_files_judged_max_per_observation": report.open_files_judged_max_per_observation,
        "removal_batches": report.removal_batches,
        "removals_refused_open": report.removals_refused_open,
        "removals_refused_unobserved": report.removals_refused_unobserved,
        "removals_refused_changed": report.removals_refused_changed,
        "budget_exhausted": report.budget_exhausted,
        "paced_bytes_per_second": report.paced_bytes_per_second,
        "pacing_slept_ms": report.pacing_slept_ms,
        "phases_ms": report.phases_ms,
    });
    let written = super::ensure_private_file(&path)
        .and_then(|()| OpenOptions::new().append(true).open(&path))
        .and_then(|mut file| writeln!(file, "{line}"));
    crate::warn_unrecorded("record the transcript archive run", written);
}

/// The last run the log recorded.
fn last_run(host_dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(host_dir.join(RUN_LOG)).ok()?;
    text.lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
}

/// The project directory name of a Claude transcript (its parent's name).
fn claude_project(transcript: &Transcript) -> String {
    transcript
        .path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether a Claude transcript may belong to a live session, from a process
/// listing: `Err` when the listing is unavailable.
fn claude_check(
    processes: &Result<Vec<ClaudeProcess>, String>,
    transcript: &Transcript,
) -> Result<(), SkipReason> {
    match processes {
        Err(_) => Err(SkipReason::ProcessCheckUnavailable),
        Ok(processes) => {
            if agents::claude_session_live(
                processes,
                &claude_project(transcript),
                transcript.agent_session_id.as_deref(),
            ) {
                Err(SkipReason::AgentRunning)
            } else {
                Ok(())
            }
        }
    }
}

/// Probes one pass reads the host through.
pub(crate) struct Probes<'a, P> {
    pub open_files: &'a mut OpenFiles<P>,
    pub agents: &'a mut dyn AgentProbe,
}

/// One pass. Does nothing at all, not even list a directory, unless
/// archiving is enabled.
pub(crate) fn run_pass<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    options: Options,
    probes: &mut Probes<'_, P>,
    hooks: &mut dyn Hooks,
    now_ms: i64,
) -> Result<RunReport, String> {
    let mut report = RunReport::new(config, options.apply);
    if !config.archive {
        return Ok(report);
    }
    let started = Instant::now();
    let fallbacks_before = safe_fs::full_sync_fallbacks();
    let _lock = if options.apply {
        match LaneLock::try_acquire(&environment.host_dir) {
            Ok(Some(lock)) => {
                report.lock = "acquired";
                Some(lock)
            }
            Ok(None) => {
                report.lock = "busy";
                return Ok(report);
            }
            Err(error) => {
                return Err(format!("cannot take the transcript archive lock: {error}"));
            }
        }
    } else {
        None
    };
    let index = Index::open(&environment.host_dir)?;
    let layout = environment.layout(config);

    let phase = Instant::now();
    let scan = providers::scan(&layout, &config.providers);
    report.phase("transcripts_scan", phase.elapsed());
    report.examined = scan.transcripts.len() as u64;
    report.unreadable_entries = scan.unreadable;
    report.scan_truncated = scan.truncated;
    let roots = Roots::open(environment, &layout, &mut report.warnings);
    if options.apply {
        // Only one run holds the lock, so every temporary file is a dead
        // run's: none is being written.
        report.leftover_temps_removed =
            archive::remove_leftover_temps(&scan.leftover_temps, &roots.all());
        match reconcile_index(&index) {
            Ok(dropped) => report.reconciled_missing = dropped,
            Err(error) => report.warnings.push(error),
        }
    }

    // Inactivity first, cheapest signal first: the last activity is never
    // older than the modification time, so a recently modified transcript
    // needs no tail read and no linkage work at all; one whose last record
    // is recent needs no linkage work either.
    let phase = Instant::now();
    let mut aged = Vec::new();
    for transcript in scan.transcripts {
        let days = config.days_for(transcript.provider);
        if let Err(reason) = eligibility::judge_age(now_ms, transcript.modified_ms, days) {
            report.skip(&transcript, reason, "");
            continue;
        }
        let record_ms = providers::last_record_ms(&transcript.path);
        report.record_tails_read += 1;
        match eligibility::judge_record(now_ms, record_ms, days) {
            Ok(()) => aged.push((transcript, record_ms)),
            Err(reason) => report.skip(&transcript, reason, ""),
        }
    }
    report.phase("transcripts_inactivity", phase.elapsed());

    let phase = Instant::now();
    let step = Instant::now();
    let cwds: BTreeSet<PathBuf> = aged
        .iter()
        .filter_map(|(transcript, _)| providers::transcript_cwd(&transcript.path))
        .collect();
    report.phase("transcripts_linkage_cwds", step.elapsed());
    let step = Instant::now();
    let known = linkage::known_broker_dbs(
        &config.repositories,
        &index,
        &cwds,
        environment.use_registry,
    );
    report.phase("transcripts_linkage_databases", step.elapsed());
    report.warnings.extend(known.warnings);
    report.linkage_issues.index_unreadable = known.index_unreadable;
    report.linkage_issues.registry_incomplete = known.registry_incomplete;
    report.broker_databases = known.databases.len() as u64;
    let step = Instant::now();
    let mut unreadable_databases = 0;
    for db in &known.databases {
        match linkage::backfill(&index, db, now_ms) {
            Ok(added) => report.links_added += added,
            Err(_) => unreadable_databases += 1,
        }
    }
    report.phase("transcripts_linkage_backfill", step.elapsed());
    report.linkage_issues.unreadable_databases = unreadable_databases;
    if unreadable_databases > 0 {
        // Counted, never named: logs carry no path outside the provider roots.
        report.warnings.push(format!(
            "{unreadable_databases} broker database(s) could not be read; unlinked transcripts are skipped this pass"
        ));
    }
    let step = Instant::now();
    match linkage::link_live_holders(
        &index,
        &known.databases,
        &layout,
        &config.providers,
        probes.open_files,
        now_ms,
    ) {
        Ok(live) => {
            report.links_added += live.added;
            if live.incomplete {
                report.linkage_issues.live_holders_incomplete = true;
                report.warnings.push(
                    "live session holders could not all be listed; unlinked transcripts are skipped this pass"
                        .into(),
                );
            }
        }
        Err(error) => {
            report.linkage_issues.live_holders_incomplete = true;
            report.warnings.push(error);
        }
    }
    report.phase("transcripts_linkage_live_holders", step.elapsed());
    let degraded = report.linkage_issues.any();
    report.linkage_degraded = degraded;
    report.phase("transcripts_linkage", phase.elapsed());

    let phase = Instant::now();
    let mut databases = BrokerDbs::default();
    let mut claude_processes: Option<Result<Vec<ClaudeProcess>, String>> = None;
    let mut eligible = Vec::new();
    for (transcript, record_ms) in aged {
        let linkage =
            match index.links_for(transcript.agent_session_id.as_deref(), &transcript.path) {
                Ok(links) => databases.linkage(&links),
                Err(_) => Linkage::Unreadable,
            };
        // An unlinked transcript is only trusted when every source of links
        // was read: otherwise it may belong to a session we could not see.
        let linkage = if degraded && linkage == Linkage::Unlinked {
            Linkage::Unreadable
        } else {
            linkage
        };
        let facts = Facts {
            now_ms,
            modified_ms: transcript.modified_ms,
            record_ms,
            inactive_days: config.days_for(transcript.provider),
            linkage: &linkage,
        };
        let open = match layout.open_root(transcript.provider) {
            Some(_) => probes.open_files.state_of(&transcript.path),
            None => eligibility::OpenState::Unknown,
        };
        if let Err(reason) = eligibility::judge(&facts, open) {
            report.skip(&transcript, reason, "");
            continue;
        }
        if transcript.provider == Provider::Claude {
            let processes =
                claude_processes.get_or_insert_with(|| probes.agents.claude_processes());
            if let Err(reason) = claude_check(processes, &transcript) {
                report.skip(&transcript, reason, "");
                continue;
            }
        }
        eligible.push(transcript);
    }
    // Largest first, so space comes back early.
    eligible.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    report.eligible_bytes = eligible.iter().map(|transcript| transcript.bytes).sum();
    report.eligible = eligible.iter().map(TranscriptView::of).collect();
    report.phase("transcripts_judge", phase.elapsed());

    if options.apply {
        let pass = Pass {
            environment,
            config,
            options,
            index: &index,
            layout: &layout,
            roots: &roots,
            now_ms,
        };
        pass.archive_eligible(eligible, probes, hooks, &mut report);
        report.full_sync_fallbacks = safe_fs::full_sync_fallbacks() - fallbacks_before;
        if let Err(error) = index.add_counters(&report.counters()) {
            report.warnings.push(error);
        }
    }
    report.open_listings = probes.open_files.probes;
    report.open_observation_refreshes = probes.open_files.refreshes;
    report.open_files_judged = probes.open_files.judged;
    report.open_files_judged_max_per_observation = probes.open_files.max_judged_per_observation;
    report.phase("transcripts_open_check", probes.open_files.probe_time);
    report.phase("transcripts_total", started.elapsed());
    if options.apply {
        record_pass_health(&index, &report);
        append_run_log(&environment.host_dir, &report);
    }
    Ok(report)
}

/// What the archiving half of a pass shares.
struct Pass<'a> {
    environment: &'a Environment,
    config: &'a TranscriptsConfig,
    options: Options,
    index: &'a Index,
    layout: &'a providers::Layout,
    roots: &'a Roots,
    now_ms: i64,
}

impl Pass<'_> {
    /// Free bytes on the volume an archive of `transcript` would go to.
    fn available_for(&self, transcript: &Transcript) -> Option<u64> {
        if let Some(bytes) = self.environment.free_space_override {
            return Some(bytes);
        }
        crate::disk_headroom::available_headroom_at_or_above(&transcript.destination)
            .map(|headroom| headroom.bytes)
    }

    /// Remove the originals of a batch of published copies, judged against
    /// one host-wide open-file observation taken now, after the batch's last
    /// copy was verified. A source that is open in it, or changed since it
    /// was compressed, keeps its place and its copy is discarded. An
    /// incomplete observation removes nothing and discards every copy of
    /// the batch: the pass's state is then exactly as before it compressed
    /// them (no index row for an archive whose original is still there), at
    /// the cost of compressing them again on a later pass.
    fn flush_removals<P: OpenFileProbe>(
        &self,
        pending: &mut Vec<archive::Published>,
        transcripts: &mut Vec<Transcript>,
        probes: &mut Probes<'_, P>,
        report: &mut RunReport,
    ) {
        if pending.is_empty() {
            return;
        }
        let started = Instant::now();
        report.removal_batches += 1;
        probes.open_files.renew();
        for (published, transcript) in pending.drain(..).zip(transcripts.drain(..)) {
            let Some((source, destination)) = self.roots.for_provider(transcript.provider) else {
                report.skip(
                    &transcript,
                    SkipReason::UnsafePath,
                    "provider root failed validation",
                );
                continue;
            };
            let confinement = archive::Confinement {
                source,
                destination,
            };
            match archive::remove_published(
                self.index,
                probes.open_files,
                &transcript,
                &confinement,
                published.before,
            ) {
                Ok(()) => {
                    let done = published.archived;
                    report.bytes_in += done.bytes_in;
                    report.bytes_out += done.bytes_out;
                    if done.resumed {
                        report.resumed += 1;
                    }
                    report.archived.push(ArchivedView {
                        provider: transcript.provider,
                        path: transcript.path.clone(),
                        destination: transcript.destination.clone(),
                        bytes_in: done.bytes_in,
                        bytes_out: done.bytes_out,
                        sha256: done.sha256,
                        resumed: done.resumed,
                    });
                }
                Err(skipped) => {
                    match skipped.reason {
                        SkipReason::OpenByProcess => report.removals_refused_open += 1,
                        SkipReason::OpenCheckUnavailable => {
                            report.removals_refused_unobserved += 1;
                        }
                        SkipReason::ModifiedDuringCompression => {
                            report.removals_refused_changed += 1;
                        }
                        _ => {}
                    }
                    report.skip(&transcript, skipped.reason, &skipped.detail);
                }
            }
        }
        report.phase("transcripts_remove", started.elapsed());
    }

    fn archive_eligible<P: OpenFileProbe>(
        &self,
        mut remaining: Vec<Transcript>,
        probes: &mut Probes<'_, P>,
        hooks: &mut dyn Hooks,
        report: &mut RunReport,
    ) {
        let started = Instant::now();
        let mut pacer = Pacer::new(Some(self.config.effective_bytes_per_second()));
        let mut compress = Duration::ZERO;
        let mut verify = Duration::ZERO;
        // Published copies whose originals are still in place; removed in
        // batches (see `flush_removals`), and always before this returns.
        let mut pending: Vec<archive::Published> = Vec::new();
        let mut pending_transcripts: Vec<Transcript> = Vec::new();
        let mut batch_started = Instant::now();
        while !remaining.is_empty() {
            if pending.len() >= REMOVAL_BATCH_FILES
                || (!pending.is_empty() && batch_started.elapsed() >= REMOVAL_BATCH_AGE)
            {
                self.flush_removals(&mut pending, &mut pending_transcripts, probes, report);
            }
            if self
                .options
                .budget
                .is_some_and(|budget| started.elapsed() >= budget)
            {
                report.budget_exhausted = true;
                break;
            }
            // The largest transcript whose compressed copy fits next to it:
            // the original only goes after the copy exists, so the lane must
            // never need space it does not have.
            let chosen = remaining
                .iter()
                .enumerate()
                .find_map(|(position, transcript)| {
                    let available = self.available_for(transcript)?;
                    pacing::fits(available, transcript.bytes).then_some((position, available))
                });
            let Some((position, available)) = chosen else {
                if !pending.is_empty() {
                    // Removing the batch's originals frees their space.
                    self.flush_removals(&mut pending, &mut pending_transcripts, probes, report);
                    continue;
                }
                report.insufficient_space = true;
                for _ in 0..remaining.len() {
                    report.count_skip(SkipReason::InsufficientSpace);
                }
                report.warnings.push(
                    "insufficient_space: no remaining eligible transcript fits in the free space of its archive volume (it needs a third of its size plus 256 MiB); free space elsewhere, then run again"
                        .into(),
                );
                break;
            };
            let transcript = remaining.remove(position);
            if self.layout.open_root(transcript.provider).is_none() {
                continue;
            }
            let Some((source, destination)) = self.roots.for_provider(transcript.provider) else {
                report.skip(
                    &transcript,
                    SkipReason::UnsafePath,
                    "provider root failed validation",
                );
                continue;
            };
            if !self
                .index
                .conflict_due(&transcript.path, self.now_ms)
                .unwrap_or(true)
            {
                report.skip(&transcript, SkipReason::DestinationConflictBackoff, "");
                continue;
            }
            if transcript.provider == Provider::Claude {
                // Fresh: a session may have started since the pass judged it.
                let processes = probes.agents.claude_processes();
                if let Err(reason) = claude_check(&processes, &transcript) {
                    report.skip(&transcript, reason, "");
                    continue;
                }
            }
            let confinement = archive::Confinement {
                source,
                destination,
            };
            pacer.set_rate(pacing::choose_rate(
                self.config.effective_bytes_per_second(),
                Some(available),
                self.config.free_space_floor_bytes,
            ));
            let mut context = ArchiveContext {
                index: self.index,
                open_files: &mut *probes.open_files,
                pacer: &mut pacer,
                hooks: &mut *hooks,
                now_ms: self.now_ms,
            };
            match archive::archive_one(&mut context, &transcript, &confinement) {
                Ok(published) => {
                    super::best_effort(self.index.clear_conflict(&transcript.path));
                    compress += published.archived.compress_time;
                    verify += published.archived.verify_time;
                    if pending.is_empty() {
                        batch_started = Instant::now();
                    }
                    pending.push(published);
                    pending_transcripts.push(transcript);
                }
                Err(skipped) => {
                    if skipped.reason == SkipReason::DestinationConflict {
                        // Recorded once, then retried with a growing backoff.
                        super::best_effort(
                            self.index.record_conflict(&transcript.path, self.now_ms),
                        );
                    }
                    report.skip(&transcript, skipped.reason, &skipped.detail);
                }
            }
        }
        // Every exit from the loop (done, budget, no space) lands here.
        self.flush_removals(&mut pending, &mut pending_transcripts, probes, report);
        report.paced_bytes_per_second = pacer.rate();
        report.pacing_slept_ms = pacer.slept().as_millis() as u64;
        report.phase("transcripts_compress", compress);
        report.phase("transcripts_verify", verify);
        report.phase("transcripts_archive", started.elapsed());
    }
}

/// One background-worker pass: nothing while archiving is off; on a
/// default-on host, nothing until the first-run notice has been shown for
/// 24 hours (the returned report then names the gate and examined nothing);
/// otherwise one paced, budgeted, applied pass.
pub(crate) fn worker_pass<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    probes: &mut Probes<'_, P>,
    hooks: &mut dyn Hooks,
    now_ms: i64,
) -> Result<RunReport, String> {
    let options = Options {
        apply: true,
        budget: Some(WORKER_RUN_BUDGET),
    };
    if config.archive {
        let state = first_run::read(&environment.host_dir);
        let gate = first_run::gate(config, &state, now_ms);
        if gate != Gate::Open {
            let mut report = RunReport::new(config, options.apply);
            report.first_run_gate = gate;
            return Ok(report);
        }
    }
    run_pass(environment, config, options, probes, hooks, now_ms)
}

/// Record that the owner archived by hand, which lifts the first-run wait
/// for the background worker too. Best effort: a failure only means the
/// worker keeps waiting out the notice.
pub(crate) fn acknowledge_manual_apply(environment: &Environment, now_ms: i64) -> Option<String> {
    first_run::acknowledge(&environment.host_dir, now_ms, "archive_apply").err()
}

/// The background worker's lane: [`worker_pass`] with the host's settings.
/// Runs only inside the detached worker, which has already lowered itself
/// to background CPU and I/O priority. Memory pressure does not pause it:
/// it holds one 1 MiB buffer at a time.
pub(crate) fn run_worker_lane() {
    if !matches!(
        std::env::var(crate::gc::ARTIFACT_SWEEP_WORKER_ENV).as_deref(),
        Ok("1")
    ) {
        return;
    }
    let Some((environment, config)) = super::host_settings() else {
        return;
    };
    if !config.archive {
        return;
    }
    let now_ms = super::now_ms();
    if !worker_pass_due(&environment.host_dir, now_ms) {
        crate::phase_telemetry::add("transcripts_worker_pass_not_due", Duration::ZERO);
        return;
    }
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    match worker_pass(environment, config, &mut probes, &mut NoHooks, now_ms) {
        Ok(report) => record_worker_pass(&environment.host_dir, &report, now_ms),
        Err(error) => {
            crate::warn_unrecorded("archive closed agent transcripts", Err::<(), _>(error));
        }
    }
}

/// `transcripts status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub schema: &'static str,
    pub enabled: bool,
    /// `default` (no explicit `archive` setting) or `settings`.
    pub enabled_source: EnabledSource,
    /// Whether the background worker may archive yet (default-on hosts wait
    /// 24 hours after the `broker status` notice).
    pub first_run: FirstRunStatus,
    pub settings_file: PathBuf,
    pub config: TranscriptsConfig,
    pub archived_files: u64,
    /// Bytes the archived transcripts took before compression.
    pub archived_bytes_before: u64,
    /// Bytes they take now.
    pub archived_bytes_after: u64,
    /// Bytes an archive pass could move right now (empty when disabled).
    pub reclaimable_now_bytes: u64,
    pub eligible_now: u64,
    pub skipped_now: BTreeMap<String, u64>,
    pub counters: BTreeMap<String, u64>,
    pub lock_holder: Option<String>,
    pub last_run: Option<serde_json::Value>,
    /// Whether session links could be read, now and over recent passes.
    pub linkage: LinkageStatus,
    /// Sources kept because their copy could not be made durable.
    pub durability: DurabilityStatus,
    pub warnings: Vec<String>,
}

/// Linkage health in `transcripts status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinkageStatus {
    pub degraded_now: bool,
    pub issues_now: LinkageIssues,
    /// Applied passes in a row that were degraded.
    pub consecutive_degraded_passes: u64,
    pub last_pass_issues: Option<LinkageIssues>,
}

/// Durability health in `transcripts status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DurabilityStatus {
    /// Transcripts left in place because a sync of their copy failed.
    pub sources_kept_sync_failed: u64,
    /// Full syncs that fell back to `fsync` (`F_FULLFSYNC` unsupported).
    pub full_sync_fallbacks: u64,
    /// Restores that published and have not finished removing their archive
    /// (the next `restore` of the same transcript finishes them).
    pub restore_intents_pending: u64,
}

/// Every archived transcript the index records (empty when disabled: a
/// disabled host touches nothing but its settings).
pub(crate) fn archived_rows(
    environment: &Environment,
    config: &TranscriptsConfig,
) -> Result<Vec<ArchiveRow>, String> {
    if !config.archive {
        return Ok(Vec::new());
    }
    match Index::open_existing(&environment.host_dir)? {
        Some(index) => Ok(index
            .archives()?
            .into_iter()
            .filter(archive_present)
            .collect()),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn status<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    probes: &mut Probes<'_, P>,
) -> Result<StatusReport, String> {
    let mut status = StatusReport {
        schema: STATUS_SCHEMA,
        enabled: config.archive,
        enabled_source: config.enabled_source,
        first_run: first_run::status(config, &environment.host_dir, super::now_ms()),
        settings_file: environment.host_dir.join(super::SETTINGS_FILE),
        config: config.clone(),
        archived_files: 0,
        archived_bytes_before: 0,
        archived_bytes_after: 0,
        reclaimable_now_bytes: 0,
        eligible_now: 0,
        skipped_now: BTreeMap::new(),
        counters: BTreeMap::new(),
        lock_holder: None,
        last_run: None,
        linkage: LinkageStatus::default(),
        durability: DurabilityStatus::default(),
        warnings: Vec::new(),
    };
    if !config.archive {
        // Disabled means no file-system access beyond the settings.
        return Ok(status);
    }
    if let Some(index) = Index::open_existing(&environment.host_dir)? {
        // Rows whose archive was moved away (by `codex unarchive`, say) are
        // not counted, even before the next pass drops them.
        let rows: Vec<ArchiveRow> = index
            .archives()?
            .into_iter()
            .filter(archive_present)
            .collect();
        status.archived_files = rows.len() as u64;
        status.archived_bytes_before = rows.iter().map(|row| row.bytes_in).sum();
        status.archived_bytes_after = rows.iter().map(|row| row.bytes_out).sum();
        status.counters = index.counters()?;
        status.durability.restore_intents_pending = index.pending_restore_intents().unwrap_or(0);
        let (passes, issues) = pass_health(&index);
        status.linkage.consecutive_degraded_passes = passes;
        status.linkage.last_pass_issues = issues;
    }
    status.durability = DurabilityStatus {
        restore_intents_pending: status.durability.restore_intents_pending,
        sources_kept_sync_failed: status
            .counters
            .get("skipped_sync_failed")
            .copied()
            .unwrap_or(0),
        full_sync_fallbacks: status
            .counters
            .get("full_sync_fallbacks")
            .copied()
            .unwrap_or(0),
    };
    status.lock_holder = LaneLock::recorded_holder(&environment.host_dir);
    status.last_run = last_run(&environment.host_dir);
    let dry = run_pass(
        environment,
        config,
        Options {
            apply: false,
            budget: None,
        },
        probes,
        &mut NoHooks,
        super::now_ms(),
    )?;
    status.reclaimable_now_bytes = dry.eligible_bytes;
    status.eligible_now = dry.eligible.len() as u64;
    status.skipped_now = dry.skipped;
    status.linkage.degraded_now = dry.linkage_degraded;
    status.linkage.issues_now = dry.linkage_issues;
    status.warnings = dry.warnings;
    Ok(status)
}

/// Where `key` (an agent session id, an original path or an archive path)
/// restores from and to.
pub(crate) fn restore_plans(
    environment: &Environment,
    config: &TranscriptsConfig,
    key: &str,
) -> Result<Vec<RestorePlan>, String> {
    let index = Index::open_existing(&environment.host_dir)?;
    let rows = match &index {
        Some(index) => index.find_archives(key)?,
        None => Vec::new(),
    };
    if !rows.is_empty() {
        return Ok(rows
            .into_iter()
            .map(|row| RestorePlan {
                archive: row.archive_path,
                original: row.original_path,
                sha256: Some(row.sha256),
            })
            .collect());
    }
    // Not indexed (archived by another host, or the index was lost): a
    // Codex archive is found by its id, and returns to where Codex keeps a
    // transcript of that name; the zstd frame checksum vouches for it.
    let layout = environment.layout(&TranscriptsConfig {
        providers: vec![Provider::Codex],
        ..config.clone()
    });
    let (Some(archived), Some(sessions)) = (layout.codex_archived(), layout.codex_sessions())
    else {
        return Ok(Vec::new());
    };
    let wanted = key.to_ascii_lowercase();
    let mut plans = Vec::new();
    let entries = match std::fs::read_dir(&archived) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(plain) = name.strip_suffix(providers::ARCHIVE_SUFFIX) else {
            continue;
        };
        let matches = providers::session_id_from_file_name(Provider::Codex, name)
            .is_some_and(|id| id == wanted)
            || path == Path::new(key);
        if matches && let Some(original) = providers::codex_session_path_for(&sessions, plain) {
            plans.push(RestorePlan {
                archive: path.clone(),
                original,
                sha256: None,
            });
        }
    }
    plans.sort_by(|a, b| a.archive.cmp(&b.archive));
    Ok(plans)
}

/// Refuse to merge into a transcript whose session may still be writing
/// it. Unknown counts as live.
fn refuse_live_merge<P: OpenFileProbe>(
    plan: &RestorePlan,
    provider: Provider,
    open_root: Option<&Path>,
    probes: &mut Probes<'_, P>,
) -> Result<(), String> {
    let session_id = plan
        .original
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| providers::session_id_from_file_name(provider, name));
    let original = plan.original.display();
    match provider {
        Provider::Claude => {
            let project = plan
                .original
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let processes = probes.agents.claude_processes().map_err(|error| {
                format!(
                    "{original} has a newer transcript and a live Claude Code session cannot be ruled out ({error}); retry when the process table can be read"
                )
            })?;
            if agents::claude_session_live(&processes, &project, session_id.as_deref()) {
                return Err(format!(
                    "{original} has a newer transcript and a Claude Code session for its project is running; close that session, then restore"
                ));
            }
        }
        Provider::Codex => {
            let open = match open_root {
                Some(_) => probes.open_files.fresh_state_of(&plan.original),
                None => eligibility::OpenState::Unknown,
            };
            match open {
                eligibility::OpenState::Closed => {}
                eligibility::OpenState::Open => {
                    return Err(format!(
                        "{original} is open in a running process; close that Codex session, then restore"
                    ));
                }
                eligibility::OpenState::Unknown => {
                    return Err(format!(
                        "{original} has a newer transcript and its open-file listing is unavailable; retry later"
                    ));
                }
            }
            let lines = probes.agents.codex_command_lines().map_err(|error| {
                format!(
                    "{original} has a newer transcript and a live Codex session cannot be ruled out ({error}); retry later"
                )
            })?;
            if session_id
                .as_deref()
                .is_some_and(|id| agents::names_session(&lines, id))
            {
                return Err(format!(
                    "{original} has a newer transcript and a Codex process for that session is running; close it, then restore"
                ));
            }
        }
    }
    Ok(())
}

/// Restore every archive `key` names, under the lane lock. A merge into a
/// newer transcript is refused while its session may be live.
pub(crate) fn restore<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    key: &str,
    probes: &mut Probes<'_, P>,
) -> Result<Vec<Restored>, String> {
    let plans = restore_plans(environment, config, key)?;
    if plans.is_empty() {
        return Err(format!("no archived transcript matches {key}"));
    }
    let Some(_lock) = LaneLock::try_acquire(&environment.host_dir)
        .map_err(|error| format!("cannot take the transcript archive lock: {error}"))?
    else {
        return Err("an archive run is in progress; retry when it finishes".into());
    };
    let index = Index::open(&environment.host_dir)?;
    let layout = environment.layout(&TranscriptsConfig {
        providers: vec![Provider::Codex, Provider::Claude],
        ..config.clone()
    });
    let mut warnings = Vec::new();
    let roots = Roots::open(environment, &layout, &mut warnings);
    let mut restored = Vec::new();
    for plan in &plans {
        let (archive_root, original_root, provider) =
            match (&roots.codex, &roots.claude, &roots.state) {
                (Some(codex), _, _) if codex.contains(&plan.archive) => {
                    (codex, codex, Provider::Codex)
                }
                (_, Some(claude), Some(state)) if state.contains(&plan.archive) => {
                    (state, claude, Provider::Claude)
                }
                _ => {
                    let mut message = format!(
                        "{} is not beneath a validated provider root",
                        plan.archive.display()
                    );
                    if let Some(warning) = warnings.first() {
                        message.push_str(&format!(" ({warning})"));
                    }
                    return Err(message);
                }
            };
        // A newer transcript at the original path means a merge (or the
        // cleanup of an interrupted one): refused while its session may be
        // writing it.
        if std::fs::symlink_metadata(&plan.original).is_ok() {
            let open_root = layout.open_root(provider);
            refuse_live_merge(plan, provider, open_root.as_deref(), probes)?;
        }
        let done = archive::restore_one(
            plan,
            archive_root,
            original_root,
            Some(&index),
            super::now_ms(),
        )?;
        super::best_effort(index.clear_conflict(&plan.original));
        restored.push(done);
    }
    let mut counters = BTreeMap::new();
    counters.insert("restored".to_string(), restored.len() as u64);
    index.add_counters(&counters)?;
    Ok(restored)
}

#[cfg(test)]
mod worker_cadence_tests {
    use super::*;

    fn report(eligible: usize, budget_exhausted: bool) -> RunReport {
        let mut report = RunReport::new(&TranscriptsConfig::default(), true);
        report.first_run_gate = Gate::Open;
        report.budget_exhausted = budget_exhausted;
        report.eligible = (0..eligible)
            .map(|index| TranscriptView {
                provider: Provider::Claude,
                path: PathBuf::from(format!("transcript-{index}.jsonl")),
                destination: PathBuf::from(format!("archive-{index}.jsonl.zst")),
                agent_session_id: None,
                bytes: 1,
            })
            .collect();
        report
    }

    #[test]
    fn an_idle_pass_defers_the_next_worker_pass_on_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let interval = i64::try_from(WORKER_PASS_INTERVAL.as_millis()).unwrap();
        let at = 1_800_000_000_000_i64;
        // No record: due.
        assert!(worker_pass_due(dir.path(), at));
        record_worker_pass(dir.path(), &report(0, false), at);
        assert!(!worker_pass_due(dir.path(), at + 1));
        assert!(!worker_pass_due(dir.path(), at + interval - 1));
        assert!(worker_pass_due(dir.path(), at + interval));
        // A clock that went back runs the pass.
        assert!(worker_pass_due(dir.path(), at - 1));
        // A pass that found work, or ran out of budget, does not defer.
        record_worker_pass(dir.path(), &report(1, false), at);
        assert!(worker_pass_due(dir.path(), at + 1));
        record_worker_pass(dir.path(), &report(0, true), at);
        assert!(worker_pass_due(dir.path(), at + 1));
        // An unreadable record runs the pass.
        std::fs::write(dir.path().join(LAST_WORKER_PASS_FILE), b"{").unwrap();
        assert!(worker_pass_due(dir.path(), at + 1));
    }
}
