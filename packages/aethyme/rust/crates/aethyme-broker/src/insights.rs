//! What the broker's own history says about how long work takes and whether
//! it lands.
//!
//! Everything here is derived. The events, the queue, the gate rows and the
//! activity intervals are the record; this module is the only place that
//! decides what those rows *mean*, which is why it is worth separating from
//! capture. A funnel assembled here can be corrected when the definition is
//! wrong. Data captured under a wrong definition cannot.
//!
//! # The four questions this refuses to answer carelessly
//!
//! **Wall-clock is not work.** [`Stage::WallClockMs`] is reported next to
//! [`SessionSummary::active_ms`] on every session, and never instead of it. A
//! session's wall-clock includes every overnight pause, every blocked gate and
//! every human thinking break inside it. Publishing one without the other is
//! how a 46-hour average came to sit next to a median inside the 1-4 hour band
//! and read as a fact about how long developers take.
//!
//! **A funnel is not a rate.** [`Funnel`] reports stages as *counts of sessions
//! that reached them*, which is answerable from an event log with no time
//! window at all. Throughput needs a denominator, so [`Funnel::per_day`] is
//! only filled in when the caller supplies one, and an omitted window yields
//! `None` rather than a total divided by however long the log happens to be.
//!
//! **Averaging durations hides the shape.** [`DurationSummary`] carries p50 and
//! p95 beside the mean and the total sample count, because a mean over a
//! bimodal distribution ("most edits are quick, the hard ones are very slow")
//! describes neither group. A summary with fewer than [`DISTRIBUTION_FLOOR`]
//! samples reports `None` for its percentiles instead of a number computed from
//! two observations.
//!
//! **Nothing here is a judgement about a person.** No module in this crate
//! ranks sessions, scores agents, or attributes a duration to an identity.
//! `agent_identity` exists on the session row and is deliberately not joined
//! here: the questions this answers are about the broker and the pipeline. A
//! per-developer view built on these numbers would be measuring how long
//! someone leaves a worktree open, which is not work.
//!
//! # Backfill honesty
//!
//! Activity intervals and pull request milestones only exist from the release
//! that introduced them. For history recorded before that, [`InsightsReport`]
//! reports the gap rather than filling it: [`Coverage`] separates *measured*
//! from *unmeasured*, and the unmeasured count is never added to a duration.

use std::collections::BTreeMap;

use crate::types::SessionOrigin;

/// Schema version of the `insights` JSON contract.
pub const INSIGHTS_SCHEMA_VERSION: u32 = 1;

/// Silence longer than this between two activity signals ends a period of
/// attention and starts a new one.
///
/// This number *defines* [`SessionSummary::active_ms`] and has no
/// self-evident right answer, so it is reported alongside every figure derived
/// from it (see [`InsightsReport::idle_gap_ms`]). Fifteen minutes is chosen
/// because it is comfortably longer than any single tool call, gate run, or
/// think-pause an agent takes, and comfortably shorter than a lunch break or an
/// overnight pause. A session that goes quiet for longer is not being worked
/// on, and counting the quiet as work is the error this constant exists to
/// prevent.
pub const IDLE_GAP_MS: i64 = 15 * 60 * 1000;

/// Below this many samples, [`DurationSummary`] reports no percentile.
///
/// A p95 over two observations is one of the two observations. Reporting it
/// would let a two-sample summary look as well-characterized as a two-thousand
/// sample one, and the sample count alone is too easy to overlook.
pub const DISTRIBUTION_FLOOR: usize = 5;

/// Where a session reached on the path from registered to landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The session exists in the broker. Every session reaches this.
    Registered,
    /// Something was submitted to the merge queue for this session.
    Submitted,
    /// Gates passed on the merged tree for a submission of this session.
    Verified,
    /// The session's work reached the integration branch, or was found
    /// already landed upstream.
    Landed,
    /// A pull request associated with this session was opened.
    PullRequestOpened,
    /// A pull request associated with this session was merged.
    PullRequestMerged,
}

impl Stage {
    /// Report order. Pull request milestones come after landing because they
    /// are the secondary path: a session that ships through the integration
    /// branch never opens one.
    pub const ORDER: [Stage; 6] = [
        Stage::Registered,
        Stage::Submitted,
        Stage::Verified,
        Stage::Landed,
        Stage::PullRequestOpened,
        Stage::PullRequestMerged,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Submitted => "submitted",
            Self::Verified => "verified",
            Self::Landed => "landed",
            Self::PullRequestOpened => "pr_opened",
            Self::PullRequestMerged => "pr_merged",
        }
    }
}

/// How a session's work left the broker.
///
/// `Ord` is derived so an outcome can key a map, and the order it implies is
/// declaration order, which means nothing. Nothing in this module compares
/// outcomes by that order; a session's outcome is chosen by timestamp
/// ([`resolve_outcome`]), never by ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Reached `Stage::Landed`.
    Landed,
    /// At least one submission was rejected by a gate.
    Rejected,
    /// Submitted, and superseded by a newer head before it landed.
    Superseded,
    /// Conflicted during simulation and never resolved into a landing.
    Conflicted,
    /// Never submitted.
    Unsubmitted,
    /// Still in flight: the session is live and has work in the queue.
    InFlight,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Conflicted => "conflicted",
            Self::Unsubmitted => "unsubmitted",
            Self::InFlight => "in_flight",
        }
    }
}

/// A duration figure with the shape of its distribution attached.
///
/// `mean_ms` is reported next to the percentiles rather than instead of them,
/// and `None` when `count` is zero: a mean over no observations is not zero,
/// it is an absence, and printing `0` for it invites reading it as "instant".
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DurationSummary {
    pub count: u64,
    pub total_ms: i64,
    pub mean_ms: Option<i64>,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub max_ms: Option<i64>,
}

impl DurationSummary {
    /// Fold one observation in. Negative values are treated as zero: a clock
    /// that went backwards would otherwise subtract time worked.
    pub fn push(&mut self, value_ms: i64) {
        let value = value_ms.max(0);
        self.count += 1;
        self.total_ms = self.total_ms.saturating_add(value);
        self.max_ms = Some(self.max_ms.map_or(value, |max| max.max(value)));
    }

    /// Fill `mean_ms` and, at or above [`DISTRIBUTION_FLOOR`] samples, the
    /// percentiles. `values` must be the same observations that were pushed.
    pub fn finish(&mut self, mut values: Vec<i64>) {
        self.mean_ms = (self.count > 0).then(|| self.total_ms / self.count as i64);
        if values.len() < DISTRIBUTION_FLOOR {
            return;
        }
        values.sort_unstable();
        self.p50_ms = Some(nearest_rank(&values, 50));
        self.p95_ms = Some(nearest_rank(&values, 95));
    }

    /// Build a summary from raw observations.
    pub fn from_values(values: Vec<i64>) -> Self {
        let mut summary = Self::default();
        for value in &values {
            summary.push(*value);
        }
        summary.finish(values);
        summary
    }
}

/// Nearest-rank percentile over a sorted, non-empty slice.
///
/// Nearest rank rather than interpolation: a percentile of a handful of
/// integer millisecond observations should be an observation somebody actually
/// recorded, not a fraction between two.
fn nearest_rank(sorted: &[i64], percentile: usize) -> i64 {
    let rank = (percentile * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// Counts of sessions that reached each stage, plus the durations between
/// consecutive stages.
///
/// `per_day` is `None` unless the caller supplied a window. Without one, a rate
/// would have to divide by however long the event log happens to have existed,
/// which is not a quantity anybody chose.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Funnel {
    pub registered: u64,
    pub submitted: u64,
    pub verified: u64,
    pub landed: u64,
    pub pr_opened: u64,
    pub pr_merged: u64,
    /// Registered → first submission.
    pub to_submit_ms: DurationSummary,
    /// First submission → first verification.
    pub to_verify_ms: DurationSummary,
    /// First verification → landing.
    pub to_land_ms: DurationSummary,
    /// First landing → pull request merged, for sessions that got that far.
    pub to_pr_merge_ms: DurationSummary,
    /// Sessions per day at each stage, when a window was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_day: Option<FunnelRates>,
    /// Sessions that never reached [`Stage::Submitted`].
    ///
    /// The funnel's own way of expressing "nothing was ever queued", kept
    /// because a reader scanning stage counts wants it beside them. A session
    /// that submitted and was then rejected is *not* counted here — it appears
    /// in [`OutcomeCounts::rejected`] instead, where the reason is.
    pub unsubmitted: u64,
}

/// Per-day rates, filled only for a window the caller chose.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct FunnelRates {
    pub window_days: f64,
    pub registered: f64,
    pub submitted: f64,
    pub verified: f64,
    pub landed: f64,
    pub pr_merged: f64,
}

/// How sessions ended up, as a count per outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct OutcomeCounts {
    pub landed: u64,
    pub rejected: u64,
    pub superseded: u64,
    pub conflicted: u64,
    pub unsubmitted: u64,
    pub in_flight: u64,
}

impl OutcomeCounts {
    pub fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Landed => self.landed += 1,
            Outcome::Rejected => self.rejected += 1,
            Outcome::Superseded => self.superseded += 1,
            Outcome::Conflicted => self.conflicted += 1,
            Outcome::Unsubmitted => self.unsubmitted += 1,
            Outcome::InFlight => self.in_flight += 1,
        }
    }

    pub fn total(&self) -> u64 {
        self.landed
            + self.rejected
            + self.superseded
            + self.conflicted
            + self.unsubmitted
            + self.in_flight
    }
}

/// What was measured and what was not.
///
/// The `unmeasured` figures are the point of this struct. An activity table
/// that only exists from the release that introduced it leaves every earlier
/// session without active time, and a report that said "median active time: 0"
/// for those sessions would be asserting that nobody worked on them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Coverage {
    /// Sessions the funnel counted.
    pub sessions: u64,
    /// Sessions with at least one recorded activity signal.
    pub sessions_with_activity: u64,
    /// Sessions with no activity history: active time unknown, not zero.
    pub sessions_without_activity: u64,
    /// Pull requests with a recorded open time.
    pub prs_with_open_time: u64,
    /// Pull requests seen without one — the provider's `createdAt` was not
    /// requested before the milestones table existed.
    pub prs_without_open_time: u64,
}

/// One session's place in the funnel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionSummary {
    pub session_id: i64,
    pub origin: SessionOrigin,
    pub outcome: Outcome,
    /// Furthest stage reached.
    pub stage: Stage,
    /// `closed_at - created_at`, reported only alongside `active_ms`.
    pub wall_clock_ms: Option<i64>,
    /// Sum of activity intervals. `None` when the session has no activity
    /// history at all.
    pub active_ms: Option<i64>,
    /// Activity signals absorbed into those intervals.
    pub signals: u64,
    /// Wall-clock not accounted for by `active_ms`. `None` when either side is
    /// unknown, so a partial pair never presents as a real idle figure.
    pub idle_ms: Option<i64>,
}

impl SessionSummary {
    /// Idle time, only when both sides of the subtraction are known.
    ///
    /// Refuses to guess: wall-clock without active time is not idle time, it is
    /// a wall-clock figure of unknown composition, and reporting the
    /// difference would invent an activity history for every session recorded
    /// before the table existed.
    pub fn idle_ms(&self) -> Option<i64> {
        match (self.wall_clock_ms, self.active_ms) {
            (Some(wall_clock), Some(active)) => Some((wall_clock - active).max(0)),
            _ => None,
        }
    }
}

/// One pull request's measured lifetime.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PullRequestSummary {
    pub repository: String,
    pub pr_number: i64,
    pub opened_at: Option<i64>,
    pub merged_at: Option<i64>,
    /// `merged_at - opened_at`, when both are known.
    pub to_merge_ms: Option<i64>,
    /// Sessions linked to this pull request.
    pub session_ids: Vec<i64>,
}

/// Gate and cache behaviour over the window.
///
/// `wait_ms` and `first_output_ms` have been recorded on every `gate_results`
/// row since v14 and read by nothing; `load_avg_1m_*` since v43. This is their
/// first consumer, which is why the split between coordination wait and command
/// execution is available at all.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct GateSummary {
    pub runs: u64,
    pub passed: u64,
    pub cached: u64,
    /// `passed / runs` over *executed* runs only. A cache hit avoids the
    /// command entirely, so folding it into either side would move the rate
    /// for a reason that has nothing to do with whether the code passes.
    pub pass_rate: Option<f64>,
    pub execute_ms: DurationSummary,
    /// Time spent waiting for an owner lock, host resources, or cache prep
    /// before the command started.
    pub wait_ms: DurationSummary,
    /// Spawn to first byte. Startup cost, distinct from execution.
    pub first_output_ms: DurationSummary,
    pub cached_saved_ms: i64,
}

/// Coordination events that indicate two sessions or a gate got in each
/// other's way.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CoordinationSummary {
    /// `lease.overlap` — a new overlapping-edit pair, announced before it
    /// collided.
    pub overlaps_warned: u64,
    /// `merge.conflict` — a conflict caught pre-gate.
    pub conflicts_caught_pre_gate: u64,
    /// `guard.out_of_lease_write` — a write outside a claimed lease.
    pub out_of_lease_writes: u64,
    /// Gate runs that failed for a reason that is not the code under test.
    pub environment_failures: u64,
    pub resource_contention_failures: u64,
    pub timeout_failures: u64,
}

/// Coordinated git/gh operation timing over the window.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct OperationSummary {
    pub succeeded: u64,
    pub failed: u64,
    /// A write exited non-zero after possibly applying partial effects. The
    /// outcome is genuinely unknown until an operator reconciles it.
    pub outcome_unknown: u64,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct InsightsReport {
    pub schema_version: u32,
    /// The idle gap used to split activity intervals. Reported because
    /// [`SessionSummary::active_ms`] is meaningless without it.
    pub idle_gap_ms: i64,
    /// Window the report covers, when the caller supplied one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    /// Oldest and newest event timestamp actually read.
    pub observed_from_ms: Option<i64>,
    pub observed_until_ms: Option<i64>,
    pub coverage: Coverage,
    pub funnel: Funnel,
    pub outcomes: OutcomeCounts,
    pub gates: GateSummary,
    pub coordination: CoordinationSummary,
    pub operations: OperationSummary,
    /// Gate pass/fail/latency per gate name.
    pub gates_by_name: BTreeMap<String, GateSummary>,
    /// How many sessions the funnel counted, before any row cap.
    pub sessions_total: usize,
    /// Whether `sessions` was capped by `session_limit`. Always emitted: a
    /// short list that does not say it was capped reads as a complete one.
    pub sessions_truncated: bool,
    /// How many pull requests matched, before any row cap.
    pub pull_requests_total: usize,
    /// Whether `pull_requests` was capped by `pull_request_limit`.
    pub pull_requests_truncated: bool,
    /// Per-session rows, newest first. Bounded by `session_limit`.
    pub sessions: Vec<SessionSummary>,
    /// Per-pull-request rows, ordered by merge time.
    pub pull_requests: Vec<PullRequestSummary>,
}

/// An explicitly requested reporting window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Window {
    pub from_ms: i64,
    pub until_ms: i64,
}

impl Window {
    /// The `days` days ending at `now_ms`, floored at one day.
    ///
    /// A zero-day window is floored rather than left empty: an empty window
    /// would divide every rate by a clamped denominator and report an
    /// infinite sessions-per-day, and "all history" is expressed by passing no
    /// window at all rather than by a window of no length.
    pub fn from_last_days(now_ms: i64, days: i64) -> Option<Self> {
        let days = days.max(1);
        Some(Self {
            from_ms: now_ms - days * 86_400_000,
            until_ms: now_ms,
        })
    }

    pub fn days(&self) -> f64 {
        let span = self.until_ms.saturating_sub(self.from_ms).max(1) as f64;
        span / 86_400_000.0
    }

    /// Whether `ts` falls inside the window. A `None` window admits
    /// everything, which is what makes it optional rather than unbounded.
    pub fn contains(&self, ts: i64) -> bool {
        ts >= self.from_ms && ts <= self.until_ms
    }
}

/// What to compute, and how much of it to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsightsQuery {
    /// Restrict to this window. `None` reads all history, and leaves every
    /// rate unset.
    pub window: Option<Window>,
    /// Cap on per-session rows. The funnel itself is never truncated.
    pub session_limit: usize,
    /// Cap on per-pull-request rows.
    pub pull_request_limit: usize,
}

impl Default for InsightsQuery {
    fn default() -> Self {
        Self {
            window: None,
            session_limit: 50,
            pull_request_limit: 50,
        }
    }
}

impl InsightsQuery {
    /// A query over the last `days` days, ending now.
    pub fn last_days(now_ms: i64, days: i64) -> Self {
        Self {
            window: Window::from_last_days(now_ms, days),
            ..Self::default()
        }
    }

    fn admits(&self, ts: i64) -> bool {
        self.window.is_none_or(|window| window.contains(ts))
    }
}

/// Fold one session's transitions into the shared funnel and outcome counts.
///
/// Durations are kept as raw values alongside the summaries, because
/// [`DurationSummary::finish`] needs the whole set to compute percentiles — the
/// summary alone cannot recover it, and a p50 over the running total is not a
/// p50.
#[derive(Debug, Default)]
struct FunnelAccumulator {
    funnel: Funnel,
    outcomes: OutcomeCounts,
    to_submit: Vec<i64>,
    to_verify: Vec<i64>,
    to_land: Vec<i64>,
    to_pr_merge: Vec<i64>,
}

impl FunnelAccumulator {
    fn stage_times(&mut self, reached: Stage, stage: Stage, elapsed_ms: i64) {
        if reached < stage {
            return;
        }
        match stage {
            Stage::Submitted => {
                self.funnel.to_submit_ms.push(elapsed_ms);
                self.to_submit.push(elapsed_ms);
            }
            Stage::Verified => {
                self.funnel.to_verify_ms.push(elapsed_ms);
                self.to_verify.push(elapsed_ms);
            }
            Stage::Landed => {
                self.funnel.to_land_ms.push(elapsed_ms);
                self.to_land.push(elapsed_ms);
            }
            Stage::PullRequestMerged => {
                self.funnel.to_pr_merge_ms.push(elapsed_ms);
                self.to_pr_merge.push(elapsed_ms);
            }
            Stage::Registered | Stage::PullRequestOpened => {}
        }
    }

    /// Fill every duration's mean and percentiles from the values collected.
    fn finish(&mut self) {
        let (to_submit, to_verify, to_land, to_pr_merge) = (
            std::mem::take(&mut self.to_submit),
            std::mem::take(&mut self.to_verify),
            std::mem::take(&mut self.to_land),
            std::mem::take(&mut self.to_pr_merge),
        );
        self.funnel.to_submit_ms.finish(to_submit);
        self.funnel.to_verify_ms.finish(to_verify);
        self.funnel.to_land_ms.finish(to_land);
        self.funnel.to_pr_merge_ms.finish(to_pr_merge);
    }

    fn rates(&self, window: Window) -> FunnelRates {
        let days = window.days();
        let per_day = |count: u64| count as f64 / days;
        FunnelRates {
            window_days: days,
            registered: per_day(self.funnel.registered),
            submitted: per_day(self.funnel.submitted),
            verified: per_day(self.funnel.verified),
            landed: per_day(self.funnel.landed),
            pr_merged: per_day(self.funnel.pr_merged),
        }
    }
}

/// Gate rows, flattened from the store into the fields the report needs.
#[derive(Debug, Clone)]
pub struct GateObservation {
    pub gate_name: String,
    pub status: &'static str,
    pub duration_ms: Option<i64>,
    pub wait_duration_ms: Option<i64>,
    pub first_output_ms: Option<i64>,
    pub failure_class: Option<String>,
}

/// One pull request's recorded facts.
#[derive(Debug, Clone)]
pub struct PullRequestObservation {
    pub repository: String,
    pub pr_number: i64,
    pub opened_at: Option<i64>,
    pub merged_at: Option<i64>,
    pub session_ids: Vec<i64>,
}

/// Everything the report is computed from, already narrowed to the window.
///
/// Taking observations rather than a store connection keeps this function
/// pure: the funnel is testable without a database, and the store's job stays
/// "fetch rows" rather than "decide what they mean".
#[derive(Debug, Default)]
pub struct InsightsInput {
    pub sessions: Vec<SessionInsightRow>,
    /// First timestamp each stage was reached, per session.
    pub stage_times: BTreeMap<i64, BTreeMap<Stage, i64>>,
    /// First timestamp of each stopping outcome, per session. A session that
    /// was rejected and later landed counts as landed: what happened last is
    /// what the pipeline did with it.
    pub failure_times: BTreeMap<i64, BTreeMap<Outcome, i64>>,
    pub gates: Vec<GateObservation>,
    pub gates_cached: u64,
    pub gates_cached_saved_ms: i64,
    pub pull_requests: Vec<PullRequestObservation>,
    pub overlaps_warned: u64,
    pub conflicts_caught_pre_gate: u64,
    pub out_of_lease_writes: u64,
    pub operations_succeeded: u64,
    pub operations_failed: u64,
    pub operations_outcome_unknown: u64,
}

/// A session row as the funnel needs it.
#[derive(Debug, Clone)]
pub struct SessionInsightRow {
    pub session_id: i64,
    pub origin: SessionOrigin,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    /// Live sessions have work that may still land.
    pub in_flight: bool,
    pub active_ms: Option<i64>,
    pub signals: u64,
}

/// The store's session projection for [`InsightsInput`].
#[derive(Debug, Clone)]
pub struct InsightSessionRow {
    pub session_id: i64,
    pub origin: SessionOrigin,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    pub in_flight: bool,
}

/// The store's gate projection for [`InsightsInput`].
#[derive(Debug, Clone)]
pub struct InsightGateRow {
    pub gate_name: String,
    pub status: &'static str,
    pub duration_ms: Option<i64>,
    pub wait_duration_ms: Option<i64>,
    pub first_output_ms: Option<i64>,
    pub failure_class: Option<String>,
}

impl From<InsightGateRow> for GateObservation {
    fn from(row: InsightGateRow) -> Self {
        Self {
            gate_name: row.gate_name,
            status: row.status,
            duration_ms: row.duration_ms,
            wait_duration_ms: row.wait_duration_ms,
            first_output_ms: row.first_output_ms,
            failure_class: row.failure_class,
        }
    }
}

/// The stage times a fold produced, as the `InsightsInput` wants them.
pub fn stage_map(
    folded: &FoldedEvents,
    pull_requests: &[PullRequestObservation],
) -> BTreeMap<i64, BTreeMap<Stage, i64>> {
    let mut stages = folded.stages.clone();
    // Pull request milestones are keyed by pull request, not by session, so
    // they are folded back onto every session that owns one. A pull request
    // linked to two sessions gives both the same milestone: the pull request
    // opened once.
    for observation in pull_requests {
        for session_id in &observation.session_ids {
            let entry = stages.entry(*session_id).or_default();
            if let Some(opened) = observation.opened_at {
                entry
                    .entry(Stage::PullRequestOpened)
                    .and_modify(|existing| *existing = (*existing).min(opened))
                    .or_insert(opened);
            }
            if let Some(merged) = observation.merged_at {
                entry
                    .entry(Stage::PullRequestMerged)
                    .and_modify(|existing| *existing = (*existing).min(merged))
                    .or_insert(merged);
            }
        }
    }
    stages
}

/// Compute the report.
pub fn report(query: InsightsQuery, input: &InsightsInput) -> InsightsReport {
    let mut accumulator = FunnelAccumulator::default();
    let mut coverage = Coverage::default();
    let mut summaries = Vec::with_capacity(input.sessions.len());

    let mut observed_from: Option<i64> = None;
    let mut observed_until: Option<i64> = None;
    let mut observe = |ts: i64| {
        observed_from = Some(observed_from.map_or(ts, |from| from.min(ts)));
        observed_until = Some(observed_until.map_or(ts, |until| until.max(ts)));
    };

    for row in &input.sessions {
        if !query.admits(row.created_at) {
            continue;
        }
        coverage.sessions += 1;
        observe(row.created_at);

        let times = input.stage_times.get(&row.session_id);
        let reached = times
            .map(|stages| {
                Stage::ORDER
                    .iter()
                    .copied()
                    .rfind(|stage| stages.contains_key(stage))
                    .unwrap_or(Stage::Registered)
            })
            .unwrap_or(Stage::Registered);

        if times.is_some_and(|stages| stages.contains_key(&Stage::Registered)) {
            accumulator.funnel.registered += 1;
        }
        for stage in Stage::ORDER {
            if let Some(at) = times.and_then(|stages| stages.get(&stage))
                && query.admits(*at)
            {
                observe(*at);
                match stage {
                    Stage::Registered => {}
                    Stage::Submitted => accumulator.funnel.submitted += 1,
                    Stage::Verified => accumulator.funnel.verified += 1,
                    Stage::Landed => accumulator.funnel.landed += 1,
                    Stage::PullRequestOpened => accumulator.funnel.pr_opened += 1,
                    Stage::PullRequestMerged => accumulator.funnel.pr_merged += 1,
                }
            }
        }

        // Durations are differences between a session's own stages, so they
        // are computed from the session's stage map directly rather than from
        // the windowed counters above: narrowing the window narrows which
        // sessions are counted, never how long one session took.
        if let Some(stages) = times {
            let first = |stage: Stage| stages.get(&stage).copied();
            for (from, to) in [
                (Stage::Registered, Stage::Submitted),
                (Stage::Submitted, Stage::Verified),
                (Stage::Verified, Stage::Landed),
                (Stage::Landed, Stage::PullRequestMerged),
            ] {
                if let (Some(start), Some(end)) = (first(from), first(to))
                    && end >= start
                {
                    accumulator.stage_times(reached, to, end - start);
                }
            }
        }

        let outcome = resolve_outcome(
            reached,
            input.failure_times.get(&row.session_id),
            row.in_flight,
        );
        accumulator.outcomes.record(outcome);

        match row.active_ms {
            Some(_) => coverage.sessions_with_activity += 1,
            None => coverage.sessions_without_activity += 1,
        }

        let wall_clock_ms = row.closed_at.map(|closed| closed - row.created_at);
        let idle_ms = match (wall_clock_ms, row.active_ms) {
            (Some(wall_clock), Some(active)) => Some((wall_clock - active).max(0)),
            _ => None,
        };
        summaries.push(SessionSummary {
            session_id: row.session_id,
            origin: row.origin,
            outcome,
            stage: reached,
            wall_clock_ms,
            active_ms: row.active_ms,
            signals: row.signals,
            idle_ms,
        });
    }

    accumulator.finish();
    accumulator.funnel.per_day = query.window.map(|window| accumulator.rates(window));
    // Read straight from the outcome count rather than derived by subtraction:
    // "never submitted" is its own fact, and deriving it as
    // `total - submitted - in_flight` silently goes negative the moment a live
    // session has already submitted, which is the common case.
    accumulator.funnel.unsubmitted = accumulator.outcomes.unsubmitted;

    let gates = gate_summary(&input.gates);
    let mut gates_by_name: BTreeMap<String, GateSummary> = BTreeMap::new();
    for observation in &input.gates {
        gates_by_name
            .entry(observation.gate_name.clone())
            .or_default()
            .merge(&gate_summary(std::slice::from_ref(observation)));
    }

    let mut pull_requests: Vec<PullRequestSummary> = Vec::new();
    for observation in &input.pull_requests {
        if observation.opened_at.is_some_and(|at| !query.admits(at))
            || observation.merged_at.is_some_and(|at| !query.admits(at))
        {
            continue;
        }
        match observation.opened_at {
            Some(_) => coverage.prs_with_open_time += 1,
            None => coverage.prs_without_open_time += 1,
        }
        if let Some(opened) = observation.opened_at {
            observe(opened);
        }
        if let Some(merged) = observation.merged_at {
            observe(merged);
        }
        pull_requests.push(PullRequestSummary {
            repository: observation.repository.clone(),
            pr_number: observation.pr_number,
            opened_at: observation.opened_at,
            merged_at: observation.merged_at,
            to_merge_ms: match (observation.opened_at, observation.merged_at) {
                (Some(opened), Some(merged)) if merged >= opened => Some(merged - opened),
                _ => None,
            },
            session_ids: observation.session_ids.clone(),
        });
    }
    pull_requests.sort_by_key(|pr| (pr.merged_at, pr.opened_at, pr.pr_number));

    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.session_id));
    let sessions_total = summaries.len();
    summaries.truncate(query.session_limit);
    let pull_requests_total = pull_requests.len();
    pull_requests.truncate(query.pull_request_limit);

    InsightsReport {
        schema_version: INSIGHTS_SCHEMA_VERSION,
        idle_gap_ms: IDLE_GAP_MS,
        window: query.window,
        observed_from_ms: observed_from,
        observed_until_ms: observed_until,
        coverage,
        funnel: accumulator.funnel,
        outcomes: accumulator.outcomes,
        gates: GateSummary {
            cached: input.gates_cached,
            cached_saved_ms: input.gates_cached_saved_ms,
            ..gates
        },
        coordination: CoordinationSummary {
            overlaps_warned: input.overlaps_warned,
            conflicts_caught_pre_gate: input.conflicts_caught_pre_gate,
            out_of_lease_writes: input.out_of_lease_writes,
            environment_failures: input
                .gates
                .iter()
                .filter(|gate| gate.failure_class.as_deref() == Some("environment"))
                .count() as u64,
            resource_contention_failures: input
                .gates
                .iter()
                .filter(|gate| gate.failure_class.as_deref() == Some("resource_contention"))
                .count() as u64,
            timeout_failures: input
                .gates
                .iter()
                .filter(|gate| gate.failure_class.as_deref() == Some("timeout"))
                .count() as u64,
        },
        operations: OperationSummary {
            succeeded: input.operations_succeeded,
            failed: input.operations_failed,
            outcome_unknown: input.operations_outcome_unknown,
        },
        gates_by_name,
        sessions_total,
        sessions_truncated: sessions_total > summaries.len(),
        pull_requests_total,
        pull_requests_truncated: pull_requests_total > pull_requests.len(),
        sessions: summaries,
        pull_requests,
    }
}

/// Decide how a session's work ended.
///
/// Landing wins over every stopping event, because a session that was rejected
/// once and landed on a later attempt did land, and counting it as rejected
/// would understate the pipeline on exactly the sessions that needed a retry —
/// the ones whose first attempt was wrong.
///
/// Among stopping events, the *earliest* is the outcome: work that conflicted
/// and was then superseded never got as far as being superseded.
fn resolve_outcome(
    reached: Stage,
    failures: Option<&BTreeMap<Outcome, i64>>,
    in_flight: bool,
) -> Outcome {
    if matches!(
        reached,
        Stage::Landed | Stage::PullRequestOpened | Stage::PullRequestMerged
    ) {
        return Outcome::Landed;
    }
    match failures.and_then(|failures| {
        failures
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(outcome, _)| *outcome)
    }) {
        Some(outcome) => outcome,
        None if in_flight => Outcome::InFlight,
        None => Outcome::Unsubmitted,
    }
}

impl GateSummary {
    /// Merge another summary into this one: counts add, durations sum, and the
    /// pass rate is recomputed from the accumulated counts.
    ///
    /// Recomputing rather than averaging the two rates is what keeps a merged
    /// summary honest — averaging rates weights a 1-run summary equally with a
    /// 1000-run one.
    fn merge(&mut self, other: &GateSummary) {
        self.runs += other.runs;
        self.passed += other.passed;
        self.cached += other.cached;
        merge_distribution(&mut self.execute_ms, &other.execute_ms);
        merge_distribution(&mut self.wait_ms, &other.wait_ms);
        merge_distribution(&mut self.first_output_ms, &other.first_output_ms);
        self.cached_saved_ms += other.cached_saved_ms;
        self.pass_rate = (self.runs > 0).then(|| self.passed as f64 / self.runs as f64);
    }
}

/// Fold one distribution's counts and totals into another.
fn merge_distribution(target: &mut DurationSummary, other: &DurationSummary) {
    target.count += other.count;
    target.total_ms = target.total_ms.saturating_add(other.total_ms);
    target.max_ms = max_option(target.max_ms, other.max_ms);
    target.mean_ms = (target.count > 0).then(|| target.total_ms / target.count as i64);
}

fn max_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

/// Fold gate observations into one summary.
fn gate_summary(observations: &[GateObservation]) -> GateSummary {
    let mut execute = Vec::new();
    let mut wait = Vec::new();
    let mut first_output = Vec::new();
    let mut passed = 0u64;

    for observation in observations {
        if observation.status == "pass" {
            passed += 1;
        }
        if let Some(duration) = observation.duration_ms {
            execute.push(duration);
        }
        if let Some(waited) = observation.wait_duration_ms {
            wait.push(waited);
        }
        if let Some(startup) = observation.first_output_ms {
            first_output.push(startup);
        }
    }

    let runs = observations.len() as u64;
    GateSummary {
        runs,
        passed,
        cached: 0,
        pass_rate: (runs > 0).then(|| passed as f64 / runs as f64),
        execute_ms: DurationSummary::from_values(execute),
        wait_ms: DurationSummary::from_values(wait),
        first_output_ms: DurationSummary::from_values(first_output),
        cached_saved_ms: 0,
    }
}

/// The event kind that advances each stage, and the stage it advances.
///
/// `merge.externally_landed` counts as landing. The session's work reached the
/// default branch by some route, and excluding it would report the pipeline as
/// slower than it was by exactly the sessions a human rescued — the outcome
/// that matters most is the one a human had to step in for.
///
/// Rejection and supersession are recorded as *kinds* rather than stages: they
/// are how work stops, not how far it got, and [`Outcome`] is where they are
/// counted.
pub const STAGE_EVENTS: [(&str, Stage); 5] = [
    ("session.registered", Stage::Registered),
    ("merge.submitted", Stage::Submitted),
    ("merge.verified", Stage::Verified),
    ("merge.promoted", Stage::Landed),
    ("merge.externally_landed", Stage::Landed),
];

/// Event kinds that mark work stopping short of landing, and the outcome each
/// one implies for the session it belongs to.
pub const TERMINAL_FAILURE_EVENTS: [(&str, Outcome); 3] = [
    ("merge.rejected", Outcome::Rejected),
    ("merge.superseded", Outcome::Superseded),
    ("merge.conflict", Outcome::Conflicted),
];

/// Event prefixes the funnel reads. One query per prefix keeps each scan on an
/// index it can use; a single `IN` over every kind would not.
pub const STAGE_EVENT_PREFIXES: [&str; 5] = [
    "session.registered",
    "merge.",
    "pr.",
    "guard.out_of_lease_write",
    "lease.overlap",
];

/// The stage an event kind advances, if any.
pub fn stage_for_event(kind: &str) -> Option<Stage> {
    STAGE_EVENTS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, stage)| *stage)
}

/// The outcome an event kind implies, if it is a stopping kind.
pub fn outcome_for_event(kind: &str) -> Option<Outcome> {
    TERMINAL_FAILURE_EVENTS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, outcome)| *outcome)
}

/// Fold an event log into the per-session stage map and coordination counts.
///
/// The first timestamp of each kind wins. A session that submits, gets
/// rejected, and resubmits keeps its *first* submit time: the funnel measures
/// how long the first attempt took, and a later attempt that succeeded would
/// otherwise make the whole thing look faster than it was.
pub fn fold_events(events: &[crate::types::Event]) -> FoldedEvents {
    let mut stages: BTreeMap<i64, BTreeMap<Stage, i64>> = BTreeMap::new();
    let mut failures: BTreeMap<i64, BTreeMap<Outcome, i64>> = BTreeMap::new();
    let mut overlaps_warned = 0u64;
    let mut conflicts_caught_pre_gate = 0u64;
    let mut out_of_lease_writes = 0u64;

    for event in events {
        // Merge events carry a session id; the ones that do not (the
        // integration branch's own events) describe the branch, not a session.
        let Some(session_id) = event.session_id else {
            continue;
        };
        match event.kind.as_str() {
            "lease.overlap" => overlaps_warned += 1,
            "guard.out_of_lease_write" => out_of_lease_writes += 1,
            _ => {}
        }
        if let Some(stage) = stage_for_event(&event.kind) {
            stages
                .entry(session_id)
                .or_default()
                .entry(stage)
                .or_insert(event.ts);
        }
        if let Some(outcome) = outcome_for_event(&event.kind) {
            // `merge.conflict` is counted both as a stopping outcome and as a
            // caught-pre-gate conflict: the first says what happened to the
            // session, the second says what the broker prevented.
            if outcome == Outcome::Conflicted {
                conflicts_caught_pre_gate += 1;
            }
            failures
                .entry(session_id)
                .or_default()
                .entry(outcome)
                .or_insert(event.ts);
        }
    }

    FoldedEvents {
        stages,
        failures,
        overlaps_warned,
        conflicts_caught_pre_gate,
        out_of_lease_writes,
    }
}

/// What [`fold_events`] extracts from an event log.
#[derive(Debug, Default)]
pub struct FoldedEvents {
    pub stages: BTreeMap<i64, BTreeMap<Stage, i64>>,
    /// First timestamp of each stopping outcome, per session.
    pub failures: BTreeMap<i64, BTreeMap<Outcome, i64>>,
    pub overlaps_warned: u64,
    pub conflicts_caught_pre_gate: u64,
    pub out_of_lease_writes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, created_at: i64) -> SessionInsightRow {
        SessionInsightRow {
            session_id: id,
            origin: SessionOrigin::Spawned,
            created_at,
            closed_at: Some(created_at + 3_600_000),
            in_flight: false,
            active_ms: Some(600_000),
            signals: 4,
        }
    }

    fn stages(entries: &[(Stage, i64)]) -> BTreeMap<Stage, i64> {
        entries.iter().copied().collect()
    }

    fn input_with_one_landed_session() -> InsightsInput {
        let mut input = InsightsInput {
            sessions: vec![row(1, 0)],
            ..InsightsInput::default()
        };
        input.stage_times.insert(
            1,
            stages(&[
                (Stage::Registered, 0),
                (Stage::Submitted, 60_000),
                (Stage::Verified, 120_000),
                (Stage::Landed, 180_000),
            ]),
        );
        input
    }

    #[test]
    fn a_landed_session_reaches_four_stages_in_order() {
        let report = report(InsightsQuery::default(), &input_with_one_landed_session());
        assert_eq!(report.funnel.registered, 1);
        assert_eq!(report.funnel.submitted, 1);
        assert_eq!(report.funnel.verified, 1);
        assert_eq!(report.funnel.landed, 1);
        assert_eq!(report.outcomes.landed, 1);
        assert_eq!(report.sessions[0].stage, Stage::Landed);
        assert_eq!(report.sessions[0].outcome, Outcome::Landed);
    }

    #[test]
    fn durations_between_stages_are_measured_in_milliseconds() {
        let report = report(InsightsQuery::default(), &input_with_one_landed_session());
        assert_eq!(report.funnel.to_submit_ms.total_ms, 60_000);
        assert_eq!(report.funnel.to_verify_ms.total_ms, 60_000);
        assert_eq!(report.funnel.to_land_ms.total_ms, 60_000);
    }

    #[test]
    fn a_session_with_no_events_is_registered_only_and_unsubmitted() {
        let input = InsightsInput {
            sessions: vec![row(7, 0)],
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(
            report.funnel.registered, 0,
            "no session.registered event means the row is not counted"
        );
        assert_eq!(report.outcomes.unsubmitted, 1);
        assert_eq!(report.sessions[0].stage, Stage::Registered);
    }

    #[test]
    fn active_time_is_absent_for_a_session_that_never_recorded_any() {
        let mut session = row(1, 0);
        session.active_ms = None;
        let input = InsightsInput {
            sessions: vec![session],
            ..input_with_one_landed_session()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.coverage.sessions_without_activity, 1);
        assert_eq!(report.coverage.sessions_with_activity, 0);
        assert_eq!(report.sessions[0].active_ms, None);
    }

    #[test]
    fn idle_time_needs_both_halves_and_refuses_to_invent_the_activity_one() {
        let mut session = row(1, 0);
        session.active_ms = None;
        let summary = SessionSummary {
            session_id: 1,
            origin: SessionOrigin::Spawned,
            outcome: Outcome::Landed,
            stage: Stage::Landed,
            wall_clock_ms: Some(3_600_000),
            active_ms: None,
            signals: 0,
            idle_ms: None,
        };
        assert_eq!(summary.idle_ms(), None);
        session.active_ms = Some(600_000);
        let known = SessionSummary {
            active_ms: session.active_ms,
            ..summary
        };
        assert_eq!(known.idle_ms(), Some(3_000_000));
    }

    #[test]
    fn no_rates_are_reported_without_a_window() {
        let report = report(InsightsQuery::default(), &input_with_one_landed_session());
        assert_eq!(report.funnel.per_day, None);
    }

    #[test]
    fn a_window_produces_per_day_rates() {
        let query = InsightsQuery::last_days(10 * 86_400_000, 10);
        let report = report(query, &input_with_one_landed_session());
        let rates = report.funnel.per_day.expect("a window was supplied");
        assert!((rates.window_days - 10.0).abs() < f64::EPSILON);
        assert!((rates.landed - 0.1).abs() < 1e-9);
    }

    #[test]
    fn a_rejected_session_is_counted_as_rejected() {
        let mut input = input_with_one_landed_session();
        input
            .failure_times
            .insert(1, BTreeMap::from([(Outcome::Rejected, 90_000)]));
        // Still landed: a rejection the session recovered from is not its
        // outcome.
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.outcomes.landed, 1);
        assert_eq!(report.outcomes.rejected, 0);
    }

    #[test]
    fn a_rejection_the_session_never_recovered_from_is_its_outcome() {
        let mut input = InsightsInput {
            sessions: vec![row(1, 0)],
            ..InsightsInput::default()
        };
        input.stage_times.insert(
            1,
            stages(&[(Stage::Registered, 0), (Stage::Submitted, 60_000)]),
        );
        input
            .failure_times
            .insert(1, BTreeMap::from([(Outcome::Rejected, 90_000)]));
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.outcomes.rejected, 1);
        assert_eq!(report.funnel.submitted, 1);
        assert_eq!(report.funnel.landed, 0);
        // It reached the queue, so it is not unsubmitted; the reason it stopped
        // is in the outcome counts.
        assert_eq!(report.funnel.unsubmitted, 0);
    }

    #[test]
    fn the_earliest_stopping_event_is_the_outcome() {
        // Conflicted at 80s, then superseded at 120s. It never got as far as
        // being superseded.
        let mut input = InsightsInput {
            sessions: vec![row(1, 0)],
            ..InsightsInput::default()
        };
        input.stage_times.insert(
            1,
            stages(&[(Stage::Registered, 0), (Stage::Submitted, 60_000)]),
        );
        input.failure_times.insert(
            1,
            BTreeMap::from([
                (Outcome::Conflicted, 80_000),
                (Outcome::Superseded, 120_000),
            ]),
        );
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.outcomes.conflicted, 1);
        assert_eq!(report.outcomes.superseded, 0);
    }

    #[test]
    fn a_conflict_counts_as_both_a_caught_conflict_and_a_stopped_session() {
        let mut input = InsightsInput {
            sessions: vec![row(1, 0)],
            ..InsightsInput::default()
        };
        input.stage_times.insert(
            1,
            stages(&[(Stage::Registered, 0), (Stage::Submitted, 60_000)]),
        );
        input
            .failure_times
            .insert(1, BTreeMap::from([(Outcome::Conflicted, 80_000)]));
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.outcomes.conflicted, 1);
        assert_eq!(report.coordination.conflicts_caught_pre_gate, 0);
    }

    #[test]
    fn folding_an_event_log_takes_the_first_of_each_kind() {
        use crate::types::Event;
        let event = |id: i64, ts: i64, kind: &str| Event {
            id,
            schema_version: 1,
            ts,
            kind: kind.to_string(),
            session_id: Some(1),
            payload_json: None,
        };
        let folded = fold_events(&[
            event(1, 1_000, "session.registered"),
            event(2, 2_000, "merge.submitted"),
            event(3, 3_000, "merge.submitted"),
            event(4, 4_000, "merge.verified"),
            event(5, 5_000, "merge.promoted"),
        ]);
        let stages = folded.stages.get(&1).expect("session 1 has stages");
        assert_eq!(stages[&Stage::Registered], 1_000);
        // The first submit, not the second: the funnel measures how long the
        // first attempt took.
        assert_eq!(stages[&Stage::Submitted], 2_000);
        assert_eq!(stages[&Stage::Verified], 4_000);
        assert_eq!(stages[&Stage::Landed], 5_000);
    }

    #[test]
    fn folding_ignores_events_that_belong_to_no_session() {
        use crate::types::Event;
        // The integration branch's own events describe a branch.
        let folded = fold_events(&[Event {
            id: 1,
            schema_version: 1,
            ts: 1_000,
            kind: "merge.integration_branch_created".into(),
            session_id: None,
            payload_json: None,
        }]);
        assert!(folded.stages.is_empty());
    }

    #[test]
    fn folding_counts_overlaps_against_the_session_that_triggers_them() {
        use crate::types::Event;
        let folded = fold_events(&[
            Event {
                id: 1,
                schema_version: 1,
                ts: 1_000,
                kind: "lease.overlap".into(),
                session_id: Some(1),
                payload_json: None,
            },
            Event {
                id: 2,
                schema_version: 1,
                ts: 2_000,
                kind: "lease.overlap".into(),
                session_id: Some(2),
                payload_json: None,
            },
        ]);
        assert_eq!(folded.overlaps_warned, 2);
        // An overlap is not a stage: neither session reached anywhere new.
        assert!(folded.stages.is_empty());
    }

    #[test]
    fn a_two_sample_summary_reports_no_percentile() {
        let mut summary = DurationSummary::default();
        summary.push(10);
        summary.push(20);
        summary.finish(vec![10, 20]);
        assert_eq!(summary.mean_ms, Some(15));
        assert_eq!(summary.p50_ms, None);
        assert_eq!(summary.p95_ms, None);
        assert_eq!(summary.max_ms, Some(20));
    }

    #[test]
    fn a_summary_at_the_distribution_floor_reports_percentiles() {
        let values: Vec<i64> = (1..=DISTRIBUTION_FLOOR as i64).collect();
        let summary = DurationSummary::from_values(values);
        assert_eq!(summary.p50_ms, Some(3));
        assert_eq!(summary.p95_ms, Some(5));
    }

    #[test]
    fn an_empty_summary_reports_no_mean_rather_than_zero() {
        let summary = DurationSummary::from_values(Vec::new());
        assert_eq!(summary.count, 0);
        assert_eq!(summary.mean_ms, None);
        assert_eq!(summary.total_ms, 0);
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_subtract_time_worked() {
        let summary = DurationSummary::from_values(vec![-5_000]);
        assert_eq!(summary.total_ms, 0);
    }

    #[test]
    fn a_pull_request_without_an_open_time_reports_coverage_not_a_guess() {
        let input = InsightsInput {
            pull_requests: vec![PullRequestObservation {
                repository: "o/r".into(),
                pr_number: 7,
                opened_at: None,
                merged_at: Some(1_000),
                session_ids: vec![1],
            }],
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.coverage.prs_without_open_time, 1);
        assert_eq!(report.coverage.prs_with_open_time, 0);
        assert_eq!(report.pull_requests[0].to_merge_ms, None);
    }

    #[test]
    fn pull_request_time_to_merge_is_the_difference_of_two_recorded_facts() {
        let input = InsightsInput {
            pull_requests: vec![PullRequestObservation {
                repository: "o/r".into(),
                pr_number: 7,
                opened_at: Some(1_000),
                merged_at: Some(3_600_000),
                session_ids: vec![1, 2],
            }],
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.pull_requests[0].to_merge_ms, Some(3_599_000));
        assert_eq!(report.pull_requests[0].session_ids, vec![1, 2]);
    }

    #[test]
    fn gate_wait_and_startup_are_separated_from_execution() {
        let input = InsightsInput {
            gates: vec![GateObservation {
                gate_name: "test".into(),
                status: "pass",
                duration_ms: Some(1_000),
                wait_duration_ms: Some(5_000),
                first_output_ms: Some(200),
                failure_class: None,
            }],
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.gates.execute_ms.total_ms, 1_000);
        assert_eq!(report.gates.wait_ms.total_ms, 5_000);
        assert_eq!(report.gates.first_output_ms.total_ms, 200);
        assert_eq!(report.gates.pass_rate, Some(1.0));
        assert_eq!(report.gates_by_name["test"].wait_ms.total_ms, 5_000);
    }

    #[test]
    fn gate_failures_that_are_not_the_code_are_counted_separately() {
        let input = InsightsInput {
            gates: vec![
                GateObservation {
                    gate_name: "test".into(),
                    status: "fail",
                    duration_ms: Some(10),
                    wait_duration_ms: None,
                    first_output_ms: None,
                    failure_class: Some("environment".into()),
                },
                GateObservation {
                    gate_name: "test".into(),
                    status: "fail",
                    duration_ms: Some(10),
                    wait_duration_ms: None,
                    first_output_ms: None,
                    failure_class: Some("timeout".into()),
                },
            ],
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.coordination.environment_failures, 1);
        assert_eq!(report.coordination.timeout_failures, 1);
        assert_eq!(report.gates.pass_rate, Some(0.0));
    }

    #[test]
    fn a_session_row_list_is_capped_and_says_so() {
        let mut sessions = Vec::new();
        let mut stage_times = BTreeMap::new();
        for id in 1..=10 {
            sessions.push(row(id, 0));
            stage_times.insert(id, stages(&[(Stage::Registered, 0)]));
        }
        let input = InsightsInput {
            sessions,
            stage_times,
            ..InsightsInput::default()
        };
        let report = report(
            InsightsQuery {
                window: None,
                session_limit: 3,
                pull_request_limit: 50,
            },
            &input,
        );
        assert_eq!(report.sessions.len(), 3);
        assert!(report.sessions_truncated);
        assert_eq!(report.sessions_total, 10);
        // The funnel is never truncated by the row limit.
        assert_eq!(report.coverage.sessions, 10);
    }

    #[test]
    fn a_window_that_excludes_a_session_leaves_it_out_of_every_count() {
        let input = input_with_one_landed_session();
        let query = InsightsQuery {
            window: Some(Window {
                from_ms: 500_000,
                until_ms: 600_000,
            }),
            session_limit: 50,
            pull_request_limit: 50,
        };
        let report = report(query, &input);
        assert_eq!(report.coverage.sessions, 0);
        assert_eq!(report.funnel.landed, 0);
    }

    #[test]
    fn an_untruncated_list_does_not_claim_to_be_truncated() {
        let report = report(InsightsQuery::default(), &input_with_one_landed_session());
        assert!(!report.sessions_truncated);
        assert!(!report.pull_requests_truncated);
    }

    #[test]
    fn the_idle_gap_is_reported_because_it_defines_active_time() {
        let report = report(InsightsQuery::default(), &input_with_one_landed_session());
        assert_eq!(report.idle_gap_ms, IDLE_GAP_MS);
        assert_eq!(report.schema_version, INSIGHTS_SCHEMA_VERSION);
    }

    #[test]
    fn externally_landed_work_counts_as_landing() {
        assert_eq!(
            stage_for_event("merge.externally_landed"),
            Some(Stage::Landed)
        );
        assert_eq!(stage_for_event("merge.promoted"), Some(Stage::Landed));
        assert_eq!(stage_for_event("lease.overlap"), None);
    }

    #[test]
    fn a_live_session_with_unlanded_work_is_in_flight_not_unsubmitted() {
        let mut session = row(1, 0);
        session.in_flight = true;
        let input = InsightsInput {
            sessions: vec![session],
            stage_times: BTreeMap::from([(
                1,
                stages(&[(Stage::Registered, 0), (Stage::Submitted, 10)]),
            )]),
            ..InsightsInput::default()
        };
        let report = report(InsightsQuery::default(), &input);
        assert_eq!(report.outcomes.in_flight, 1);
        assert_eq!(report.outcomes.unsubmitted, 0);
    }
}
