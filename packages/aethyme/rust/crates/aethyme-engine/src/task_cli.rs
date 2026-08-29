//! Native `aethyme task` front end (python-retirement Phase 1).
//!
//! Replaces the delegated Click group `task pack|context|anchors|scope|
//! next|expand|explain`. JSON modes reuse [`crate::graph_cli::pretty_json`]
//! (Python's `json.dumps(indent=2)`) except `context --json-output`,
//! which Python emitted compact-with-spaces (`json.dumps(result)` —
//! `", "` / `": "` separators): [`compact_json_python_style`] replicates
//! that. `next` shares the relation renderer with the graph group;
//! `explain` renders from the redb-built context pack so task flows do not
//! rebuild an in-memory `RepositoryMap`.
//!
//! Golden-verified byte parity against the Python renderers.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::graph::navigation::{
    task_anchors_view_redb, task_expand_view_redb, task_next_view_redb, task_scope_view_redb,
};
use crate::graph_cli::{emit_completeness_signals, pretty_json, render_relation};
use crate::model::task::TaskInput;
use crate::pipeline::{build_context_pack_redb, build_context_pack_with_content_redb};
use crate::store::redb::graph_store::GraphStore;

/// Run `task <subcommand> ...`. `args` excludes the leading `task`.
pub fn run(args: &[String]) -> Result<(), String> {
    let Some(subcommand) = args.first() else {
        return Err(
            "missing task subcommand (pack | context | anchors | scope | next | expand | explain)"
                .to_string(),
        );
    };
    let rest = &args[1..];
    let json_output = rest.iter().any(|a| a == "--json-output");

    let repo = {
        let raw = opt_value(rest, "--repo")
            .ok_or_else(|| format!("usage: aethyme task {subcommand} --repo <path> ..."))?;
        let path = PathBuf::from(&raw);
        if !path.is_dir() {
            return Err(format!("repository path is not a directory: {raw}"));
        }
        path.canonicalize().map_err(|e| e.to_string())?
    };

    let task_text = || {
        opt_value(rest, "--task")
            .ok_or_else(|| format!("usage: aethyme task {subcommand} --repo <path> --task <text>"))
    };

    match subcommand.as_str() {
        "pack" => {
            let raw = pack_json(&repo, &task_text()?)?;
            if json_output {
                println!("{}", pretty_json(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                println!("{}", render_pack_summary(&payload));
            }
        }
        "context" => {
            let content_budget: usize = opt_value(rest, "--content-budget")
                .and_then(|s| s.parse().ok())
                .unwrap_or(80_000);
            let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
            let task = TaskInput::from_task_text(&task_text()?);
            let pack = build_context_pack_with_content_redb(&repo, &store, task, content_budget)
                .map_err(|e| e.to_string())?;
            let mut raw = Vec::new();
            crate::json::write_context_pack(&mut raw, &pack).map_err(|e| e.to_string())?;
            let raw = String::from_utf8(raw).map_err(|e| e.to_string())?;
            if json_output {
                println!("{}", compact_json_python_style(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                println!("{}", render_pack_summary(&payload));
            }
        }
        "anchors" => {
            let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
            let task = TaskInput::from_task_text(&task_text()?);
            let raw = crate::json::task_anchors_view(
                &task_anchors_view_redb(&store, &task).map_err(|e| e.to_string())?,
            );
            if json_output {
                println!("{}", pretty_json(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                let mut out = String::new();
                out.push_str(&format!(
                    "Task: {}\n",
                    payload["task"].as_str().unwrap_or_default()
                ));
                if let Some(anchors) = payload.get("anchors").and_then(Value::as_array) {
                    for anchor in anchors {
                        out.push_str(&format!(
                            "- {} ({}): {}\n",
                            anchor["id"].as_str().unwrap_or_default(),
                            anchor["kind"].as_str().unwrap_or_default(),
                            anchor["reason"].as_str().unwrap_or_default(),
                        ));
                    }
                }
                print!("{out}");
            }
        }
        "scope" => {
            let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
            let task = TaskInput::from_task_text(&task_text()?);
            let raw = crate::json::task_scope_view(
                &task_scope_view_redb(&store, &task).map_err(|e| e.to_string())?,
            );
            if json_output {
                println!("{}", pretty_json(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                print!("{}", render_scope(&payload));
            }
        }
        "next" => {
            let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
            let task = TaskInput::from_task_text(&task_text()?);
            let raw = crate::json::graph_relation(
                &task_next_view_redb(&store, &task).map_err(|e| e.to_string())?,
            );
            if json_output {
                println!("{}", pretty_json(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                print!("{}", render_relation(&payload));
            }
        }
        "expand" => {
            let node = opt_value(rest, "--node").ok_or_else(|| {
                "usage: aethyme task expand --repo <path> --node <target>".to_string()
            })?;
            let store = GraphStore::open_read_only(&repo).map_err(|e| e.to_string())?;
            let raw = crate::json::task_expand_view(
                &task_expand_view_redb(&store, &node).map_err(|e| e.to_string())?,
            );
            if json_output {
                println!("{}", pretty_json(&raw));
            } else {
                let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                print!("{}", render_task_expand(&payload));
            }
        }
        "explain" => {
            let task_value =
                opt_value(rest, "--task").unwrap_or_else(|| "Explain this repo".to_string());
            println!("{}", render_explain(&repo, &task_value)?);
        }
        other => return Err(format!("unsupported task subcommand: {other}")),
    }
    Ok(())
}

fn opt_value(rest: &[String], flag: &str) -> Option<String> {
    rest.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn pack_json(repo: &Path, task_text: &str) -> Result<String, String> {
    let store = GraphStore::open_read_only(repo).map_err(|e| e.to_string())?;
    let task = TaskInput::from_task_text(task_text);
    let pack = build_context_pack_redb(repo, &store, task).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    crate::json::write_context_pack(&mut raw, &pack).map_err(|e| e.to_string())?;
    String::from_utf8(raw).map_err(|e| e.to_string())
}

// ── Text renderers (ports of the Click / rendering/context_pack.py) ──

fn value_reason_line(item: &Value) -> String {
    if let Some(obj) = item.as_object() {
        let value = obj
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Value::Object(obj.clone()).to_string());
        match obj
            .get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            Some(reason) => format!("- {value} ({reason})\n"),
            None => format!("- {value}\n"),
        }
    } else {
        format!("- {}\n", item.as_str().unwrap_or_default())
    }
}

fn render_scope(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Task: {}\n",
        payload["task"].as_str().unwrap_or_default()
    ));
    for (key, heading) in [
        ("in_scope_files", "In-scope files:"),
        ("in_scope_areas", "In-scope areas:"),
        ("out_of_scope", "Out of scope:"),
    ] {
        if let Some(items) = payload.get(key).and_then(Value::as_array) {
            if !items.is_empty() {
                out.push_str(heading);
                out.push('\n');
                for item in items {
                    out.push_str(&value_reason_line(item));
                }
            }
        }
    }
    if let Some(risks) = payload.get("risks").and_then(Value::as_array) {
        if !risks.is_empty() {
            out.push_str("Risks:\n");
            for item in risks {
                out.push_str(&format!("- {}\n", item.as_str().unwrap_or_default()));
            }
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

fn render_task_expand(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Node: {}\n",
        payload["node"].as_str().unwrap_or_default()
    ));
    for section in ["dependencies", "impact", "docs", "configs", "risks"] {
        let Some(items) = payload.get(section).and_then(Value::as_array) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        let mut heading: String = section.chars().next().unwrap().to_uppercase().collect();
        heading.push_str(&section[1..]);
        out.push_str(&format!("{heading}:\n"));
        for item in items {
            out.push_str(&format!("- {}\n", item.as_str().unwrap_or_default()));
        }
    }
    emit_completeness_signals(payload, &mut out);
    out
}

fn render_pack_summary(pack: &Value) -> String {
    let mut lines: Vec<String> = vec![format!(
        "Task: {}",
        pack["task"]["raw"].as_str().unwrap_or_default()
    )];
    if let Some(anchors) = pack.get("anchors").and_then(Value::as_array) {
        if !anchors.is_empty() {
            lines.push("Anchors:".to_string());
            for anchor in anchors {
                lines.push(format!(
                    "- {} ({})",
                    anchor["id"].as_str().unwrap_or_default(),
                    anchor["reason"].as_str().unwrap_or_default(),
                ));
            }
        }
    }
    if let Some(navigation) = pack.get("navigation_order").and_then(Value::as_array) {
        if !navigation.is_empty() {
            lines.push("Navigation order:".to_string());
            for item in navigation {
                lines.push(format!("- {}", item.as_str().unwrap_or_default()));
            }
        }
    }
    if let Some(risks) = pack.get("risk_flags").and_then(Value::as_array) {
        if !risks.is_empty() {
            lines.push("High-risk areas:".to_string());
            for risk in risks {
                lines.push(format!(
                    "- {} ({}): {}",
                    risk["scope"].as_str().unwrap_or_default(),
                    risk["area"].as_str().unwrap_or_default(),
                    risk["reason"].as_str().unwrap_or_default(),
                ));
            }
        }
    }
    if let Some(confidence) = pack.get("confidence").and_then(Value::as_object) {
        let anchor = confidence.get("anchor_confidence").filter(|v| !v.is_null());
        let scope = confidence.get("scope_confidence").filter(|v| !v.is_null());
        if anchor.is_some() || scope.is_some() {
            let fmt = |v: Option<&Value>| {
                v.map(crate::graph_cli::render_number)
                    .unwrap_or_else(|| "n/a".to_string())
            };
            lines.push(format!(
                "Confidence: anchor={}, scope={}",
                fmt(anchor),
                fmt(scope)
            ));
        }
    }
    if let Some(budget) = pack.get("budget").and_then(Value::as_object) {
        let budget_fields = [
            "max_anchors",
            "max_files",
            "max_snippets",
            "dependency_depth",
            "impact_depth",
            "content_budget",
            "max_content_files",
            "max_lines_per_file",
        ];
        let rendered: Vec<String> = budget_fields
            .iter()
            .filter_map(|field| {
                budget
                    .get(*field)
                    .map(|v| format!("{field}={}", crate::graph_cli::render_number(v)))
            })
            .collect();
        if !rendered.is_empty() {
            lines.push(format!("Caps: {}", rendered.join(", ")));
        }
    }
    let mut capped: Vec<String> = Vec::new();
    if let Some(file_contents) = pack.get("file_contents").and_then(Value::as_array) {
        for file_content in file_contents {
            let Some(obj) = file_content.as_object() else {
                continue;
            };
            let (Some(end_line), Some(total_lines)) = (
                obj.get("end_line").and_then(Value::as_i64),
                obj.get("total_lines").and_then(Value::as_i64),
            ) else {
                continue;
            };
            if total_lines > end_line {
                let path = obj
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                capped.push(format!("{path} ({end_line}/{total_lines} lines)"));
            }
        }
    }
    if !capped.is_empty() {
        lines.push("Truncated content:".to_string());
        for item in capped.iter().take(5) {
            lines.push(format!("- {item}"));
        }
    }
    lines.join("\n")
}

fn render_explain(repo: &Path, task_text: &str) -> Result<String, String> {
    let pack: Value =
        serde_json::from_str(&pack_json(repo, task_text)?).map_err(|e| e.to_string())?;
    Ok(render_explain_from_pack(&pack))
}

fn render_explain_from_pack(pack: &Value) -> String {
    let summary = &pack["summary"];
    let snapshot = &summary["snapshot"];
    let languages = snapshot
        .get("languages")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|joined| !joined.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let top_level_dirs = snapshot
        .get("top_level_dirs")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let count =
        |count_key: &str| -> i64 { summary.get(count_key).and_then(Value::as_i64).unwrap_or(0) };
    let mut lines = vec![
        format!(
            "Task: {}",
            pack["task"]["raw"].as_str().unwrap_or("Explain this repo")
        ),
        format!("Languages: {languages}"),
        format!("Top-level directories: {top_level_dirs}"),
        format!("Files indexed: {}", count("files_count")),
        format!("Functions indexed: {}", count("functions_count")),
        format!("Classes indexed: {}", count("classes_count")),
        format!("Docs indexed: {}", count("docs_count")),
        format!("Configs indexed: {}", count("configs_count")),
    ];

    let overview = &pack["overview"];
    let str_list = |key: &str| -> Vec<String> {
        overview
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    let overview_docs = str_list("overview_docs");
    if let Some(first) = overview_docs.first() {
        lines.push(format!("README: {first}"));
    }
    for (key, heading, cap) in [
        ("code_areas", "Code areas:", 3),
        ("reference_areas", "Reference areas:", 3),
        ("subareas", "Key subareas:", 4),
        ("key_configs", "Key configs:", 3),
        ("entrypoints", "Entrypoints:", 3),
        ("representative_code_files", "Representative code:", 3),
        ("representative_docs", "Representative docs:", 3),
    ] {
        let items = str_list(key);
        if !items.is_empty() {
            lines.push(String::new());
            lines.push(heading.to_string());
            for item in items.iter().take(cap) {
                lines.push(format!("- {item}"));
            }
        }
    }
    if let Some(navigation) = pack.get("navigation_order").and_then(Value::as_array) {
        if !navigation.is_empty() {
            lines.push(String::new());
            lines.push("Navigation order:".to_string());
            for item in navigation.iter().take(5) {
                lines.push(format!("- {}", item.as_str().unwrap_or_default()));
            }
        }
    }
    lines.join("\n")
}

/// Python's `json.dumps(obj)` default: compact but with `", "` and
/// `": "` separators. Textual transform of the engine's no-space
/// compact JSON, preserving key order.
pub fn compact_json_python_style(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len() + compact.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
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
            ',' => out.push_str(", "),
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::compact_json_python_style;

    #[test]
    fn scope_render_surfaces_reason_fields() {
        // Ported from test_task_scope_non_json_renders_reason_fields when
        // the task group went native (Phase 1).
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"task":"Update auth flow",
                "in_scope_files":[{"value":"src/auth.py","reason":"anchor file"}],
                "in_scope_areas":[{"value":"src","reason":"contains anchor"}],
                "out_of_scope":[{"value":"docs","reason":"non-runtime"}],
                "risks":["auth regression"]}"#,
        )
        .unwrap();
        let rendered = super::render_scope(&payload);
        assert!(
            rendered.contains("- src/auth.py (anchor file)"),
            "{rendered}"
        );
        assert!(rendered.contains("- src (contains anchor)"), "{rendered}");
        assert!(rendered.contains("- docs (non-runtime)"), "{rendered}");
        assert!(rendered.contains("- auth regression"), "{rendered}");
    }

    #[test]
    fn pack_summary_surfaces_confidence_caps_truncation() {
        // Ported from test_render_pack_summary_includes_confidence_caps_
        // and_truncation (rendering/context_pack.py deleted in Phase 1).
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"task":{"raw":"Explain this repo"},
                "confidence":{"anchor_confidence":0.91,"scope_confidence":0.73},
                "budget":{"max_anchors":3,"max_files":5},
                "file_contents":[{"path":"src/big.py","end_line":120,"total_lines":300}]}"#,
        )
        .unwrap();
        let rendered = super::render_pack_summary(&payload);
        assert!(
            rendered.contains("Confidence: anchor=0.91, scope=0.73"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Caps: max_anchors=3, max_files=5"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- src/big.py (120/300 lines)"),
            "{rendered}"
        );
    }

    #[test]
    fn explain_renderer_uses_context_pack_summary() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"task":{"raw":"Explain this repo"},
                "summary":{
                    "snapshot":{"languages":["rust"],"top_level_dirs":["src"],"readme_path":"README.md"},
                    "files_count":2,
                    "functions_count":3,
                    "classes_count":1,
                    "docs_count":1,
                    "configs_count":1
                },
                "overview":{
                    "overview_docs":["README.md"],
                    "code_areas":["src"],
                    "reference_areas":["docs"],
                    "subareas":["src/graph"],
                    "entrypoints":["src/bin/aethyme.rs"],
                    "key_configs":["Cargo.toml"],
                    "representative_code_files":["src/lib.rs"],
                    "representative_docs":["README.md"]
                },
                "navigation_order":["src/lib.rs"],
                "risk_flags":[],
                "confidence":{"anchor_confidence":0.85,"scope_confidence":0.8},
                "budget":{"max_anchors":5,"max_files":8}}"#,
        )
        .unwrap();
        let rendered = super::render_explain_from_pack(&payload);

        assert!(rendered.contains("Task: Explain this repo"), "{rendered}");
        assert!(rendered.contains("Languages: rust"), "{rendered}");
        assert!(
            rendered.contains("Top-level directories: src"),
            "{rendered}"
        );
        assert!(rendered.contains("Files indexed: 2"), "{rendered}");
        assert!(rendered.contains("Functions indexed: 3"), "{rendered}");
        assert!(rendered.contains("README: README.md"), "{rendered}");
        assert!(rendered.contains("- src/lib.rs"), "{rendered}");
    }

    #[test]
    fn python_compact_separators() {
        assert_eq!(
            compact_json_python_style(r#"{"a":1,"b":["x,y","z:w"],"c":{}}"#),
            r#"{"a": 1, "b": ["x,y", "z:w"], "c": {}}"#
        );
    }
}
