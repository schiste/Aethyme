//! Native `aethyme graph` front end (python-retirement Phase 1).
//!
//! Replaces the delegated Click group `graph node|children|parents|
//! callers|callees|docs|configs|expand|overview`. Each subcommand reads
//! the redb store, produces the same engine JSON the Python wrapper
//! consumed, and renders it with the exact Click renderer logic:
//!
//! - `--json-output`: Python did `json.dumps(engine_json, indent=2)`,
//!   which preserves the engine's field emission order. serde_json's
//!   default map sorts keys, so re-emitting through `Value` would
//!   reorder — instead [`pretty_json`] re-indents the engine's compact
//!   string textually, preserving order by construction.
//! - text mode: field lookups go through `serde_json::Value` (order-
//!   independent); the one order-sensitive iteration (overview
//!   `signals`) takes its key order from the raw JSON via
//!   [`object_key_order`].
//!
//! Golden-verified byte parity against the Python renderers.

use std::path::PathBuf;

use serde_json::Value;

use crate::cli_args::{self, FlagSpec};
use crate::cli_error::CliError;
use crate::graph::navigation::{
    GraphRelationView, callees_view_redb, callers_view_redb, children_view_redb, configs_view_redb,
    docs_view_redb, graph_expand_view_redb, graph_overview_view_redb, node_view_redb,
    parents_view_redb,
};
use crate::store::redb::graph_store::{GraphStore, GraphStoreError, ReadOnlyGraphStore};

/// Navigation subcommands handled here. The lifecycle subcommands
/// (`status`, `units`, `materialize`, `refresh`, `impact`) are routed
/// by the CLI before this function is reached; they are named in the
/// usage message so a caller that mistypes one is told what exists
/// rather than being sent to rebuild a graph store it did not need.
const NAVIGATION_SUBCOMMANDS: &[&str] = &[
    "node", "children", "parents", "callers", "callees", "docs", "configs", "expand", "overview",
];

/// Subcommands that require a `<target>` positional. The previous code
/// printed one usage line for all nine, which was wrong for eight of
/// them: only `overview` takes no target.
const TARGET_SUBCOMMANDS: &[&str] = &[
    "node", "children", "parents", "callers", "callees", "docs", "configs", "expand",
];

fn usage_line(subcommand: &str) -> String {
    if TARGET_SUBCOMMANDS.contains(&subcommand) {
        format!("usage: aethyme graph {subcommand} <repo_path> <target> [--json|--json-output]")
    } else {
        format!("usage: aethyme graph {subcommand} <repo_path> [--json|--json-output]")
    }
}

/// Per-subcommand help for `aethyme graph <subcommand> --help`.
fn graph_help() -> String {
    let mut out = String::from("aethyme graph — repository graph lifecycle and navigation\n\n");
    for subcommand in NAVIGATION_SUBCOMMANDS {
        out.push_str(&format!("  {}\n", usage_line(subcommand)));
    }
    out.push_str(
        "\nLifecycle subcommands (status, units, materialize, refresh, impact) are \
         documented by `aethyme graph --help`.\n",
    );
    out
}

/// Require the target positional for subcommands that take one.
fn require_target<'a>(subcommand: &str, target: Option<&'a str>) -> Result<&'a str, CliError> {
    target.ok_or_else(|| CliError::usage(usage_line(subcommand)))
}

/// Run `graph <subcommand> ...`. `args` excludes the leading `graph`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    // `--help` must succeed on every subcommand, so it is handled here
    // rather than by the strict flag parser below. Without this,
    // `aethyme graph node --help` is an unknown-flag usage error and the
    // help contract breaks.
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", graph_help());
        return Ok(());
    }

    let Some(subcommand) = args.first() else {
        return Err(CliError::usage(format!(
            "missing graph subcommand\nnavigation: {}\nlifecycle: status | units | materialize | refresh | impact",
            NAVIGATION_SUBCOMMANDS.join(" | ")
        )));
    };
    // Validate the subcommand before touching the filesystem. Opening
    // the store first meant a typo such as `graph nodez` reported a
    // missing graph store, sending the caller to build one it did not
    // need.
    if !NAVIGATION_SUBCOMMANDS.contains(&subcommand.as_str()) {
        return Err(CliError::usage(format!(
            "unsupported graph subcommand: {subcommand}\nnavigation: {}\nlifecycle: status | units | materialize | refresh | impact",
            NAVIGATION_SUBCOMMANDS.join(" | ")
        )));
    }

    let rest = &args[1..];
    let mut specs = cli_args::json_specs().to_vec();
    specs.push(FlagSpec::value("--repo"));
    let parsed = cli_args::parse(&format!("aethyme graph {subcommand}"), rest, &specs)?;
    let json_output = parsed.wants_json();

    // `--repo` is accepted as an alternative to the positional. The
    // positional wins when both are present, preserving the
    // pre-existing `<repo_path> <target>` form.
    let repo_raw = match parsed.positional(0) {
        Some(raw) => raw.clone(),
        None => parsed
            .option("--repo")
            .map(str::to_string)
            .ok_or_else(|| CliError::usage(usage_line(subcommand)))?,
    };
    let repo = {
        let path = PathBuf::from(&repo_raw);
        if !path.is_dir() {
            return Err(CliError::runtime(format!(
                "repository path is not a directory: {repo_raw}"
            )));
        }
        path.canonicalize().map_err(|e| CliError::runtime(e.to_string()))?
    };
    // With `--repo` the target becomes the first positional; with the
    // repo given positionally it is the second.
    let target_index = usize::from(parsed.positional(0).is_some());
    if TARGET_SUBCOMMANDS.contains(&subcommand.as_str()) {
        parsed.expect_max_positionals(&format!("aethyme graph {subcommand}"), target_index + 1)?;
    }
    let store = GraphStore::open_read_only(&repo).map_err(|e| CliError::runtime(e.to_string()))?;

    let target_arg: Option<&str> = if TARGET_SUBCOMMANDS.contains(&subcommand.as_str()) {
        Some(
            parsed
                .positional(target_index)
                .ok_or_else(|| CliError::usage(usage_line(subcommand)))?
                .as_str(),
        )
    } else {
        None
    };

    let raw_json = render_subcommand_json(&store, subcommand, target_arg)?;

    if json_output {
        println!("{}", pretty_json(&raw_json));
        return Ok(());
    }

    let payload: Value = serde_json::from_str(&raw_json)
        .map_err(|e| CliError::runtime(e.to_string()))?;
    let rendered = match subcommand.as_str() {
        "node" => render_node(&payload),
        "expand" => render_expand(&payload),
        "overview" => render_overview(&payload, &raw_json),
        _ => render_relation(&payload),
    };
    print!("{rendered}");
    Ok(())
}

/// Render one `graph <subcommand>` invocation as the engine JSON.
///
/// `target` is the resolved target for subcommands that take one and
/// `None` for `overview`.
fn render_subcommand_json(
    store: &ReadOnlyGraphStore,
    subcommand: &str,
    target: Option<&str>,
) -> Result<String, CliError> {
    let target = require_target(subcommand, target);
    match subcommand {
        "node" => {
            let t = target?;
            let view = node_view_redb(store, t)
                .map_err(|e| CliError::runtime(e.to_string()))?
                .ok_or_else(|| CliError::runtime(format!("node not found: {t}")))?;
            Ok(crate::json::graph_node_view(&view))
        }
        // The relation subcommands used to render an empty list for an
        // unresolvable target and exit 0, so a typo'd symbol was
        // indistinguishable from a symbol with genuinely no callers.
        // Resolving first reports not-found instead; a node that does
        // resolve but has no such relation still exits 0 with an empty
        // `items`.
        "children" => relation_json(store, subcommand, target?, children_view_redb),
        "parents" => relation_json(store, subcommand, target?, parents_view_redb),
        "callers" => relation_json(store, subcommand, target?, callers_view_redb),
        "callees" => relation_json(store, subcommand, target?, callees_view_redb),
        "docs" => relation_json(store, subcommand, target?, docs_view_redb),
        "configs" => relation_json(store, subcommand, target?, configs_view_redb),
        "expand" => {
            let t = target?;
            let view = graph_expand_view_redb(store, t)
                .map_err(|e| CliError::runtime(e.to_string()))?
                .ok_or_else(|| CliError::runtime(format!("node not found: {t}")))?;
            Ok(crate::json::graph_expand_view(&view))
        }
        "overview" => {
            let view = graph_overview_view_redb(store)
                .map_err(|e| CliError::runtime(e.to_string()))?;
            Ok(crate::json::repo_overview_view(&view))
        }
        other => Err(CliError::usage(format!(
            "unsupported graph subcommand: {other}"
        ))),
    }
}

/// Resolve `target`, then run one relation view and render it as JSON.
///
/// The resolution step is what separates "no such node" from "this node
/// has no such relation". The engine's relation views return an empty
/// item list for an unresolvable target so library callers can treat
/// "absent" and "empty" uniformly; a CLI caller cannot, because an
/// agent scripting the command needs to tell a typo from a genuine
/// empty result.
fn relation_json(
    store: &ReadOnlyGraphStore,
    subcommand: &str,
    target: &str,
    view: fn(&ReadOnlyGraphStore, &str) -> Result<GraphRelationView, GraphStoreError>,
) -> Result<String, CliError> {
    if node_view_redb(store, target)
        .map_err(|e| CliError::runtime(e.to_string()))?
        .is_none()
    {
        return Err(CliError::runtime(format!(
            "node not found: {target}\n\
             a `{subcommand}` relation needs a real node; \
             look one up with `aethyme query symbol <name>`"
        )));
    }
    let view = view(store, target).map_err(|e| CliError::runtime(e.to_string()))?;
    Ok(crate::json::graph_relation(&view))
}

// ── Text renderers (ports of the Click renderers, dict-driven) ────────

fn opt_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn render_node(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "ID: {}",
        payload["id"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    out.push_str(&format!(
        "Kind: {}",
        payload["kind"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    out.push_str(&format!(
        "Label: {}",
        payload["label"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    if let Some(confidence) = payload.get("confidence").filter(|v| !v.is_null()) {
        out.push_str(&format!("Confidence: {}", render_number(confidence)));
        out.push('\n');
    }
    if let Some(path) = opt_str(payload, "path") {
        out.push_str(&format!("Path: {path}"));
        out.push('\n');
    }
    if let Some(area) = opt_str(payload, "area") {
        out.push_str(&format!("Area: {area}"));
        out.push('\n');
    }
    if let Some(language) = opt_str(payload, "language") {
        out.push_str(&format!("Language: {language}"));
        out.push('\n');
    }
    if let Some(annotations) = payload.get("annotations").and_then(Value::as_array)
        && !annotations.is_empty()
    {
        out.push_str("Annotations:");
        out.push('\n');
        for annotation in annotations {
            out.push_str(&format!("- {}", annotation.as_str().unwrap_or_default()));
            out.push('\n');
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

pub(crate) fn render_relation(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Target: {}",
        payload["target"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    out.push_str(&format!(
        "Relation: {}",
        payload["relation"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    if let Some(items) = payload.get("items").and_then(Value::as_array) {
        for item in items {
            out.push_str(&format!(
                "- {} ({}, {}, conf={})",
                item["display"].as_str().unwrap_or_default(),
                item["kind"].as_str().unwrap_or_default(),
                item["relation"].as_str().unwrap_or_default(),
                render_number(&item["confidence"]),
            ));
            out.push('\n');
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

fn render_expand(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Target: {} ({})",
        payload["target"]["label"].as_str().unwrap_or_default(),
        payload["target"]["kind"].as_str().unwrap_or_default(),
    ));
    out.push('\n');
    if let Some(path) = opt_str(&payload["target"], "path") {
        out.push_str(&format!("Path: {path}"));
        out.push('\n');
    }
    for label in [
        "parents", "children", "callers", "callees", "docs", "configs",
    ] {
        let Some(items) = payload.get(label).and_then(Value::as_array) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("{}:", capitalize(label)));
        out.push('\n');
        for item in items {
            out.push_str(&format!(
                "- {} ({}, {}, conf={})",
                item["display"].as_str().unwrap_or_default(),
                item["kind"].as_str().unwrap_or_default(),
                item["relation"].as_str().unwrap_or_default(),
                render_number(&item["confidence"]),
            ));
            out.push('\n');
        }
    }
    if let Some(risks) = payload.get("risks").and_then(Value::as_array)
        && !risks.is_empty()
    {
        out.push_str("Risks:");
        out.push('\n');
        for risk in risks {
            out.push_str(&format!("- {}", risk.as_str().unwrap_or_default()));
            out.push('\n');
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

fn render_overview(payload: &Value, raw_json: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Repository: {}",
        payload["repo"].as_str().unwrap_or_default()
    ));
    out.push('\n');
    if let Some(signals) = payload.get("signals").and_then(Value::as_object)
        && !signals.is_empty()
    {
        out.push_str("Signals:");
        out.push('\n');
        // Python iterates dict insertion order = the engine's emission
        // order; serde's map is sorted, so recover order from the raw
        // JSON text.
        for name in object_key_order(raw_json, "signals") {
            let Some(signal) = signals.get(&name) else {
                continue;
            };
            out.push_str(&format!(
                "- {}: {} ({})",
                name.replace('_', " "),
                render_number(&signal["score"]),
                signal["level"].as_str().unwrap_or_default(),
            ));
            out.push('\n');
        }
    }
    for label in [
        "code_areas",
        "reference_areas",
        "subareas",
        "overview_docs",
        "key_configs",
        "entrypoints",
        "representative_code_files",
        "representative_docs",
    ] {
        let Some(items) = payload.get(label).and_then(Value::as_array) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("{}:", title_case(label)));
        out.push('\n');
        for item in items {
            out.push_str(&format!("- {}", item.as_str().unwrap_or_default()));
            out.push('\n');
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

pub(crate) fn emit_completeness_signals(payload: &Value, out: &mut String) {
    if let Some(truncated) = payload.get("truncated").and_then(Value::as_bool) {
        out.push_str(&format!(
            "Truncated: {}",
            if truncated { "yes" } else { "no" }
        ));
        out.push('\n');
        if truncated && let Some(reason) = opt_str(payload, "reason") {
            out.push_str(&format!("Truncation reason: {reason}"));
            out.push('\n');
        }
    }
    match payload.get("confidence") {
        Some(value) if value.is_number() => {
            out.push_str(&format!("Confidence: {}\n", render_number(value)));
        }
        Some(Value::Object(confidence)) => {
            let anchor = confidence.get("anchor_confidence").filter(|v| !v.is_null());
            let scope = confidence.get("scope_confidence").filter(|v| !v.is_null());
            if anchor.is_some() || scope.is_some() {
                let fmt =
                    |v: Option<&Value>| v.map(render_number).unwrap_or_else(|| "n/a".to_string());
                out.push_str(&format!(
                    "Confidence: anchor={}, scope={}",
                    fmt(anchor),
                    fmt(scope)
                ));
                out.push('\n');
            }
        }
        _ => {}
    }
    if let Some(caps) = payload.get("caps").and_then(Value::as_object)
        && !caps.is_empty()
    {
        // Python: json.dumps(caps) — compact with ", "/": " separators,
        // insertion order == sorted here is acceptable only if the
        // engine emits caps sorted; caps objects are small fixed maps
        // emitted in code order which is alphabetical today.
        let rendered: Vec<String> = caps
            .iter()
            .map(|(k, v)| format!("\"{k}\": {}", render_number(v)))
            .collect();
        out.push_str(&format!("Caps: {{{}}}", rendered.join(", ")));
        out.push('\n');
    }
}

pub(crate) fn render_number(value: &Value) -> String {
    // Match Python's str() for ints and floats appearing in JSON.
    if let Some(i) = value.as_i64() {
        return i.to_string();
    }
    if let Some(f) = value.as_f64() {
        return format!("{f}");
    }
    value.to_string()
}

fn capitalize(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn title_case(label: &str) -> String {
    label
        .split('_')
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Order-preserving JSON helpers ─────────────────────────────────────

/// Re-indent compact JSON with 2-space indentation, preserving key
/// order — the textual equivalent of Python's
/// `json.dumps(json.loads(s), indent=2)` for JSON that contains no
/// insignificant whitespace (the engine emits compact JSON).
pub fn pretty_json(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len() * 2);
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = compact.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                // Empty containers stay inline, matching json.dumps.
                if chars.peek() == Some(&close) {
                    out.push(c);
                    out.push(close);
                    chars.next();
                    continue;
                }
                depth += 1;
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Return the keys of the object stored under `field` in `raw` compact
/// JSON, in emission order.
pub(crate) fn object_key_order(raw: &str, field: &str) -> Vec<String> {
    let marker = format!("\"{field}\":{{");
    let Some(start) = raw.find(&marker) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = start + marker.len();
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    let mut expecting_key = true;
    while i < bytes.len() && depth > 0 {
        let c = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
                current.push(c);
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                if expecting_key && depth == 1 {
                    keys.push(current.clone());
                    expecting_key = false;
                }
            } else {
                current.push(c);
            }
        } else {
            match c {
                '"' => {
                    in_string = true;
                    current.clear();
                }
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                ',' if depth == 1 => expecting_key = true,
                _ => {}
            }
        }
        i += 1;
    }
    keys
}

/// Regression: the strict flag parser rejects unknown flags, so
/// `aethyme graph node --help` became an unknown-flag usage error unless
/// help is short-circuited first. `help_everywhere` covers this across
/// every command, but it is slow enough that a regression here can slip
/// through review; this pins the graph front end specifically.
#[cfg(test)]
mod help_flag_tests {
    use super::run;

    fn help_exits_zero(args: &[&str]) {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        assert!(
            run(&owned).is_ok(),
            "`aethyme graph {} --help` must succeed",
            owned.join(" ")
        );
    }

    #[test]
    fn every_navigation_subcommand_accepts_help() {
        for subcommand in super::NAVIGATION_SUBCOMMANDS {
            help_exits_zero(&[subcommand, "--help"]);
            help_exits_zero(&[subcommand, "-h"]);
        }
    }

    #[test]
    fn help_wins_over_a_missing_target() {
        // `graph node` without a target is a usage error; with `--help`
        // it must print help instead.
        help_exits_zero(&["node", "--help"]);
    }
}

#[cfg(test)]
mod tests {
    use super::{object_key_order, pretty_json};

    #[test]
    fn pretty_json_matches_python_indent2_shapes() {
        assert_eq!(pretty_json("{}"), "{}");
        assert_eq!(pretty_json("[]"), "[]");
        assert_eq!(
            pretty_json(r#"{"a":1,"b":[1,2],"c":{"d":"x, y"}}"#),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {\n    \"d\": \"x, y\"\n  }\n}"
        );
        // Braces/colons inside strings must not trigger structure.
        assert_eq!(
            pretty_json(r#"{"k":"a{b}:c\",d"}"#),
            "{\n  \"k\": \"a{b}:c\\\",d\"\n}"
        );
    }

    #[test]
    fn node_render_surfaces_completeness_signals() {
        // Ported from tests/local/test_cli_completeness_signals.py::
        // test_graph_node_non_json_surfaces_completeness_signals when the
        // graph group went native (Phase 1). Same synthetic payload, same
        // expected lines.
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"id":"fn:demo:main","kind":"function","label":"main",
                "confidence":920,"truncated":true,
                "reason":"result cap reached","caps":{"max_items":50}}"#,
        )
        .unwrap();
        let rendered = super::render_node(&payload);
        assert!(rendered.contains("Confidence: 920"), "{rendered}");
        assert!(rendered.contains("Truncated: yes"), "{rendered}");
        assert!(
            rendered.contains("Truncation reason: result cap reached"),
            "{rendered}"
        );
        assert!(rendered.contains("Caps: {\"max_items\": 50}"), "{rendered}");
    }

    #[test]
    fn pretty_json_preserves_multibyte_utf8() {
        // Caught by the unicode fixture file (Départ.php): the original
        // byte-cast tokenizer emitted mojibake for non-ASCII content.
        assert_eq!(
            pretty_json(r#"{"path":"legacy/Départ.php"}"#),
            "{\n  \"path\": \"legacy/Départ.php\"\n}"
        );
    }

    #[test]
    fn key_order_is_emission_order() {
        let raw = r#"{"repo":"r","signals":{"boundary_clarity":{"score":1},"entrypoint_clarity":{"score":2},"config_hygiene":{"score":3}}}"#;
        assert_eq!(
            object_key_order(raw, "signals"),
            vec!["boundary_clarity", "entrypoint_clarity", "config_hygiene"]
        );
    }
}
