//! Declared budgets and their trends (#1239).
//!
//! `[budgets.seconds]` and `[budgets.memory_mib]` in `environments.toml`
//! declare what a repository promises: how long a warm `up` takes, how much
//! memory a profile needs. Aethyme times every lifecycle step it drives
//! (`up.<profile>` for a warm up, `up.cold` for one that provisioned, `park`,
//! `down`, and `provision|start|health.<component>`), records any duration a
//! hook reports in its receipt's `metrics` object, and keeps a bounded window
//! per metric in `environments.db` (failures apart, so a crash loop never
//! reads as fast). Memory comes from the supervisor's footprint samples
//! (#1218), as `memory.<profile>`.
//!
//! `env budgets` reports, per metric, the count, p50, p95, latest value, the
//! trend between the older and newer halves of the window, and a status:
//! `ok` (p95 within budget), `watch` (only p50 within), `over` (p50 over),
//! `no-data`, or `no-budget` for a measured metric nothing constrains.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::spec::BudgetsSpec;
use super::store::Timing;

/// Fewest values a trend is computed from: two per half.
pub const MIN_TREND_VALUES: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricReport {
    pub metric: String,
    /// `seconds` or `memory_mib`.
    pub unit: &'static str,
    pub budget: Option<u64>,
    pub count: usize,
    pub failures: usize,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub latest: Option<f64>,
    pub trend: Option<Trend>,
    pub status: &'static str,
}

/// The newer half of the window against the older half.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trend {
    pub older_p50: f64,
    pub newer_p50: f64,
    /// Positive when the newer half is slower or larger.
    pub change_pct: f64,
}

/// Nearest-rank percentile of `values` (unsorted), `None` when empty.
pub fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((fraction * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

fn trend(values: &[f64]) -> Option<Trend> {
    if values.len() < MIN_TREND_VALUES {
        return None;
    }
    let (older, newer) = values.split_at(values.len() / 2);
    let older_p50 = percentile(older, 0.5)?;
    let newer_p50 = percentile(newer, 0.5)?;
    let change_pct = if older_p50 > 0.0 {
        ((newer_p50 - older_p50) / older_p50 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    Some(Trend {
        older_p50,
        newer_p50,
        change_pct,
    })
}

fn status(budget: Option<u64>, p50: Option<f64>, p95: Option<f64>) -> &'static str {
    match (budget, p50, p95) {
        (_, None, _) | (_, _, None) => "no-data",
        (None, _, _) => "no-budget",
        (Some(budget), _, Some(p95)) if p95 <= budget as f64 => "ok",
        (Some(budget), Some(p50), _) if p50 <= budget as f64 => "watch",
        _ => "over",
    }
}

fn metric(
    name: String,
    unit: &'static str,
    budget: Option<u64>,
    values: &[f64],
    failures: usize,
) -> MetricReport {
    let p50 = percentile(values, 0.5);
    let p95 = percentile(values, 0.95);
    MetricReport {
        status: status(budget, p50, p95),
        metric: name,
        unit,
        budget,
        count: values.len(),
        failures,
        p50,
        p95,
        latest: values.last().copied(),
        trend: trend(values),
    }
}

/// Every budgeted or measured metric of one repository, time metrics first.
/// `timings` are oldest first; `footprints` are byte samples per profile,
/// oldest first.
pub fn report(
    budgets: &BudgetsSpec,
    timings: &BTreeMap<String, Vec<Timing>>,
    footprints: &BTreeMap<String, Vec<u64>>,
) -> Vec<MetricReport> {
    let mut reports = Vec::new();
    let names: BTreeSet<&String> = budgets.seconds.keys().chain(timings.keys()).collect();
    for name in names {
        let recorded = timings.get(name).map(Vec::as_slice).unwrap_or_default();
        let values: Vec<f64> = recorded
            .iter()
            .filter(|t| t.ok)
            .map(|t| t.seconds)
            .collect();
        let failures = recorded.iter().filter(|t| !t.ok).count();
        reports.push(metric(
            name.clone(),
            "seconds",
            budgets.seconds.get(name).copied(),
            &values,
            failures,
        ));
    }
    let profiles: BTreeSet<&String> = budgets.memory_mib.keys().chain(footprints.keys()).collect();
    for profile in profiles {
        let values: Vec<f64> = footprints
            .get(profile)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|bytes| *bytes as f64 / (1024.0 * 1024.0))
            .collect();
        reports.push(metric(
            format!("memory.{profile}"),
            "memory_mib",
            budgets.memory_mib.get(profile).copied(),
            &values,
            0,
        ));
    }
    reports
}

/// The duration metrics a hook reported in its receipt: every numeric value
/// of a top-level `metrics` object, in seconds. Anything else is ignored.
pub fn hook_metrics(receipt: &serde_json::Value) -> Vec<(String, f64)> {
    receipt
        .get("metrics")
        .and_then(serde_json::Value::as_object)
        .map(|metrics| {
            metrics
                .iter()
                .filter(|(name, _)| !name.is_empty())
                .filter_map(|(name, value)| {
                    value
                        .as_f64()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .map(|v| (name.clone(), v))
                })
                .collect()
        })
        .unwrap_or_default()
}
