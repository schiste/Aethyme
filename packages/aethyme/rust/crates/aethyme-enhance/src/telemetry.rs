//! Repo-local experience telemetry — byte-parity port of
//! `src/indexing/experience_telemetry.py` (event append, summaries,
//! wrapper-invocation recording, status artifacts). Since the Phase 3
//! flip the standalone telemetry CLI commands dispatch here too.
//!
//! Parity-first: events carry real wall-clock timestamps and the exact
//! Python `json.dumps` (compact, `", "` separators) line shape.
//!
//! ## On-disk format & versioning (Phase 3 decision, 2026-07-29)
//!
//! The ledger is JSONL: one event object per line, appended, never
//! rewritten. Every row's **first key** is
//! `"schema_version": "aethyme-experience-telemetry-v1"` — written by
//! the Python implementation since v1 and preserved verbatim by this
//! port. That per-row field IS the version mechanism:
//!
//! - real repos already have files full of such rows, and this reader
//!   parses them unchanged (readers key off `event_type`/`payload` and
//!   tolerate unknown fields and unknown event types);
//! - a first-line file header was rejected because it would break the
//!   "every line is an event" contract both readers rely on, breaks
//!   append-only concurrency (two writers would race to write the
//!   header), and would not survive log truncation/rotation;
//! - future format changes bump the per-row `schema_version`; readers
//!   detect rows they do not understand by that field. The transition
//!   Python reader ignored the field entirely, so v1 rows written by
//!   either implementation remain mutually readable.

use std::path::Path;

use crate::onboarding::{ACT_STARTER_JSON_PATH, ONBOARDING_JSON_PATH, override_freshness};
use crate::pyjson::{self, Value};
use crate::timeutil::now_iso_utc;
use crate::util::{py_splitlines, resolve_path};

pub const TELEMETRY_LOG_PATH: &str = ".aethyme/generated/experience-telemetry.jsonl";
pub const STATUS_JSON_PATH: &str = ".aethyme/generated/experience-status.json";
pub const STATUS_MARKDOWN_PATH: &str = ".aethyme/generated/experience-status.md";

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn py_yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Append one telemetry event to the repo-local ledger.
pub fn append_event(repo_path: &Path, event_type: &str, payload: Value) -> Result<(), String> {
    let repo_path = resolve_path(repo_path);
    let log_path = repo_path.join(TELEMETRY_LOG_PATH);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let event = obj(vec![
        ("schema_version", Value::str("aethyme-experience-telemetry-v1")),
        ("event_type", Value::str(event_type)),
        ("timestamp", Value::str(now_iso_utc())),
        ("repo_path", Value::str(repo_path.display().to_string())),
        ("payload", payload),
    ]);
    let line = format!("{}\n", pyjson::dumps_compact(&event));
    use std::io::Write;
    let mut handle = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)
        .map_err(|e| format!("{}: {e}", log_path.display()))?;
    handle
        .write_all(line.as_bytes())
        .map_err(|e| format!("{}: {e}", log_path.display()))
}

/// Summarize repo-local experience telemetry.
pub fn summarize_events(repo_path: &Path) -> Result<Value, String> {
    let repo_path = resolve_path(repo_path);
    let log_path = repo_path.join(TELEMETRY_LOG_PATH);
    if !log_path.exists() {
        return Ok(obj(vec![
            ("exists", Value::Bool(false)),
            ("path", Value::str(TELEMETRY_LOG_PATH)),
            ("event_count", Value::Int(0)),
            ("by_type", Value::object()),
            ("last_event_type", Value::Null),
        ]));
    }
    let text =
        std::fs::read_to_string(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut counts = Value::object();
    let mut last_event_type: Option<String> = None;
    let mut total: i128 = 0;
    for line in py_splitlines(&text) {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let Ok(event) = pyjson::loads(line) else {
            let current = counts
                .get("invalid_json")
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0);
            counts.set("invalid_json", Value::Int(current + 1));
            last_event_type = Some("invalid_json".to_string());
            continue;
        };
        let event_type = match event.get("event_type") {
            Some(v) if v.truthy() => v.py_str(),
            _ => "unknown".to_string(),
        };
        let current = counts
            .get(&event_type)
            .and_then(|v| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        counts.set(&event_type, Value::Int(current + 1));
        last_event_type = Some(event_type);
    }
    Ok(obj(vec![
        ("exists", Value::Bool(true)),
        ("path", Value::str(TELEMETRY_LOG_PATH)),
        ("event_count", Value::Int(total)),
        ("by_type", counts),
        (
            "last_event_type",
            last_event_type.map(Value::Str).unwrap_or(Value::Null),
        ),
    ]))
}

/// Detailed repo-local telemetry report.
pub fn detailed_report(repo_path: &Path) -> Result<Value, String> {
    let repo_path = resolve_path(repo_path);
    let log_path = repo_path.join(TELEMETRY_LOG_PATH);
    let summary = summarize_events(&repo_path)?;
    let freshness = override_freshness(&repo_path);
    if !log_path.exists() {
        let kpis = derive_kpis(&Value::object(), &Value::object(), &Value::object(), &freshness);
        let mut report = summary;
        report.set("recent_events", Value::Array(vec![]));
        report.set("wrapper_invocations", Value::object());
        report.set("latest_payloads", Value::object());
        report.set("freshness", freshness);
        report.set("kpis", kpis);
        return Ok(report);
    }

    let text =
        std::fs::read_to_string(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut recent_events: Vec<Value> = Vec::new();
    let mut wrapper_invocations = Value::object();
    let mut latest_payloads = Value::object();

    for line in py_splitlines(&text) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = pyjson::loads(line) else {
            continue;
        };
        let event_type = match event.get("event_type") {
            Some(v) if v.truthy() => v.py_str(),
            _ => "unknown".to_string(),
        };
        let payload = match event.get("payload") {
            Some(v) if v.truthy() => v.clone(),
            _ => Value::object(),
        };
        latest_payloads.set(&event_type, payload.clone());
        if event_type == "wrapper.invocation" {
            let wrapper_name = match payload.get("wrapper_name") {
                Some(v) if v.truthy() => v.py_str(),
                _ => "unknown".to_string(),
            };
            let current = wrapper_invocations
                .get(&wrapper_name)
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0);
            wrapper_invocations.set(&wrapper_name, Value::Int(current + 1));
        }
        recent_events.push(obj(vec![
            (
                "timestamp",
                event.get("timestamp").cloned().unwrap_or(Value::Null),
            ),
            ("event_type", Value::str(event_type)),
            ("payload", payload),
        ]));
    }

    let by_type = summary.get("by_type").cloned().unwrap_or(Value::object());
    let kpis = derive_kpis(&latest_payloads, &wrapper_invocations, &by_type, &freshness);
    let recent_tail: Vec<Value> = recent_events
        .iter()
        .rev()
        .take(10)
        .rev()
        .cloned()
        .collect();
    let mut report = summary;
    report.set("recent_events", Value::Array(recent_tail));
    report.set("wrapper_invocations", wrapper_invocations);
    report.set("latest_payloads", latest_payloads);
    report.set("freshness", freshness);
    report.set("kpis", kpis);
    Ok(report)
}

/// Record that an Aethyme-provided wrapper or hook was invoked.
/// Mirrors Python `record_wrapper_invocation`: `details` is attached
/// only when truthy (a non-empty object).
pub fn record_wrapper_invocation(
    repo_path: &Path,
    wrapper_name: &str,
    details: Option<Value>,
) -> Result<(), String> {
    let mut payload = Value::object();
    payload.set("wrapper_name", Value::str(wrapper_name));
    if let Some(details) = details {
        if details.truthy() {
            payload.set("details", details);
        }
    }
    append_event(repo_path, "wrapper.invocation", payload)
}

/// Compact payload from generated onboarding and Act artifacts.
pub fn event_payload_from_generated_artifacts(repo_path: &Path) -> Result<Value, String> {
    let repo_path = resolve_path(repo_path);
    let onboarding_path = repo_path.join(ONBOARDING_JSON_PATH);
    let act_path = repo_path.join(ACT_STARTER_JSON_PATH);
    let mut payload = Value::object();
    if onboarding_path.exists() {
        let text = std::fs::read_to_string(&onboarding_path)
            .map_err(|e| format!("{}: {e}", onboarding_path.display()))?;
        let onboarding = pyjson::loads(&text)
            .map_err(|e| format!("{}: {e}", onboarding_path.display()))?;
        payload.set("onboarding", onboarding_summary_payload(&onboarding)?);
    }
    if act_path.exists() {
        let text = std::fs::read_to_string(&act_path)
            .map_err(|e| format!("{}: {e}", act_path.display()))?;
        let act =
            pyjson::loads(&text).map_err(|e| format!("{}: {e}", act_path.display()))?;
        payload.set("act", act_summary_payload(&act)?);
    }
    Ok(payload)
}

fn req<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value.get(key).ok_or_else(|| format!("KeyError: '{key}'"))
}

/// The onboarding summary block shared by deploy events and `summarize`.
pub fn onboarding_summary_payload(onboarding: &Value) -> Result<Value, String> {
    let telemetry = req(onboarding, "telemetry")?;
    let counts = req(telemetry, "counts")?;
    Ok(obj(vec![
        ("commands", req(counts, "commands")?.clone()),
        ("areas", req(counts, "areas")?.clone()),
        ("entrypoints", req(counts, "entrypoints")?.clone()),
        ("notes", req(counts, "notes")?.clone()),
        (
            "overrides_applied",
            req(telemetry, "overrides_applied")?.clone(),
        ),
        (
            "override_invalid",
            req(telemetry, "override_invalid")?.clone(),
        ),
    ]))
}

/// The act summary block shared by deploy events and `summarize`.
pub fn act_summary_payload(act: &Value) -> Result<Value, String> {
    let commands = req(act, "commands")?;
    let telemetry = req(act, "telemetry")?;
    Ok(obj(vec![
        (
            "has_fast_test",
            Value::Bool(
                commands
                    .get("fast_test")
                    .map(Value::truthy)
                    .unwrap_or(false),
            ),
        ),
        ("entrypoints", req(telemetry, "entrypoint_count")?.clone()),
        (
            "caution_zones",
            req(telemetry, "caution_zone_count")?.clone(),
        ),
    ]))
}

/// Build a compact repo experience status artifact.
pub fn build_status_artifact(repo_path: &Path) -> Result<Value, String> {
    let report = detailed_report(repo_path)?;
    let recommendation = recommended_next_action(&report);
    let empty_obj = Value::object();
    let empty_list = Value::Array(vec![]);
    let by_type = report.get("by_type").unwrap_or(&empty_obj);
    let latest_payloads = report.get("latest_payloads").unwrap_or(&empty_obj);
    let freshness = report.get("freshness").unwrap_or(&empty_obj);
    let kpis = report.get("kpis").unwrap_or(&empty_obj);
    let count_of = |key: &str| -> i128 {
        match by_type.get(key) {
            Some(Value::Int(n)) => *n,
            _ => 0,
        }
    };
    let recent_events = report.get("recent_events").unwrap_or(&empty_list);
    let recent_tail: Value = match recent_events.as_array() {
        Some(items) => Value::Array(items.iter().rev().take(5).rev().cloned().collect()),
        None => Value::Array(vec![]),
    };
    Ok(obj(vec![
        ("schema_version", Value::str("aethyme-experience-status-v1")),
        (
            "path",
            obj(vec![
                (
                    "telemetry",
                    report.get("path").cloned().unwrap_or(Value::Null),
                ),
                ("status_json", Value::str(STATUS_JSON_PATH)),
                ("status_markdown", Value::str(STATUS_MARKDOWN_PATH)),
            ]),
        ),
        (
            "enhancement",
            obj(vec![
                ("installed", Value::Bool(count_of("enhance.deploy") > 0)),
                ("verified", Value::Bool(count_of("enhance.verify") > 0)),
            ]),
        ),
        (
            "artifacts",
            obj(vec![
                (
                    "onboarding_present",
                    Value::Bool(latest_onboarding_payload(latest_payloads).truthy()),
                ),
                (
                    "act_present",
                    Value::Bool(latest_act_payload(latest_payloads).truthy()),
                ),
                (
                    "override_exists",
                    Value::Bool(
                        freshness
                            .get("override_exists")
                            .map(Value::truthy)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "override_regeneration_required",
                    Value::Bool(
                        freshness
                            .get("regeneration_required")
                            .map(Value::truthy)
                            .unwrap_or(false),
                    ),
                ),
                (
                    "stale_targets",
                    match freshness.get("stale_targets") {
                        Some(v) if v.truthy() => v.clone(),
                        _ => Value::Array(vec![]),
                    },
                ),
            ]),
        ),
        ("kpis", kpis.clone()),
        (
            "signals",
            match kpis.get("signals") {
                Some(v) => v.clone(),
                None => Value::Array(vec![]),
            },
        ),
        (
            "suggestions",
            match kpis.get("suggestions") {
                Some(v) => v.clone(),
                None => Value::Array(vec![]),
            },
        ),
        (
            "wrapper_invocations",
            report
                .get("wrapper_invocations")
                .cloned()
                .unwrap_or(Value::object()),
        ),
        ("recent_events", recent_tail),
        ("recommended_next_action", recommendation),
    ]))
}

/// Render the experience status artifact as compact Markdown.
pub fn render_status_markdown(status: &Value) -> Result<String, String> {
    let enhancement = req(status, "enhancement")?;
    let artifacts = req(status, "artifacts")?;
    let kpis = req(status, "kpis")?;
    let signals = req(status, "signals")?;
    let suggestions = req(status, "suggestions")?;
    let recommendation = req(status, "recommended_next_action")?;

    let truthy_of = |value: &Value, key: &str| -> bool {
        value.get(key).map(Value::truthy).unwrap_or(false)
    };

    let mut lines: Vec<String> = vec![
        "# Aethyme Experience Status".to_string(),
        String::new(),
        "## Summary".to_string(),
        String::new(),
        format!(
            "- Enhancement installed: `{}`",
            py_yes_no(truthy_of(enhancement, "installed"))
        ),
        format!(
            "- Enhancement verified: `{}`",
            py_yes_no(truthy_of(enhancement, "verified"))
        ),
        format!(
            "- Onboarding present: `{}`",
            py_yes_no(truthy_of(artifacts, "onboarding_present"))
        ),
        format!(
            "- Act starter present: `{}`",
            py_yes_no(truthy_of(artifacts, "act_present"))
        ),
        format!(
            "- Override exists: `{}`",
            py_yes_no(truthy_of(artifacts, "override_exists"))
        ),
        format!(
            "- Override regeneration required: `{}`",
            py_yes_no(truthy_of(artifacts, "override_regeneration_required"))
        ),
        format!(
            "- Wrapper invocations: `{}`",
            kpis.get("wrapper_total").unwrap_or(&Value::Int(0)).py_str()
        ),
        format!(
            "- Fast test detected: `{}`",
            py_yes_no(truthy_of(kpis, "act_has_fast_test"))
        ),
        String::new(),
        "## Attention Signals".to_string(),
        String::new(),
    ];
    if signals.truthy() {
        if let Some(items) = signals.as_array() {
            for signal in items {
                lines.push(format!(
                    "- `{}` `{}`: {}",
                    req(signal, "status")?.py_str(),
                    req(signal, "code")?.py_str(),
                    req(signal, "message")?.py_str(),
                ));
            }
        }
    } else {
        lines.push("- none".to_string());
    }

    lines.push(String::new());
    lines.push("## Suggestions".to_string());
    lines.push(String::new());
    if suggestions.truthy() {
        if let Some(items) = suggestions.as_array() {
            for suggestion in items {
                lines.push(format!(
                    "- `{}`: {}",
                    req(suggestion, "code")?.py_str(),
                    req(suggestion, "message")?.py_str(),
                ));
            }
        }
    } else {
        lines.push("- none".to_string());
    }

    lines.push(String::new());
    lines.push("## Recommended Next Action".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- Command: `{}`",
        req(recommendation, "command")?.py_str()
    ));
    lines.push(format!(
        "- Reason: {}",
        req(recommendation, "reason")?.py_str()
    ));
    if artifacts
        .get("stale_targets")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        let stale = artifacts.get("stale_targets").expect("checked above");
        let joined = stale
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(Value::py_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!("- Stale targets: `{joined}`"));
    }

    if let Some(recent) = status.get("recent_events") {
        if recent.truthy() {
            lines.push(String::new());
            lines.push("## Recent Events".to_string());
            lines.push(String::new());
            if let Some(items) = recent.as_array() {
                for event in items {
                    lines.push(format!(
                        "- `{}` `{}`",
                        req(event, "timestamp")?.py_str(),
                        req(event, "event_type")?.py_str(),
                    ));
                }
            }
        }
    }

    Ok(format!("{}\n", lines.join("\n")))
}

/// Write repo-local experience status artifacts; return the status payload.
pub fn write_status_artifacts(repo_path: &Path) -> Result<Value, String> {
    let repo_path = resolve_path(repo_path);
    let status = build_status_artifact(&repo_path)?;
    let status_json_path = repo_path.join(STATUS_JSON_PATH);
    let status_markdown_path = repo_path.join(STATUS_MARKDOWN_PATH);
    if let Some(parent) = status_json_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(
        &status_json_path,
        format!("{}\n", pyjson::dumps_indent2(&status)),
    )
    .map_err(|e| format!("{}: {e}", status_json_path.display()))?;
    std::fs::write(&status_markdown_path, render_status_markdown(&status)?)
        .map_err(|e| format!("{}: {e}", status_markdown_path.display()))?;
    Ok(status)
}

fn derive_kpis(
    latest_payloads: &Value,
    wrapper_invocations: &Value,
    counts: &Value,
    freshness: &Value,
) -> Value {
    let onboarding = latest_onboarding_payload(latest_payloads);
    let act = latest_act_payload(latest_payloads);
    let wrapper_total: i128 = wrapper_invocations
        .as_object()
        .map(|entries| {
            entries
                .iter()
                .map(|(_, v)| match v {
                    Value::Int(n) => *n,
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or(0);

    let deploy_count = match counts.get("enhance.deploy") {
        Some(Value::Int(n)) => *n,
        _ => 0,
    };

    let mut signals: Vec<Value> = Vec::new();
    if deploy_count > 0 && wrapper_total == 0 {
        signals.push(obj(vec![
            ("status", Value::str("attention")),
            ("code", Value::str("enhanced_but_no_wrapper_usage")),
            (
                "message",
                Value::str(
                    "Enhancement was deployed, but no Aethyme wrapper invocation has been recorded yet.",
                ),
            ),
        ]));
    }
    if onboarding
        .get("override_invalid")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        signals.push(obj(vec![
            ("status", Value::str("attention")),
            ("code", Value::str("invalid_override_present")),
            (
                "message",
                Value::str("Onboarding override file exists but is invalid."),
            ),
        ]));
    }
    let act_has_fast_test = act
        .get("has_fast_test")
        .map(Value::truthy)
        .unwrap_or(false);
    if onboarding.truthy() && !act_has_fast_test {
        signals.push(obj(vec![
            ("status", Value::str("attention")),
            ("code", Value::str("no_fast_test_detected")),
            (
                "message",
                Value::str(
                    "Onboarding/Act artifacts exist but no fast test command was detected.",
                ),
            ),
        ]));
    }
    if onboarding
        .get("overrides_applied")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        signals.push(obj(vec![
            ("status", Value::str("info")),
            ("code", Value::str("maintainer_overrides_active")),
            (
                "message",
                Value::str("Maintainer onboarding overrides are active for this repository."),
            ),
        ]));
    }
    if freshness
        .get("regeneration_required")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        let stale_joined = freshness
            .get("stale_targets")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(Value::py_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        signals.push(obj(vec![
            ("status", Value::str("attention")),
            ("code", Value::str("override_regeneration_required")),
            (
                "message",
                Value::str(format!(
                    "Onboarding overrides changed after generated artifacts and require regeneration ({stale_joined})."
                )),
            ),
        ]));
    }

    let suggestions = suggestions_from_signals(&signals);
    obj(vec![
        ("wrapper_total", Value::Int(wrapper_total)),
        (
            "onboarding_commands",
            onboarding
                .get("commands")
                .cloned()
                .unwrap_or(Value::Int(0)),
        ),
        (
            "onboarding_notes",
            onboarding.get("notes").cloned().unwrap_or(Value::Int(0)),
        ),
        (
            "act_has_fast_test",
            act.get("has_fast_test")
                .cloned()
                .unwrap_or(Value::Bool(false)),
        ),
        (
            "override_regeneration_required",
            Value::Bool(
                freshness
                    .get("regeneration_required")
                    .map(Value::truthy)
                    .unwrap_or(false),
            ),
        ),
        (
            "stale_targets",
            freshness
                .get("stale_targets")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        ),
        ("signals", Value::Array(signals)),
        ("suggestions", Value::Array(suggestions)),
    ])
}

fn latest_onboarding_payload(latest_payloads: &Value) -> Value {
    for event_type in ["enhance.verify", "enhance.deploy", "repo.compile-skills"] {
        let payload = match latest_payloads.get(event_type) {
            Some(v) if v.truthy() => v.clone(),
            _ => Value::object(),
        };
        if let Some(onboarding @ Value::Object(_)) = payload.get("onboarding") {
            return onboarding.clone();
        }
    }
    Value::object()
}

fn latest_act_payload(latest_payloads: &Value) -> Value {
    for event_type in ["enhance.verify", "enhance.deploy", "repo.compile-skills"] {
        let payload = match latest_payloads.get(event_type) {
            Some(v) if v.truthy() => v.clone(),
            _ => Value::object(),
        };
        if let Some(act @ Value::Object(_)) = payload.get("act") {
            return act.clone();
        }
    }
    Value::object()
}

fn suggestions_from_signals(signals: &[Value]) -> Vec<Value> {
    let codes: Vec<String> = signals
        .iter()
        .filter_map(|signal| signal.get("code").map(Value::py_str))
        .collect();
    let has = |code: &str| codes.iter().any(|c| c == code);
    let mut suggestions = Vec::new();
    if has("enhanced_but_no_wrapper_usage") {
        suggestions.push(obj(vec![
            ("code", Value::str("load_onboarding_and_use_wrapper")),
            (
                "message",
                Value::str(
                    "Load `repo-onboarding` first and invoke `.codex/skills/aethyme/aethyme-explore` or the equivalent Aethyme wrapper for the next broad task.",
                ),
            ),
        ]));
    }
    if has("invalid_override_present") {
        suggestions.push(obj(vec![
            ("code", Value::str("fix_or_reinitialize_override")),
            (
                "message",
                Value::str(
                    "Run `aethyme repo validate-onboarding-overrides <repo>` to inspect errors, then fix the JSON or rerun `aethyme repo init-onboarding-overrides <repo> --force`.",
                ),
            ),
        ]));
    }
    if has("no_fast_test_detected") {
        suggestions.push(obj(vec![
            ("code", Value::str("add_fast_test_override")),
            (
                "message",
                Value::str(
                    "Add a maintainer override for `commands[].test` in `.aethyme/overrides/onboarding.json`, then rerun `aethyme repo compile-skills <repo>` or `aethyme enhance deploy --repo <repo>`.",
                ),
            ),
        ]));
    }
    if has("maintainer_overrides_active") {
        suggestions.push(obj(vec![
            ("code", Value::str("review_override_drift")),
            (
                "message",
                Value::str(
                    "Review whether the override notes and commands are still current after recent repo changes and regenerate onboarding if needed.",
                ),
            ),
        ]));
    }
    if has("override_regeneration_required") {
        suggestions.push(obj(vec![
            ("code", Value::str("regenerate_onboarding_artifacts")),
            (
                "message",
                Value::str(
                    "Rerun `aethyme repo compile-skills <repo>` or `aethyme enhance deploy --repo <repo>` so onboarding and Act artifacts match the current override file.",
                ),
            ),
        ]));
    }
    suggestions
}

fn recommended_next_action(report: &Value) -> Value {
    let empty_obj = Value::object();
    let kpis = match report.get("kpis") {
        Some(v) if v.truthy() => v,
        _ => &empty_obj,
    };
    let suggestion_codes: Vec<String> = kpis
        .get("suggestions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|s| s.get("code").map(Value::py_str))
                .collect()
        })
        .unwrap_or_default();
    let has = |code: &str| suggestion_codes.iter().any(|c| c == code);
    if has("regenerate_onboarding_artifacts") {
        return obj(vec![
            ("command", Value::str("aethyme enhance deploy --repo <repo>")),
            (
                "reason",
                Value::str("Override file is newer than generated onboarding/Act artifacts."),
            ),
        ]);
    }
    if has("fix_or_reinitialize_override") {
        return obj(vec![
            (
                "command",
                Value::str("aethyme repo validate-onboarding-overrides <repo>"),
            ),
            (
                "reason",
                Value::str(
                    "Override file is invalid and should be fixed before relying on generated skills.",
                ),
            ),
        ]);
    }
    if has("load_onboarding_and_use_wrapper") {
        return obj(vec![
            (
                "command",
                Value::str("aethyme repo experience-telemetry <repo>"),
            ),
            (
                "reason",
                Value::str(
                    "Enhancement is installed but there is no wrapper usage recorded yet.",
                ),
            ),
        ]);
    }
    if has("add_fast_test_override") {
        return obj(vec![
            (
                "command",
                Value::str("aethyme repo init-onboarding-overrides <repo>"),
            ),
            (
                "reason",
                Value::str(
                    "Generated Act starter has no fast test command and may need a maintainer override.",
                ),
            ),
        ]);
    }
    obj(vec![
        (
            "command",
            Value::str("aethyme explore --repo <repo> --request \"<task>\" --format answer-json"),
        ),
        (
            "reason",
            Value::str("Experience layer is in a healthy state; proceed with Explore."),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aethyme-enhance-telemetry-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn append_and_summarize_round_trip() {
        let repo = fixture_repo("append");
        let mut payload = Value::object();
        payload.set("force", Value::Bool(true));
        append_event(&repo, "enhance.deploy", payload).unwrap();
        let line = std::fs::read_to_string(repo.join(TELEMETRY_LOG_PATH)).unwrap();
        // Compact separators with spaces — Python's default dumps shape.
        assert!(line.starts_with(
            "{\"schema_version\": \"aethyme-experience-telemetry-v1\", \"event_type\": \"enhance.deploy\", \"timestamp\": \""
        ));
        assert!(line.contains("\"payload\": {\"force\": true}"));
        let summary = summarize_events(&repo).unwrap();
        assert_eq!(summary.get("event_count"), Some(&Value::Int(1)));
        assert_eq!(
            summary.get("last_event_type").and_then(Value::as_str),
            Some("enhance.deploy")
        );
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn record_wrapper_invocation_shapes_payload_like_python() {
        let repo = fixture_repo("wrapper");
        // With details.
        let mut details = Value::object();
        details.set("source", Value::str("wrapper"));
        record_wrapper_invocation(&repo, "aethyme-explore", Some(details)).unwrap();
        // Empty details map → Python's `parsed_details or None` drops it.
        record_wrapper_invocation(&repo, "aethyme-sessionstart-hook", Some(Value::object()))
            .unwrap();
        let text = std::fs::read_to_string(repo.join(TELEMETRY_LOG_PATH)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(
            "\"payload\": {\"wrapper_name\": \"aethyme-explore\", \"details\": {\"source\": \"wrapper\"}}"
        ));
        assert!(lines[1].contains("\"payload\": {\"wrapper_name\": \"aethyme-sessionstart-hook\"}"));

        let report = detailed_report(&repo).unwrap();
        let invocations = report.get("wrapper_invocations").unwrap();
        assert_eq!(invocations.get("aethyme-explore"), Some(&Value::Int(1)));
        assert_eq!(
            invocations.get("aethyme-sessionstart-hook"),
            Some(&Value::Int(1))
        );
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn reader_parses_rows_written_by_the_python_implementation() {
        // Verbatim rows captured from a real dogfood-repo ledger written
        // by `src/indexing/experience_telemetry.py` (v1 format), plus the
        // tolerated degradations: blank lines, invalid JSON, a missing
        // event_type, and an unknown future schema_version row.
        let repo = fixture_repo("compat");
        let log_path = repo.join(TELEMETRY_LOG_PATH);
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(
            &log_path,
            concat!(
                "{\"schema_version\": \"aethyme-experience-telemetry-v1\", \"event_type\": \"enhance.deploy\", \"timestamp\": \"2026-07-20T09:15:02+00:00\", \"repo_path\": \"/tmp/demo\", \"payload\": {\"force\": true, \"actions\": 12}}\n",
                "\n",
                "{\"schema_version\": \"aethyme-experience-telemetry-v1\", \"event_type\": \"wrapper.invocation\", \"timestamp\": \"2026-07-20T09:16:40+00:00\", \"repo_path\": \"/tmp/demo\", \"payload\": {\"wrapper_name\": \"aethyme-explore\", \"details\": {\"source\": \"wrapper\"}}}\n",
                "not json at all\n",
                "{\"timestamp\": \"2026-07-20T09:17:00+00:00\", \"payload\": {}}\n",
                "{\"schema_version\": \"aethyme-experience-telemetry-v9\", \"event_type\": \"future.event\", \"payload\": {\"x\": 1}}\n",
            ),
        )
        .unwrap();

        let summary = summarize_events(&repo).unwrap();
        assert_eq!(summary.get("event_count"), Some(&Value::Int(5)));
        let by_type = summary.get("by_type").unwrap();
        assert_eq!(by_type.get("enhance.deploy"), Some(&Value::Int(1)));
        assert_eq!(by_type.get("wrapper.invocation"), Some(&Value::Int(1)));
        assert_eq!(by_type.get("invalid_json"), Some(&Value::Int(1)));
        assert_eq!(by_type.get("unknown"), Some(&Value::Int(1)));
        assert_eq!(by_type.get("future.event"), Some(&Value::Int(1)));

        let report = detailed_report(&repo).unwrap();
        assert_eq!(
            report
                .get("wrapper_invocations")
                .unwrap()
                .get("aethyme-explore"),
            Some(&Value::Int(1))
        );
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn status_artifact_flags_missing_wrapper_usage() {
        let repo = fixture_repo("status");
        let mut payload = Value::object();
        payload.set("force", Value::Bool(false));
        append_event(&repo, "enhance.deploy", payload).unwrap();
        let status = write_status_artifacts(&repo).unwrap();
        let signals = status.get("signals").unwrap().as_array().unwrap();
        assert!(
            signals
                .iter()
                .any(|s| s.get("code").and_then(Value::as_str)
                    == Some("enhanced_but_no_wrapper_usage"))
        );
        let markdown =
            std::fs::read_to_string(repo.join(STATUS_MARKDOWN_PATH)).unwrap();
        assert!(markdown.contains("# Aethyme Experience Status"));
        assert!(markdown.contains("- Enhancement installed: `yes`"));
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
