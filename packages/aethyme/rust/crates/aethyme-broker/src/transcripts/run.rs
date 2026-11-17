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
use super::eligibility::{self, Facts, Linkage, SkipReason};
use super::index::{ArchiveRow, Index};
use super::linkage::{self, BrokerDbs};
use super::open_files::{OpenFileProbe, OpenFiles, SystemProbe};
use super::pacing::{self, Pacer};
use super::providers::{self, Provider, Transcript};
use super::safe_fs::{self, Root};
use super::{Environment, LaneLock, TranscriptsConfig};

pub const RUN_SCHEMA: &str = "aethyme.transcripts-archive/v1";
pub const STATUS_SCHEMA: &str = "aethyme.transcripts-status/v1";
/// How long an open-file listing is reused before it is taken again (the
/// listing right before an original is removed is always fresh).
pub(crate) const OPEN_LISTING_MAX_AGE: Duration = Duration::from_secs(2);
/// Wall time one background-worker pass may spend; the next pass resumes.
pub(crate) const WORKER_RUN_BUDGET: Duration = Duration::from_secs(10 * 60);
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

/// What one pass found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunReport {
    pub schema: &'static str,
    pub enabled: bool,
    pub apply: bool,
    /// `acquired`, `busy` (another run holds it; nothing was done) or
    /// `not_taken` (a dry run reads only).
    pub lock: &'static str,
    pub examined: u64,
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
    /// Open-file listings taken (each covers one provider root).
    pub open_listings: u64,
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
    fn new(enabled: bool, apply: bool) -> Self {
        Self {
            schema: RUN_SCHEMA,
            enabled,
            apply,
            lock: "not_taken",
            examined: 0,
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
            open_listings: 0,
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
            SkipReason::TooRecent | SkipReason::DestinationConflictBackoff
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
        "examined": report.examined,
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
        "insufficient_space": report.insufficient_space,
        "full_sync_fallbacks": report.full_sync_fallbacks,
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
    let mut report = RunReport::new(config.archive, options.apply);
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

    // The age rule first: recent transcripts need no linkage work at all.
    let mut aged = Vec::new();
    for transcript in scan.transcripts {
        match eligibility::judge_age(
            now_ms,
            transcript.modified_ms,
            config.days_for(transcript.provider),
        ) {
            Ok(()) => aged.push(transcript),
            Err(reason) => report.skip(&transcript, reason, ""),
        }
    }

    let phase = Instant::now();
    let cwds: BTreeSet<PathBuf> = aged
        .iter()
        .filter_map(|transcript| providers::transcript_cwd(&transcript.path))
        .collect();
    let known = linkage::known_broker_dbs(
        &config.repositories,
        &index,
        &cwds,
        environment.use_registry,
    );
    report.warnings.extend(known.warnings);
    let mut degraded = known.index_unreadable;
    report.broker_databases = known.databases.len() as u64;
    let mut unreadable_databases = 0;
    for db in &known.databases {
        match linkage::backfill(&index, db, now_ms) {
            Ok(added) => report.links_added += added,
            Err(_) => unreadable_databases += 1,
        }
    }
    if unreadable_databases > 0 {
        degraded = true;
        // Counted, never named: logs carry no path outside the provider roots.
        report.warnings.push(format!(
            "{unreadable_databases} broker database(s) could not be read; unlinked transcripts are skipped this pass"
        ));
    }
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
                degraded = true;
                report.warnings.push(
                    "live session holders could not all be listed; unlinked transcripts are skipped this pass"
                        .into(),
                );
            }
        }
        Err(error) => {
            degraded = true;
            report.warnings.push(error);
        }
    }
    report.linkage_degraded = degraded;
    report.phase("transcripts_linkage", phase.elapsed());

    let phase = Instant::now();
    let mut databases = BrokerDbs::default();
    let mut claude_processes: Option<Result<Vec<ClaudeProcess>, String>> = None;
    let mut eligible = Vec::new();
    for transcript in aged {
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
            archive_after_days: config.days_for(transcript.provider),
            linkage: &linkage,
        };
        let open = match layout.open_root(transcript.provider) {
            Some(root) => probes.open_files.state_of(&root, &transcript.path),
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
    report.phase("transcripts_open_check", probes.open_files.probe_time);
    report.phase("transcripts_total", started.elapsed());
    if options.apply {
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
        while !remaining.is_empty() {
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
            let Some(open_root) = self.layout.open_root(transcript.provider) else {
                continue;
            };
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
                open_root: &open_root,
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
                Ok(done) => {
                    super::best_effort(self.index.clear_conflict(&transcript.path));
                    compress += done.compress_time;
                    verify += done.verify_time;
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
        report.paced_bytes_per_second = pacer.rate();
        report.pacing_slept_ms = pacer.slept().as_millis() as u64;
        report.phase("transcripts_compress", compress);
        report.phase("transcripts_verify", verify);
        report.phase("transcripts_archive", started.elapsed());
    }
}

/// The background worker's lane: one paced, budgeted pass when this host
/// opted in, nothing otherwise. Runs only inside the detached worker, which
/// has already lowered itself to background CPU and I/O priority. Memory
/// pressure does not pause it: it holds one 1 MiB buffer at a time.
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
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let mut agents = SystemAgents;
    let mut probes = Probes {
        open_files: &mut open_files,
        agents: &mut agents,
    };
    let options = Options {
        apply: true,
        budget: Some(WORKER_RUN_BUDGET),
    };
    crate::warn_unrecorded(
        "archive closed agent transcripts",
        run_pass(
            environment,
            config,
            options,
            &mut probes,
            &mut NoHooks,
            super::now_ms(),
        ),
    );
}

/// `transcripts status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub schema: &'static str,
    pub enabled: bool,
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
    pub warnings: Vec<String>,
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
    }
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

/// Restore every archive `key` names, under the lane lock.
pub(crate) fn restore(
    environment: &Environment,
    config: &TranscriptsConfig,
    key: &str,
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
        let (archive_root, original_root) = match (&roots.codex, &roots.claude, &roots.state) {
            (Some(codex), _, _) if codex.contains(&plan.archive) => (codex, codex),
            (_, Some(claude), Some(state)) if state.contains(&plan.archive) => (state, claude),
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
        let done = archive::restore_one(plan, archive_root, original_root, Some(&index))?;
        super::best_effort(index.clear_conflict(&plan.original));
        restored.push(done);
    }
    let mut counters = BTreeMap::new();
    counters.insert("restored".to_string(), restored.len() as u64);
    index.add_counters(&counters)?;
    Ok(restored)
}
