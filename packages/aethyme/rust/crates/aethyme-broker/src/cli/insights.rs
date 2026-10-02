//! `broker advanced insights`: how long work takes and whether it lands.
//!
//! A read-only view over the broker's own history, assembled by
//! [`crate::insights`]. Distinct from `broker advanced metrics`, which
//! accounts for what the broker *cost* (gate executions against cache hits);
//! this one reports what the pipeline *did*.
//!
//! Report-only, like `metrics`: reading this writes nothing, which is what
//! makes two snapshots comparable and lets the command be run from cron or a
//! dashboard without perturbing what it measures.
//!
//! The rendering rule that governs both forms: a figure the broker has not
//! measured prints as absent, never as zero, and a number that depends on a
//! threshold carries that threshold. Active time means nothing without the idle
//! gap that split the intervals, so the gap is on the first line of the text
//! output and a field in the JSON.

use crate::cli::{Parsed, UsageError};
use crate::cli_output::out;
use crate::insights::{
    self, DurationSummary, InsightsInput, InsightsQuery, InsightsReport, PullRequestObservation,
    SessionInsightRow,
};

/// Largest `--session-limit` a caller may ask for.
///
/// The funnel is computed over every session in the window regardless; only the
/// per-session row list is bounded, and a bound this large keeps one invocation
/// from printing a repository's entire history.
const MAX_SESSION_ROWS: usize = 500;

/// Same ceiling for pull request rows.
const MAX_PULL_REQUEST_ROWS: usize = 500;

/// Default window when `--days` is absent.
///
/// Thirty days is long enough for a rate to mean something and short enough
/// that the retention window for gate rows (also 30 days by default) has not
/// quietly truncated the gates half of the report. A caller who wants all
/// history asks for it explicitly with `--days 0`.
const DEFAULT_WINDOW_DAYS: i64 = 30;

/// `broker advanced insights`.
pub(super) fn run_insights(parsed: Parsed) -> Result<(), UsageError> {
    let days = parsed.days.unwrap_or(DEFAULT_WINDOW_DAYS);
    if days < 0 {
        // Without this, a negative window fails the `days > 0` filter below and
        // silently reports all history — a typo answered as `--days 0`.
        return Err(UsageError::Message(
            "--days must not be negative (0 means all history)".into(),
        ));
    }
    let session_limit = bounded(
        "--session-limit",
        parsed.session_limit,
        50,
        MAX_SESSION_ROWS,
    )?;
    let pull_request_limit = bounded(
        "--pull-request-limit",
        parsed.pull_request_limit,
        50,
        MAX_PULL_REQUEST_ROWS,
    )?;

    let report = assemble(days, session_limit, pull_request_limit)?;

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    out!("{}", render(&report).trim_end());
    Ok(())
}

fn bounded(
    flag: &str,
    value: Option<i64>,
    default: usize,
    max: usize,
) -> Result<usize, UsageError> {
    match value {
        None => Ok(default),
        Some(raw) if raw < 0 => Err(UsageError::Message(format!("{flag} must not be negative"))),
        Some(raw) => Ok((raw as usize).min(max)),
    }
}

/// Read the store and compute the report.
fn assemble(
    days: i64,
    session_limit: usize,
    pull_request_limit: usize,
) -> Result<InsightsReport, UsageError> {
    let mut broker = open_read_only()?;
    let store = broker.store();
    let now = crate::clock::epoch_ms();

    // `--days 0` means all recorded history, expressed as no window at all —
    // which also leaves every rate unset, because a rate over an unbounded log
    // would divide by however long the database has existed.
    let query = InsightsQuery {
        window: insights::Window::from_last_days(now, days).filter(|_| days > 0),
        session_limit,
        pull_request_limit,
    };

    // Every section covers the same period. The funnel filters sessions by
    // their creation; the gate, cache, coordination and operation figures are
    // counts of things that happened, so they take the window's start directly.
    // Without this, the default 30-day funnel sat beside lifetime gate and
    // coordination figures with nothing saying so.
    let since_ms = query.window.map_or(0, |window| window.from_ms);

    let mut events = Vec::new();
    for prefix in insights::STAGE_EVENT_PREFIXES {
        events.extend(store.events_after_filtered(0, i64::MAX, Some(prefix))?);
    }
    events.retain(|event| event.ts >= since_ms);
    // Merge events arrive out of order across prefixes; the fold takes the
    // first of each kind by timestamp, so order does not change the answer.
    events.sort_by_key(|event| event.id);
    let folded = insights::fold_events(&events);

    let pull_requests = load_pull_requests(store)?;
    let stage_times = insights::stage_map(&folded, &pull_requests);
    let activity = store.session_activity_totals()?;

    let sessions: Vec<SessionInsightRow> = store
        .insight_session_rows()?
        .into_iter()
        .map(|row| {
            let (active_ms, signals) = activity
                .get(&row.session_id)
                .copied()
                .map_or((None, 0), |(ms, signals)| (Some(ms), signals));
            SessionInsightRow {
                session_id: row.session_id,
                origin: row.origin,
                created_at: row.created_at,
                closed_at: row.closed_at,
                in_flight: row.in_flight,
                active_ms,
                signals,
            }
        })
        .collect();

    // Cache hits are not funnel events, so they are not in `events`: read them
    // on their own. Counting them from the funnel's events would report zero
    // cache hits for every repository.
    let mut cache_hits =
        store.events_after_filtered(0, i64::MAX, Some(crate::events::GATE_CACHED))?;
    cache_hits.retain(|event| event.ts >= since_ms);
    let (cached, cached_saved_ms) = cached_gate_totals(&cache_hits);

    let (succeeded, failed, outcome_unknown) = store.insight_operation_counts(since_ms)?;

    let input = InsightsInput {
        sessions,
        stage_times,
        failure_times: folded.failures,
        gates: store
            .insight_gate_rows(since_ms)?
            .into_iter()
            .map(Into::into)
            .collect(),
        gates_cached: cached,
        gates_cached_saved_ms: cached_saved_ms,
        pull_requests,
        overlaps_warned: folded.overlaps_warned,
        conflicts_caught_pre_gate: folded.conflicts_caught_pre_gate,
        out_of_lease_writes: folded.out_of_lease_writes,
        operations_succeeded: succeeded,
        operations_failed: failed,
        operations_outcome_unknown: outcome_unknown,
    };

    Ok(insights::report(query, &input))
}

fn open_read_only() -> Result<crate::Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    Ok(crate::Broker::open_snapshot(&cwd)?)
}

fn load_pull_requests(
    store: &crate::BrokerStore,
) -> Result<Vec<PullRequestObservation>, UsageError> {
    let mut pull_requests = Vec::new();
    for (repository, pr_number, opened_at, merged_at) in store.pull_request_milestones()? {
        pull_requests.push(PullRequestObservation {
            session_ids: store.pull_request_sessions(&repository, pr_number)?,
            repository,
            pr_number,
            opened_at,
            merged_at,
        });
    }
    Ok(pull_requests)
}

/// `gate.cached` carries the avoided run's duration in `saved_ms`.
fn cached_gate_totals(events: &[crate::types::Event]) -> (u64, i64) {
    let mut count = 0u64;
    let mut saved_ms = 0i64;
    for event in events
        .iter()
        .filter(|e| e.kind == crate::events::GATE_CACHED)
    {
        count += 1;
        saved_ms = saved_ms.saturating_add(
            event
                .payload_json
                .as_deref()
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
                .and_then(|value| value.get("saved_ms").and_then(|ms| ms.as_i64()))
                .unwrap_or(0),
        );
    }
    (count, saved_ms)
}

/// Render the text form.
fn render(report: &InsightsReport) -> String {
    let mut out = String::new();
    out.push_str("Aethyme insights\n\n");
    // First, because every duration below is measured against it.
    out.push_str(&format!(
        "Active time counts gaps under {} of silence as working and splits above it.\n\n",
        minutes(report.idle_gap_ms)
    ));

    if let Some(window) = report.window {
        out.push_str(&format!(
            "Window: {} to {} ({:.1} days)\n",
            timestamp(window.from_ms),
            timestamp(window.until_ms),
            window_days(report)
        ));
    } else {
        out.push_str("Window: all recorded history (no per-day rates: there is no denominator)\n");
    }
    out.push_str(&format!(
        "Sessions counted: {}\n\n",
        report.coverage.sessions
    ));

    out.push_str("Funnel\n");
    let funnel = &report.funnel;
    out.push_str(&format!("  registered    {}\n", funnel.registered));
    out.push_str(&format!("  submitted     {}\n", funnel.submitted));
    out.push_str(&format!("  verified      {}\n", funnel.verified));
    out.push_str(&format!("  landed        {}\n", funnel.landed));
    out.push_str(&format!("  pr opened     {}\n", funnel.pr_opened));
    out.push_str(&format!("  pr merged     {}\n", funnel.pr_merged));
    if let Some(rates) = &funnel.per_day {
        out.push_str(&format!(
            "  per day: registered {:.2}, landed {:.2}\n",
            rates.registered, rates.landed
        ));
    }

    out.push_str("\nTime to stage\n");
    out.push_str(&format!(
        "  register -> submit  {}\n",
        duration(&funnel.to_submit_ms)
    ));
    out.push_str(&format!(
        "  submit -> verify    {}\n",
        duration(&funnel.to_verify_ms)
    ));
    out.push_str(&format!(
        "  verify -> landed    {}\n",
        duration(&funnel.to_land_ms)
    ));
    out.push_str(&format!(
        "  landed -> pr merge  {}\n",
        duration(&funnel.to_pr_merge_ms)
    ));

    out.push_str("\nOutcomes\n");
    let outcomes = &report.outcomes;
    out.push_str(&format!("  landed {}\n", outcomes.landed));
    out.push_str(&format!("  rejected {}\n", outcomes.rejected));
    out.push_str(&format!("  superseded {}\n", outcomes.superseded));
    out.push_str(&format!("  conflicted {}\n", outcomes.conflicted));
    out.push_str(&format!("  unsubmitted {}\n", outcomes.unsubmitted));
    out.push_str(&format!("  in flight {}\n", outcomes.in_flight));

    out.push_str("\nCoverage\n");
    let coverage = &report.coverage;
    out.push_str(&format!(
        "  sessions with recorded activity   {} of {}\n",
        coverage.sessions_with_activity, coverage.sessions
    ));
    if coverage.sessions_without_activity > 0 {
        // Stated as what it is: the table did not exist yet, so those sessions
        // have no active time rather than zero of it.
        out.push_str(&format!(
            "  sessions without active time      {} (recorded before activity tracking; unknown, not zero)\n",
            coverage.sessions_without_activity
        ));
    }
    out.push_str(&format!(
        "  pull requests with open time      {}\n",
        coverage.prs_with_open_time
    ));
    if coverage.prs_without_open_time > 0 {
        out.push_str(&format!(
            "  pull requests without open time   {} (open time unrecorded; time-to-merge unknown)\n",
            coverage.prs_without_open_time
        ));
    }

    out.push_str("\nGates\n");
    let gates = &report.gates;
    out.push_str(&format!(
        "  runs {} (cached {}), saved {}\n",
        gates.runs,
        gates.cached,
        duration_ms(gates.cached_saved_ms)
    ));
    out.push_str(&format!("  pass rate {}\n", rate(gates.pass_rate)));
    out.push_str(&format!("  execute  {}\n", duration(&gates.execute_ms)));
    out.push_str(&format!(
        "  wait     {} (lock and resource acquisition, not the command)\n",
        duration(&gates.wait_ms)
    ));
    out.push_str(&format!(
        "  startup  {} (spawn to first byte)\n",
        duration(&gates.first_output_ms)
    ));

    out.push_str("\nCoordination\n");
    let coordination = &report.coordination;
    out.push_str(&format!(
        "  overlaps warned        {}\n",
        coordination.overlaps_warned
    ));
    out.push_str(&format!(
        "  conflicts caught       {} (before gates ran)\n",
        coordination.conflicts_caught_pre_gate
    ));
    out.push_str(&format!(
        "  out-of-lease writes    {}\n",
        coordination.out_of_lease_writes
    ));
    out.push_str(&format!(
        "  gate failures that are not the code: environment {}, contention {}, timeout {}\n",
        coordination.environment_failures,
        coordination.resource_contention_failures,
        coordination.timeout_failures
    ));

    if !report.gates_by_name.is_empty() {
        out.push_str("\nGates by name\n");
        for (name, summary) in &report.gates_by_name {
            out.push_str(&format!(
                "  {:<24} runs {:>5}  pass {}  execute {}\n",
                name,
                summary.runs,
                rate(summary.pass_rate),
                duration(&summary.execute_ms)
            ));
        }
    }

    let operations = &report.operations;
    out.push_str("\nCoordinated operations\n");
    out.push_str(&format!(
        "  succeeded {}, failed {}, outcome unknown {}\n",
        operations.succeeded, operations.failed, operations.outcome_unknown
    ));
    if operations.outcome_unknown > 0 {
        out.push_str(
            "  An unknown outcome means a write may have applied. Reconcile with\n  \
             `aethyme broker advanced operations reconcile` before trusting the counts.\n",
        );
    }

    if !report.pull_requests.is_empty() {
        out.push_str("\nPull requests\n");
        for pull_request in &report.pull_requests {
            out.push_str(&format!(
                "  {}#{}  opened {}  merged {}  to merge {}\n",
                pull_request.repository,
                pull_request.pr_number,
                optional_timestamp(pull_request.opened_at),
                optional_timestamp(pull_request.merged_at),
                optional_duration(pull_request.to_merge_ms),
            ));
        }
        if report.pull_requests_truncated {
            out.push_str(&format!(
                "  ... {} of {} shown; raise --pull-request-limit for the rest\n",
                report.pull_requests.len(),
                report.pull_requests_total
            ));
        }
    }

    if !report.sessions.is_empty() {
        out.push_str("\nRecent sessions\n");
        for session in &report.sessions {
            out.push_str(&format!(
                "  #{:<6} {:<12} {:<9} wall {:>10}  active {:>10}  signals {}\n",
                session.session_id,
                session.origin.as_str(),
                session.outcome.as_str(),
                optional_duration(session.wall_clock_ms),
                optional_duration(session.active_ms),
                session.signals,
            ));
        }
        if report.sessions_truncated {
            out.push_str(&format!(
                "  ... {} of {} shown; raise --session-limit for the rest\n",
                report.sessions.len(),
                report.sessions_total
            ));
        }
    }

    out.push_str(
        "\nThese figures describe the broker and the pipeline. They are not a\n\
         measure of any person: a session's wall-clock includes every pause in it.\n",
    );
    out
}

fn window_days(report: &InsightsReport) -> f64 {
    report.window.map_or(0.0, |window| window.days())
}

/// A duration, with the distribution's shape rather than just its mean.
fn duration(summary: &DurationSummary) -> String {
    if summary.count == 0 {
        return "unmeasured".to_string();
    }
    let mut text = format!(
        "p50 {}  mean {}  p95 {}",
        optional_duration(summary.p50_ms),
        duration_ms(summary.mean_ms.unwrap_or(0)),
        optional_duration(summary.p95_ms),
    );
    if (summary.count as usize) < insights::DISTRIBUTION_FLOOR {
        text.push_str(&format!(
            "  ({} samples, too few for a percentile)",
            summary.count
        ));
    }
    text
}

fn duration_ms(ms: i64) -> String {
    if ms == 0 {
        return "0s".to_string();
    }
    let seconds = ms / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m{:02}s", seconds % 60);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h{:02}m", minutes % 60);
    }
    format!("{}d{:02}h", hours / 24, hours % 24)
}

fn minutes(ms: i64) -> String {
    if ms % 60_000 == 0 {
        format!("{}m", ms / 60_000)
    } else {
        duration_ms(ms)
    }
}

/// A single duration, or an explicit absence. Never `0`, because a missing
/// measurement and an instantaneous one are different facts.
fn optional_duration(ms: Option<i64>) -> String {
    ms.map_or("unknown".to_string(), duration_ms)
}

fn optional_timestamp(ms: Option<i64>) -> String {
    ms.map_or("unknown".to_string(), timestamp)
}

fn rate(value: Option<f64>) -> String {
    value.map_or_else(
        || "unmeasured".to_string(),
        |value| format!("{:.1}%", value * 100.0),
    )
}

/// Unix milliseconds as an ISO-8601 UTC instant.
///
/// Civil-from-days rather than a date library: the broker already depends on
/// exactly one calendar conversion elsewhere and a report nobody parses is not
/// a reason to add a dependency. Valid for every instant this can see, since
/// they come from `now_ms()`.
fn timestamp(ms: i64) -> String {
    let seconds = ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
///
/// Howard Hinnant's `civil_from_days`: the era is a 400-year cycle, the year
/// within it shifts by the accumulated leap days, and the day of the year is
/// converted with a March-based formula that avoids a separate leap adjustment.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capped_limit_is_a_ceiling_rather_than_an_error() {
        // A caller asking for more rows than the ceiling gets the ceiling, not
        // a refusal: the funnel is computed over every session either way, so
        // the limit only ever decides how much detail is printed.
        assert_eq!(bounded("--session-limit", None, 50, 500).ok(), Some(50));
        assert_eq!(bounded("--session-limit", Some(10), 50, 500).ok(), Some(10));
        assert_eq!(
            bounded("--session-limit", Some(10_000), 50, 500).ok(),
            Some(500)
        );
    }

    #[test]
    fn a_negative_limit_is_rejected_rather_than_silently_becoming_zero() {
        assert!(bounded("--session-limit", Some(-1), 50, 500).is_err());
    }

    #[test]
    fn an_unmeasured_duration_says_so_instead_of_printing_zero() {
        assert_eq!(duration(&DurationSummary::default()), "unmeasured");
    }

    #[test]
    fn a_duration_below_the_distribution_floor_says_how_many_samples_it_has() {
        let summary = DurationSummary::from_values(vec![1_000, 2_000]);
        let text = duration(&summary);
        assert!(text.contains("p50 unknown"), "{text}");
        assert!(text.contains("2 samples"), "{text}");
    }

    #[test]
    fn durations_render_at_the_scale_a_reader_expects() {
        assert_eq!(duration_ms(0), "0s");
        assert_eq!(duration_ms(45_000), "45s");
        assert_eq!(duration_ms(90_000), "1m30s");
        assert_eq!(duration_ms(7_200_000), "2h00m");
        assert_eq!(duration_ms(180_000_000), "2d02h");
    }

    #[test]
    fn the_idle_gap_appears_in_the_human_output() {
        // Without it, every active-time figure below is unreadable.
        let report = InsightsReport {
            schema_version: insights::INSIGHTS_SCHEMA_VERSION,
            idle_gap_ms: insights::IDLE_GAP_MS,
            window: None,
            observed_from_ms: None,
            observed_until_ms: None,
            coverage: Default::default(),
            funnel: Default::default(),
            outcomes: Default::default(),
            gates: Default::default(),
            coordination: Default::default(),
            operations: Default::default(),
            gates_by_name: Default::default(),
            sessions_total: 0,
            sessions_truncated: false,
            pull_requests_total: 0,
            pull_requests_truncated: false,
            sessions: Vec::new(),
            pull_requests: Vec::new(),
        };
        let text = render(&report);
        assert!(text.contains("15m"), "{text}");
    }

    #[test]
    fn a_report_with_no_window_explains_why_it_has_no_rates() {
        let report = InsightsReport {
            schema_version: insights::INSIGHTS_SCHEMA_VERSION,
            idle_gap_ms: insights::IDLE_GAP_MS,
            window: None,
            observed_from_ms: None,
            observed_until_ms: None,
            coverage: Default::default(),
            funnel: Default::default(),
            outcomes: Default::default(),
            gates: Default::default(),
            coordination: Default::default(),
            operations: Default::default(),
            gates_by_name: Default::default(),
            sessions_total: 0,
            sessions_truncated: false,
            pull_requests_total: 0,
            pull_requests_truncated: false,
            sessions: Vec::new(),
            pull_requests: Vec::new(),
        };
        assert!(render(&report).contains("no per-day rates"));
    }

    #[test]
    fn cached_gate_totals_sum_the_avoided_runs() {
        use crate::types::Event;
        let event = |kind: &str, payload: &str| Event {
            id: 1,
            schema_version: 1,
            ts: 0,
            kind: kind.into(),
            session_id: None,
            payload_json: Some(payload.into()),
        };
        let (count, saved) = cached_gate_totals(&[
            event("gate.cached", r#"{"saved_ms":1200}"#),
            event("gate.cached", r#"{"saved_ms":800}"#),
            event("gate.cached", "not json"),
            event("gate.pass", r#"{"saved_ms":9999}"#),
        ]);
        assert_eq!(count, 3);
        // The unparseable line counts as a hit with nothing known to have been
        // saved; the gate.pass is not a cache hit at all.
        assert_eq!(saved, 2_000);
    }
}
