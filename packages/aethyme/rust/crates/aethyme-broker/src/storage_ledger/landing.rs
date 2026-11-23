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
//!   once per archive;
//! - a commit is probed once per tip: "contained" and "not contained" are
//!   facts about two commit ids, so they are kept;
//! - when the tip moves forward, one `rev-list <new> ^<old>` says which
//!   commits known not contained have since landed; when it moves anywhere
//!   else, everything known is dropped;
//! - only "not contained" is persisted across processes ([`LandingProver::save`]), so a
//!   damaged or stale cache file can keep an archive, never remove one; a
//!   "contained" answer is always proven by this process;
//! - every Git read runs at an ordinary disk priority
//!   (`crate::git::prepare_git_child_io`) within [`PROBE_BUDGET`], and
//!   the process stops probing after [`PROCESS_PROBE_BUDGET`]; an unproven
//!   commit keeps its archive.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
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
/// All probes of one process, at most: the rest stay unproven (kept).
pub const PROCESS_PROBE_BUDGET: Duration = Duration::from_secs(120);
/// A forward move listing more commits than this drops what is known.
const MAX_ADVANCE_COMMITS: usize = 50_000;
/// Facts kept per repository; above it a repository starts over.
const MAX_FACTS_PER_REPOSITORY: usize = 100_000;

/// Counts of each decision, for telemetry and the pass report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandingStats {
    pub tip_reads: u64,
    pub tip_memo_hits: u64,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RepositoryFacts {
    /// The default tip every fact below is relative to.
    tip: String,
    /// Proven by this process; never persisted.
    #[serde(skip)]
    contained: BTreeSet<String>,
    not_contained: BTreeSet<String>,
    #[serde(skip)]
    resolved_at: Option<Instant>,
    /// Asked by this process: only these are saved.
    #[serde(skip)]
    touched: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    schema: u32,
    repositories: BTreeMap<String, RepositoryFacts>,
}

/// What one Git read gave.
enum Read {
    Success(String),
    /// Exit status 1: a definite "no" from `merge-base --is-ancestor`.
    No,
    Failed,
    TimedOut,
}

/// Proves commits landed, remembering what it learned (see the module).
#[derive(Debug)]
pub struct LandingProver {
    repositories: BTreeMap<String, RepositoryFacts>,
    probe_budget: Duration,
    process_budget: Duration,
    probing: Duration,
    stats: LandingStats,
}

impl Default for LandingProver {
    fn default() -> Self {
        Self::with_budgets(PROBE_BUDGET, PROCESS_PROBE_BUDGET)
    }
}

impl LandingProver {
    /// A prover with explicit per-read and per-process budgets.
    pub fn with_budgets(probe_budget: Duration, process_budget: Duration) -> Self {
        Self {
            repositories: BTreeMap::new(),
            probe_budget,
            process_budget,
            probing: Duration::ZERO,
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
    /// contained in `repository`'s default branch; otherwise why not, in
    /// the words the storage report has always used.
    pub fn prove(&mut self, repository: &Path, commits: &[String]) -> Result<(), String> {
        let key = repository.to_string_lossy().into_owned();
        let tip = self.current_tip(repository, &key)?;
        let mut seen = BTreeSet::new();
        for commit in commits {
            if !seen.insert(commit.as_str()) {
                self.stats.duplicate_commits += 1;
                crate::phase_telemetry::add("storage_landing_duplicate_commit", Duration::ZERO);
                continue;
            }
            let facts = self.repositories.entry(key.clone()).or_default();
            if facts.contained.contains(commit) {
                self.stats.cached_contained += 1;
                crate::phase_telemetry::add("storage_landing_cached_contained", Duration::ZERO);
                continue;
            }
            if facts.not_contained.contains(commit) {
                self.stats.cached_not_contained += 1;
                crate::phase_telemetry::add("storage_landing_cached_not_contained", Duration::ZERO);
                return Err(not_in(commit, &tip));
            }
            if self.probing >= self.process_budget {
                self.stats.budget_spent += 1;
                crate::phase_telemetry::add("storage_landing_budget_spent", Duration::ZERO);
                return Err(format!(
                    "commit {} was not proven landed: this process's proof budget is spent",
                    short(commit)
                ));
            }
            self.stats.probes += 1;
            let read = self.timed_read(
                repository,
                &["merge-base", "--is-ancestor", commit, &tip],
                "storage_landing_probe",
            );
            let facts = self.repositories.entry(key.clone()).or_default();
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
                    return Err(not_in(commit, &tip));
                }
                // A missing object or any other failure: not proven, and
                // not remembered either.
                Read::Failed => {
                    self.stats.probe_unproven += 1;
                    crate::phase_telemetry::add("storage_landing_probe_unproven", Duration::ZERO);
                    return Err(not_in(commit, &tip));
                }
                Read::TimedOut => {
                    self.stats.probe_timeouts += 1;
                    crate::phase_telemetry::add("storage_landing_probe_timeout", Duration::ZERO);
                    return Err(format!(
                        "commit {} was not proven landed within {} s",
                        short(commit),
                        self.probe_budget.as_secs()
                    ));
                }
            }
        }
        Ok(())
    }

    /// The default tip, read at most once per [`TIP_TTL`]; a moved tip
    /// carries forward what stays true.
    fn current_tip(&mut self, repository: &Path, key: &str) -> Result<String, String> {
        if let Some(facts) = self.repositories.get_mut(key) {
            facts.touched = true;
            if !facts.tip.is_empty()
                && facts
                    .resolved_at
                    .is_some_and(|resolved| resolved.elapsed() < TIP_TTL)
            {
                self.stats.tip_memo_hits += 1;
                crate::phase_telemetry::add("storage_landing_tip_memo_hit", Duration::ZERO);
                return Ok(facts.tip.clone());
            }
        }
        self.stats.tip_reads += 1;
        let tip = {
            let _span = crate::phase_telemetry::span("storage_landing_default_tip");
            self.default_tip(repository)
        }
        .ok_or("no default branch")?;
        let mut facts = self.repositories.remove(key).unwrap_or_default();
        if !facts.tip.is_empty() && facts.tip != tip {
            facts = self.advance(repository, facts, &tip);
        }
        facts.tip = tip.clone();
        facts.resolved_at = Some(Instant::now());
        facts.touched = true;
        if facts.contained.len() + facts.not_contained.len() > MAX_FACTS_PER_REPOSITORY {
            facts.contained.clear();
            facts.not_contained.clear();
        }
        self.repositories.insert(key.to_string(), facts);
        Ok(tip)
    }

    /// Carry facts from `facts.tip` to `tip`: only along a forward move,
    /// where the commits that landed meanwhile are exactly
    /// `rev-list <tip> ^<old tip>`.
    fn advance(&mut self, repository: &Path, facts: RepositoryFacts, tip: &str) -> RepositoryFacts {
        let _span = crate::phase_telemetry::span("storage_landing_tip_advance");
        let old = facts.tip.clone();
        let reset = |stats: &mut LandingStats| {
            stats.tip_reset += 1;
            crate::phase_telemetry::add("storage_landing_tip_reset", Duration::ZERO);
            RepositoryFacts::default()
        };
        if !matches!(
            self.read(repository, &["merge-base", "--is-ancestor", &old, tip]),
            Read::Success(_)
        ) {
            return reset(&mut self.stats);
        }
        let mut facts = facts;
        if !facts.not_contained.is_empty() {
            let limit = format!("--max-count={}", MAX_ADVANCE_COMMITS + 1);
            let range = format!("^{old}");
            let Read::Success(listed) =
                self.read(repository, &["rev-list", &limit, tip, &range, "--"])
            else {
                return reset(&mut self.stats);
            };
            let landed: BTreeSet<&str> = listed.lines().map(str::trim).collect();
            if landed.len() > MAX_ADVANCE_COMMITS {
                return reset(&mut self.stats);
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

    /// The commit the repository's default branch points at, preferring the
    /// remote's view.
    fn default_tip(&mut self, repository: &Path) -> Option<String> {
        let mut names = Vec::new();
        if let Read::Success(name) = self.read(
            repository,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        ) {
            names.push(name);
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
        names.into_iter().find_map(|name| {
            match self.read(
                repository,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{name}^{{commit}}"),
                ],
            ) {
                Read::Success(commit) if is_full_commit_id(&commit) => Some(commit),
                _ => None,
            }
        })
    }

    fn timed_read(&mut self, repository: &Path, args: &[&str], span: &'static str) -> Read {
        let _span = crate::phase_telemetry::span(span);
        let started = Instant::now();
        let read = read_git(repository, args, self.probe_budget);
        self.probing += started.elapsed();
        read
    }

    /// A tip or range read, within [`PROBE_BUDGET`].
    fn read(&self, repository: &Path, args: &[&str]) -> Read {
        read_git(repository, args, PROBE_BUDGET)
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
        for (repository, mut facts) in file.repositories {
            if !is_full_commit_id(&facts.tip)
                || facts.not_contained.len() > MAX_FACTS_PER_REPOSITORY
                || !facts
                    .not_contained
                    .iter()
                    .all(|commit| is_full_commit_id(commit))
            {
                continue;
            }
            facts.contained.clear();
            facts.resolved_at = None;
            facts.touched = false;
            self.repositories.entry(repository).or_insert(facts);
        }
    }

    /// Persist the "not contained" facts of the repositories this process
    /// asked about (a repository nobody asks about any more drops out).
    pub fn save(&self, host_state: &Path) -> std::io::Result<()> {
        let _span = crate::phase_telemetry::span("storage_landing_cache_save");
        let repositories = self
            .repositories
            .iter()
            .filter(|(_, facts)| facts.touched && is_full_commit_id(&facts.tip))
            .map(|(repository, facts)| (repository.clone(), facts.clone()))
            .collect::<BTreeMap<_, _>>();
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
/// `budget`; never automatic maintenance.
fn read_git(repository: &Path, args: &[&str], budget: Duration) -> Read {
    let mut command = crate::git::git_command();
    command
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
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
