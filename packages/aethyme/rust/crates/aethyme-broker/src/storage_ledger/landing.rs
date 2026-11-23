//! Whether recorded commits are in their repository's default branch: the
//! proof the storage worker needs before it may remove a recovery archive.
//!
//! The worker asked this of every archive on every pass, one
//! `git merge-base --is-ancestor` per recorded commit, spawned with a plain
//! `git` that inherited the worker's throttled disk priority and had no
//! deadline. On the busiest repository the last pass spent 125 s of its
//! 211 s in eviction, removed nothing, and re-proved the same 1,978 archives
//! unlanded; one probe was seen running for over 50 s.
//!
//! A [`LandingProver`] answers the same question cheaper, and never in the
//! unsafe direction:
//! - each repository's default tip is resolved once per [`TIP_TTL`], not
//!   once per archive; every "contained" answer is then confirmed against
//!   a freshly read tip, so a rewound default branch never lends an old
//!   answer to a removal;
//! - a commit is probed once per tip: "contained" and "not contained" are
//!   facts about two commit ids, so they are kept, and every commit of an
//!   archive is checked against them before any probe;
//! - when the tip moves forward, one `rev-list <new> ^<old>` says which
//!   commits known not contained have since landed; when it moves anywhere
//!   else, everything known is dropped;
//! - only "not contained" is persisted across processes
//!   ([`LandingProver::save`]), and only for commits this process asked
//!   about, so a damaged or stale cache file can keep an archive, never
//!   remove one;
//! - every Git read (tips, ranges and probes) runs at an ordinary disk
//!   priority (`crate::git::prepare_git_child_io`), ignores replace refs,
//!   grafts and lazy fetches, and is bounded by [`PROBE_BUDGET`], by an
//!   active Git deadline, and by [`PROCESS_PROBE_BUDGET`] for all reads of
//!   the process together; an archive whose commits could not be proven
//!   either way is reported unproven and kept.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The cache of facts known not contained, in the host state directory.
pub const CACHE_FILE: &str = "storage-landing-cache.json";
const CACHE_SCHEMA: u32 = 1;
/// How long a resolved default tip is reused before it is read again.
pub const TIP_TTL: Duration = Duration::from_secs(30);
/// One Git read, at most.
pub const PROBE_BUDGET: Duration = Duration::from_secs(20);
/// All Git reads of one process together, at most: the rest stay unproven.
pub const PROCESS_PROBE_BUDGET: Duration = Duration::from_secs(120);
/// A forward move listing more commits than this drops what is known.
const MAX_ADVANCE_COMMITS: usize = 50_000;
/// Facts kept per repository; above it a repository starts over.
const MAX_FACTS_PER_REPOSITORY: usize = 100_000;

/// Why commits are not proven landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Git answered: a commit is not in the default branch, or the archive
    /// holds work outside any commit.
    NotLanded(String),
    /// No answer either way (a budget, a deadline, a Git failure, a tip
    /// that moved): kept, and reported as unproven, not as unlanded.
    Unproven(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLanded(why) | Self::Unproven(why) => formatter.write_str(why),
        }
    }
}

/// Counts of each decision, for telemetry and the pass report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandingStats {
    pub tip_reads: u64,
    pub tip_memo_hits: u64,
    pub live_tip_checks: u64,
    pub live_tip_moved: u64,
    pub duplicate_commits: u64,
    pub cached_contained: u64,
    pub cached_not_contained: u64,
    pub probes: u64,
    pub probe_contained: u64,
    pub probe_not_contained: u64,
    pub probe_unproven: u64,
    pub probe_timeouts: u64,
    pub budget_spent: u64,
    pub tip_advanced: u64,
    pub tip_reset: u64,
}

impl LandingStats {
    /// The counts by stable name, for the pass report.
    pub fn as_map(&self) -> BTreeMap<String, u64> {
        [
            ("tip_reads", self.tip_reads),
            ("tip_memo_hits", self.tip_memo_hits),
            ("live_tip_checks", self.live_tip_checks),
            ("live_tip_moved", self.live_tip_moved),
            ("duplicate_commits", self.duplicate_commits),
            ("cached_contained", self.cached_contained),
            ("cached_not_contained", self.cached_not_contained),
            ("probes", self.probes),
            ("probe_contained", self.probe_contained),
            ("probe_not_contained", self.probe_not_contained),
            ("probe_unproven", self.probe_unproven),
            ("probe_timeouts", self.probe_timeouts),
            ("budget_spent", self.budget_spent),
            ("tip_advanced", self.tip_advanced),
            ("tip_reset", self.tip_reset),
        ]
        .into_iter()
        .map(|(name, count)| (name.to_string(), count))
        .collect()
    }
}

#[derive(Debug, Clone, Default)]
struct RepositoryFacts {
    /// The default tip every fact below is relative to.
    tip: String,
    /// Proven by this process; never persisted.
    contained: BTreeSet<String>,
    not_contained: BTreeSet<String>,
    /// Commits this process asked about: only these are saved.
    asked: BTreeSet<String>,
    resolved_at: Option<Instant>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    schema: u32,
    repositories: Vec<CachedRepository>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedRepository {
    repository: PathBuf,
    tip: String,
    not_contained: BTreeSet<String>,
}

/// What one Git read gave.
enum Read {
    Success(String),
    /// Exit status 1: a definite "no" from `merge-base --is-ancestor`.
    No,
    Failed,
    TimedOut,
    /// The process budget was spent before the read started.
    BudgetSpent,
}

/// Proves commits landed, remembering what it learned (see the module).
#[derive(Debug)]
pub struct LandingProver {
    repositories: BTreeMap<PathBuf, RepositoryFacts>,
    probe_budget: Duration,
    process_budget: Duration,
    reading: Duration,
    stats: LandingStats,
}

impl Default for LandingProver {
    fn default() -> Self {
        Self::with_budgets(PROBE_BUDGET, PROCESS_PROBE_BUDGET)
    }
}

impl LandingProver {
    /// A prover with an explicit budget per probe and for all of the
    /// process's reads (tip and range reads keep [`PROBE_BUDGET`] each).
    pub fn with_budgets(probe_budget: Duration, process_budget: Duration) -> Self {
        Self {
            repositories: BTreeMap::new(),
            probe_budget,
            process_budget,
            reading: Duration::ZERO,
            stats: LandingStats::default(),
        }
    }

    pub fn stats(&self) -> &LandingStats {
        &self.stats
    }

    /// Forget when each tip was read, so the next question reads it again.
    pub fn expire_tips(&mut self) {
        for facts in self.repositories.values_mut() {
            facts.resolved_at = None;
        }
    }

    /// `Ok` when every commit (full hexadecimal ids, already validated) is
    /// contained in `repository`'s default branch as it is now.
    pub fn prove(&mut self, repository: &Path, commits: &[String]) -> Result<(), Refusal> {
        let mut unique = Vec::new();
        let mut seen = BTreeSet::new();
        for commit in commits {
            if seen.insert(commit.as_str()) {
                unique.push(commit.clone());
            } else {
                self.stats.duplicate_commits += 1;
                crate::phase_telemetry::add("storage_landing_duplicate_commit", Duration::ZERO);
            }
        }
        let tip = self.current_tip(repository, false)?;
        self.prove_at(repository, &unique, &tip)?;
        // A removal follows an `Ok`: confirm the answer against the tip as
        // it is now, never a remembered one.
        self.stats.live_tip_checks += 1;
        crate::phase_telemetry::add("storage_landing_live_tip_check", Duration::ZERO);
        let live = self.read_tip(repository)?;
        if live == tip {
            return Ok(());
        }
        self.stats.live_tip_moved += 1;
        crate::phase_telemetry::add("storage_landing_live_tip_moved", Duration::ZERO);
        let tip = self.current_tip(repository, true)?;
        self.prove_at(repository, &unique, &tip)?;
        if self.read_tip(repository)? == tip {
            return Ok(());
        }
        Err(Refusal::Unproven(
            "the default branch kept moving while it was checked".into(),
        ))
    }

    /// Every commit against `tip`: what is known first (so one known
    /// unlanded commit costs no probe), then probes for the rest.
    fn prove_at(
        &mut self,
        repository: &Path,
        commits: &[String],
        tip: &str,
    ) -> Result<(), Refusal> {
        let facts = self
            .repositories
            .entry(repository.to_path_buf())
            .or_default();
        facts.asked.extend(commits.iter().cloned());
        if let Some(commit) = commits
            .iter()
            .find(|commit| facts.not_contained.contains(*commit))
        {
            self.stats.cached_not_contained += 1;
            crate::phase_telemetry::add("storage_landing_cached_not_contained", Duration::ZERO);
            return Err(Refusal::NotLanded(not_in(commit, tip)));
        }
        let unknown: Vec<String> = commits
            .iter()
            .filter(|commit| !facts.contained.contains(*commit))
            .cloned()
            .collect();
        let known = (commits.len() - unknown.len()) as u64;
        if known > 0 {
            self.stats.cached_contained += known;
            crate::phase_telemetry::add("storage_landing_cached_contained", Duration::ZERO);
        }
        for commit in &unknown {
            let probe_budget = self.probe_budget;
            let read = self.read(
                repository,
                &["merge-base", "--is-ancestor", commit, tip],
                probe_budget,
                Some("storage_landing_probe"),
            );
            if !matches!(read, Read::BudgetSpent) {
                self.stats.probes += 1;
            }
            let facts = self
                .repositories
                .entry(repository.to_path_buf())
                .or_default();
            match read {
                Read::Success(_) => {
                    self.stats.probe_contained += 1;
                    crate::phase_telemetry::add("storage_landing_probe_contained", Duration::ZERO);
                    facts.contained.insert(commit.clone());
                }
                Read::No => {
                    self.stats.probe_not_contained += 1;
                    crate::phase_telemetry::add(
                        "storage_landing_probe_not_contained",
                        Duration::ZERO,
                    );
                    facts.not_contained.insert(commit.clone());
                    return Err(Refusal::NotLanded(not_in(commit, tip)));
                }
                // A missing object or any other failure: not proven, and
                // not remembered either.
                Read::Failed => {
                    self.stats.probe_unproven += 1;
                    crate::phase_telemetry::add("storage_landing_probe_unproven", Duration::ZERO);
                    return Err(Refusal::Unproven(format!(
                        "commit {} could not be checked against {tip}",
                        short(commit)
                    )));
                }
                Read::TimedOut => {
                    self.stats.probe_timeouts += 1;
                    crate::phase_telemetry::add("storage_landing_probe_timeout", Duration::ZERO);
                    return Err(Refusal::Unproven(format!(
                        "commit {} was not proven landed within its time budget",
                        short(commit)
                    )));
                }
                Read::BudgetSpent => return Err(self.budget_spent()),
            }
        }
        Ok(())
    }

    /// The default tip, from the memo while it is younger than [`TIP_TTL`]
    /// (unless `fresh`); a moved tip carries forward what stays true.
    fn current_tip(&mut self, repository: &Path, fresh: bool) -> Result<String, Refusal> {
        if !fresh
            && let Some(facts) = self.repositories.get(repository)
            && !facts.tip.is_empty()
            && facts
                .resolved_at
                .is_some_and(|resolved| resolved.elapsed() < TIP_TTL)
        {
            self.stats.tip_memo_hits += 1;
            crate::phase_telemetry::add("storage_landing_tip_memo_hit", Duration::ZERO);
            return Ok(facts.tip.clone());
        }
        self.stats.tip_reads += 1;
        let tip = self.read_tip(repository)?;
        let mut facts = self.repositories.remove(repository).unwrap_or_default();
        if !facts.tip.is_empty() && facts.tip != tip {
            facts = self.advance(repository, facts, &tip);
        }
        facts.tip = tip.clone();
        facts.resolved_at = Some(Instant::now());
        if facts.contained.len() + facts.not_contained.len() > MAX_FACTS_PER_REPOSITORY {
            facts.contained.clear();
            facts.not_contained.clear();
        }
        self.repositories.insert(repository.to_path_buf(), facts);
        Ok(tip)
    }

    /// Carry facts from `facts.tip` to `tip`: only along a forward move,
    /// where the commits that landed meanwhile are exactly
    /// `rev-list <tip> ^<old tip>`. What was asked is kept either way.
    fn advance(&mut self, repository: &Path, facts: RepositoryFacts, tip: &str) -> RepositoryFacts {
        let _span = crate::phase_telemetry::span("storage_landing_tip_advance");
        let old = facts.tip.clone();
        let reset = |stats: &mut LandingStats, asked: BTreeSet<String>| {
            stats.tip_reset += 1;
            crate::phase_telemetry::add("storage_landing_tip_reset", Duration::ZERO);
            RepositoryFacts {
                asked,
                ..RepositoryFacts::default()
            }
        };
        let forward = self.read(
            repository,
            &["merge-base", "--is-ancestor", &old, tip],
            PROBE_BUDGET,
            None,
        );
        if !matches!(forward, Read::Success(_)) {
            return reset(&mut self.stats, facts.asked);
        }
        let mut facts = facts;
        if !facts.not_contained.is_empty() {
            let limit = format!("--max-count={}", MAX_ADVANCE_COMMITS + 1);
            let range = format!("^{old}");
            let Read::Success(listed) = self.read(
                repository,
                &["rev-list", &limit, tip, &range, "--"],
                PROBE_BUDGET,
                None,
            ) else {
                return reset(&mut self.stats, facts.asked);
            };
            let landed: BTreeSet<&str> = listed.lines().map(str::trim).collect();
            if landed.len() > MAX_ADVANCE_COMMITS {
                return reset(&mut self.stats, facts.asked);
            }
            let now_landed: Vec<String> = facts
                .not_contained
                .iter()
                .filter(|commit| landed.contains(commit.as_str()))
                .cloned()
                .collect();
            for commit in now_landed {
                facts.not_contained.remove(&commit);
                facts.contained.insert(commit);
            }
        }
        self.stats.tip_advanced += 1;
        crate::phase_telemetry::add("storage_landing_tip_advanced", Duration::ZERO);
        facts
    }

    /// The commit the repository's default branch points at now, preferring
    /// the remote's view.
    fn read_tip(&mut self, repository: &Path) -> Result<String, Refusal> {
        let _span = crate::phase_telemetry::span("storage_landing_default_tip");
        let mut names = Vec::new();
        match self.read(
            repository,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            PROBE_BUDGET,
            None,
        ) {
            Read::Success(name) => names.push(name),
            Read::BudgetSpent => return Err(self.budget_spent()),
            _ => {}
        }
        names.extend(
            [
                "refs/remotes/origin/main",
                "refs/remotes/origin/master",
                "refs/heads/main",
                "refs/heads/master",
            ]
            .map(str::to_string),
        );
        for name in names {
            let spec = format!("{name}^{{commit}}");
            match self.read(
                repository,
                &["rev-parse", "--verify", "--quiet", &spec],
                PROBE_BUDGET,
                None,
            ) {
                Read::Success(commit) if is_full_commit_id(&commit) => return Ok(commit),
                Read::BudgetSpent => return Err(self.budget_spent()),
                Read::TimedOut => {
                    return Err(Refusal::Unproven(
                        "its default branch could not be read in time".into(),
                    ));
                }
                _ => {}
            }
        }
        Err(Refusal::Unproven("no default branch".into()))
    }

    fn budget_spent(&mut self) -> Refusal {
        self.stats.budget_spent += 1;
        crate::phase_telemetry::add("storage_landing_budget_spent", Duration::ZERO);
        Refusal::Unproven("not proven landed: this process's proof budget is spent".into())
    }

    /// One read against the process budget: each read gets at most
    /// `budget`, what is left of the process budget, and what is left of
    /// an active Git deadline.
    fn read(
        &mut self,
        repository: &Path,
        args: &[&str],
        budget: Duration,
        span: Option<&'static str>,
    ) -> Read {
        let left = self.process_budget.saturating_sub(self.reading);
        if left.is_zero() {
            return Read::BudgetSpent;
        }
        let mut budget = budget.min(left);
        if let Some(deadline) = crate::git::active_git_deadline() {
            budget = budget.min(deadline.saturating_duration_since(Instant::now()));
        }
        let _span = span.map(crate::phase_telemetry::span);
        let started = Instant::now();
        let read = read_git(repository, args, budget);
        self.reading += started.elapsed();
        read
    }

    /// Merge the persisted "not contained" facts into this prover.
    pub fn load(&mut self, host_state: &Path) {
        let _span = crate::phase_telemetry::span("storage_landing_cache_load");
        let Ok(bytes) = std::fs::read(host_state.join(CACHE_FILE)) else {
            return;
        };
        let Ok(file) = serde_json::from_slice::<CacheFile>(&bytes) else {
            return;
        };
        if file.schema != CACHE_SCHEMA {
            return;
        }
        for cached in file.repositories {
            if !is_full_commit_id(&cached.tip)
                || cached.not_contained.len() > MAX_FACTS_PER_REPOSITORY
                || !cached
                    .not_contained
                    .iter()
                    .all(|commit| is_full_commit_id(commit))
            {
                continue;
            }
            self.repositories
                .entry(cached.repository)
                .or_insert(RepositoryFacts {
                    tip: cached.tip,
                    not_contained: cached.not_contained,
                    ..RepositoryFacts::default()
                });
        }
    }

    /// Persist the "not contained" facts this process asked about (a
    /// removed archive's commits, and a repository nobody asks about any
    /// more, drop out).
    pub fn save(&self, host_state: &Path) -> std::io::Result<()> {
        let _span = crate::phase_telemetry::span("storage_landing_cache_save");
        let repositories: Vec<CachedRepository> = self
            .repositories
            .iter()
            .filter(|(_, facts)| !facts.asked.is_empty() && is_full_commit_id(&facts.tip))
            .map(|(repository, facts)| CachedRepository {
                repository: repository.clone(),
                tip: facts.tip.clone(),
                not_contained: facts
                    .not_contained
                    .intersection(&facts.asked)
                    .cloned()
                    .collect(),
            })
            .collect();
        let path = host_state.join(CACHE_FILE);
        if repositories.is_empty() && !path.exists() {
            return Ok(());
        }
        let file = CacheFile {
            schema: CACHE_SCHEMA,
            repositories,
        };
        let bytes = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
        super::write_atomically(&path, &bytes)
    }
}

/// One read-only Git command at an ordinary disk priority, within
/// `budget`: no automatic maintenance, no lazy fetch, and ancestry from
/// the real commits only (replace refs and grafts can fake it), in exactly
/// `repository` (inherited repository routing is dropped).
fn read_git(repository: &Path, args: &[&str], budget: Duration) -> Read {
    let mut command = crate::git::git_command();
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(variable);
    }
    command
        .arg("-c")
        .arg("core.useReplaceRefs=false")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    #[cfg(unix)]
    command.env("GIT_GRAFT_FILE", "/dev/null");
    #[cfg(windows)]
    command.env("GIT_GRAFT_FILE", "NUL");
    crate::git::prepare_git_child_io(&mut command);
    match crate::bounded_output::output_within(&mut command, budget) {
        Ok(Some(output)) if output.status.success() => {
            Read::Success(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Ok(Some(output)) if output.status.code() == Some(1) => Read::No,
        Ok(Some(_)) | Err(_) => Read::Failed,
        Ok(None) => Read::TimedOut,
    }
}

fn is_full_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.chars().all(|c| c.is_ascii_hexdigit())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn not_in(commit: &str, tip: &str) -> String {
    format!("commit {} is not in {tip}", short(commit))
}

/// The prover every archive check in this process shares.
pub fn shared() -> std::sync::MutexGuard<'static, LandingProver> {
    static SHARED: OnceLock<Mutex<LandingProver>> = OnceLock::new();
    SHARED
        .get_or_init(|| Mutex::new(LandingProver::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
