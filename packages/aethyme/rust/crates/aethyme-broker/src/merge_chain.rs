//! `broker advanced merge-chain`: land pull requests one at a time, each
//! verified on the exact commit that merges.
//!
//! A queue of green pull requests is not a queue of green merges. Each merge
//! moves the default branch, so the next pull request's checks ran against a
//! base that no longer exists. The chain therefore updates each pull request
//! onto the current base, waits for the checks of *that* head, merges with
//! `--match-head-commit` so a push in between cannot slip in untested, and
//! then waits for the default branch's own runs on the merge commit before it
//! touches the next one.
//!
//! Everything is keyed on a pull-request number and a commit SHA. The script
//! this replaces once matched a stale `ALL MERGED` line in its own log and
//! merged a pull request nobody had tested against its predecessor; a check
//! run superseded by a later run of the same name once read as a failure.
//! Neither can happen here: progress is read from the provider, and only the
//! latest run of each check or workflow counts.
//!
//! Reads go straight to `gh` (read-only provider inspection). Every write --
//! `pr ready`, `pr update-branch`, `pr merge`, `workflow run` -- goes through
//! [`ChainWriter`], which the CLI backs with the coordinated-operation
//! journal, so each one is authorized, queued and recorded like any
//! `broker advanced gh` call. Waiting happens between operations, never while
//! one holds the repository lock.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, Instant};

/// How a pull request is merged. The chain does not guess: the default is a
/// merge commit, and anything else is the caller's explicit choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "merge" => Some(Self::Merge),
            "squash" => Some(Self::Squash),
            "rebase" => Some(Self::Rebase),
            _ => None,
        }
    }

    fn gh_flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MergeChainOptions {
    pub repository: String,
    pub pull_requests: Vec<u64>,
    pub merge_method: MergeMethod,
    pub poll_interval: Duration,
    /// How long to wait for a pull request's checks on one head.
    pub checks_timeout: Duration,
    /// How long to wait for the default branch's runs on a merge commit.
    pub main_timeout: Duration,
    /// With no default-branch run on the merge commit after this long,
    /// dispatch `gates_workflow` (when one is configured).
    pub dispatch_after: Duration,
    pub gates_workflow: Option<String>,
    pub dry_run: bool,
}

/// A pull request as the chain needs it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PullRequestState {
    pub number: u64,
    pub state: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
    #[serde(rename = "headRefOid")]
    pub head: String,
    #[serde(rename = "baseRefName")]
    pub base: String,
    #[serde(rename = "mergeCommit", default)]
    pub merge_commit: Option<CommitRef>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CommitRef {
    pub oid: String,
}

impl PullRequestState {
    fn merged(&self) -> bool {
        self.state.eq_ignore_ascii_case("MERGED")
    }

    fn closed(&self) -> bool {
        self.state.eq_ignore_ascii_case("CLOSED")
    }

    fn merge_sha(&self) -> Option<&str> {
        self.merge_commit.as_ref().map(|commit| commit.oid.as_str())
    }
}

/// One check run or workflow run, reduced to what decides a verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunState {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    /// Orders runs of the same name: the provider's start or creation time,
    /// then its numeric id. ISO-8601 strings sort chronologically.
    pub started_at: String,
    pub id: u64,
}

impl RunState {
    fn completed(&self) -> bool {
        self.status == "completed"
    }

    fn passed(&self) -> bool {
        matches!(
            self.conclusion.as_deref(),
            Some("success" | "skipped" | "neutral")
        )
    }
}

/// The latest run of each name. A rerun or a re-trigger leaves the older run
/// on the commit, typically `cancelled`; only the newest one says anything
/// about the commit now.
pub fn latest_runs(runs: &[RunState]) -> Vec<RunState> {
    let mut latest: BTreeMap<&str, &RunState> = BTreeMap::new();
    for run in runs {
        let newer = latest.get(run.name.as_str()).is_none_or(|current| {
            (run.started_at.as_str(), run.id) > (current.started_at.as_str(), current.id)
        });
        if newer {
            latest.insert(run.name.as_str(), run);
        }
    }
    latest.into_values().cloned().collect()
}

/// What a set of runs says about a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    /// No run has appeared yet.
    Absent,
    Pending {
        pending: Vec<String>,
    },
    Failed {
        failed: Vec<String>,
    },
    Passed {
        names: Vec<String>,
    },
}

pub fn run_verdict(runs: &[RunState]) -> RunVerdict {
    let latest = latest_runs(runs);
    if latest.is_empty() {
        return RunVerdict::Absent;
    }
    let failed: Vec<String> = latest
        .iter()
        .filter(|run| run.completed() && !run.passed())
        .map(|run| {
            format!(
                "{} ({})",
                run.name,
                run.conclusion.as_deref().unwrap_or("no conclusion")
            )
        })
        .collect();
    if !failed.is_empty() {
        return RunVerdict::Failed { failed };
    }
    let pending: Vec<String> = latest
        .iter()
        .filter(|run| !run.completed())
        .map(|run| run.name.clone())
        .collect();
    if !pending.is_empty() {
        return RunVerdict::Pending { pending };
    }
    RunVerdict::Passed {
        names: latest.into_iter().map(|run| run.name).collect(),
    }
}

/// Read-only provider access.
pub trait ChainReader {
    fn pull_request(&self, number: u64) -> Result<PullRequestState, String>;
    /// Commits `base` has that `head` lacks.
    fn behind_by(&self, base: &str, head: &str) -> Result<u64, String>;
    fn check_runs(&self, sha: &str) -> Result<Vec<RunState>, String>;
    /// Workflow runs for `sha` on branch `base`.
    fn branch_runs(&self, sha: &str, base: &str) -> Result<Vec<RunState>, String>;
}

/// How a coordinated write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Succeeded {
        operation: i64,
    },
    Failed {
        operation: i64,
        detail: String,
    },
    /// The write may or may not have happened; `recovery` says how to find out.
    Unknown {
        operation: i64,
        recovery: String,
    },
}

/// Every provider write the chain makes.
pub trait ChainWriter {
    fn write(&mut self, args: Vec<String>) -> Result<WriteOutcome, String>;
}

/// Where a pull request's turn currently is, for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Inspect,
    Ready,
    UpdateBranch,
    Checks,
    Merge,
    MainChecks,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Ready => "ready",
            Self::UpdateBranch => "update-branch",
            Self::Checks => "checks",
            Self::Merge => "merge",
            Self::MainChecks => "main-checks",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MergedPullRequest {
    pub number: u64,
    pub head: String,
    pub merge_commit: String,
    /// True when an earlier run of the chain merged it and this run only
    /// re-verified the default branch.
    pub already_merged: bool,
    pub main_checks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChainStop {
    pub pull_request: u64,
    pub stage: Stage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    pub reason: String,
    pub next_action: String,
    /// The exit-status class the CLI reports.
    #[serde(skip)]
    pub exit_code: u8,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannedStep {
    pub pull_request: u64,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeChainReport {
    pub repository: String,
    pub merge_method: MergeMethod,
    pub dry_run: bool,
    pub merged: Vec<MergedPullRequest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlannedStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<ChainStop>,
}

/// Elapsed time and the pause between polls; a seam so tests need not sleep.
pub trait ChainClock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock;

impl ChainClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

struct Chain<'a, R, W, C> {
    options: &'a MergeChainOptions,
    reader: &'a R,
    writer: &'a mut W,
    clock: &'a C,
    progress: &'a mut dyn FnMut(String),
}

type Step<T> = Result<T, Box<ChainStop>>;

fn rerun_hint() -> &'static str {
    "re-run the same merge-chain command; pull requests already merged are skipped \
     and the last one's default-branch checks are verified again"
}

impl<R: ChainReader, W: ChainWriter, C: ChainClock> Chain<'_, R, W, C> {
    fn say(&mut self, line: String) {
        (self.progress)(line);
    }

    fn stop(
        &self,
        number: u64,
        stage: Stage,
        reason: impl Into<String>,
        next_action: impl Into<String>,
        exit_code: u8,
    ) -> Box<ChainStop> {
        Box::new(ChainStop {
            pull_request: number,
            stage,
            head: None,
            merge_commit: None,
            reason: reason.into(),
            next_action: next_action.into(),
            exit_code,
        })
    }

    fn read<T>(&self, number: u64, stage: Stage, value: Result<T, String>) -> Step<T> {
        value.map_err(|error| {
            self.stop(
                number,
                stage,
                format!("reading the provider failed: {error}"),
                rerun_hint(),
                crate::exit_status::ENVIRONMENT,
            )
        })
    }

    fn write(&mut self, number: u64, stage: Stage, args: Vec<String>) -> Step<i64> {
        let printed = args.join(" ");
        match self.writer.write(args) {
            Ok(WriteOutcome::Succeeded { operation }) => {
                self.say(format!(
                    "#{number} {}: gh {printed} (operation {operation})",
                    stage.label()
                ));
                Ok(operation)
            }
            Ok(WriteOutcome::Failed { operation, detail }) => Err(self.stop(
                number,
                stage,
                format!("gh {printed} failed (operation {operation}): {detail}"),
                format!(
                    "inspect `aethyme broker advanced operations show {operation}`, fix the \
                     cause, then {}",
                    rerun_hint()
                ),
                crate::exit_status::FAILED,
            )),
            Ok(WriteOutcome::Unknown {
                operation,
                recovery,
            }) => Err(self.stop(
                number,
                stage,
                format!("the outcome of gh {printed} is unknown (operation {operation})"),
                recovery,
                crate::exit_status::OUTCOME_UNKNOWN,
            )),
            Err(error) => Err(self.stop(
                number,
                stage,
                format!("gh {printed} was not run: {error}"),
                rerun_hint(),
                crate::exit_status::REFUSED,
            )),
        }
    }

    /// Waits until the pull request's head is no longer `old_head`: an
    /// update-branch is acknowledged before the new head exists.
    fn wait_for_new_head(&mut self, number: u64, old_head: &str) -> Step<PullRequestState> {
        let started = self.clock.now();
        loop {
            let pr = self.read(
                number,
                Stage::UpdateBranch,
                self.reader.pull_request(number),
            )?;
            if pr.head != old_head {
                return Ok(pr);
            }
            if self.clock.now().duration_since(started) >= self.options.checks_timeout {
                return Err(self.stop(
                    number,
                    Stage::UpdateBranch,
                    format!("the head stayed at {old_head} after update-branch"),
                    rerun_hint(),
                    crate::exit_status::FAILED,
                ));
            }
            self.clock.sleep(self.options.poll_interval);
        }
    }

    fn wait_for_checks(&mut self, number: u64, head: &str) -> Step<Vec<String>> {
        let started = self.clock.now();
        let mut last_pending: Option<Vec<String>> = None;
        loop {
            let pr = self.read(number, Stage::Checks, self.reader.pull_request(number))?;
            if pr.head != head {
                let mut stop = self.stop(
                    number,
                    Stage::Checks,
                    format!(
                        "the head moved from {head} to {} while its checks ran",
                        pr.head
                    ),
                    format!("the new head has not been verified; {}", rerun_hint()),
                    crate::exit_status::REFUSED,
                );
                stop.head = Some(pr.head);
                return Err(stop);
            }
            let runs = self.read(number, Stage::Checks, self.reader.check_runs(head))?;
            match run_verdict(&runs) {
                RunVerdict::Passed { names } => return Ok(names),
                RunVerdict::Failed { failed } => {
                    let mut stop = self.stop(
                        number,
                        Stage::Checks,
                        format!("checks failed on {head}: {}", failed.join(", ")),
                        format!(
                            "fix pull request #{number} and push; nothing after it was \
                             merged; then {}",
                            rerun_hint()
                        ),
                        crate::exit_status::VERIFICATION_FAILED,
                    );
                    stop.head = Some(head.to_string());
                    return Err(stop);
                }
                RunVerdict::Pending { pending } => {
                    if last_pending.as_ref() != Some(&pending) {
                        self.say(format!(
                            "#{number} checks: waiting on {}",
                            pending.join(", ")
                        ));
                        last_pending = Some(pending);
                    }
                }
                RunVerdict::Absent => {}
            }
            if self.clock.now().duration_since(started) >= self.options.checks_timeout {
                let mut stop = self.stop(
                    number,
                    Stage::Checks,
                    format!(
                        "checks on {head} did not finish within {}s",
                        self.options.checks_timeout.as_secs()
                    ),
                    rerun_hint(),
                    crate::exit_status::FAILED,
                );
                stop.head = Some(head.to_string());
                return Err(stop);
            }
            self.clock.sleep(self.options.poll_interval);
        }
    }

    /// Waits for the default branch's runs on `merge_commit`. Returns the
    /// passing run names and the workflow dispatched, if one was.
    fn wait_for_main(
        &mut self,
        number: u64,
        merge_commit: &str,
        base: &str,
    ) -> Step<(Vec<String>, Option<String>)> {
        let started = self.clock.now();
        let mut dispatched: Option<String> = None;
        let mut last_pending: Option<Vec<String>> = None;
        loop {
            let runs = self.read(
                number,
                Stage::MainChecks,
                self.reader.branch_runs(merge_commit, base),
            )?;
            let elapsed = self.clock.now().duration_since(started);
            match run_verdict(&runs) {
                RunVerdict::Passed { names } => return Ok((names, dispatched)),
                RunVerdict::Failed { failed } => {
                    let mut stop = self.stop(
                        number,
                        Stage::MainChecks,
                        format!(
                            "{base} is red after merging #{number}: {}",
                            failed.join(", ")
                        ),
                        format!(
                            "fix {base} before anything else lands; the remaining pull \
                             requests were not merged; then {}",
                            rerun_hint()
                        ),
                        crate::exit_status::VERIFICATION_FAILED,
                    );
                    stop.merge_commit = Some(merge_commit.to_string());
                    return Err(stop);
                }
                RunVerdict::Pending { pending } => {
                    if last_pending.as_ref() != Some(&pending) {
                        self.say(format!(
                            "#{number} {base} checks on {}: waiting on {}",
                            short(merge_commit),
                            pending.join(", ")
                        ));
                        last_pending = Some(pending);
                    }
                }
                RunVerdict::Absent => {
                    if dispatched.is_none()
                        && elapsed >= self.options.dispatch_after
                        && let Some(workflow) = self.options.gates_workflow.clone()
                    {
                        self.say(format!(
                            "#{number} no {base} run on {} after {}s; dispatching {workflow}",
                            short(merge_commit),
                            elapsed.as_secs()
                        ));
                        let args = vec![
                            "workflow".into(),
                            "run".into(),
                            workflow.clone(),
                            "--ref".into(),
                            base.to_string(),
                        ];
                        self.write(number, Stage::MainChecks, args)?;
                        dispatched = Some(workflow);
                    }
                }
            }
            if elapsed >= self.options.main_timeout {
                let mut stop = self.stop(
                    number,
                    Stage::MainChecks,
                    format!(
                        "{base} checks on merge commit {merge_commit} did not finish within {}s",
                        self.options.main_timeout.as_secs()
                    ),
                    if self.options.gates_workflow.is_none() {
                        format!(
                            "no run appeared or finished; pass --gates-workflow <file> to \
                             dispatch one, or {}",
                            rerun_hint()
                        )
                    } else {
                        rerun_hint().to_string()
                    },
                    crate::exit_status::FAILED,
                );
                stop.merge_commit = Some(merge_commit.to_string());
                return Err(stop);
            }
            self.clock.sleep(self.options.poll_interval);
        }
    }

    fn verify_already_merged(
        &mut self,
        pr: &PullRequestState,
        report: &mut MergeChainReport,
    ) -> Step<()> {
        let Some(merge_commit) = pr.merge_sha().map(str::to_string) else {
            return Err(self.stop(
                pr.number,
                Stage::Inspect,
                "merged, but the provider reports no merge commit",
                rerun_hint(),
                crate::exit_status::FAILED,
            ));
        };
        self.say(format!(
            "#{} already merged as {}; verifying {} again",
            pr.number,
            short(&merge_commit),
            pr.base
        ));
        let (main_checks, dispatched) = self.wait_for_main(pr.number, &merge_commit, &pr.base)?;
        report.merged.push(MergedPullRequest {
            number: pr.number,
            head: pr.head.clone(),
            merge_commit,
            already_merged: true,
            main_checks,
            dispatched,
        });
        Ok(())
    }

    /// Lands one open pull request, starting from the state `run` read.
    fn land(&mut self, mut pr: PullRequestState, report: &mut MergeChainReport) -> Step<()> {
        let number = pr.number;
        if pr.is_draft {
            let args = vec!["pr".into(), "ready".into(), number.to_string()];
            self.write(number, Stage::Ready, args)?;
        }
        let behind = self.read(
            number,
            Stage::UpdateBranch,
            self.reader.behind_by(&pr.base, &pr.head),
        )?;
        if behind > 0 {
            let args = vec!["pr".into(), "update-branch".into(), number.to_string()];
            self.write(number, Stage::UpdateBranch, args)?;
            let old = pr.head.clone();
            pr = self.wait_for_new_head(number, &old)?;
            self.say(format!(
                "#{number} updated onto {}: head {} -> {}",
                pr.base,
                short(&old),
                short(&pr.head)
            ));
        }
        let head = pr.head.clone();
        let checks = self.wait_for_checks(number, &head)?;
        self.say(format!(
            "#{number} checks passed on {}: {}",
            short(&head),
            checks.join(", ")
        ));
        let args = vec![
            "pr".into(),
            "merge".into(),
            number.to_string(),
            self.options.merge_method.gh_flag().into(),
            "--match-head-commit".into(),
            head.clone(),
        ];
        self.write(number, Stage::Merge, args)?;
        let merged = self.read(number, Stage::Merge, self.reader.pull_request(number))?;
        let Some(merge_commit) = merged.merge_sha().filter(|_| merged.merged()) else {
            let mut stop = self.stop(
                number,
                Stage::Merge,
                format!(
                    "the merge was accepted but the pull request reads {} with no merge commit",
                    merged.state
                ),
                rerun_hint(),
                crate::exit_status::FAILED,
            );
            stop.head = Some(head);
            return Err(stop);
        };
        let merge_commit = merge_commit.to_string();
        self.say(format!("#{number} merged as {}", short(&merge_commit)));
        let (main_checks, dispatched) = self.wait_for_main(number, &merge_commit, &pr.base)?;
        self.say(format!(
            "#{number} {} green on {}: {}",
            pr.base,
            short(&merge_commit),
            main_checks.join(", ")
        ));
        report.merged.push(MergedPullRequest {
            number,
            head,
            merge_commit,
            already_merged: false,
            main_checks,
            dispatched,
        });
        Ok(())
    }

    fn plan(&mut self, report: &mut MergeChainReport) -> Step<()> {
        for &number in &self.options.pull_requests {
            let pr = self.read(number, Stage::Inspect, self.reader.pull_request(number))?;
            let mut steps = Vec::new();
            if pr.merged() {
                steps.push(format!(
                    "already merged as {}: skip (the last merged one is re-verified on {})",
                    pr.merge_sha().map(short).unwrap_or("?"),
                    pr.base
                ));
            } else if pr.closed() {
                steps.push("closed without merging: the chain would stop here".into());
            } else {
                if pr.is_draft {
                    steps.push("gh pr ready".into());
                }
                let behind = self.read(
                    number,
                    Stage::UpdateBranch,
                    self.reader.behind_by(&pr.base, &pr.head),
                )?;
                if behind > 0 {
                    steps.push(format!("gh pr update-branch ({behind} behind {})", pr.base));
                }
                steps.push("wait for the latest run of each check on the new head".into());
                steps.push(format!(
                    "gh pr merge {} --match-head-commit <head>",
                    self.options.merge_method.gh_flag()
                ));
                steps.push(format!(
                    "wait for {} runs on the merge commit{}",
                    pr.base,
                    match &self.options.gates_workflow {
                        Some(workflow) => format!(
                            " (dispatch {workflow} after {}s without one)",
                            self.options.dispatch_after.as_secs()
                        ),
                        None => String::new(),
                    }
                ));
            }
            report.plan.push(PlannedStep {
                pull_request: number,
                steps,
            });
        }
        Ok(())
    }

    fn run(&mut self, report: &mut MergeChainReport) -> Step<()> {
        if self.options.dry_run {
            return self.plan(report);
        }
        // An earlier run may have merged a prefix of the chain and stopped
        // (or crashed) before the default branch went green. Re-verifying
        // only the last merged one is enough: its merge commit contains
        // every earlier one.
        let mut last_merged: Option<PullRequestState> = None;
        for &number in &self.options.pull_requests {
            let pr = self.read(number, Stage::Inspect, self.reader.pull_request(number))?;
            if pr.merged() {
                last_merged = Some(pr);
                continue;
            }
            if let Some(previous) = last_merged.take() {
                self.verify_already_merged(&previous, report)?;
            }
            if pr.closed() {
                return Err(self.stop(
                    number,
                    Stage::Inspect,
                    "closed without merging",
                    format!(
                        "reopen pull request #{number} or drop it from the list, then {}",
                        rerun_hint()
                    ),
                    crate::exit_status::REFUSED,
                ));
            }
            self.land(pr, report)?;
        }
        if let Some(previous) = last_merged.take() {
            self.verify_already_merged(&previous, report)?;
        }
        Ok(())
    }
}

/// Runs the chain. A stop is part of the report, not an error: what merged
/// before it is as much the answer as where it stopped.
pub fn run_merge_chain<R: ChainReader, W: ChainWriter, C: ChainClock>(
    options: &MergeChainOptions,
    reader: &R,
    writer: &mut W,
    clock: &C,
    progress: &mut dyn FnMut(String),
) -> MergeChainReport {
    let mut report = MergeChainReport {
        repository: options.repository.clone(),
        merge_method: options.merge_method,
        dry_run: options.dry_run,
        merged: Vec::new(),
        plan: Vec::new(),
        stopped: None,
    };
    let mut chain = Chain {
        options,
        reader,
        writer,
        clock,
        progress,
    };
    if let Err(stop) = chain.run(&mut report) {
        report.stopped = Some(*stop);
    }
    report
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// [`ChainReader`] over the `gh` CLI.
pub struct GhChainReader {
    pub repository: String,
}

impl GhChainReader {
    fn gh(&self, args: &[&str]) -> Result<serde_json::Value, String> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|error| format!("cannot run gh: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "gh {} exited {}: {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("gh {} returned invalid JSON: {error}", args.join(" ")))
    }

    fn runs(value: &serde_json::Value, key: &str, time_field: &str) -> Vec<RunState> {
        value[key]
            .as_array()
            .map(|runs| {
                runs.iter()
                    .map(|run| RunState {
                        name: run["name"].as_str().unwrap_or_default().to_string(),
                        status: run["status"].as_str().unwrap_or_default().to_string(),
                        conclusion: run["conclusion"].as_str().map(str::to_string),
                        started_at: run[time_field].as_str().unwrap_or_default().to_string(),
                        id: run["id"].as_u64().unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl ChainReader for GhChainReader {
    fn pull_request(&self, number: u64) -> Result<PullRequestState, String> {
        let value = self.gh(&[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &self.repository,
            "--json",
            "number,state,isDraft,headRefOid,baseRefName,mergeCommit",
        ])?;
        serde_json::from_value(value).map_err(|error| format!("unexpected pr view JSON: {error}"))
    }

    fn behind_by(&self, base: &str, head: &str) -> Result<u64, String> {
        let value = self.gh(&[
            "api",
            &format!("repos/{}/compare/{base}...{head}", self.repository),
        ])?;
        value["behind_by"]
            .as_u64()
            .ok_or_else(|| "compare response has no behind_by".to_string())
    }

    fn check_runs(&self, sha: &str) -> Result<Vec<RunState>, String> {
        let value = self.gh(&[
            "api",
            &format!(
                "repos/{}/commits/{sha}/check-runs?per_page=100",
                self.repository
            ),
        ])?;
        Ok(Self::runs(&value, "check_runs", "started_at"))
    }

    fn branch_runs(&self, sha: &str, base: &str) -> Result<Vec<RunState>, String> {
        let value = self.gh(&[
            "api",
            &format!(
                "repos/{}/actions/runs?head_sha={sha}&branch={base}&per_page=100",
                self.repository
            ),
        ])?;
        Ok(Self::runs(&value, "workflow_runs", "created_at"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, status: &str, conclusion: Option<&str>, at: &str, id: u64) -> RunState {
        RunState {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            started_at: at.into(),
            id,
        }
    }

    #[test]
    fn a_superseded_cancelled_run_does_not_fail_the_commit() {
        let runs = [
            run(
                "lint",
                "completed",
                Some("cancelled"),
                "2026-10-03T15:10:13Z",
                1,
            ),
            run(
                "lint",
                "completed",
                Some("success"),
                "2026-10-03T15:10:20Z",
                2,
            ),
            run(
                "tests",
                "completed",
                Some("success"),
                "2026-10-03T15:10:19Z",
                3,
            ),
        ];
        assert_eq!(
            run_verdict(&runs),
            RunVerdict::Passed {
                names: vec!["lint".into(), "tests".into()]
            }
        );
    }

    #[test]
    fn the_newest_run_decides_even_when_an_older_one_passed() {
        let runs = [
            run(
                "lint",
                "completed",
                Some("success"),
                "2026-10-03T15:00:00Z",
                1,
            ),
            run(
                "lint",
                "completed",
                Some("failure"),
                "2026-10-03T15:10:00Z",
                2,
            ),
        ];
        assert_eq!(
            run_verdict(&runs),
            RunVerdict::Failed {
                failed: vec!["lint (failure)".into()]
            }
        );
    }

    #[test]
    fn a_pending_newest_run_keeps_the_commit_pending() {
        let runs = [
            run(
                "lint",
                "completed",
                Some("failure"),
                "2026-10-03T15:00:00Z",
                1,
            ),
            run("lint", "in_progress", None, "2026-10-03T15:10:00Z", 2),
        ];
        assert_eq!(
            run_verdict(&runs),
            RunVerdict::Pending {
                pending: vec!["lint".into()]
            }
        );
        assert_eq!(run_verdict(&[]), RunVerdict::Absent);
    }
}
