//! Native `aethyme facts` and `aethyme intents` front ends
//! (python-retirement Phase 1 closeout).
//!
//! `facts public-functions|function-usage` mirror the engine's
//! `facts-*` subcommand arms in-process and the Click renderers
//! byte-for-byte. `intents` serves the contract catalog embedded at
//! build time from `intents_catalog.json` — extracted verbatim from
//! the Python `_intent_catalog()` output (indent=2), so the emitted
//! bytes are unchanged; `--request` splices the same two trailing keys
//! Python appended.

use std::path::PathBuf;

use serde_json::Value;

use crate::graph::facts::{function_usage_fact, public_function_facts};
use crate::map::RepositoryMap;

const INTENTS_CATALOG: &str = include_str!("intents_catalog.json");

/// Run `facts <subcommand> ...`. `args` excludes the leading `facts`.
pub fn run_facts(args: &[String]) -> Result<(), String> {
    let Some(subcommand) = args.first() else {
        return Err("missing facts subcommand (public-functions | function-usage)".to_string());
    };
    let rest = &args[1..];
    let json_output = rest.iter().any(|a| a == "--json-output");
    let repo = {
        let raw = opt_value(rest, "--repo")
            .ok_or_else(|| format!("usage: aethyme facts {subcommand} --repo <path> ..."))?;
        let path = PathBuf::from(&raw);
        if !path.is_dir() {
            return Err(format!("repository path is not a directory: {raw}"));
        }
        path.canonicalize().map_err(|e| e.to_string())?
    };
    // Facts remain map-based (same as the engine CLI arms they mirror).
    let map = RepositoryMap::build_with_fragment_preference(&repo, false, |_| {})
        .map_err(|e| e.to_string())?
        .0;

    match subcommand.as_str() {
        "public-functions" => {
            let scope = opt_value(rest, "--scope").ok_or_else(|| {
                "usage: aethyme facts public-functions --repo <path> --scope <prefix>".to_string()
            })?;
            let include_methods = rest.iter().any(|a| a == "--include-methods");
            let facts = public_function_facts(&map, &scope, include_methods);
            let raw = serde_json::to_string_pretty(&facts).map_err(|e| e.to_string())?;
            if json_output {
                println!("{raw}");
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                if let Some(items) = payload.as_array() {
                    for item in items {
                        println!(
                            "- {}::{} [{}]",
                            item["defined_in"].as_str().unwrap_or_default(),
                            item["name"].as_str().unwrap_or_default(),
                            item["exposure_kind"].as_str().unwrap_or_default(),
                        );
                    }
                }
            }
        }
        "function-usage" => {
            let target = opt_value(rest, "--target").ok_or_else(|| {
                "usage: aethyme facts function-usage --repo <path> --target <fn> --boundary <prefix>"
                    .to_string()
            })?;
            let boundary = opt_value(rest, "--boundary").ok_or_else(|| {
                "usage: aethyme facts function-usage --repo <path> --target <fn> --boundary <prefix>"
                    .to_string()
            })?;
            let roots: Vec<String> = opt_value(rest, "--roots")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let fact = public_function_facts(&map, &boundary, true)
                .into_iter()
                .find(|fact| {
                    fact.id == target || fact.name == target || fact.qualified_name == target
                })
                .ok_or_else(|| format!("function fact not found for target: {target}"))?;
            let usage = function_usage_fact(&map, &fact, &boundary, &roots);
            let raw = serde_json::to_string_pretty(&usage).map_err(|e| e.to_string())?;
            if json_output {
                println!("{raw}");
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                println!(
                    "Function: {}",
                    payload["function"]["qualified_name"].as_str().unwrap_or_default()
                );
                println!(
                    "Boundary: {}",
                    payload["boundary"].as_str().unwrap_or_default()
                );
                for (key, heading) in [
                    ("internal_callers", "Internal callers:"),
                    ("external_callers", "External callers:"),
                ] {
                    if let Some(items) = payload.get(key).and_then(Value::as_array) {
                        if !items.is_empty() {
                            println!("{heading}");
                            for item in items {
                                println!("- {}", item.as_str().unwrap_or_default());
                            }
                        }
                    }
                }
            }
        }
        other => return Err(format!("unsupported facts subcommand: {other}")),
    }
    Ok(())
}

/// Run `intents ...`. `args` excludes the leading `intents`.
pub fn run_intents(args: &[String]) -> Result<(), String> {
    let format = opt_value(args, "--format").unwrap_or_else(|| "compact-json".to_string());
    if format != "compact-json" {
        return Err(format!("Unsupported intents format: {format}"));
    }
    let catalog = INTENTS_CATALOG.trim_end();
    match opt_value(args, "--request") {
        Some(request) if !request.is_empty() => {
            // Python appended `request` and `selection_status` to the dict;
            // with insertion-ordered dumps they land after the last key.
            let body = catalog
                .strip_suffix("\n}")
                .ok_or_else(|| "intents catalog malformed".to_string())?;
            let raw_value = serde_json::to_string(&Value::String(request))
                .map_err(|e| e.to_string())?;
            println!(
                "{body},\n  \"request\": {{\n    \"raw\": {raw_value}\n  }},\n  \"selection_status\": \"default_available_choose_specialized_when_clear\"\n}}"
            );
        }
        _ => println!("{catalog}"),
    }
    Ok(())
}

fn opt_value(rest: &[String], flag: &str) -> Option<String> {
    rest.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

#[cfg(test)]
mod tests {
    use super::INTENTS_CATALOG;

    #[test]
    fn embedded_catalog_is_valid_json_with_expected_intents() {
        let value: serde_json::Value = serde_json::from_str(INTENTS_CATALOG).unwrap();
        let explore = value["modes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["mode"] == "explore")
            .expect("explore mode");
        let names: Vec<&str> = explore["intents"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["intent"].as_str())
            .collect();
        assert!(names.contains(&"task_localization_query"));
        assert!(names.contains(&"usage_boundary_query"));
    }
}
