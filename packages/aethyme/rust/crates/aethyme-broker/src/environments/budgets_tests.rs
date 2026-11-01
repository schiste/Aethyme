//! Declared budgets and their report (#1239).

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::budgets::{MIN_PROPOSAL_VALUES, hook_metrics, over_budget, percentile, propose, report};
use super::spec::{BudgetsSpec, EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentStore, TIMINGS_KEPT, Timing};
use super::{Loaded, parse_loaded};

fn ok(values: &[f64]) -> Vec<Timing> {
    values
        .iter()
        .map(|&seconds| Timing {
            seconds,
            at: 0,
            ok: true,
        })
        .collect()
}

fn find<'a>(
    reports: &'a [super::budgets::MetricReport],
    name: &str,
) -> &'a super::budgets::MetricReport {
    reports
        .iter()
        .find(|r| r.metric == name)
        .unwrap_or_else(|| panic!("{name} missing from {reports:#?}"))
}

/// Status follows the issue: ok when p95 is within budget, watch when only
/// p50 is, over when p50 is over, no-data without values; failed runs are
/// counted apart and never make a metric look fast.
#[test]
fn statuses_follow_p50_and_p95_against_the_budget() {
    let budgets = BudgetsSpec {
        seconds: BTreeMap::from([
            ("up.backend".to_string(), 30),
            ("park".to_string(), 15),
            ("down".to_string(), 5),
            ("up.cold".to_string(), 3600),
        ]),
        memory_mib: BTreeMap::from([("backend".to_string(), 350)]),
        ..BudgetsSpec::default()
    };
    let mut up = ok(&[10.0, 12.0, 20.0, 25.0]);
    up.push(Timing {
        seconds: 0.1,
        at: 0,
        ok: false,
    });
    let timings = BTreeMap::from([
        ("up.backend".to_string(), up),
        ("park".to_string(), ok(&[10.0, 14.0, 20.0, 30.0])),
        ("down".to_string(), ok(&[6.0, 7.0, 8.0])),
        ("start.api".to_string(), ok(&[1.0])),
    ]);
    let mib = 1024 * 1024;
    let footprints = BTreeMap::from([("backend".to_string(), vec![200 * mib, 300 * mib])]);
    let reports = report(&budgets, &timings, &footprints);

    let up = find(&reports, "up.backend");
    assert_eq!((up.status, up.count, up.failures), ("ok", 4, 1));
    assert_eq!(up.p50, Some(12.0));
    assert_eq!(up.latest, Some(25.0));
    assert_eq!(find(&reports, "park").status, "watch");
    assert_eq!(find(&reports, "down").status, "over");
    assert_eq!(find(&reports, "up.cold").status, "no-data");
    assert_eq!(find(&reports, "start.api").status, "no-budget");
    let memory = find(&reports, "memory.backend");
    assert_eq!((memory.unit, memory.status), ("memory_mib", "ok"));
    assert_eq!(memory.p95, Some(300.0));
}

/// The trend compares the newer half of the window with the older half.
#[test]
fn the_trend_compares_the_newer_half_with_the_older() {
    let timings = BTreeMap::from([
        ("up.p".to_string(), ok(&[10.0, 10.0, 20.0, 20.0])),
        ("park".to_string(), ok(&[1.0, 2.0, 3.0])),
    ]);
    let reports = report(&BudgetsSpec::default(), &timings, &BTreeMap::new());
    let trend = find(&reports, "up.p").trend.clone().expect("four values");
    assert_eq!((trend.older_p50, trend.newer_p50), (10.0, 20.0));
    assert_eq!(trend.change_pct, 100.0);
    assert!(find(&reports, "park").trend.is_none(), "too few values");
    assert_eq!(percentile(&[5.0, 1.0, 3.0], 0.5), Some(3.0));
}

/// A hook reports its own durations as a receipt's numeric `metrics`.
#[test]
fn hook_metrics_take_only_non_negative_numbers() {
    let receipt = serde_json::json!({
        "metrics": {"clone": 6.5, "bad": "x", "negative": -1, "": 2},
        "other": 1,
    });
    assert_eq!(hook_metrics(&receipt), vec![("clone".to_string(), 6.5)]);
    assert!(hook_metrics(&serde_json::json!({"metrics": 3})).is_empty());
}

/// The window is bounded per (metric, outcome): a burst of failures never
/// evicts the successful runs a trend is computed from.
#[test]
fn timings_are_bounded_per_metric_and_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EnvironmentStore::open(&dir.path().join("environments.db")).unwrap();
    for i in 0..(TIMINGS_KEPT + 5) {
        store
            .record_timing("repo", "park", "env", i as f64, true)
            .unwrap();
    }
    for _ in 0..3 {
        store
            .record_timing("repo", "park", "env", 0.5, false)
            .unwrap();
    }
    store
        .record_timing("other", "park", "env", 1.0, true)
        .unwrap();
    let timings = store.timings("repo").unwrap();
    let park = &timings["park"];
    assert_eq!(park.iter().filter(|t| t.ok).count(), TIMINGS_KEPT);
    assert_eq!(park.iter().filter(|t| !t.ok).count(), 3);
    assert_eq!(
        park.iter().find(|t| t.ok).map(|t| t.seconds),
        Some(5.0),
        "the oldest runs are evicted first"
    );
}

fn validation(text: &str) -> Vec<&'static str> {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    super::validate(&file, &HostEnvironmentsFile::default(), "t", "h")
        .into_iter()
        .map(|d| d.code)
        .collect()
}

/// Budgets are validated: positive values, and a memory budget names a
/// declared profile. Unknown time metrics are allowed (hooks report them).
#[test]
fn budgets_are_validated() {
    let base = r#"
schema = 1
[[profiles]]
name = "backend"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
"#;
    let valid = format!(
        "{base}[budgets.seconds]\n\"up.backend\" = 30\n\"prebuild.anything\" = 600\n[budgets.memory_mib]\nbackend = 350\n"
    );
    assert!(validation(&valid).is_empty(), "{:?}", validation(&valid));
    let zero = format!("{base}[budgets.seconds]\npark = 0\n");
    assert_eq!(validation(&zero), vec!["invalid_budget"]);
    let unknown = format!("{base}[budgets.memory_mib]\nfrontend = 100\n");
    assert_eq!(validation(&unknown), vec!["unknown_profile"]);
}

/// `doctor` lists only the metrics that are `over`, never `watch` or `ok`.
#[test]
fn doctor_lists_only_metrics_over_budget() {
    let budgets = BudgetsSpec {
        seconds: [
            ("park".to_string(), 10),
            ("down".to_string(), 10),
            ("up.cold".to_string(), 10),
        ]
        .into_iter()
        .collect(),
        ..BudgetsSpec::default()
    };
    let timings: BTreeMap<String, Vec<Timing>> = [
        ("park".to_string(), ok(&[1.0, 2.0, 3.0])),
        // p50 within, p95 over: watch.
        ("down".to_string(), ok(&[1.0, 2.0, 50.0])),
        ("up.cold".to_string(), ok(&[20.0, 30.0, 40.0])),
    ]
    .into_iter()
    .collect();
    let over: Vec<String> = over_budget(report(&budgets, &timings, &BTreeMap::new()))
        .into_iter()
        .map(|m| m.metric)
        .collect();
    assert_eq!(over, vec!["up.cold".to_string()]);
}

/// `calibrate` proposes a budget change only from enough successful runs,
/// raising one whose p50 is over and tightening one the p95 with headroom
/// beats by more than half; a budget met with ordinary headroom is kept.
#[test]
fn calibrate_proposes_raises_and_tightens_from_enough_runs() {
    let budgets = BudgetsSpec {
        seconds: [
            ("up.cold".to_string(), 10),
            ("park".to_string(), 100),
            ("down".to_string(), 12),
            ("thin".to_string(), 10),
        ]
        .into_iter()
        .collect(),
        memory_mib: [("backend".to_string(), 1000)].into_iter().collect(),
        ..BudgetsSpec::default()
    };
    let runs = |value: f64| ok(&[value; MIN_PROPOSAL_VALUES]);
    let timings: BTreeMap<String, Vec<Timing>> = [
        ("up.cold".to_string(), runs(20.0)),
        ("park".to_string(), runs(4.0)),
        ("down".to_string(), runs(6.0)),
        ("thin".to_string(), ok(&[50.0; MIN_PROPOSAL_VALUES - 1])),
    ]
    .into_iter()
    .collect();
    let footprints: BTreeMap<String, Vec<u64>> = [(
        "backend".to_string(),
        vec![200 * 1024 * 1024; MIN_PROPOSAL_VALUES],
    )]
    .into_iter()
    .collect();
    let proposals: Vec<(String, &str, u64, u64)> =
        propose(&report(&budgets, &timings, &footprints))
            .into_iter()
            .map(|p| (p.key, p.reason, p.current, p.proposed))
            .collect();
    assert_eq!(
        proposals,
        vec![
            ("park".to_string(), "tighten", 100, 6),
            ("up.cold".to_string(), "raise", 10, 30),
            ("backend".to_string(), "tighten", 1000, 300),
        ]
    );
}

/// The proposal is written into the diff under its own section, keeping a
/// quoted key quoted and leaving every other line alone.
#[test]
fn calibrate_diff_rewrites_only_the_proposed_budget_lines() {
    let original = "schema = 1\n[budgets.seconds]\n\"up.cold\" = 10\npark = 100\ndown = 12\n[budgets.memory_mib]\nbackend = 1000\npark = 7\n";
    let proposals = vec![
        super::budgets::BudgetProposal {
            metric: "up.cold".into(),
            unit: "seconds",
            key: "up.cold".into(),
            current: 10,
            proposed: 30,
            count: 10,
            p50: 20.0,
            p95: 20.0,
            reason: "raise",
        },
        super::budgets::BudgetProposal {
            metric: "memory.backend".into(),
            unit: "memory_mib",
            key: "backend".into(),
            current: 1000,
            proposed: 300,
            count: 10,
            p50: 200.0,
            p95: 200.0,
            reason: "tighten",
        },
    ];
    let diff = super::metrics::proposal_diff(original, None, &proposals);
    let changed: Vec<&str> = diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .collect();
    assert_eq!(
        changed,
        vec![
            "-\"up.cold\" = 10",
            "+\"up.cold\" = 30",
            "-backend = 1000",
            "+backend = 300",
        ]
    );
}
