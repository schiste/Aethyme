//! Prebuild (#1237, E12): build a component's template for the tip of a ref
//! before anyone asks, in a quiet window, from the previous template.
//!
//! ```toml
//! [[prebuild]]
//! name = "db-template"
//! profile = "backend"
//! component = "db"                   # declares template / template_inputs
//! ref = "default_branch"             # or integration, or any local ref
//! derive_from_previous = true        # AETHYME_PREBUILD_PREVIOUS_ID/_DIGEST
//! when = ["host_idle", "outside:09:00-19:00"]   # any of; empty = always
//! keep = 2
//! ```
//!
//! The entries are read from the default branch's environments file, so a
//! worktree cannot declare what the host builds. For each, the tip of `ref`
//! is checked out in a host-owned clone that shares the repository's
//! objects (`git clone --shared`; nothing is registered in the repository),
//! the component is compiled from the file at that commit, and its template
//! digest is computed exactly as `env up` computes it. When the digest
//! already has a template, nothing runs. Otherwise the template is built
//! under the digest's own exclusive key once a condition in `when` holds,
//! so a concurrent `env up` of the same digest waits for it instead of
//! building it twice. A failed build leaves the previous template published
//! and is not retried until the digest changes (or `env prebuild --run`).
//!
//! Each failure and the recovery after it are reported through
//! `[[automation.outcomes]] source = "prebuild:<name>"` when the default
//! branch declares it (one issue per prebuild; the owners of the commits
//! since the last good build). Every run is recorded on the prebuild's
//! state and as the `prebuild.<name>` timing that `env budgets` reads.
//!
//! The template hook runs without an environment: it gets the component's
//! `env` and `pass_env`, `AETHYME_ENV_TEMPLATE_ID` and the
//! `AETHYME_PREBUILD_*` variables, but no environment ports or namespace.
//! A digest that reads `pass_env` or `cache_env` values only matches what
//! `env up` computes when the supervisor sees the same values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::cache::{
    BuildJob, EnsuredTemplate, TemplateJob, build_digest, repository_dir, template_digest,
};
use super::compile::CompiledComponent;
use super::compile::{CompileContext, EnvironmentOwner, compile};
use super::lifecycle::Lifecycle;
use super::spec::{EnvironmentsFile, HostEnvironmentsFile, PrebuildArtifact, PrebuildSpec};
use super::{ENVIRONMENTS_FILE, parse_loaded};

pub const PREBUILD_SCHEMA: &str = "aethyme.env-prebuild/v1";
/// How often the supervisor looks at prebuilds.
pub const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Runs kept on each prebuild's state.
pub const RUNS_KEPT: usize = 20;
/// The host directory prebuild checkouts and logs live in.
pub const PREBUILD_DIRECTORY: &str = "prebuild";

/// One `when` condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// No environment on the host was active for `quiet_minutes`.
    HostIdle,
    /// Local time in `[start, end)` minutes after midnight (wrapping).
    Inside(u16, u16),
    Outside(u16, u16),
}

fn minutes(text: &str) -> Option<u16> {
    let (hours, mins) = text.split_once(':')?;
    if hours.len() != 2 || mins.len() != 2 {
        return None;
    }
    let (hours, mins) = (hours.parse::<u16>().ok()?, mins.parse::<u16>().ok()?);
    (hours < 24 && mins < 60).then_some(hours * 60 + mins)
}

impl Condition {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text == "host_idle" {
            return Ok(Self::HostIdle);
        }
        let window = |rest: &str| {
            let (start, end) = rest.split_once('-')?;
            let (start, end) = (minutes(start)?, minutes(end)?);
            (start != end).then_some((start, end))
        };
        let parsed = match text.split_once(':') {
            Some(("inside", rest)) => window(rest).map(|(s, e)| Self::Inside(s, e)),
            Some(("outside", rest)) => window(rest).map(|(s, e)| Self::Outside(s, e)),
            _ => None,
        };
        parsed.ok_or_else(|| {
            format!(
                "when condition {text:?} is invalid; expected host_idle, inside:HH:MM-HH:MM or outside:HH:MM-HH:MM with two different times"
            )
        })
    }

    fn holds(self, inputs: &GateInputs, quiet_minutes: u64) -> bool {
        let inside = |start: u16, end: u16| {
            let now = inputs.local_minutes;
            if start < end {
                (start..end).contains(&now)
            } else {
                now >= start || now < end
            }
        };
        match self {
            Self::HostIdle => inputs.last_activity_ms.is_none_or(|last| {
                inputs.now_ms.saturating_sub(last) >= quiet_minutes as i64 * 60_000
            }),
            Self::Inside(start, end) => inside(start, end),
            Self::Outside(start, end) => !inside(start, end),
        }
    }
}

/// What the gate looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateInputs {
    /// Local time, minutes after midnight.
    pub local_minutes: u16,
    /// The newest client activity of any environment on the host.
    pub last_activity_ms: Option<i64>,
    pub now_ms: i64,
}

/// Open when `when` is empty or any condition holds; otherwise why not.
pub fn gate(spec: &PrebuildSpec, inputs: &GateInputs) -> Result<(), String> {
    if spec.when.is_empty() {
        return Ok(());
    }
    let mut parsed = Vec::new();
    for text in &spec.when {
        parsed.push(Condition::parse(text)?);
    }
    if parsed
        .iter()
        .any(|condition| condition.holds(inputs, spec.quiet_minutes))
    {
        Ok(())
    } else {
        Err(format!("waiting for {}", spec.when.join(" or ")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Never looked at.
    #[default]
    New,
    /// The tip's template is published.
    Ready,
    /// The tip's template is missing and the gate is closed.
    Waiting,
    /// The tip's template failed to build; held until the digest changes.
    Failed,
}

/// One build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrebuildRun {
    pub commit: String,
    pub digest: String,
    pub template_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    pub started_at: i64,
    pub seconds: f64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a prebuild left behind, kept as JSON in the environment store.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrebuildState {
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The tip last looked at, and its template digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The last tip whose template was published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_commit: Option<String>,
    /// The digest whose build failed: not retried while it stays the tip's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_digest: Option<String>,
    /// A failure was reported and its recovery not yet.
    #[serde(default)]
    pub failure_reported: bool,
    #[serde(default)]
    pub runs: Vec<PrebuildRun>,
}

impl PrebuildState {
    /// Whether the supervisor must stay up for this prebuild.
    pub fn pending(&self) -> bool {
        matches!(self.status, Status::Waiting | Status::New)
    }
}

/// Closed while host memory in use is above `max_memory_pct`. An unknown
/// reading does not block: no reading is a missing signal, not pressure.
pub fn memory_gate(spec: &PrebuildSpec, used_pct: Option<u32>) -> Result<(), String> {
    match (spec.max_memory_pct, used_pct) {
        (Some(max), Some(used)) if used > max => Err(format!(
            "waiting for host memory in use to fall to {max}% (now {used}%)"
        )),
        _ => Ok(()),
    }
}

/// Host memory in use, as a percentage: macOS `kern.memorystatus_level`
/// (the share the kernel considers available), Linux `MemAvailable` over
/// `MemTotal`. `None` when unreadable.
pub fn host_memory_used_pct() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let mut level: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let name = c"kern.memorystatus_level";
        // SAFETY: `level` and `size` outlive the call and `size` is the
        // exact size of `level`; no new value is written (null, 0).
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut level as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status == 0 && (0..=100).contains(&level) {
            return Some(100 - level as u32);
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kib| kib.parse::<u64>().ok())
        };
        let (total, available) = (field("MemTotal:")?, field("MemAvailable:")?);
        (total > 0).then(|| (100 - (available.min(total) * 100 / total)) as u32)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// What a prebuild builds for its component.
pub fn artifact_of(
    spec: &PrebuildSpec,
    component: &CompiledComponent,
) -> Result<PrebuildArtifact, String> {
    let has_template = component.commands.template.is_some();
    let has_build = component.commands.build.is_some() && !component.cache_inputs.is_empty();
    match spec.artifact {
        Some(PrebuildArtifact::Template) if has_template => Ok(PrebuildArtifact::Template),
        Some(PrebuildArtifact::Build) if has_build => Ok(PrebuildArtifact::Build),
        None if has_template => Ok(PrebuildArtifact::Template),
        None if has_build => Ok(PrebuildArtifact::Build),
        _ => Err(format!(
            "{} declares no {} to prebuild",
            component.name,
            match spec.artifact {
                Some(PrebuildArtifact::Build) => "cacheable build (build + cache_inputs + output)",
                Some(PrebuildArtifact::Template) | None => "template or cacheable build",
            }
        )),
    }
}

/// What to do about one tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The tip's template exists: nothing to build.
    Published,
    /// This digest already failed; held until the digest changes.
    Held,
    Wait(String),
    Build,
}

/// The pure decision for a tip whose digest is known. `force` builds a
/// missing template whatever the gate and the last failure say.
pub fn decide(
    state: &PrebuildState,
    digest: &str,
    exists: bool,
    gate: Result<(), String>,
    force: bool,
) -> Decision {
    if exists {
        Decision::Published
    } else if force {
        Decision::Build
    } else if state.failed_digest.as_deref() == Some(digest) {
        Decision::Held
    } else {
        match gate {
            Ok(()) => Decision::Build,
            Err(reason) => Decision::Wait(reason),
        }
    }
}

/// One prebuild's result for a report.
#[derive(Debug, Clone, Serialize)]
pub struct PrebuildReport {
    pub name: String,
    pub reference: String,
    /// `unchanged`, `published`, `held`, `waiting`, `built`, `failed` or
    /// `error`.
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<PrebuildRun>,
    /// Older templates evicted after a build.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pruned: Vec<PathBuf>,
    /// What the outcome hook did, when a failure or recovery was reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
}

/// Minutes after local midnight.
pub fn local_minutes(now_ms: i64) -> u16 {
    #[cfg(unix)]
    {
        let seconds = (now_ms / 1000) as libc::time_t;
        // SAFETY: `localtime_r` writes only into `tm`, which outlives the
        // call; a null return leaves it unused.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&seconds, &mut tm).is_null() {
                return (tm.tm_hour * 60 + tm.tm_min) as u16;
            }
        }
    }
    ((now_ms / 60_000).rem_euclid(24 * 60)) as u16
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::git::git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn home(root: &Path, repository: &str, name: &str) -> PathBuf {
    root.join(PREBUILD_DIRECTORY)
        .join(repository_dir(repository))
        .join(name)
}

/// The prebuild's own checkout of `commit`: a clone sharing `main_root`'s
/// objects, so nothing is fetched and no worktree is registered in the
/// repository. Ignored files survive between commits (dependency caches);
/// they never count toward a template digest.
pub fn checkout_at(
    root: &Path,
    repository: &str,
    name: &str,
    main_root: &Path,
    commit: &str,
) -> Result<PathBuf, String> {
    let checkout = home(root, repository, name).join("checkout");
    if !checkout.join(".git").exists() {
        if checkout.exists() {
            std::fs::remove_dir_all(&checkout).map_err(|e| e.to_string())?;
        }
        let parent = checkout.parent().expect("checkout has a parent");
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        git(
            parent,
            &[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                &main_root.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        )?;
    }
    git(
        &checkout,
        &["checkout", "--quiet", "--detach", "--force", commit],
    )?;
    git(&checkout, &["clean", "-fdq"])?;
    Ok(checkout)
}

fn load_states(lifecycle: &Lifecycle, repository: &str) -> BTreeMap<String, PrebuildState> {
    lifecycle
        .store
        .prebuild_states()
        .unwrap_or_default()
        .into_iter()
        .filter(|(repo, ..)| repo == repository)
        .map(|(_, name, _, state)| (name, serde_json::from_str(&state).unwrap_or_default()))
        .collect()
}

/// The newest client activity of any environment on the host.
fn host_last_activity(lifecycle: &Lifecycle) -> Option<i64> {
    lifecycle
        .store
        .list(None)
        .ok()?
        .iter()
        .filter_map(|record| record.last_activity_at)
        .max()
}

/// Report a failure or recovery through the outcome hook, opening the
/// broker only now. Without a declared rule nothing is written and the
/// report says so.
pub trait OutcomeReporter {
    fn report(&mut self, request: &crate::OutcomeReportRequest) -> serde_json::Value;
}

/// The real reporter: `main_root`'s trusted rules, broker and issue journal.
pub struct RepositoryReporter<'a> {
    pub main_root: &'a Path,
}

impl OutcomeReporter for RepositoryReporter<'_> {
    fn report(&mut self, request: &crate::OutcomeReportRequest) -> serde_json::Value {
        let attempt = || -> Result<serde_json::Value, String> {
            let repo = crate::GitRepo::discover(self.main_root).map_err(|e| e.to_string())?;
            let policy = crate::AutomationIssuesPolicy::load_trusted(&repo)?;
            if !policy
                .outcomes
                .iter()
                .any(|rule| rule.source == request.source)
            {
                return Ok(serde_json::json!({
                    "status": "no_outcome_rule",
                    "message": format!("declare [[automation.outcomes]] source = {:?} to report it", request.source),
                }));
            }
            let slug = repo
                .remote_url("origin")
                .ok()
                .and_then(|url| super::budget_outcomes::github_slug(url.trim()));
            let mut broker = crate::Broker::open(self.main_root).map_err(|e| e.to_string())?;
            let mut issues = crate::RepositoryIssueSink {
                repo: &repo,
                session: None,
            };
            let report = crate::report_outcome(
                &mut broker,
                &policy.outcomes,
                slug.as_deref(),
                request,
                &mut issues,
                super::activity::now_ms(),
            )
            .map_err(|error| format!("{}: {error}", error.code()))?;
            let mut value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
            value["status"] = if report.incomplete() {
                "incomplete".into()
            } else {
                "reported".into()
            };
            Ok(value)
        };
        attempt()
            .unwrap_or_else(|error| serde_json::json!({ "status": "failed", "message": error }))
    }
}

fn short(text: &str) -> &str {
    &text[..12.min(text.len())]
}

/// Look at every prebuild the default branch declares for `main_root` and do
/// what each needs. `force` (`env prebuild --run`) builds a missing template
/// now, ignoring the gate and an earlier failure; `only` limits it to one.
pub fn run_for_repository(
    lifecycle: &mut Lifecycle,
    main_root: &Path,
    host: &HostEnvironmentsFile,
    force: bool,
    only: Option<&str>,
    reporter: &mut dyn OutcomeReporter,
) -> Result<Vec<PrebuildReport>, String> {
    let Some(file) = super::shared::declared_file(main_root)? else {
        return Ok(Vec::new());
    };
    if file.prebuild.is_empty() {
        return Ok(Vec::new());
    }
    let repo = crate::GitRepo::discover(main_root).map_err(|e| e.to_string())?;
    let repository = crate::gates::git_origin_fingerprint(&repo);
    let mut states = load_states(lifecycle, &repository);
    let mut reports = Vec::new();
    for spec in file
        .prebuild
        .iter()
        .filter(|s| only.is_none_or(|n| n == s.name))
    {
        let mut state = states.remove(&spec.name).unwrap_or_default();
        let report = run_one(
            lifecycle,
            main_root,
            &repository,
            host,
            spec,
            &mut state,
            force,
            reporter,
        )
        .unwrap_or_else(|error| PrebuildReport {
            name: spec.name.clone(),
            reference: spec.reference.clone(),
            result: "error",
            detail: Some(error),
            commit: None,
            digest: None,
            run: None,
            pruned: Vec::new(),
            outcome: None,
        });
        let encoded = serde_json::to_string(&state).map_err(|e| e.to_string())?;
        lifecycle
            .store
            .set_prebuild_state(
                &repository,
                &spec.name,
                &main_root.to_string_lossy(),
                &encoded,
            )
            .map_err(|e| e.to_string())?;
        reports.push(report);
    }
    if let Some(only) = only
        && reports.is_empty()
    {
        return Err(format!(
            "the default branch declares no [[prebuild]] named {only:?}"
        ));
    }
    Ok(reports)
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    lifecycle: &mut Lifecycle,
    main_root: &Path,
    repository: &str,
    host: &HostEnvironmentsFile,
    spec: &PrebuildSpec,
    state: &mut PrebuildState,
    force: bool,
    reporter: &mut dyn OutcomeReporter,
) -> Result<PrebuildReport, String> {
    let tip = super::shared::resolve_ref(main_root, &spec.reference)?;
    let mut report = PrebuildReport {
        name: spec.name.clone(),
        reference: spec.reference.clone(),
        result: "unchanged",
        detail: None,
        commit: Some(tip.clone()),
        digest: state.digest.clone(),
        run: None,
        pruned: Vec::new(),
        outcome: None,
    };
    // A tip already looked at needs nothing unless it is still waiting for
    // its window: no checkout, no digest, one rev-parse.
    if !force && state.commit.as_deref() == Some(tip.as_str()) && !state.pending() {
        return Ok(report);
    }
    let checkout = checkout_at(&lifecycle.root, repository, &spec.name, main_root, &tip)?;
    let text = std::fs::read_to_string(checkout.join(ENVIRONMENTS_FILE))
        .map_err(|error| format!("{ENVIRONMENTS_FILE} at {}: {error}", short(&tip)))?;
    let file: EnvironmentsFile =
        parse_loaded(checkout.join(ENVIRONMENTS_FILE), ENVIRONMENTS_FILE, &text)
            .value
            .map_err(|diagnostic| diagnostic.render())?;
    let compiled = compile(
        &file,
        host,
        &spec.profile,
        ENVIRONMENTS_FILE,
        &CompileContext {
            repository: repository.to_string(),
            owner: EnvironmentOwner::Ref {
                name: format!("prebuild:{}", spec.name),
            },
            worktree_fingerprint: crate::console::worktree_fingerprint(&checkout),
            source_commit: Some(tip.clone()),
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .map_err(|error| format!("compile {} at {}: {error}", spec.profile, short(&tip)))?;
    let component = compiled
        .components
        .iter()
        .find(|c| c.name == spec.component)
        .ok_or_else(|| {
            format!(
                "{} has no component {:?} at {}",
                spec.profile,
                spec.component,
                short(&tip)
            )
        })?;
    let artifact = artifact_of(spec, component)?;
    let digest = match artifact {
        PrebuildArtifact::Template => template_digest(&checkout, component)?,
        PrebuildArtifact::Build => build_digest(&checkout, component)?,
    }
    .ok_or_else(|| {
        format!(
            "{} declares nothing to prebuild at {}",
            spec.component,
            short(&tip)
        )
    })?;
    state.commit = Some(tip.clone());
    state.digest = Some(digest.clone());
    report.digest = Some(digest.clone());
    if state
        .failed_digest
        .as_deref()
        .is_some_and(|failed| failed != digest)
    {
        state.failed_digest = None;
    }

    let now = super::activity::now_ms();
    let inputs = GateInputs {
        local_minutes: local_minutes(now),
        last_activity_ms: host_last_activity(lifecycle),
        now_ms: now,
    };
    let exists = match artifact {
        PrebuildArtifact::Template => {
            lifecycle.template_exists(repository, &component.name, &digest)
        }
        PrebuildArtifact::Build => lifecycle.build_exists(repository, &component.name, &digest),
    };
    let window = gate(spec, &inputs).and_then(|()| memory_gate(spec, host_memory_used_pct()));
    match decide(state, &digest, exists, window, force) {
        Decision::Published => {
            state.status = Status::Ready;
            state.reason = None;
            state.last_ok_commit = Some(tip.clone());
            report.result = "published";
            if state.failure_reported {
                report.outcome = Some(recover(state, spec, &tip, &digest, reporter));
            }
        }
        Decision::Held => {
            state.status = Status::Failed;
            state.reason = Some(
                "the build for this digest failed; it is retried when the digest changes or with `env prebuild --run`".into(),
            );
            report.result = "held";
            report.detail = state.reason.clone();
        }
        Decision::Wait(reason) => {
            state.status = Status::Waiting;
            state.reason = Some(reason.clone());
            report.result = "waiting";
            report.detail = Some(reason);
        }
        Decision::Build => {
            let previous = (spec.derive_from_previous && artifact == PrebuildArtifact::Template)
                .then(|| lifecycle.newest_template(repository, &component.name, Some(&digest)))
                .flatten();
            let mut vars = component.env.clone();
            vars.insert("AETHYME_PREBUILD_NAME".into(), spec.name.clone());
            vars.insert("AETHYME_PREBUILD_REF".into(), spec.reference.clone());
            vars.insert("AETHYME_PREBUILD_COMMIT".into(), tip.clone());
            vars.insert("AETHYME_PREBUILD_DIGEST".into(), digest.clone());
            if let Some((marker, previous_digest)) = &previous {
                vars.insert(
                    "AETHYME_PREBUILD_PREVIOUS_ID".into(),
                    marker.template_id.clone(),
                );
                vars.insert(
                    "AETHYME_PREBUILD_PREVIOUS_DIGEST".into(),
                    previous_digest.clone(),
                );
            }
            let log = home(&lifecycle.root, repository, &spec.name).join("prebuild.log");
            let started_at = now;
            let clock = Instant::now();
            // A preemptible build yields to foreground work: an environment
            // active since it started (when it waited for host_idle), or
            // memory past max_memory_pct. Not under --run.
            let watcher = (spec.preemptible && !force)
                .then(|| lifecycle.store.reopen().ok())
                .flatten();
            let idle_gated = spec.when.iter().any(|w| w == "host_idle");
            let mut stop = move || -> Option<String> {
                if let Some(store) = &watcher
                    && idle_gated
                    && let Some(last) = store
                        .list(None)
                        .ok()
                        .and_then(|records| records.iter().filter_map(|r| r.last_activity_at).max())
                    && last > started_at
                {
                    return Some("an environment became active".into());
                }
                if watcher.is_some()
                    && let (Some(max), Some(used)) = (spec.max_memory_pct, host_memory_used_pct())
                    && used > max
                {
                    return Some(format!("host memory in use reached {used}% (max {max}%)"));
                }
                None
            };
            let keep = Some(
                component
                    .template_keep
                    .map_or(spec.keep, |k| k.max(spec.keep)),
            );
            let timeout = Duration::from_secs(spec.timeout_seconds);
            let result = match artifact {
                PrebuildArtifact::Template => lifecycle.ensure_template_job(
                    TemplateJob {
                        repository,
                        component,
                        worktree: &checkout,
                        vars,
                        log: log.clone(),
                        timeout,
                        env_id: None,
                        keep,
                        stop: Some(&mut stop),
                    },
                    &digest,
                ),
                PrebuildArtifact::Build => lifecycle
                    .ensure_build_job(
                        BuildJob {
                            repository,
                            component,
                            worktree: &checkout,
                            log: log.clone(),
                            timeout,
                            stop: Some(&mut stop),
                        },
                        &digest,
                    )
                    .map(|built| EnsuredTemplate {
                        id: digest[..16].to_string(),
                        digest: digest.clone(),
                        built,
                        pruned: Vec::new(),
                    }),
            };
            if let Err(super::exec::DriverError::Preempted { reason, .. }) = &result {
                state.status = Status::Waiting;
                state.reason = Some(format!(
                    "stopped to yield: {reason}; builds again in the next window"
                ));
                report.result = "preempted";
                report.detail = state.reason.clone();
                return Ok(report);
            }
            let seconds = clock.elapsed().as_secs_f64();
            let derived_from = previous
                .as_ref()
                .map(|(marker, _)| marker.template_id.clone());
            let (ok, error, template_id) = match &result {
                Ok(EnsuredTemplate { id, pruned, .. }) => {
                    report.pruned = pruned.clone();
                    (true, None, id.clone())
                }
                Err(error) => (false, Some(error.to_string()), digest[..16].to_string()),
            };
            let run = PrebuildRun {
                commit: tip.clone(),
                digest: digest.clone(),
                template_id,
                derived_from,
                started_at,
                seconds,
                ok,
                error: error.clone(),
            };
            state.runs.push(run.clone());
            let excess = state.runs.len().saturating_sub(RUNS_KEPT);
            state.runs.drain(..excess);
            crate::warn_unrecorded(
                "record a prebuild timing",
                lifecycle.store.record_timing(
                    repository,
                    &format!("prebuild.{}", spec.name),
                    &format!("prebuild:{}", spec.name),
                    seconds,
                    ok,
                ),
            );
            report.run = Some(run);
            if ok {
                state.status = Status::Ready;
                state.reason = None;
                state.failed_digest = None;
                state.last_ok_commit = Some(tip.clone());
                report.result = "built";
                if state.failure_reported {
                    report.outcome = Some(recover(state, spec, &tip, &digest, reporter));
                }
            } else {
                let error = error.unwrap_or_default();
                state.status = Status::Failed;
                state.failed_digest = Some(digest.clone());
                state.reason = Some(error.clone());
                report.result = "failed";
                report.detail = Some(format!("{error}; log: {}", log.display()));
                let request = crate::OutcomeReportRequest {
                    source: format!("prebuild:{}", spec.name),
                    failed: true,
                    key: None,
                    commits: state
                        .last_ok_commit
                        .as_ref()
                        .filter(|ok| **ok != tip)
                        .map(|ok| format!("{ok}..{tip}")),
                    title: Some(format!(
                        "Prebuild {} failed at {} ({})",
                        spec.name,
                        short(&tip),
                        spec.reference
                    )),
                    body: Some(format!(
                        "The `{}` template for `{}` at `{tip}` (digest `{digest}`) failed to build after {seconds:.0}s; the previous template stays in use.\n\n```\n{}\n```\n\nLog on the host: `{}`. Retry with `aethyme env prebuild --run --name {}`.",
                        spec.component,
                        spec.reference,
                        cut(&error, 2000),
                        log.display(),
                        spec.name
                    )),
                };
                let outcome = reporter.report(&request);
                if outcome["status"] != "no_outcome_rule" && outcome["status"] != "failed" {
                    state.failure_reported = true;
                }
                report.outcome = Some(outcome);
            }
        }
    }
    Ok(report)
}

fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn recover(
    state: &mut PrebuildState,
    spec: &PrebuildSpec,
    tip: &str,
    digest: &str,
    reporter: &mut dyn OutcomeReporter,
) -> serde_json::Value {
    let outcome = reporter.report(&crate::OutcomeReportRequest {
        source: format!("prebuild:{}", spec.name),
        failed: false,
        key: None,
        commits: None,
        title: None,
        body: Some(format!(
            "The `{}` template for `{}` at `{tip}` (digest `{digest}`) is published again.",
            spec.component, spec.reference
        )),
    });
    if outcome["status"] != "failed" {
        state.failure_reported = false;
    }
    outcome
}

/// Whether any recorded prebuild is waiting for its window: the supervisor
/// stays up for it.
pub fn any_pending(lifecycle: &Lifecycle) -> bool {
    lifecycle
        .store
        .prebuild_states()
        .unwrap_or_default()
        .iter()
        .any(|(.., state)| {
            serde_json::from_str::<PrebuildState>(state).is_ok_and(|state| state.pending())
        })
}

/// The host file prebuilds compile against.
pub fn host_file() -> Result<HostEnvironmentsFile, String> {
    let (_, loaded) = super::load_host_file().map_err(|error| error.to_string())?;
    loaded.value.map_err(|diagnostic| diagnostic.render())
}

/// The repositories the supervisor looks at: every main root with an
/// environment or a recorded prebuild.
pub fn supervised_roots(lifecycle: &Lifecycle) -> std::collections::BTreeSet<PathBuf> {
    let mut roots: std::collections::BTreeSet<PathBuf> = lifecycle
        .store
        .list(None)
        .map(|records| {
            records
                .into_iter()
                .filter_map(|r| r.main_root.map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();
    roots.extend(
        lifecycle
            .store
            .prebuild_states()
            .unwrap_or_default()
            .into_iter()
            .map(|(_, _, main_root, _)| PathBuf::from(main_root)),
    );
    roots
}

/// After a fetch or merge moved the default branch (#1366): when the
/// default branch declares `[[prebuild]]`, record its prebuilds so the
/// supervisor looks at this repository, and start the supervisor if none
/// runs. Never fails the caller; `AETHYME_PREBUILD_NUDGE=0` turns it off.
pub fn nudge_after_fetch(main_root: &Path) {
    if std::env::var_os("AETHYME_PREBUILD_NUDGE").is_some_and(|value| value == "0") {
        return;
    }
    let Ok(Some(file)) = super::shared::declared_file(main_root) else {
        return;
    };
    if file.prebuild.is_empty() {
        return;
    }
    let Some(root) = super::logs::environments_directory() else {
        return;
    };
    if let Err(error) = register_prebuilds(main_root, &file) {
        eprintln!("warning: prebuild: could not record this repository's prebuilds: {error}");
        return;
    }
    let started =
        std::env::current_exe().and_then(|exe| super::supervisor::ensure_running(&root, &exe));
    if let Err(error) = started {
        eprintln!("warning: prebuild: the environment supervisor did not start: {error}");
    }
}

/// Give every declared prebuild a state row (status `new` when it has none),
/// so the supervisor looks at the repository and stays up until it has.
fn register_prebuilds(main_root: &Path, file: &EnvironmentsFile) -> Result<(), String> {
    let repo = crate::GitRepo::discover(main_root).map_err(|e| e.to_string())?;
    let repository = crate::gates::git_origin_fingerprint(&repo);
    let mut store = super::store::EnvironmentStore::open_default().map_err(|e| e.to_string())?;
    let known: std::collections::BTreeSet<String> = store
        .prebuild_states()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(repo, ..)| *repo == repository)
        .map(|(_, name, ..)| name)
        .collect();
    let fresh = serde_json::to_string(&PrebuildState::default()).map_err(|e| e.to_string())?;
    for spec in file
        .prebuild
        .iter()
        .filter(|spec| !known.contains(&spec.name))
    {
        store
            .set_prebuild_state(
                &repository,
                &spec.name,
                &main_root.to_string_lossy(),
                &fresh,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
