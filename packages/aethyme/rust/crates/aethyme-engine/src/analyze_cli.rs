//! Native `aethyme analyze dead-code` (the final Phase 1 flip).
//!
//! Mirrors the engine's `analyze-dead-code` arm in-process and ports
//! the Click command's three formats: `summary` (text), `full-json`
//! (answer + `command_observability`), and `eval-json` (the
//! task-ready `unused_functions` shape from `_dead_code_eval_json`).
//!
//! Ordered emission: Python emitted dicts in insertion order; serde
//! maps sort keys, so composed objects are built textually and
//! pretty-printed with [`crate::graph_cli::pretty_json`]. The
//! `output_size_bytes` fixed-point loop is ported as-is.
//!
//! Sanctioned reinterpretation (recorded in cli-surface-v1.md): the
//! observability `index_freshness` and `engine` blocks are now
//! store/binary-backed — the Python originals reported transport-era
//! cache keys and shelled `git status` for dirty state, output the
//! CTO wrapper corrupts in Chau7 tabs (chau7#77).

use std::path::PathBuf;

use serde_json::Value;

use crate::graph::analyzers::analyze_dead_code;
use crate::graph_cli::pretty_json;
use crate::map::RepositoryMap;

/// Run `analyze <subcommand> ...`. `args` excludes the leading `analyze`.
pub fn run(args: &[String]) -> Result<(), String> {
    let Some(subcommand) = args.first() else {
        return Err("missing analyze subcommand (dead-code)".to_string());
    };
    if subcommand != "dead-code" {
        return Err(format!("unsupported analyze subcommand: {subcommand}"));
    }
    let rest = &args[1..];

    let repo = {
        let raw = opt(rest, "--repo")
            .ok_or_else(|| "usage: aethyme analyze dead-code --repo <path> --scope <prefix>".to_string())?;
        let path = PathBuf::from(&raw);
        if !path.is_dir() {
            return Err(format!("repository path is not a directory: {raw}"));
        }
        path.canonicalize().map_err(|e| e.to_string())?
    };
    let scope = opt(rest, "--scope")
        .ok_or_else(|| "usage: aethyme analyze dead-code --repo <path> --scope <prefix>".to_string())?;
    let boundary = opt(rest, "--boundary").unwrap_or_else(|| "outside-directory".to_string());
    if boundary != "outside-directory" {
        return Err(format!("unsupported --boundary: {boundary}"));
    }
    let roots: Vec<String> = opt(rest, "--roots")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let include_methods = rest.iter().any(|a| a == "--include-methods");
    let show_observability = rest.iter().any(|a| a == "--show-observability");
    let json_output = rest.iter().any(|a| a == "--json-output");
    let mut format = opt(rest, "--format").unwrap_or_else(|| "summary".to_string());
    if json_output && format == "summary" {
        format = "full-json".to_string();
    }

    let map = RepositoryMap::build_with_fragment_preference(&repo, false, |_| {})
        .map_err(|e| e.to_string())?
        .0;
    let answer = analyze_dead_code(&map, &scope, &roots, include_methods);
    let raw = serde_json::to_string(&answer).map_err(|e| e.to_string())?;
    let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;

    match format.as_str() {
        "full-json" => {
            let observability = observability_json(&repo, &scope, &roots, &boundary,
                include_methods, "full-json", &payload);
            // Python appended command_observability to the answer dict,
            // and its size loop also stamped output_size_bytes into the
            // answer's own observability block (dict assignment appends
            // the key last).
            let with_slot = inject_answer_size_slot(&raw);
            let body = with_slot
                .strip_suffix('}')
                .ok_or_else(|| "answer JSON malformed".to_string())?
                .to_string();
            let template = format!("{body},\"command_observability\":{observability}}}");
            // Python order: command_observability loop first (its marker is
            // the LAST occurrence), then the answer observability (FIRST).
            let after_first = fixed_point_output_size(&template, Occurrence::Last);
            println!("{}", fixed_point_pretty(&after_first, Occurrence::First));
        }
        "eval-json" => {
            let mut parts: Vec<String> = Vec::new();
            let items = |key: &str, statuses: Option<&[&str]>| -> String {
                let list = payload.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
                let rendered: Vec<String> = list
                    .iter()
                    .filter(|c| match statuses {
                        Some(allowed) => c
                            .get("status")
                            .and_then(Value::as_str)
                            .map(|s| allowed.contains(&s))
                            .unwrap_or(false),
                        None => true,
                    })
                    .map(eval_item_json)
                    .collect();
                format!("[{}]", rendered.join(","))
            };
            parts.push(format!(
                "\"unused_functions\":{}",
                items("candidates", Some(&["Unused", "Ambiguous"]))
            ));
            parts.push(format!("\"excluded_functions\":{}", items("excluded", None)));
            if show_observability {
                parts.push(format!(
                    "\"observability\":{}",
                    observability_json(&repo, &scope, &roots, &boundary,
                        include_methods, "eval-json", &payload)
                ));
            }
            let template = format!("{{{}}}", parts.join(","));
            println!("{}", fixed_point_output_size(&template, Occurrence::Last));
        }
        "summary" => {
            println!(
                "Analyzer: {} v{}",
                payload["analyzer"].as_str().unwrap_or_default(),
                payload["version"].as_str().unwrap_or_default(),
            );
            println!("Scope: {}", payload["query"]["scope"].as_str().unwrap_or_default());
            let s = &payload["summary"];
            println!(
                "Candidates: {} total, {} unused, {} ambiguous, {} used",
                s["total_candidates"], s["unused"], s["ambiguous"], s["used"]
            );
            println!("Candidates:");
            if let Some(candidates) = payload.get("candidates").and_then(Value::as_array) {
                for c in candidates {
                    println!(
                        "- {}::{} [{}] conf={:.2}",
                        c["function"]["defined_in"].as_str().unwrap_or_default(),
                        c["function"]["name"].as_str().unwrap_or_default(),
                        c["status"].as_str().unwrap_or_default(),
                        c["confidence"].as_f64().unwrap_or(0.0),
                    );
                }
            }
        }
        other => return Err(format!("unsupported --format: {other}")),
    }
    Ok(())
}

fn opt(rest: &[String], flag: &str) -> Option<String> {
    rest.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn jstr(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// One eval-json item, key order matching `_dead_code_eval_item`.
fn eval_item_json(candidate: &Value) -> String {
    let function = candidate.get("function").cloned().unwrap_or(Value::Null);
    let evidence = candidate.get("evidence").cloned().unwrap_or(Value::Null);
    let get = |v: &Value, key: &str| v.get(key).cloned().unwrap_or(Value::Null);
    let get_list = |v: &Value, key: &str| {
        v.get(key).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
    };
    format!(
        "{{\"function_name\":{},\"defined_in\":{},\"status\":{},\"external_callers\":{},\"internal_callers\":{},\"evidence\":{{\"searched_roots\":{},\"external_callers\":{},\"internal_callers\":{},\"docs_config_references\":{},\"ambiguity\":{}}},\"confidence\":{},\"reason\":{}}}",
        jstr(&get(&function, "name")),
        jstr(&get(&function, "defined_in")),
        jstr(&get(candidate, "status")),
        jstr(&get_list(&evidence, "external_callers")),
        jstr(&get_list(&evidence, "internal_callers")),
        jstr(&get_list(&evidence, "searched_roots")),
        jstr(&get_list(&evidence, "external_callers")),
        jstr(&get_list(&evidence, "internal_callers")),
        jstr(&get_list(&evidence, "docs_config_references")),
        jstr(&get_list(candidate, "ambiguity")),
        jstr(&get(candidate, "confidence")),
        jstr(&get(candidate, "rationale")),
    )
}

/// The observability block, key order matching the Python original;
/// freshness/engine fields are the sanctioned store/binary-backed
/// reinterpretation.
fn observability_json(
    repo: &std::path::Path,
    scope: &str,
    roots: &[String],
    boundary: &str,
    include_methods: bool,
    output_format: &str,
    answer: &Value,
) -> String {
    let store = repo.join(".aethyme").join("graph_store.redb");
    let rust_obs = answer.get("observability").cloned().unwrap_or(Value::Null);
    let get_obj = |key: &str| {
        rust_obs
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    };
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        "{{\"command\":\"analyze dead-code\",\"repo_path\":{},\"scope\":{},\"boundary\":{},\"roots\":{},\"include_methods\":{},\"output_format\":{},\"index_freshness\":{{\"status\":\"store_backed\",\"graph_store_present\":{},\"graph_store_path\":{}}},\"engine\":{{\"runtime\":\"native\",\"binary_path\":{},\"version\":{}}},\"graph_fact_count\":{{\"graph\":{},\"facts\":{}}},\"confidence_summary\":{},\"degraded_reasons\":{},\"failure_reason\":null,\"output_size_bytes\":null}}",
        jstr(&Value::String(repo.display().to_string())),
        jstr(&Value::String(scope.to_string())),
        jstr(&Value::String(boundary.to_string())),
        serde_json::to_string(roots).unwrap_or_else(|_| "[]".to_string()),
        include_methods,
        jstr(&Value::String(output_format.to_string())),
        store.is_file(),
        jstr(&Value::String(store.display().to_string())),
        jstr(&Value::String(binary)),
        jstr(&Value::String(env!("CARGO_PKG_VERSION").to_string())),
        jstr(&get_obj("graph_counts")),
        jstr(&get_obj("fact_counts")),
        jstr(&get_obj("confidence_summary")),
        jstr(&rust_obs.get("degraded_reasons").cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()))),
    )
}

/// Port of `_echo_json_with_output_size` / `_set_output_size`: up to 5
/// iterations of "measure rendered bytes; stop if the stored value
/// already equals the measurement, else store it and re-render" —
/// including Python's quirk that the final value can lag the final
/// text length when digit counts shift.
#[derive(Clone, Copy)]
enum Occurrence {
    First,
    Last,
}

fn fixed_point_output_size(compact: &str, which: Occurrence) -> String {
    fixed_point_pretty(&pretty_json(compact), which)
}

fn fixed_point_pretty(pretty: &str, which: Occurrence) -> String {
    let mut current = pretty.to_string();
    for _ in 0..5 {
        let size = current.len();
        let (existing, updated) = replace_output_size(&current, size, which);
        if existing == Some(size) {
            break;
        }
        current = updated;
    }
    current
}

/// Insert an `"output_size_bytes":null` slot at the end of the answer's
/// own observability object (Python's dict assignment appended it).
fn inject_answer_size_slot(raw: &str) -> String {
    let Some(start) = raw.find("\"observability\":{") else {
        return raw.to_string();
    };
    let open = start + "\"observability\":".len();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, c) in raw[open..].char_indices() {
        if in_string {
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
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = open + offset;
                    let empty = raw[open + 1..end].trim().is_empty();
                    let comma = if empty { "" } else { "," };
                    return format!(
                        "{}{comma}\"output_size_bytes\":null{}",
                        &raw[..end],
                        &raw[end..]
                    );
                }
            }
            _ => {}
        }
    }
    raw.to_string()
}

/// Returns (existing value if numeric, text with the value replaced).
fn replace_output_size(text: &str, size: usize, which: Occurrence) -> (Option<usize>, String) {
    let marker = "\"output_size_bytes\": ";
    let found = match which {
        Occurrence::First => text.find(marker),
        Occurrence::Last => text.rfind(marker),
    };
    let Some(i) = found else {
        return (None, text.to_string());
    };
    let value_start = i + marker.len();
    let value_end = text[value_start..]
        .find(|c: char| c == ',' || c == '\n' || c == '}')
        .map(|off| value_start + off)
        .unwrap_or(text.len());
    let existing = text[value_start..value_end].trim().parse::<usize>().ok();
    (
        existing,
        format!("{}{size}{}", &text[..value_start], &text[value_end..]),
    )
}

#[cfg(test)]
mod tests {
    use super::{fixed_point_output_size, replace_output_size, Occurrence};

    #[test]
    fn output_size_converges_like_python() {
        let compact = r#"{"a":1,"observability":{"output_size_bytes":null}}"#;
        let rendered = fixed_point_output_size(compact, Occurrence::Last);
        let reported: usize = rendered
            .split("\"output_size_bytes\": ")
            .nth(1)
            .unwrap()
            .trim_end_matches(['\n', '}', ' '])
            .parse()
            .unwrap();
        // Python semantics: value equals the length of SOME prior render;
        // within ±2 of the final length when digit counts are stable.
        assert!(reported.abs_diff(rendered.len()) <= 2, "{rendered}");
    }

    #[test]
    fn replace_reports_existing_numeric_value() {
        let (existing, updated) =
            replace_output_size("\"output_size_bytes\": 42}", 50, Occurrence::Last);
        assert_eq!(existing, Some(42));
        assert!(updated.contains(": 50}"));
    }
}
