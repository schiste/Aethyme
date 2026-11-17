//! One archiving pass: scan, link, judge, then (with `apply`) archive the
//! eligible transcripts largest first, so space comes back early. Plus the
//! status and restore entry points the CLI shares.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::archive::{self, ArchiveContext, Hooks, NoHooks, RestorePlan, Restored};
use super::eligibility::{self, Facts, SkipReason};
use super::index::{ArchiveRow, Index};
use super::linkage::{self, BrokerDbs};
use super::open_files::{OpenFileProbe, OpenFiles, SystemProbe};
use super::pacing::{self, Pacer};
use super::providers::{self, Provider, Transcript};
use super::{Environment, LaneLock, TranscriptsConfig};

pub const RUN_SCHEMA: &str = "aethyme.transcripts-archive/v1";
pub const STATUS_SCHEMA: &str = "aethyme.transcripts-status/v1";
/// How long an open-file listing is reused before it is taken again.
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

/// One skipped transcript in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedView {
    pub path: PathBuf,
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
    pub unreadable_entries: u64,
    pub scan_truncated: bool,
    pub broker_databases: u64,
    pub links_added: u64,
    /// Open-file listings taken (each covers one provider root).
    pub open_listings: u64,
    pub paced_bytes_per_second: Option<u64>,
    pub pacing_slept_ms: u64,
    pub budget_exhausted: bool,
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
            unreadable_entries: 0,
            scan_truncated: false,
            broker_databases: 0,
            links_added: 0,
            open_listings: 0,
            paced_bytes_per_second: None,
            pacing_slept_ms: 0,
            budget_exhausted: false,
            phases_ms: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    fn skip(&mut self, path: &Path, reason: SkipReason, detail: &str) {
        *self.skipped.entry(reason.label().to_string()).or_default() += 1;
        if reason == SkipReason::VerifyFailed {
            self.verify_failures += 1;
        }
        if reason != SkipReason::TooRecent && self.skipped_examples.len() < MAX_SKIPPED_EXAMPLES {
            self.skipped_examples.push(SkippedView {
                path: path.to_path_buf(),
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
        for (reason, count) in &self.skipped {
            counters.insert(format!("skipped_{reason}"), *count);
        }
        counters
    }
}

fn remove_leftover_temps(paths: &[PathBuf]) -> u64 {
    let mut removed = 0;
    for path in paths {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
            if let Some(parent) = path.parent() {
                super::sync_directory(parent);
            }
        }
    }
    removed
}

/// Append one line to `<host dir>/runs.jsonl`, rotating it past its cap.
fn append_run_log(host_dir: &Path, report: &RunReport) {
    let path = host_dir.join(RUN_LOG);
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > RUN_LOG_MAX_BYTES) {
        let _ = std::fs::rename(&path, host_dir.join(format!("{RUN_LOG}.1")));
    }
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
        "budget_exhausted": report.budget_exhausted,
        "paced_bytes_per_second": report.paced_bytes_per_second,
        "pacing_slept_ms": report.pacing_slept_ms,
        "phases_ms": report.phases_ms,
    });
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
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

/// One pass. Does nothing at all, not even list a directory, unless
/// archiving is enabled.
pub(crate) fn run_pass<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    options: Options,
    open_files: &mut OpenFiles<P>,
    hooks: &mut dyn Hooks,
    now_ms: i64,
) -> Result<RunReport, String> {
    let mut report = RunReport::new(config.archive, options.apply);
    if !config.archive {
        return Ok(report);
    }
    let started = Instant::now();
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
                return Err(format!(
                    "cannot take the transcript archive lock in {}: {error}",
                    environment.host_dir.display()
                ));
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
    if options.apply {
        // Only one run holds the lock, so every temporary file is a dead
        // run's: none is being written.
        report.leftover_temps_removed = remove_leftover_temps(&scan.leftover_temps);
    }

    // The age rule first: recent transcripts need no linkage work at all.
    let mut aged = Vec::new();
    for transcript in scan.transcripts {
        match eligibility::judge_age(now_ms, transcript.modified_ms, config.archive_after_days) {
            Ok(()) => aged.push(transcript),
            Err(reason) => report.skip(&transcript.path, reason, ""),
        }
    }

    let phase = Instant::now();
    let cwds: BTreeSet<PathBuf> = aged
        .iter()
        .filter_map(|transcript| providers::transcript_cwd(&transcript.path))
        .collect();
    let (broker_dbs, warnings) = linkage::known_broker_dbs(
        &config.repositories,
        &index,
        &cwds,
        environment.use_registry,
    );
    report.warnings.extend(warnings);
    report.broker_databases = broker_dbs.len() as u64;
    for db in &broker_dbs {
        match linkage::backfill(&index, db, now_ms) {
            Ok(added) => report.links_added += added,
            Err(error) => report.warnings.push(error),
        }
    }
    match linkage::link_live_holders(
        &index,
        &broker_dbs,
        &layout,
        &config.providers,
        open_files,
        now_ms,
    ) {
        Ok(added) => report.links_added += added,
        Err(error) => report.warnings.push(error),
    }
    report.phase("transcripts_linkage", phase.elapsed());

    let phase = Instant::now();
    let mut databases = BrokerDbs::default();
    let mut eligible = Vec::new();
    for transcript in aged {
        let linkage =
            match index.links_for(transcript.agent_session_id.as_deref(), &transcript.path) {
                Ok(links) => databases.linkage(&links),
                Err(_) => eligibility::Linkage::Unreadable,
            };
        let facts = Facts {
            now_ms,
            modified_ms: transcript.modified_ms,
            archive_after_days: config.archive_after_days,
            linkage: &linkage,
        };
        let open = match layout.open_root(transcript.provider) {
            Some(root) => open_files.state_of(&root, &transcript.path),
            None => eligibility::OpenState::Unknown,
        };
        match eligibility::judge(&facts, open) {
            Ok(()) => eligible.push(transcript),
            Err(reason) => report.skip(&transcript.path, reason, ""),
        }
    }
    // Largest first, so space comes back early.
    eligible.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    report.eligible_bytes = eligible.iter().map(|transcript| transcript.bytes).sum();
    report.eligible = eligible.iter().map(TranscriptView::of).collect();
    report.phase("transcripts_judge", phase.elapsed());

    if options.apply {
        archive_eligible(
            environment,
            config,
            options,
            &index,
            &layout,
            eligible,
            open_files,
            hooks,
            now_ms,
            &mut report,
        );
        if let Err(error) = index.add_counters(&report.counters()) {
            report.warnings.push(error);
        }
    }
    report.open_listings = open_files.probes;
    report.phase("transcripts_open_check", open_files.probe_time);
    report.phase("transcripts_total", started.elapsed());
    if options.apply {
        append_run_log(&environment.host_dir, &report);
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn archive_eligible<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    options: Options,
    index: &Index,
    layout: &providers::Layout,
    eligible: Vec<Transcript>,
    open_files: &mut OpenFiles<P>,
    hooks: &mut dyn Hooks,
    now_ms: i64,
    report: &mut RunReport,
) {
    let started = Instant::now();
    let space_probe = |transcript: &Transcript| {
        transcript
            .destination
            .parent()
            .and_then(crate::disk_headroom::available_bytes)
            .or_else(|| crate::disk_headroom::available_bytes(&environment.host_dir))
    };
    let mut pacer = Pacer::new(Some(config.effective_bytes_per_second()));
    let mut compress = Duration::ZERO;
    let mut verify = Duration::ZERO;
    for transcript in eligible {
        if options
            .budget
            .is_some_and(|budget| started.elapsed() >= budget)
        {
            report.budget_exhausted = true;
            break;
        }
        let Some(root) = layout.open_root(transcript.provider) else {
            continue;
        };
        pacer.set_rate(pacing::choose_rate(
            config.effective_bytes_per_second(),
            space_probe(&transcript),
            config.free_space_floor_bytes,
        ));
        let mut context = ArchiveContext {
            index,
            open_files: &mut *open_files,
            pacer: &mut pacer,
            hooks: &mut *hooks,
            now_ms,
        };
        match archive::archive_one(&mut context, &transcript, &root) {
            Ok(done) => {
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
                    sha256: done.sha256.clone(),
                    resumed: done.resumed,
                });
            }
            Err(skipped) => report.skip(&transcript.path, skipped.reason, &skipped.detail),
        }
    }
    report.paced_bytes_per_second = pacer.rate();
    report.pacing_slept_ms = pacer.slept().as_millis() as u64;
    report.phase("transcripts_compress", compress);
    report.phase("transcripts_verify", verify);
    report.phase("transcripts_archive", started.elapsed());
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
    let Some(environment) = Environment::from_process() else {
        return;
    };
    let config = match TranscriptsConfig::load(&environment.host_dir) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: aethyme transcripts: {error}");
            return;
        }
    };
    if !config.archive {
        return;
    }
    let mut open_files = OpenFiles::new(SystemProbe, OPEN_LISTING_MAX_AGE);
    let options = Options {
        apply: true,
        budget: Some(WORKER_RUN_BUDGET),
    };
    crate::warn_unrecorded(
        "archive closed agent transcripts",
        run_pass(
            &environment,
            &config,
            options,
            &mut open_files,
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

/// Every archived transcript the index records.
pub(crate) fn archived_rows(environment: &Environment) -> Result<Vec<ArchiveRow>, String> {
    match Index::open_existing(&environment.host_dir)? {
        Some(index) => index.archives(),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn status<P: OpenFileProbe>(
    environment: &Environment,
    config: &TranscriptsConfig,
    open_files: &mut OpenFiles<P>,
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
    if let Some(index) = Index::open_existing(&environment.host_dir)? {
        let rows = index.archives()?;
        status.archived_files = rows.len() as u64;
        status.archived_bytes_before = rows.iter().map(|row| row.bytes_in).sum();
        status.archived_bytes_after = rows.iter().map(|row| row.bytes_out).sum();
        status.counters = index.counters()?;
    }
    status.lock_holder = LaneLock::recorded_holder(&environment.host_dir);
    status.last_run = last_run(&environment.host_dir);
    if config.archive {
        let dry = run_pass(
            environment,
            config,
            Options {
                apply: false,
                budget: None,
            },
            open_files,
            &mut NoHooks,
            super::now_ms(),
        )?;
        status.reclaimable_now_bytes = dry.eligible_bytes;
        status.eligible_now = dry.eligible.len() as u64;
        status.skipped_now = dry.skipped;
        status.warnings = dry.warnings;
    }
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
    let mut restored = Vec::new();
    for plan in &plans {
        restored.push(archive::restore_one(plan, Some(&index))?);
    }
    let mut counters = BTreeMap::new();
    counters.insert("restored".to_string(), restored.len() as u64);
    index.add_counters(&counters)?;
    Ok(restored)
}
