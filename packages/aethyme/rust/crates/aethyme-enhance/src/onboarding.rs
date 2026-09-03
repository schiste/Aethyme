//! Deterministic repo-onboarding skill generation — byte-parity port of
//! `src/indexing/onboarding.py` (artifact building, skill rendering,
//! override init/validate, freshness, recommendation). The onboarding
//! CLI helpers dispatch here via `repo_cli` since the Phase 3 flip.
//!
//! All JSON is built as ordered [`Value`]s so `json.dumps(..., indent=2)`
//! byte shapes survive the port, including override-injected content.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::pyjson::{self, py_bool, Value};
use crate::util::{py_splitlines, resolve_path};

pub const ONBOARDING_JSON_PATH: &str = ".aethyme/generated/onboarding.json";
pub const ACT_STARTER_JSON_PATH: &str = ".aethyme/generated/act-starter.json";
pub const ONBOARDING_CLAUDE_PATH: &str = ".claude/skills/repo-onboarding/SKILL.md";
pub const ONBOARDING_CODEX_PATH: &str = ".codex/skills/repo-onboarding/SKILL.md";
pub const ACT_CLAUDE_PATH: &str = ".claude/skills/repo-act/SKILL.md";
pub const ACT_CODEX_PATH: &str = ".codex/skills/repo-act/SKILL.md";
pub const ONBOARDING_OVERRIDE_PATH: &str = ".aethyme/overrides/onboarding.json";

const GENERATED_DEPLOY_PATHS: [&str; 4] = [
    "AGENTS.md",
    "CLAUDE.md",
    ".claude/settings.local.json",
    ".claude/hooks/aethyme-load-context.sh",
];
const GENERATED_DEPLOY_PREFIXES: [&str; 7] = [
    ".aethyme/generated/",
    ".claude/skills/aethyme/",
    ".codex/skills/aethyme/",
    ".claude/skills/repo-onboarding/",
    ".codex/skills/repo-onboarding/",
    ".claude/skills/repo-act/",
    ".codex/skills/repo-act/",
];

/// `KeyError`-style lookup: Python indexes (`artifact["repo"]`) crash on
/// missing keys; the port surfaces the same condition as a command error.
fn req<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value.get(key).ok_or_else(|| format!("KeyError: '{key}'"))
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn str_list(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| Value::str(*s)).collect())
}

/// `', '.join(items)` with Python str interpolation per item.
fn join_items(value: &Value, sep: &str) -> String {
    match value.as_array() {
        Some(items) => items
            .iter()
            .map(Value::py_str)
            .collect::<Vec<_>>()
            .join(sep),
        None => String::new(),
    }
}

/// Return deterministic onboarding artifacts for `repo_path` as ordered
/// (relative path, content) pairs — the Python `expected_onboarding_files`
/// dict in insertion order.
pub fn expected_onboarding_files(repo_path: &Path) -> Result<Vec<(String, String)>, String> {
    let artifact = build_onboarding_artifact(repo_path)?;
    let onboarding_markdown = render_onboarding_skill(&artifact)?;
    let act_artifact = build_act_starter_artifact(&artifact)?;
    let act_markdown = render_act_skill(&act_artifact)?;
    Ok(vec![
        (
            ONBOARDING_JSON_PATH.to_string(),
            format!("{}\n", pyjson::dumps_indent2(&artifact)),
        ),
        (
            ACT_STARTER_JSON_PATH.to_string(),
            format!("{}\n", pyjson::dumps_indent2(&act_artifact)),
        ),
        (
            ONBOARDING_CLAUDE_PATH.to_string(),
            onboarding_markdown.clone(),
        ),
        (ONBOARDING_CODEX_PATH.to_string(), onboarding_markdown),
        (ACT_CLAUDE_PATH.to_string(), act_markdown.clone()),
        (ACT_CODEX_PATH.to_string(), act_markdown),
    ])
}

/// Build a deterministic onboarding artifact for a repository.
pub fn build_onboarding_artifact(repo_path: &Path) -> Result<Value, String> {
    let repo_path = resolve_path(repo_path);
    let snapshot = snapshot_metadata(&repo_path)?;
    let surface_files = OnboardingFileSnapshot::discover(&repo_path)?;

    let manifests = detect_manifests(&repo_path);
    let package_manager = primary_package_manager(&repo_path, &manifests);
    let commands = collect_commands(&repo_path, &manifests, package_manager)?;
    let primary_commands = primary_commands_map(&commands);
    let areas = collect_areas(&surface_files);
    let entrypoints = collect_entrypoints(&repo_path, &manifests, &surface_files)?;
    let primary_entrypoints = primary_entrypoints_map(&entrypoints);
    let caution_zones = collect_caution_zones(&surface_files);
    let repo_kind = infer_repo_kind(&repo_path, &manifests, &areas);
    let languages = infer_languages(&repo_path, &manifests);
    let overrides = load_overrides(&repo_path);

    let mut artifact = obj(vec![
        ("schema_version", Value::str("aethyme-onboarding-v1")),
        (
            "repo",
            obj(vec![
                ("name", Value::str(snapshot.repo_name.clone())),
                // The artifact is repository policy that is expected to be
                // committed and consumed from other clones. Never persist the
                // machine-specific checkout path.
                ("root", Value::str(".")),
                ("kind", Value::str(repo_kind)),
                (
                    "languages",
                    Value::Array(languages.iter().map(|l| Value::str(*l)).collect()),
                ),
                ("package_manager", Value::str(package_manager)),
                (
                    "manifests",
                    Value::Array(manifests.iter().map(|m| Value::str(m.clone())).collect()),
                ),
            ]),
        ),
        ("commands", Value::Array(commands.clone())),
        ("primary_commands", primary_commands),
        ("areas", Value::Array(areas)),
        ("entrypoints", Value::Array(entrypoints)),
        ("primary_entrypoints", primary_entrypoints),
        ("caution_zones", Value::Array(caution_zones)),
        ("navigation_recipes", navigation_recipes()),
        ("summon", summon_policy()),
        (
            "freshness",
            obj(vec![
                ("source_digest", Value::str(snapshot.source_digest)),
                // Commit identity is optional informational provenance, not
                // part of the canonical artifact. Leaving the slot null keeps
                // a commit or rebase from changing generated policy bytes.
                ("source_commit", Value::Null),
                (
                    "source_file_count",
                    Value::Int(snapshot.source_file_count as i128),
                ),
                ("generator", Value::str("deterministic")),
            ]),
        ),
    ]);
    apply_overrides(&mut artifact, &overrides)?;
    let telemetry = generation_telemetry(&artifact, &overrides);
    artifact.set("telemetry", telemetry);
    Ok(artifact)
}

/// Build a deterministic Act starter artifact from onboarding facts.
pub fn build_act_starter_artifact(onboarding_artifact: &Value) -> Result<Value, String> {
    let repo = req(onboarding_artifact, "repo")?;
    let empty = Value::Array(vec![]);
    let empty_obj = Value::object();
    let commands = onboarding_artifact.get("commands").unwrap_or(&empty);
    let primary_commands = onboarding_artifact
        .get("primary_commands")
        .unwrap_or(&empty_obj);
    let entrypoints = onboarding_artifact.get("entrypoints").unwrap_or(&empty);
    let primary_entrypoints = onboarding_artifact
        .get("primary_entrypoints")
        .unwrap_or(&empty_obj);
    let caution_zones = onboarding_artifact.get("caution_zones").unwrap_or(&empty);

    // `primary.get(label) or _first_command_of_kind(...)` — Python `or`
    // falls through on falsy (None / "").
    let primary_or_first = |label: &str, kind: &str| -> Result<Value, String> {
        if let Some(value) = primary_commands.get(label) {
            if value.truthy() {
                return Ok(value.clone());
            }
        }
        if let Some(found) = first_command_of_kind(commands, kind)? {
            return Ok(found);
        }
        Ok(Value::Null)
    };

    let entrypoints_slice = |limit: usize| -> Value {
        match entrypoints.as_array() {
            Some(items) => Value::Array(items.iter().take(limit).cloned().collect()),
            None => entrypoints.clone(),
        }
    };
    let caution_slice = match caution_zones.as_array() {
        Some(items) => Value::Array(items.iter().take(5).cloned().collect()),
        None => caution_zones.clone(),
    };
    let len_of = |value: &Value| -> i128 {
        match value {
            Value::Array(items) => items.len() as i128,
            Value::Object(entries) => entries.len() as i128,
            _ => 0,
        }
    };

    Ok(obj(vec![
        ("schema_version", Value::str("aethyme-act-starter-v1")),
        (
            "repo",
            obj(vec![
                ("name", req(repo, "name")?.clone()),
                ("kind", req(repo, "kind")?.clone()),
            ]),
        ),
        ("recommended_mode", Value::str("repo-act")),
        (
            "starter_checklists",
            obj(vec![
                (
                    "debugging",
                    str_list(&[
                        "Load repo-onboarding first if the repository or area is unfamiliar.",
                        "Run Aethyme Explore on the user task before broad manual search.",
                        "Verify answer candidates before concluding when trust_policy is not answer-safe.",
                        "Identify the nearest fast test or validation command before editing.",
                        "Inspect caution zones before changing generated or vendored areas.",
                    ]),
                ),
                (
                    "change_validation",
                    str_list(&[
                        "Run the fastest relevant test command first.",
                        "Check likely entrypoints and impacted callers before finishing.",
                        "Re-run lint/build only after the focused validation passes.",
                        "Document any override-based maintainer note that affected the change plan.",
                    ]),
                ),
            ]),
        ),
        (
            "commands",
            obj(vec![
                ("fast_test", primary_or_first("fast_test", "test")?),
                // Python: direct `.get("full_test")` with no truthiness
                // fallback — a falsy-but-present value passes through.
                (
                    "full_test",
                    primary_commands
                        .get("full_test")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                ("lint", primary_or_first("lint", "lint")?),
                ("build", primary_or_first("build", "build")?),
                ("dev", primary_or_first("dev", "dev")?),
            ]),
        ),
        ("entrypoints", entrypoints_slice(3)),
        ("primary_entrypoints", primary_entrypoints.clone()),
        ("caution_zones", caution_slice),
        (
            "telemetry",
            obj(vec![
                ("derived_from", Value::str(ONBOARDING_JSON_PATH)),
                ("entrypoint_count", Value::Int(len_of(entrypoints))),
                (
                    "primary_entrypoint_count",
                    Value::Int(len_of(primary_entrypoints)),
                ),
                ("caution_zone_count", Value::Int(len_of(caution_zones))),
            ]),
        ),
    ]))
}

/// Render the onboarding artifact into an agent-facing skill.
pub fn render_onboarding_skill(artifact: &Value) -> Result<String, String> {
    let repo = req(artifact, "repo")?;
    let commands = req(artifact, "commands")?;
    let empty_obj = Value::object();
    let primary_commands = match artifact.get("primary_commands") {
        Some(v) if v.truthy() => v,
        _ => &empty_obj,
    };
    let areas = req(artifact, "areas")?;
    let entrypoints = req(artifact, "entrypoints")?;
    let primary_entrypoints = match artifact.get("primary_entrypoints") {
        Some(v) if v.truthy() => v,
        _ => &empty_obj,
    };
    let caution_zones = req(artifact, "caution_zones")?;
    let recipes = req(artifact, "navigation_recipes")?;
    let empty_list = Value::Array(vec![]);
    let notes = match artifact.get("notes") {
        Some(v) if v.truthy() => v,
        _ => &empty_list,
    };
    let summon = req(artifact, "summon")?;
    let freshness = req(artifact, "freshness")?;
    let telemetry = req(artifact, "telemetry")?;

    let mut lines: Vec<String> = vec![
        "---".to_string(),
        "name: repo-onboarding".to_string(),
        format!("description: {}", req(summon, "description")?.py_str()),
        "---".to_string(),
        String::new(),
        format!("# Repo Onboarding: {}", req(repo, "name")?.py_str()),
        String::new(),
        "## When to Use".to_string(),
        String::new(),
        "- Load this skill first when the repository is unfamiliar or the request is broad."
            .to_string(),
        format!(
            "- Recommended when: {}.",
            join_items(req(summon, "recommended_when")?, ", ")
        ),
        format!(
            "- Skip when: {}.",
            join_items(req(summon, "skip_when")?, ", ")
        ),
        "- Use `.codex/skills/aethyme/SKILL.md` or `.claude/skills/aethyme/SKILL.md` for Aethyme's short operating contract after orientation; load its `references/` files only when needed."
            .to_string(),
        String::new(),
        "## Repo Identity".to_string(),
        String::new(),
        format!("- Kind: `{}`", req(repo, "kind")?.py_str()),
        {
            let joined = join_items(req(repo, "languages")?, ", ");
            format!(
                "- Languages: `{}`",
                if joined.is_empty() { "unknown".to_string() } else { joined }
            )
        },
        format!(
            "- Package manager: `{}`",
            req(repo, "package_manager")?.py_str()
        ),
    ];

    if req(repo, "manifests")?.truthy() {
        lines.push(format!(
            "- Key manifests: `{}`",
            join_items(req(repo, "manifests")?, ", ")
        ));
    }

    lines.push(String::new());
    lines.push("## Start Here".to_string());
    lines.push(String::new());
    if primary_commands.truthy() {
        for label in ["install", "dev", "fast_test", "full_test", "lint", "build"] {
            if let Some(command) = primary_commands.get(label) {
                if command.truthy() {
                    lines.push(format!("- `{label}`: `{}`", command.py_str()));
                }
            }
        }
        lines.push(String::new());
        lines.push("## Supporting Commands".to_string());
        lines.push(String::new());
    }
    if let Some(items) = commands.as_array() {
        for command in items.iter().take(5) {
            lines.push(format!(
                "- `{}` ({}; {} confidence from `{}`)",
                req(command, "command")?.py_str(),
                req(command, "kind")?.py_str(),
                req(command, "confidence")?.py_str(),
                req(command, "source")?.py_str(),
            ));
        }
    }

    if primary_entrypoints.truthy() {
        lines.push(String::new());
        lines.push("## Entrypoints".to_string());
        lines.push(String::new());
        for label in ["app", "cli", "worker", "test"] {
            if let Some(entrypoint) = primary_entrypoints.get(label) {
                if entrypoint.truthy() {
                    lines.push(format!(
                        "- `{label}`: `{}` ({}; {} confidence)",
                        req(entrypoint, "path")?.py_str(),
                        req(entrypoint, "reason")?.py_str(),
                        entrypoint
                            .get("confidence")
                            .unwrap_or(&Value::str("medium"))
                            .py_str(),
                    ));
                }
            }
        }
        lines.push(String::new());
        lines.push("## Additional Entrypoints".to_string());
        lines.push(String::new());
    }
    if entrypoints.truthy() {
        if let Some(items) = entrypoints.as_array() {
            for entrypoint in items.iter().take(5) {
                lines.push(format!(
                    "- `{}` ({}; role={}; {}; {} confidence)",
                    req(entrypoint, "path")?.py_str(),
                    req(entrypoint, "kind")?.py_str(),
                    entrypoint
                        .get("role")
                        .unwrap_or(&Value::str("unknown"))
                        .py_str(),
                    req(entrypoint, "reason")?.py_str(),
                    entrypoint
                        .get("confidence")
                        .unwrap_or(&Value::str("medium"))
                        .py_str(),
                ));
            }
        }
    }

    if areas.truthy() {
        lines.push(String::new());
        lines.push("## Repo Map".to_string());
        lines.push(String::new());
        if let Some(items) = areas.as_array() {
            for area in items.iter().take(8) {
                lines.push(format!(
                    "- `{}` ({}; {}; {} confidence)",
                    req(area, "path")?.py_str(),
                    req(area, "role")?.py_str(),
                    req(area, "reason")?.py_str(),
                    area.get("confidence")
                        .unwrap_or(&Value::str("medium"))
                        .py_str(),
                ));
            }
        }
    }

    lines.push(String::new());
    lines.push("## Aethyme Recipes".to_string());
    lines.push(String::new());
    if let Some(items) = recipes.as_array() {
        for recipe in items {
            lines.push(format!("- `{}`", req(recipe, "command")?.py_str()));
            lines.push(format!("  Purpose: {}", req(recipe, "purpose")?.py_str()));
        }
    }

    if caution_zones.truthy() {
        lines.push(String::new());
        lines.push("## Caution Zones".to_string());
        lines.push(String::new());
        if let Some(items) = caution_zones.as_array() {
            for zone in items.iter().take(6) {
                lines.push(format!(
                    "- `{}` ({})",
                    req(zone, "path")?.py_str(),
                    req(zone, "reason")?.py_str(),
                ));
            }
        }
    }

    if notes.truthy() {
        lines.push(String::new());
        lines.push("## Maintainer Notes".to_string());
        lines.push(String::new());
        if let Some(items) = notes.as_array() {
            for note in items.iter().take(8) {
                lines.push(format!("- {}", note.py_str()));
            }
        }
    }

    lines.push(String::new());
    lines.push("## Freshness".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- Source digest: `{}`",
        req(freshness, "source_digest")?.py_str()
    ));
    let source_commit = req(freshness, "source_commit")?;
    if source_commit.truthy() {
        lines.push(format!("- Source commit: `{}`", source_commit.py_str()));
    }
    lines.push(format!(
        "- Tracked source files: `{}`",
        req(freshness, "source_file_count")?.py_str()
    ));
    lines.push(format!(
        "- Overrides applied: `{}`",
        match req(telemetry, "overrides_applied")? {
            Value::Bool(b) => py_bool(*b).to_string(),
            other => other.py_str(),
        }
    ));
    lines.push(format!(
        "- Sections generated: `{}`",
        join_items(req(telemetry, "sections")?, ", ")
    ));

    Ok(format!("{}\n", lines.join("\n")))
}

/// Render the Act starter artifact into an agent-facing skill.
pub fn render_act_skill(artifact: &Value) -> Result<String, String> {
    let repo = req(artifact, "repo")?;
    let commands = req(artifact, "commands")?;
    let entrypoints = req(artifact, "entrypoints")?;
    let empty_obj = Value::object();
    let primary_entrypoints = match artifact.get("primary_entrypoints") {
        Some(v) if v.truthy() => v,
        _ => &empty_obj,
    };
    let caution_zones = req(artifact, "caution_zones")?;
    let checklists = req(artifact, "starter_checklists")?;

    let mut lines: Vec<String> = vec![
        "---".to_string(),
        "name: repo-act".to_string(),
        "description: Use after repo-onboarding or Explore when moving from orientation into execution planning, debugging checklists, and validation steps. Skip when only broad repo orientation is needed."
            .to_string(),
        "---".to_string(),
        String::new(),
        format!("# Repo Act: {}", req(repo, "name")?.py_str()),
        String::new(),
        "## When to Use".to_string(),
        String::new(),
        "- Load this after repo-onboarding when the next step is execution or validation planning."
            .to_string(),
        "- Skip it for broad repo overview questions with no action plan yet.".to_string(),
        String::new(),
        "## Debugging Checklist".to_string(),
        String::new(),
    ];
    if let Some(items) = req(checklists, "debugging")?.as_array() {
        for item in items {
            lines.push(format!("- {}", item.py_str()));
        }
    }
    lines.push(String::new());
    lines.push("## Validation Checklist".to_string());
    lines.push(String::new());
    if let Some(items) = req(checklists, "change_validation")?.as_array() {
        for item in items {
            lines.push(format!("- {}", item.py_str()));
        }
    }
    lines.push(String::new());
    lines.push("## Useful Commands".to_string());
    lines.push(String::new());
    if let Some(entries) = commands.as_object() {
        for (label, command) in entries {
            if command.truthy() {
                lines.push(format!("- `{label}`: `{}`", command.py_str()));
            }
        }
    }
    if primary_entrypoints.truthy() {
        lines.push(String::new());
        lines.push("## Primary Entrypoints".to_string());
        lines.push(String::new());
        if let Some(entries) = primary_entrypoints.as_object() {
            for (label, entrypoint) in entries {
                lines.push(format!(
                    "- `{label}`: `{}` ({})",
                    req(entrypoint, "path")?.py_str(),
                    req(entrypoint, "reason")?.py_str(),
                ));
            }
        }
    }
    if entrypoints.truthy() {
        lines.push(String::new());
        lines.push("## Likely Entrypoints".to_string());
        lines.push(String::new());
        if let Some(items) = entrypoints.as_array() {
            for entrypoint in items {
                lines.push(format!(
                    "- `{}` ({})",
                    req(entrypoint, "path")?.py_str(),
                    req(entrypoint, "reason")?.py_str(),
                ));
            }
        }
    }
    if caution_zones.truthy() {
        lines.push(String::new());
        lines.push("## Caution Zones".to_string());
        lines.push(String::new());
        if let Some(items) = caution_zones.as_array() {
            for zone in items {
                lines.push(format!(
                    "- `{}` ({})",
                    req(zone, "path")?.py_str(),
                    req(zone, "reason")?.py_str(),
                ));
            }
        }
    }
    Ok(format!("{}\n", lines.join("\n")))
}

// ── repository fact collection ──────────────────────────────────────────────

const MANIFEST_NAMES: [&str; 14] = [
    "package.json",
    "pnpm-workspace.yaml",
    "pyproject.toml",
    "Cargo.toml",
    "composer.json",
    "go.mod",
    "Makefile",
    "justfile",
    "Justfile",
    "Procfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

fn detect_manifests(repo_path: &Path) -> Vec<String> {
    MANIFEST_NAMES
        .iter()
        .filter(|name| repo_path.join(name).exists())
        .map(|name| name.to_string())
        .collect()
}

fn has_manifest(manifests: &[String], name: &str) -> bool {
    manifests.iter().any(|m| m == name)
}

fn primary_package_manager(repo_path: &Path, manifests: &[String]) -> &'static str {
    if has_manifest(manifests, "pnpm-workspace.yaml") || repo_path.join("pnpm-lock.yaml").exists() {
        return "pnpm";
    }
    if repo_path.join("package-lock.json").exists() {
        return "npm";
    }
    if repo_path.join("yarn.lock").exists() {
        return "yarn";
    }
    if has_manifest(manifests, "Cargo.toml") {
        return "cargo";
    }
    if has_manifest(manifests, "composer.json") {
        return "composer";
    }
    if has_manifest(manifests, "go.mod") {
        return "go";
    }
    if has_manifest(manifests, "pyproject.toml") {
        return "python";
    }
    if has_manifest(manifests, "Makefile") {
        return "make";
    }
    if has_manifest(manifests, "justfile") || has_manifest(manifests, "Justfile") {
        return "just";
    }
    "unknown"
}

struct CommandAdder {
    commands: Vec<Value>,
    seen: HashSet<String>,
}

impl CommandAdder {
    fn new() -> Self {
        CommandAdder {
            commands: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn add(&mut self, kind: &str, command: &str, source: &str, confidence: &str) {
        if self.seen.contains(command) {
            return;
        }
        self.seen.insert(command.to_string());
        self.commands.push(obj(vec![
            ("kind", Value::str(kind)),
            ("command", Value::str(command)),
            ("source", Value::str(source)),
            ("confidence", Value::str(confidence)),
        ]));
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_json_file(path: &Path) -> Result<Value, String> {
    pyjson::loads(&read_text(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

fn collect_commands(
    repo_path: &Path,
    manifests: &[String],
    package_manager: &str,
) -> Result<Vec<Value>, String> {
    let mut adder = CommandAdder::new();

    if matches!(package_manager, "pnpm" | "npm" | "yarn") && repo_path.join("package.json").exists()
    {
        let package_json = load_json_file(&repo_path.join("package.json"))?;
        let empty = Value::object();
        let scripts = match package_json.get("scripts") {
            Some(v) if v.truthy() => v.clone(),
            _ => empty,
        };
        let install_cmd = match package_manager {
            "pnpm" => {
                if repo_path.join("pnpm-lock.yaml").exists() {
                    "pnpm install --frozen-lockfile"
                } else {
                    "pnpm install"
                }
            }
            "npm" => "npm install",
            _ => {
                if repo_path.join(".yarnrc.yml").exists() {
                    "yarn install --immutable"
                } else {
                    "yarn install"
                }
            }
        };
        adder.add("install", install_cmd, "package.json", "high");
        let prefix = if package_manager == "npm" {
            "npm run".to_string()
        } else {
            package_manager.to_string()
        };
        for script_name in ["test", "lint", "dev", "build", "start"] {
            if scripts.get(script_name).is_some() {
                let mapped_kind = match script_name {
                    "test" => "fast_test",
                    "start" => "dev",
                    other => other,
                };
                adder.add(
                    mapped_kind,
                    &format!("{prefix} {script_name}"),
                    &format!("package.json:scripts.{script_name}"),
                    "high",
                );
            }
        }
        for script_name in ["test:unit", "test:quick", "test:fast"] {
            if scripts.get(script_name).is_some() {
                adder.add(
                    "fast_test",
                    &format!("{prefix} {script_name}"),
                    &format!("package.json:scripts.{script_name}"),
                    "high",
                );
            }
        }
        for script_name in ["test:integration", "test:e2e", "test:ci", "test:full"] {
            if scripts.get(script_name).is_some() {
                adder.add(
                    "full_test",
                    &format!("{prefix} {script_name}"),
                    &format!("package.json:scripts.{script_name}"),
                    "high",
                );
            }
        }
        if scripts.truthy() {
            if let Some(primary) = guess_primary_script(&scripts) {
                adder.add(
                    "primary-script",
                    &format!("{package_manager} {primary}"),
                    &format!("package.json:scripts.{primary}"),
                    "medium",
                );
            }
        }
    }

    if has_manifest(manifests, "pyproject.toml") {
        adder.add("fast_test", "pytest", "pyproject.toml", "medium");
        adder.add("lint", "ruff check .", "pyproject.toml", "medium");
        adder.add(
            "install",
            "python -m pip install -e .",
            "pyproject.toml",
            "medium",
        );
    }

    if has_manifest(manifests, "Cargo.toml") {
        adder.add("build", "cargo build", "Cargo.toml", "high");
        adder.add("fast_test", "cargo test", "Cargo.toml", "high");
        adder.add("dev", "cargo run", "Cargo.toml", "medium");
    }

    if has_manifest(manifests, "composer.json") {
        adder.add("install", "composer install", "composer.json", "high");
        let composer_json = load_json_file(&repo_path.join("composer.json"))?;
        let empty = Value::object();
        let scripts = match composer_json.get("scripts") {
            Some(v) if v.truthy() => v.clone(),
            _ => empty,
        };
        for script_name in ["test", "lint"] {
            if scripts.get(script_name).is_some() {
                let mapped_kind = if script_name == "test" {
                    "fast_test"
                } else {
                    script_name
                };
                adder.add(
                    mapped_kind,
                    &format!("composer {script_name}"),
                    &format!("composer.json:scripts.{script_name}"),
                    "high",
                );
            }
        }
    }

    if has_manifest(manifests, "Makefile") {
        let targets = make_targets(&repo_path.join("Makefile"))?;
        for script_name in ["install", "lint", "dev", "build"] {
            if targets.contains(script_name) {
                adder.add(
                    script_name,
                    &format!("make {script_name}"),
                    &format!("Makefile:{script_name}"),
                    "medium",
                );
            }
        }
        for script_name in ["test", "unit-test", "test-fast", "quick-test"] {
            if targets.contains(script_name) {
                adder.add(
                    "fast_test",
                    &format!("make {script_name}"),
                    &format!("Makefile:{script_name}"),
                    "medium",
                );
            }
        }
        for script_name in ["test-all", "integration-test", "e2e-test", "full-test"] {
            if targets.contains(script_name) {
                adder.add(
                    "full_test",
                    &format!("make {script_name}"),
                    &format!("Makefile:{script_name}"),
                    "medium",
                );
            }
        }
    }

    for justfile_name in ["justfile", "Justfile"] {
        if has_manifest(manifests, justfile_name) {
            let targets = make_targets(&repo_path.join(justfile_name))?;
            for script_name in ["install", "lint", "dev", "build"] {
                if targets.contains(script_name) {
                    adder.add(
                        script_name,
                        &format!("just {script_name}"),
                        &format!("{justfile_name}:{script_name}"),
                        "high",
                    );
                }
            }
            for script_name in ["test", "unit-test", "test-fast", "quick-test"] {
                if targets.contains(script_name) {
                    adder.add(
                        "fast_test",
                        &format!("just {script_name}"),
                        &format!("{justfile_name}:{script_name}"),
                        "high",
                    );
                }
            }
            for script_name in ["test-all", "integration-test", "e2e-test", "full-test"] {
                if targets.contains(script_name) {
                    adder.add(
                        "full_test",
                        &format!("just {script_name}"),
                        &format!("{justfile_name}:{script_name}"),
                        "high",
                    );
                }
            }
        }
    }

    for inferred in commands_from_procfile(repo_path)? {
        adder.add(&inferred.0, &inferred.1, &inferred.2, &inferred.3);
    }
    for inferred in commands_from_compose(repo_path)? {
        adder.add(&inferred.0, &inferred.1, &inferred.2, &inferred.3);
    }
    for inferred in commands_from_github_actions(repo_path) {
        adder.add(&inferred.0, &inferred.1, &inferred.2, &inferred.3);
    }

    let mut ranked = adder.commands;
    ranked.sort_by(|a, b| {
        let key = |c: &Value| {
            (
                kind_priority(&c.get("kind").map(Value::py_str).unwrap_or_default()),
                -confidence_rank(&c.get("confidence").map(Value::py_str).unwrap_or_default()),
                c.get("source").map(Value::py_str).unwrap_or_default(),
                c.get("command").map(Value::py_str).unwrap_or_default(),
            )
        };
        key(a).cmp(&key(b))
    });
    ranked.truncate(16);
    Ok(ranked)
}

fn make_targets(makefile_path: &Path) -> Result<HashSet<String>, String> {
    let mut targets = HashSet::new();
    let text = read_text(makefile_path)?;
    for line in py_splitlines(&text) {
        if !line.contains(':') || line.starts_with('\t') || line.starts_with('#') {
            continue;
        }
        let target = line
            .split_once(':')
            .map(|(head, _)| head)
            .unwrap_or(line)
            .trim();
        if !target.is_empty() && !target.contains(' ') && !target.starts_with('.') {
            targets.insert(target.to_string());
        }
    }
    Ok(targets)
}

const AREA_ROLE_MAP: &[(&str, &str)] = &[
    ("src", "source"),
    ("lib", "source"),
    ("app", "application"),
    ("apps", "application"),
    ("packages", "workspace"),
    ("services", "workspace"),
    ("backend", "service"),
    ("frontend", "service"),
    ("cmd", "entrypoints"),
    ("internal", "internal"),
    ("includes", "source"),
    ("tests", "tests"),
    ("test", "tests"),
    ("__tests__", "tests"),
    ("spec", "tests"),
    ("specs", "tests"),
    ("docs", "docs"),
    ("doc", "docs"),
    ("scripts", "tooling"),
    ("tools", "tooling"),
    ("tooling", "tooling"),
    ("bin", "tooling"),
    (".github", "automation"),
    ("infra", "infrastructure"),
    ("deploy", "infrastructure"),
    ("deployments", "infrastructure"),
    ("docker", "infrastructure"),
    ("terraform", "infrastructure"),
    ("k8s", "infrastructure"),
    ("helm", "infrastructure"),
    ("public", "assets"),
    ("static", "assets"),
    ("assets", "assets"),
    ("vendor", "generated_or_vendor"),
    ("third_party", "generated_or_vendor"),
    ("generated", "generated_or_vendor"),
    ("dist", "generated_or_vendor"),
    ("build", "generated_or_vendor"),
    ("coverage", "generated_or_vendor"),
    ("target", "generated_or_vendor"),
    ("node_modules", "generated_or_vendor"),
];

fn area_role_reason(role: &str) -> &'static str {
    match role {
        "source" => "conventional source directory",
        "application" => "top-level application area",
        "workspace" => "workspace-style package container",
        "service" => "service or deployable area",
        "tests" => "conventional test directory",
        "docs" => "documentation area",
        "tooling" => "developer tooling or scripts",
        "automation" => "automation and CI configuration",
        "infrastructure" => "deployment or infrastructure configuration",
        "assets" => "public assets or static files",
        "generated_or_vendor" => "generated output or vendored dependencies",
        "entrypoints" => "binary or service entrypoint area",
        "internal" => "internal implementation area",
        _ => unreachable!("all mapped roles covered"),
    }
}

struct OnboardingFileSnapshot {
    paths: HashSet<String>,
}

impl OnboardingFileSnapshot {
    fn discover(repo_path: &Path) -> Result<Self, String> {
        if let Some(output) = run_git_bytes(repo_path, &["ls-files", "--cached", "-z"]) {
            return Ok(Self {
                paths: parse_git_paths_z(&output).into_iter().collect(),
            });
        }
        if repo_path.join(".git").exists() {
            return Err("git ls-files failed while building onboarding surfaces".to_string());
        }

        let mut discovered = Vec::new();
        collect_recursive(repo_path, &mut discovered)?;
        let paths = discovered
            .into_iter()
            .filter(|path| path.is_file())
            .filter_map(|path| {
                path.strip_prefix(repo_path).ok().map(|relative| {
                    relative
                        .components()
                        .map(|component| component.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/")
                })
            })
            .collect();
        Ok(Self { paths })
    }

    fn contains_file(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    fn contains_directory(&self, path: &str) -> bool {
        let prefix = format!("{path}/");
        self.paths
            .iter()
            .any(|candidate| candidate.starts_with(&prefix))
    }

    fn contains_kind(&self, path: &str, kind: &str) -> bool {
        match kind {
            "directory" => self.contains_directory(path),
            _ => self.contains_file(path),
        }
    }

    fn top_level_directories(&self) -> Vec<String> {
        let mut directories = self
            .paths
            .iter()
            .filter_map(|path| {
                path.split_once('/')
                    .map(|(directory, _)| directory.to_string())
            })
            .collect::<Vec<_>>();
        directories.sort();
        directories.dedup();
        directories
    }
}

fn parse_git_paths_z(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn collect_areas(surface_files: &OnboardingFileSnapshot) -> Vec<Value> {
    let mut areas = Vec::new();
    for name in surface_files.top_level_directories() {
        let Some((_, role)) = AREA_ROLE_MAP.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        areas.push(obj(vec![
            ("path", Value::str(name.clone())),
            ("role", Value::str(*role)),
            ("reason", Value::str(area_role_reason(role))),
            ("confidence", Value::str("high")),
        ]));
    }
    areas
}

struct EntrypointAdder {
    candidates: Vec<Value>,
    seen: HashSet<(String, String)>,
}

impl EntrypointAdder {
    fn new() -> Self {
        EntrypointAdder {
            candidates: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn add(&mut self, path: &str, kind: &str, role: &str, reason: &str, confidence: &str) {
        let key = (path.to_string(), role.to_string());
        if self.seen.contains(&key) {
            return;
        }
        self.seen.insert(key);
        self.candidates.push(obj(vec![
            ("path", Value::str(path)),
            ("kind", Value::str(kind)),
            ("role", Value::str(role)),
            ("reason", Value::str(reason)),
            ("confidence", Value::str(confidence)),
        ]));
    }
}

fn collect_entrypoints(
    repo_path: &Path,
    manifests: &[String],
    surface_files: &OnboardingFileSnapshot,
) -> Result<Vec<Value>, String> {
    let mut adder = EntrypointAdder::new();

    let conventional_roots: &[(&str, &str, &str, &str, &str)] = &[
        (
            "main.py",
            "file",
            "app",
            "conventional Python application entrypoint",
            "medium",
        ),
        (
            "app.py",
            "file",
            "app",
            "common Python application entrypoint",
            "medium",
        ),
        (
            "manage.py",
            "file",
            "cli",
            "common framework management entrypoint",
            "high",
        ),
        (
            "main.go",
            "file",
            "app",
            "conventional Go application entrypoint",
            "medium",
        ),
        (
            "cmd",
            "directory",
            "cli",
            "conventional Go or CLI command root",
            "medium",
        ),
        (
            "src/main.py",
            "file",
            "app",
            "conventional source application entrypoint",
            "medium",
        ),
        (
            "src/index.ts",
            "file",
            "app",
            "conventional TypeScript application entrypoint",
            "medium",
        ),
        (
            "src/index.js",
            "file",
            "app",
            "conventional JavaScript application entrypoint",
            "medium",
        ),
        (
            "src/main.ts",
            "file",
            "app",
            "conventional TypeScript application entrypoint",
            "medium",
        ),
        (
            "src/main.rs",
            "file",
            "app",
            "conventional Rust binary entrypoint",
            "high",
        ),
        (
            "public/index.php",
            "file",
            "app",
            "common PHP public entrypoint",
            "high",
        ),
        (
            "index.php",
            "file",
            "app",
            "common PHP entrypoint",
            "medium",
        ),
        (
            "tests",
            "directory",
            "test",
            "conventional test root",
            "medium",
        ),
        (
            "test",
            "directory",
            "test",
            "conventional test root",
            "medium",
        ),
    ];
    for (relative, kind, role, reason, confidence) in conventional_roots.iter().copied() {
        if surface_files.contains_kind(relative, kind) {
            adder.add(relative, kind, role, reason, confidence);
        }
    }

    if surface_files.contains_file("package.json") && repo_path.join("package.json").is_file() {
        add_tracked_entrypoint_candidates(
            &mut adder,
            entrypoints_from_package_json(repo_path)?,
            surface_files,
        );
    }
    if surface_files.contains_file("Procfile") && repo_path.join("Procfile").is_file() {
        add_tracked_entrypoint_candidates(
            &mut adder,
            entrypoints_from_procfile(repo_path)?,
            surface_files,
        );
    }
    if let Some(compose_name) = COMPOSE_FILENAMES
        .into_iter()
        .find(|name| surface_files.contains_file(name) && repo_path.join(name).is_file())
    {
        add_tracked_entrypoint_candidates(
            &mut adder,
            entrypoints_from_compose_file(repo_path, compose_name)?,
            surface_files,
        );
    }

    if has_manifest(manifests, "Cargo.toml")
        && surface_files.contains_file("Cargo.toml")
        && surface_files.contains_file("src/lib.rs")
    {
        adder.add(
            "src/lib.rs",
            "file",
            "cli",
            "library root for Rust crate",
            "medium",
        );
    }

    let mut candidates = adder.candidates;
    candidates.sort_by(|a, b| {
        let key = |c: &Value| {
            (
                entrypoint_role_priority(&c.get("role").map(Value::py_str).unwrap_or_default()),
                -confidence_rank(&c.get("confidence").map(Value::py_str).unwrap_or_default()),
                c.get("path").map(Value::py_str).unwrap_or_default(),
            )
        };
        key(a).cmp(&key(b))
    });
    candidates.truncate(8);
    Ok(candidates)
}

fn add_tracked_entrypoint_candidates(
    adder: &mut EntrypointAdder,
    candidates: Vec<EntrypointTuple>,
    surface_files: &OnboardingFileSnapshot,
) {
    for candidate in candidates {
        let source_backed = matches!(candidate.1.as_str(), "script" | "process" | "service");
        if source_backed || surface_files.contains_kind(&candidate.0, &candidate.1) {
            adder.add(
                &candidate.0,
                &candidate.1,
                &candidate.2,
                &candidate.3,
                &candidate.4,
            );
        }
    }
}

const CAUTION_ZONE_NAMES: [&str; 11] = [
    "vendor",
    "third_party",
    "generated",
    "dist",
    "build",
    "coverage",
    "target",
    "node_modules",
    "migrations",
    "fixtures",
    "snapshots",
];

fn collect_caution_zones(surface_files: &OnboardingFileSnapshot) -> Vec<Value> {
    let mut caution = Vec::new();
    for name in surface_files.top_level_directories() {
        if CAUTION_ZONE_NAMES.contains(&name.as_str()) {
            caution.push(obj(vec![
                ("path", Value::str(name)),
                (
                    "reason",
                    Value::str("likely generated, vendored, fixture, or migration-heavy area"),
                ),
            ]));
        }
    }
    caution
}

fn infer_repo_kind(repo_path: &Path, manifests: &[String], areas: &[Value]) -> &'static str {
    let area_paths: HashSet<String> = areas
        .iter()
        .filter_map(|a| a.get("path").map(Value::py_str))
        .collect();
    if has_manifest(manifests, "pnpm-workspace.yaml")
        || area_paths.contains("packages")
        || area_paths.contains("apps")
    {
        return "monorepo";
    }
    if has_manifest(manifests, "Cargo.toml")
        && repo_path.join("src/lib.rs").exists()
        && !repo_path.join("src/main.rs").exists()
    {
        return "library";
    }
    if has_manifest(manifests, "composer.json") || area_paths.contains("public") {
        return "application";
    }
    if has_manifest(manifests, "go.mod") || area_paths.contains("cmd") {
        return "service";
    }
    if has_manifest(manifests, "pyproject.toml") && area_paths.contains("src") {
        return "library_or_service";
    }
    "repository"
}

fn infer_languages(repo_path: &Path, manifests: &[String]) -> Vec<&'static str> {
    let mut languages = Vec::new();
    if has_manifest(manifests, "pyproject.toml") {
        languages.push("python");
    }
    if has_manifest(manifests, "package.json") {
        if ["tsconfig.json", "src/index.ts", "src/main.ts"]
            .iter()
            .any(|rel| repo_path.join(rel).exists())
        {
            languages.push("typescript");
        } else {
            languages.push("javascript");
        }
    }
    if has_manifest(manifests, "Cargo.toml") {
        languages.push("rust");
    }
    if has_manifest(manifests, "composer.json") {
        languages.push("php");
    }
    if has_manifest(manifests, "go.mod") {
        languages.push("go");
    }
    languages
}

fn navigation_recipes() -> Value {
    Value::Array(vec![
        obj(vec![
            (
                "purpose",
                Value::str("Broad repository orientation for a user request"),
            ),
            (
                "command",
                Value::str(
                    "aethyme explore --repo \"$PWD\" --request \"<task>\" --format answer-json",
                ),
            ),
        ]),
        obj(vec![
            ("purpose", Value::str("Quick deterministic repo summary")),
            (
                "command",
                Value::str(format!(
                    "aethyme repo inspect \"$PWD\" --mode brief --json-output"
                )),
            ),
        ]),
        obj(vec![
            ("purpose", Value::str("Trace likely impact before editing")),
            (
                "command",
                Value::str(format!(
                    "aethyme graph callers \"$PWD\" \"<symbol-or-file>\" --json-output"
                )),
            ),
        ]),
    ])
}

fn guess_primary_script(scripts: &Value) -> Option<&'static str> {
    ["dev", "start", "serve", "storybook"]
        .into_iter()
        .find(|candidate| scripts.get(candidate).is_some())
}

type CommandTuple = (String, String, String, String);
type EntrypointTuple = (String, String, String, String, String);

fn commands_from_github_actions(repo_path: &Path) -> Vec<CommandTuple> {
    let workflows_dir = repo_path.join(".github").join("workflows");
    if !workflows_dir.exists() {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut workflows: Vec<PathBuf> = match std::fs::read_dir(&workflows_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| {
                // Python glob `*.y*ml`: ends with "ml" and has ".y"
                // somewhere before it.
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name.ends_with("ml") && name.len() > 2 && name[..name.len() - 2].contains(".y")
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    workflows.sort();
    for workflow in workflows {
        let Ok(text) = std::fs::read_to_string(&workflow) else {
            continue;
        };
        lines.extend(py_splitlines(&text).into_iter().map(str::to_string));
    }
    let joined = lines.join("\n");
    let patterns: [(&str, &str); 7] = [
        ("fast_test", "pnpm test"),
        ("fast_test", "npm test"),
        ("fast_test", "cargo test"),
        ("fast_test", "pytest"),
        ("lint", "ruff check ."),
        ("lint", "cargo clippy"),
        ("build", "cargo build"),
    ];
    patterns
        .iter()
        .filter(|(_, command)| joined.contains(command))
        .map(|(kind, command)| {
            (
                kind.to_string(),
                command.to_string(),
                "github-actions".to_string(),
                "medium".to_string(),
            )
        })
        .collect()
}

fn commands_from_procfile(repo_path: &Path) -> Result<Vec<CommandTuple>, String> {
    let procfile_path = repo_path.join("Procfile");
    if !procfile_path.exists() {
        return Ok(Vec::new());
    }
    let mut commands = Vec::new();
    let text = read_text(&procfile_path)?;
    for line in py_splitlines(&text) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') || !stripped.contains(':') {
            continue;
        }
        let (name, command) = stripped.split_once(':').expect("checked contains ':'");
        let role = name.trim().to_lowercase();
        let command = command.trim();
        // Python computes `kind` through role buckets that all resolve to
        // "dev" today; kept literal for parity.
        let kind = "dev";
        commands.push((
            kind.to_string(),
            command.to_string(),
            format!("Procfile:{role}"),
            "medium".to_string(),
        ));
    }
    Ok(commands)
}

const COMPOSE_FILENAMES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

fn compose_file(repo_path: &Path) -> Option<&'static str> {
    COMPOSE_FILENAMES
        .into_iter()
        .find(|name| repo_path.join(name).exists())
}

fn commands_from_compose(repo_path: &Path) -> Result<Vec<CommandTuple>, String> {
    let Some(compose_name) = compose_file(repo_path) else {
        return Ok(Vec::new());
    };
    let contents = read_text(&repo_path.join(compose_name))?;
    let mut commands = Vec::new();
    if contents.contains("services:") {
        commands.push((
            "dev".to_string(),
            format!("docker compose -f {compose_name} up"),
            compose_name.to_string(),
            "medium".to_string(),
        ));
    }
    for service_name in ["test", "tests", "app", "web"] {
        if contents.contains(&format!("{service_name}:")) {
            if service_name == "test" || service_name == "tests" {
                commands.push((
                    "fast_test".to_string(),
                    format!("docker compose -f {compose_name} run --rm {service_name}"),
                    format!("{compose_name}:{service_name}"),
                    "low".to_string(),
                ));
            }
            break;
        }
    }
    Ok(commands)
}

fn entrypoints_from_package_json(repo_path: &Path) -> Result<Vec<EntrypointTuple>, String> {
    let package_json_path = repo_path.join("package.json");
    if !package_json_path.exists() {
        return Ok(Vec::new());
    }
    let package_json = load_json_file(&package_json_path)?;
    let mut candidates = Vec::new();
    for (field, role, reason) in [
        ("main", "app", "package main entrypoint"),
        ("bin", "cli", "package CLI entrypoint"),
    ] {
        match package_json.get(field) {
            Some(Value::Str(value)) => {
                let path = value.trim_start_matches(['.', '/']);
                if repo_path.join(path).exists() {
                    candidates.push((
                        path.to_string(),
                        "file".to_string(),
                        role.to_string(),
                        reason.to_string(),
                        "high".to_string(),
                    ));
                }
            }
            Some(Value::Object(entries)) if field == "bin" => {
                for (_, bin_value) in entries {
                    let Value::Str(bin_path) = bin_value else {
                        continue;
                    };
                    let path = bin_path.trim_start_matches(['.', '/']);
                    if repo_path.join(path).exists() {
                        candidates.push((
                            path.to_string(),
                            "file".to_string(),
                            "cli".to_string(),
                            "package bin entrypoint".to_string(),
                            "high".to_string(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    let empty = Value::object();
    let scripts = match package_json.get("scripts") {
        Some(v) if v.truthy() => v,
        _ => &empty,
    };
    for (script_name, role, reason) in [
        ("dev", "app", "package dev script"),
        ("start", "app", "package start script"),
        ("worker", "worker", "package worker script"),
        ("test", "test", "package test script"),
    ] {
        if scripts.get(script_name).is_some() {
            candidates.push((
                format!("package.json:scripts.{script_name}"),
                "script".to_string(),
                role.to_string(),
                reason.to_string(),
                "medium".to_string(),
            ));
        }
    }
    Ok(candidates)
}

fn entrypoints_from_procfile(repo_path: &Path) -> Result<Vec<EntrypointTuple>, String> {
    let procfile_path = repo_path.join("Procfile");
    if !procfile_path.exists() {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    let text = read_text(&procfile_path)?;
    for line in py_splitlines(&text) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') || !stripped.contains(':') {
            continue;
        }
        let (name, command) = stripped.split_once(':').expect("checked contains ':'");
        let process_name = name.trim().to_lowercase();
        let (role, reason) = match process_name.as_str() {
            "worker" | "job" | "consumer" => ("worker", "Procfile worker process entrypoint"),
            "test" | "tests" => ("test", "Procfile test process entrypoint"),
            "cli" | "console" => ("cli", "Procfile CLI process entrypoint"),
            _ => ("app", "Procfile process entrypoint"),
        };
        candidates.push((
            format!("Procfile:{process_name}"),
            "process".to_string(),
            role.to_string(),
            reason.to_string(),
            "high".to_string(),
        ));
        if let Some(inferred_path) = extract_path_like_token(command.trim(), repo_path) {
            let kind = if repo_path.join(&inferred_path).is_file() {
                "file"
            } else {
                "directory"
            };
            candidates.push((
                inferred_path,
                kind.to_string(),
                role.to_string(),
                format!("{reason} target"),
                "medium".to_string(),
            ));
        }
    }
    Ok(candidates)
}

fn entrypoints_from_compose_file(
    repo_path: &Path,
    compose_name: &str,
) -> Result<Vec<EntrypointTuple>, String> {
    let contents = read_text(&repo_path.join(compose_name))?;
    let mut candidates = Vec::new();
    for service_name in ["app", "web", "server", "worker", "test", "tests"] {
        if !contents.contains(&format!("{service_name}:")) {
            continue;
        }
        let (role, reason) = match service_name {
            "worker" => ("worker", "compose worker entrypoint"),
            "test" | "tests" => ("test", "compose test service entrypoint"),
            _ => ("app", "compose service entrypoint"),
        };
        candidates.push((
            format!("{compose_name}:{service_name}"),
            "service".to_string(),
            role.to_string(),
            reason.to_string(),
            "medium".to_string(),
        ));
    }
    Ok(candidates)
}

fn extract_path_like_token(command: &str, repo_path: &Path) -> Option<String> {
    for token in command.split_whitespace() {
        let normalized = token
            .trim_matches(['"', '\''])
            .trim_start_matches(['.', '/']);
        if normalized.is_empty() || normalized.starts_with('-') {
            continue;
        }
        if repo_path.join(normalized).exists() {
            return Some(normalized.to_string());
        }
    }
    None
}

fn kind_priority(kind: &str) -> i32 {
    match kind {
        "install" => 0,
        "dev" => 1,
        "fast_test" => 2,
        "full_test" => 3,
        "lint" => 4,
        "build" => 5,
        "primary-script" => 6,
        _ => 99,
    }
}

fn entrypoint_role_priority(role: &str) -> i32 {
    match role {
        "app" => 0,
        "cli" => 1,
        "worker" => 2,
        "test" => 3,
        _ => 99,
    }
}

fn confidence_rank(confidence: &str) -> i32 {
    match confidence {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn primary_commands_map(commands: &[Value]) -> Value {
    let mut primary = Value::object();
    for kind in ["install", "dev", "fast_test", "full_test", "lint", "build"] {
        let mut ranked: Vec<&Value> = commands
            .iter()
            .filter(|c| c.get("kind").map(Value::py_str).as_deref() == Some(kind))
            .collect();
        ranked.sort_by(|a, b| {
            let key = |c: &Value| {
                (
                    -confidence_rank(&c.get("confidence").unwrap_or(&Value::Null).py_str()),
                    source_priority(kind, &c.get("source").unwrap_or(&Value::Null).py_str()),
                    c.get("command").unwrap_or(&Value::Null).py_str(),
                )
            };
            key(a).cmp(&key(b))
        });
        if let Some(first) = ranked.first() {
            primary.set(
                kind,
                Value::str(first.get("command").unwrap_or(&Value::Null).py_str()),
            );
        }
    }
    primary
}

fn primary_entrypoints_map(entrypoints: &[Value]) -> Value {
    let mut primary = Value::object();
    for role in ["app", "cli", "worker", "test"] {
        let mut ranked: Vec<&Value> = entrypoints
            .iter()
            .filter(|e| e.get("role").map(Value::py_str).as_deref() == Some(role))
            .collect();
        ranked.sort_by(|a, b| {
            let key = |e: &Value| {
                (
                    -confidence_rank(&e.get("confidence").unwrap_or(&Value::Null).py_str()),
                    entrypoint_kind_priority(&e.get("kind").unwrap_or(&Value::Null).py_str()),
                    e.get("path").unwrap_or(&Value::Null).py_str(),
                )
            };
            key(a).cmp(&key(b))
        });
        if let Some(first) = ranked.first() {
            primary.set(role, (*first).clone());
        }
    }
    primary
}

fn entrypoint_kind_priority(kind: &str) -> i32 {
    match kind {
        "file" => 0,
        "directory" => 1,
        "process" => 2,
        "service" => 3,
        "script" => 4,
        _ => 99,
    }
}

fn source_priority(kind: &str, source: &str) -> i32 {
    if matches!(
        kind,
        "install" | "fast_test" | "full_test" | "lint" | "build"
    ) {
        if source.starts_with("justfile") || source.starts_with("Justfile") {
            return 0;
        }
        if source.starts_with("Makefile") {
            return 1;
        }
    }
    if source.starts_with("package.json") {
        return 2;
    }
    if source == "Cargo.toml" {
        return 3;
    }
    if source == "pyproject.toml" {
        return 4;
    }
    if source == "composer.json" {
        return 5;
    }
    if source.starts_with("Procfile") {
        return 6;
    }
    if source.starts_with("docker-compose") || source.starts_with("compose") {
        return 7;
    }
    if source == "github-actions" {
        return 8;
    }
    99
}

fn first_command_of_kind(commands: &Value, kind: &str) -> Result<Option<Value>, String> {
    let Some(items) = commands.as_array() else {
        return Ok(None);
    };
    for command in items {
        if !command.is_object() {
            return Err("onboarding commands entries must be objects".to_string());
        }
        if command.get("kind").map(Value::py_str).as_deref() == Some(kind) {
            return Ok(Some(Value::str(
                command.get("command").unwrap_or(&Value::Null).py_str(),
            )));
        }
    }
    Ok(None)
}

// ── overrides ───────────────────────────────────────────────────────────────

const ONBOARDING_OVERRIDE_KEYS: &[&str] = &[
    "repo",
    "summon",
    "commands",
    "areas",
    "entrypoints",
    "caution_zones",
    "navigation_recipes",
    "notes",
];
const ONBOARDING_REPO_OVERRIDE_KEYS: &[&str] = &[
    "name",
    "root",
    "kind",
    "languages",
    "package_manager",
    "manifests",
];
const ONBOARDING_SUMMON_OVERRIDE_KEYS: &[&str] = &["description", "recommended_when", "skip_when"];

fn unknown_override_errors(payload: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(entries) = payload.as_object() else {
        return errors;
    };
    for (key, value) in entries {
        if !ONBOARDING_OVERRIDE_KEYS.contains(&key.as_str()) {
            errors.push(format!(
                "unknown field $.{key}; allowed keys: {}",
                ONBOARDING_OVERRIDE_KEYS.join(", ")
            ));
            continue;
        }
        let nested_allowed = match key.as_str() {
            "repo" => Some(ONBOARDING_REPO_OVERRIDE_KEYS),
            "summon" => Some(ONBOARDING_SUMMON_OVERRIDE_KEYS),
            _ => None,
        };
        if let (Some(allowed), Some(nested)) = (nested_allowed, value.as_object()) {
            for (nested_key, _) in nested {
                if !allowed.contains(&nested_key.as_str()) {
                    errors.push(format!(
                        "unknown field $.{key}.{nested_key}; allowed keys: {}",
                        allowed.join(", ")
                    ));
                }
            }
        }
    }
    errors
}

/// `_load_overrides`: onboarding override payload with the exact Python
/// tolerances — missing → `{}`, unreadable/invalid/non-object →
/// `{_invalid_override: true, _source: …}`, else payload + `_source`.
pub fn load_overrides(repo_path: &Path) -> Value {
    let override_path = repo_path.join(ONBOARDING_OVERRIDE_PATH);
    if !override_path.exists() {
        return Value::object();
    }
    let invalid = || {
        obj(vec![
            ("_invalid_override", Value::Bool(true)),
            ("_source", Value::str(ONBOARDING_OVERRIDE_PATH)),
        ])
    };
    let Ok(text) = std::fs::read_to_string(&override_path) else {
        return invalid();
    };
    let Ok(mut payload) = pyjson::loads(&text) else {
        return invalid();
    };
    if !payload.is_object() {
        return invalid();
    }
    if !unknown_override_errors(&payload).is_empty() {
        return invalid();
    }
    payload.set("_source", Value::str(ONBOARDING_OVERRIDE_PATH));
    payload
}

fn apply_overrides(artifact: &mut Value, overrides: &Value) -> Result<(), String> {
    if let Some(Value::Object(repo_overrides)) = overrides.get("repo") {
        let repo_overrides = repo_overrides.clone();
        let repo = artifact.get("repo").cloned().ok_or("KeyError: 'repo'")?;
        let mut repo = repo;
        for (key, value) in &repo_overrides {
            if !matches!(value, Value::Null) {
                repo.set(key, value.clone());
            }
        }
        artifact.set("repo", repo);
    }

    if let Some(Value::Object(summon_overrides)) = overrides.get("summon") {
        let summon_overrides = summon_overrides.clone();
        let summon = artifact
            .get("summon")
            .cloned()
            .ok_or("KeyError: 'summon'")?;
        let mut summon = summon;
        for (key, value) in &summon_overrides {
            if !matches!(value, Value::Null) {
                summon.set(key, value.clone());
            }
        }
        artifact.set("summon", summon);
    }

    for key in [
        "commands",
        "areas",
        "entrypoints",
        "caution_zones",
        "navigation_recipes",
    ] {
        if let Some(value @ Value::Array(_)) = overrides.get(key) {
            artifact.set(key, value.clone());
        }
    }
    if let Some(Value::Array(entrypoints)) = artifact.get("entrypoints") {
        artifact.set("primary_entrypoints", primary_entrypoints_map(entrypoints));
    }

    if let Some(notes @ Value::Array(_)) = overrides.get("notes") {
        artifact.set("notes", notes.clone());
    }
    Ok(())
}

/// `override_template`: the starter onboarding override for maintainers.
pub fn override_template() -> Value {
    obj(vec![
        ("repo", obj(vec![("kind", Value::str("service"))])),
        (
            "commands",
            Value::Array(vec![obj(vec![
                ("kind", Value::str("test")),
                ("command", Value::str("./scripts/test-fast.sh")),
                ("source", Value::str("manual-override")),
                ("confidence", Value::str("high")),
            ])]),
        ),
        (
            "notes",
            str_list(&[
                "Use sandbox credentials from 1Password.",
                "Do not edit generated files under src/gen directly.",
            ]),
        ),
        (
            "summon",
            obj(vec![
                ("recommended_when", str_list(&["first touch", "broad task"])),
                ("skip_when", str_list(&["single known file"])),
            ]),
        ),
    ])
}

/// Python `json.JSONDecodeError.msg` analogue: the pyjson error without
/// its `: char N` position suffix (Python's `.msg` excludes position).
pub fn json_error_msg(error: &str) -> String {
    match error.rfind(": char ") {
        Some(pos) if error[pos + 7..].chars().all(|c| c.is_ascii_digit()) => {
            error[..pos].to_string()
        }
        _ => error.to_string(),
    }
}

/// `validate_overrides`: validation status for the onboarding override
/// file — the exact Python result dict shape.
pub fn validate_overrides(repo_path: &Path) -> Value {
    let repo_path = resolve_path(repo_path);
    let override_path = repo_path.join(ONBOARDING_OVERRIDE_PATH);
    if !override_path.exists() {
        return obj(vec![
            ("ok", Value::Bool(true)),
            ("exists", Value::Bool(false)),
            ("path", Value::str(ONBOARDING_OVERRIDE_PATH)),
            ("errors", Value::Array(vec![])),
        ]);
    }
    let text = std::fs::read_to_string(&override_path).unwrap_or_default();
    let payload = match pyjson::loads(&text) {
        Ok(payload) => payload,
        Err(error) => {
            return obj(vec![
                ("ok", Value::Bool(false)),
                ("exists", Value::Bool(true)),
                ("path", Value::str(ONBOARDING_OVERRIDE_PATH)),
                (
                    "errors",
                    Value::Array(vec![Value::str(format!(
                        "invalid JSON: {}",
                        json_error_msg(&error)
                    ))]),
                ),
            ]);
        }
    };
    if !payload.is_object() {
        return obj(vec![
            ("ok", Value::Bool(false)),
            ("exists", Value::Bool(true)),
            ("path", Value::str(ONBOARDING_OVERRIDE_PATH)),
            (
                "errors",
                Value::Array(vec![Value::str("override root must be a JSON object")]),
            ),
        ]);
    }
    let mut errors: Vec<Value> = Vec::new();
    errors.extend(
        unknown_override_errors(&payload)
            .into_iter()
            .map(Value::str),
    );
    if let Some(notes) = payload.get("notes") {
        let valid = match notes {
            Value::Null => true,
            Value::Array(items) => items.iter().all(|item| matches!(item, Value::Str(_))),
            _ => false,
        };
        if !valid {
            errors.push(Value::str("notes must be a list of strings"));
        }
    }
    if let Some(commands) = payload.get("commands") {
        if !matches!(commands, Value::Null | Value::Array(_)) {
            errors.push(Value::str("commands must be a list"));
        }
    }
    if let Some(summon) = payload.get("summon") {
        if !matches!(summon, Value::Null | Value::Object(_)) {
            errors.push(Value::str("summon must be an object"));
        }
    }
    obj(vec![
        ("ok", Value::Bool(errors.is_empty())),
        ("exists", Value::Bool(true)),
        ("path", Value::str(ONBOARDING_OVERRIDE_PATH)),
        ("errors", Value::Array(errors)),
    ])
}

fn summon_policy() -> Value {
    obj(vec![
        (
            "description",
            Value::str(
                "Use when starting work in an unfamiliar repository, when the task asks \
for repo overview, setup, architecture, entrypoints, test commands, \
or where to begin. Skip for narrow file-scoped edits once the relevant \
paths are already known.",
            ),
        ),
        (
            "recommended_when",
            str_list(&[
                "first task in repo",
                "repo overview",
                "setup or run instructions",
                "architecture or entrypoints",
                "where should I start",
                "broad debugging or feature-localization request",
            ]),
        ),
        (
            "skip_when",
            str_list(&[
                "known file-scoped edit",
                "follow-up inside already identified area",
                "task already localized to concrete files",
            ]),
        ),
        (
            "task_signals",
            str_list(&[
                "unfamiliar repository",
                "broad task with no path named",
                "user asks how the repo is organized",
            ]),
        ),
    ])
}

fn generation_telemetry(artifact: &Value, overrides: &Value) -> Value {
    let count = |key: &str| -> i128 {
        artifact
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.len() as i128)
            .unwrap_or(0)
    };
    obj(vec![
        (
            "sections",
            str_list(&[
                "repo",
                "commands",
                "areas",
                "entrypoints",
                "caution_zones",
                "navigation_recipes",
                "summon",
                "freshness",
            ]),
        ),
        (
            "counts",
            obj(vec![
                ("commands", Value::Int(count("commands"))),
                ("areas", Value::Int(count("areas"))),
                ("entrypoints", Value::Int(count("entrypoints"))),
                ("caution_zones", Value::Int(count("caution_zones"))),
                (
                    "navigation_recipes",
                    Value::Int(count("navigation_recipes")),
                ),
                ("notes", Value::Int(count("notes"))),
            ]),
        ),
        ("overrides_applied", Value::Bool(overrides.truthy())),
        (
            "override_source",
            overrides.get("_source").cloned().unwrap_or(Value::Null),
        ),
        (
            "override_invalid",
            Value::Bool(
                overrides
                    .get("_invalid_override")
                    .map(Value::truthy)
                    .unwrap_or(false),
            ),
        ),
    ])
}

// ── freshness & recommendation ──────────────────────────────────────────────

fn mtime_f64(path: &Path) -> Option<f64> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    // CPython computes st_mtime as sec + nsec * 1e-9 in double math.
    Some(duration.as_secs() as f64 + duration.subsec_nanos() as f64 * 1e-9)
}

/// Whether generated onboarding artifacts are fresh relative to overrides.
pub fn override_freshness(repo_path: &Path) -> Value {
    let repo_path = resolve_path(repo_path);
    let override_path = repo_path.join(ONBOARDING_OVERRIDE_PATH);
    let onboarding_path = repo_path.join(ONBOARDING_JSON_PATH);
    let act_path = repo_path.join(ACT_STARTER_JSON_PATH);

    let onboarding_exists = onboarding_path.exists();
    let act_exists = act_path.exists();
    if !override_path.exists() {
        return obj(vec![
            ("override_exists", Value::Bool(false)),
            (
                "generated_exists",
                obj(vec![
                    ("onboarding", Value::Bool(onboarding_exists)),
                    ("act", Value::Bool(act_exists)),
                ]),
            ),
            ("regeneration_required", Value::Bool(false)),
            ("stale_targets", Value::Array(vec![])),
        ]);
    }

    let override_mtime = mtime_f64(&override_path).unwrap_or(0.0);
    let mut stale_targets = Vec::new();
    let onboarding_mtime = if onboarding_exists {
        mtime_f64(&onboarding_path)
    } else {
        None
    };
    let act_mtime = if act_exists {
        mtime_f64(&act_path)
    } else {
        None
    };
    if !onboarding_exists || onboarding_mtime.unwrap_or(0.0) < override_mtime {
        stale_targets.push(Value::str("onboarding"));
    }
    if !act_exists || act_mtime.unwrap_or(0.0) < override_mtime {
        stale_targets.push(Value::str("act"));
    }

    obj(vec![
        ("override_exists", Value::Bool(true)),
        ("override_path", Value::str(ONBOARDING_OVERRIDE_PATH)),
        ("override_mtime", Value::Float(override_mtime)),
        (
            "generated_exists",
            obj(vec![
                ("onboarding", Value::Bool(onboarding_exists)),
                ("act", Value::Bool(act_exists)),
            ]),
        ),
        (
            "generated_mtime",
            obj(vec![
                (
                    "onboarding",
                    onboarding_mtime.map(Value::Float).unwrap_or(Value::Null),
                ),
                ("act", act_mtime.map(Value::Float).unwrap_or(Value::Null)),
            ]),
        ),
        (
            "regeneration_required",
            Value::Bool(!stale_targets.is_empty()),
        ),
        ("stale_targets", Value::Array(stale_targets)),
    ])
}

/// Small deterministic recommendation surface for the repo.
pub fn recommendation_summary(repo_path: &Path) -> Result<Value, String> {
    let artifact = build_onboarding_artifact(repo_path)?;
    Ok(obj(vec![
        ("recommended_skill", Value::str("repo-onboarding")),
        ("recommended_mode", Value::str("explore")),
        (
            "reason",
            Value::str("broad repo orientation and safe first-step guidance"),
        ),
        ("summon", req(&artifact, "summon")?.clone()),
        ("telemetry", req(&artifact, "telemetry")?.clone()),
    ]))
}

// ── snapshots ───────────────────────────────────────────────────────────────

struct OnboardingSnapshot {
    repo_name: String,
    source_digest: String,
    source_file_count: usize,
}

fn snapshot_metadata(repo_path: &Path) -> Result<OnboardingSnapshot, String> {
    let (source_digest, source_file_count) = if repo_path.join(".git").exists() {
        git_snapshot(repo_path)?
    } else {
        filesystem_snapshot(repo_path)?
    };
    Ok(OnboardingSnapshot {
        repo_name: repository_name(repo_path),
        source_digest,
        source_file_count,
    })
}

fn run_git(repo_path: &Path, args: &[&str]) -> Option<String> {
    let stdout = run_git_bytes(repo_path, args)?;
    Some(String::from_utf8_lossy(&stdout).into_owned())
}

fn run_git_bytes(repo_path: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

fn repository_name(repo_path: &Path) -> String {
    canonical_remote_repository_name(repo_path)
        .or_else(|| common_git_directory_repository_name(repo_path))
        .or_else(|| path_file_name(repo_path))
        .unwrap_or_default()
}

fn canonical_remote_repository_name(repo_path: &Path) -> Option<String> {
    let remotes = run_git(repo_path, &["remote"])?
        .lines()
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let remote = if remotes.iter().any(|remote| remote == "origin") {
        "origin"
    } else if remotes.len() == 1 {
        remotes[0].as_str()
    } else {
        return None;
    };
    let url = run_git(repo_path, &["remote", "get-url", remote])?;
    repository_name_from_remote_url(url.trim())
}

fn repository_name_from_remote_url(url: &str) -> Option<String> {
    let without_query = url.split(['?', '#']).next()?.trim_end_matches(['/', '\\']);
    let name = without_query
        .rsplit(|character| matches!(character, '/' | ':' | '\\'))
        .next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (!name.is_empty() && !matches!(name, "." | "..")).then(|| name.to_string())
}

fn common_git_directory_repository_name(repo_path: &Path) -> Option<String> {
    let common = run_git(repo_path, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_relative() {
        repo_path.join(common)
    } else {
        common
    };
    let common = common.canonicalize().unwrap_or(common);
    if common.file_name().is_some_and(|name| name == ".git") {
        return common.parent().and_then(path_file_name);
    }
    path_file_name(&common).map(|name| name.strip_suffix(".git").unwrap_or(&name).to_string())
}

fn path_file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn git_snapshot(repo_path: &Path) -> Result<(String, usize), String> {
    let tracked = run_git_bytes(repo_path, &["ls-files", "--cached", "-z"])
        .ok_or_else(|| "git ls-files failed while building onboarding source digest".to_string())?;
    let mut digest = Sha256::new();
    let mut file_count = 0usize;
    for relative in tracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        if is_generated_deploy_path(relative) {
            continue;
        }
        hash_source_entry(&mut digest, repo_path, relative)?;
        file_count += 1;
    }
    Ok((format!("{:x}", digest.finalize()), file_count))
}

fn filesystem_snapshot(repo_path: &Path) -> Result<(String, usize), String> {
    // Python: sorted(repo_path.rglob("*")) — Path ordering compares
    // per-component, not the joined string.
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_recursive(repo_path, &mut paths)?;
    paths.sort_by(|a, b| {
        let parts = |p: &PathBuf| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        };
        parts(a).cmp(&parts(b))
    });
    let mut digest = Sha256::new();
    let mut file_count = 0usize;
    for path in paths {
        let relative = path
            .strip_prefix(repo_path)
            .map_err(|e| e.to_string())?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if is_generated_deploy_path(relative.as_bytes()) {
            continue;
        }
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.is_dir() {
            continue;
        }
        hash_source_entry(&mut digest, repo_path, relative.as_bytes())?;
        file_count += 1;
    }
    Ok((format!("{:x}", digest.finalize()), file_count))
}

fn is_generated_deploy_path(relative: &[u8]) -> bool {
    GENERATED_DEPLOY_PATHS
        .iter()
        .any(|generated| generated.as_bytes() == relative)
        || GENERATED_DEPLOY_PREFIXES
            .iter()
            .any(|generated| relative.starts_with(generated.as_bytes()))
}

fn hash_source_entry(digest: &mut Sha256, repo_path: &Path, relative: &[u8]) -> Result<(), String> {
    hash_digest_frame(digest, relative);
    let path = repo_path.join(path_buf_from_bytes(relative));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            digest.update(b"symlink");
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            hash_digest_frame(digest, &path_bytes(&target));
        }
        Ok(metadata) if metadata.is_file() => {
            digest.update(b"file");
            let content =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            hash_digest_frame(digest, &content);
        }
        Ok(metadata) if metadata.is_dir() => {
            // Gitlinks are tracked directory entries. Their checked-out HEAD
            // is the content identity available without traversing ignored
            // or untracked files inside the nested repository.
            digest.update(b"gitlink");
            let head = run_git_bytes(&path, &["rev-parse", "HEAD"]).unwrap_or_default();
            hash_digest_frame(digest, &head);
        }
        Ok(_) => digest.update(b"other"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update(b"missing");
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    Ok(())
}

fn hash_digest_frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[cfg(unix)]
fn path_buf_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_buf_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        // rglob descends into directories (not through symlinked dirs).
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(path.clone());
        if is_dir {
            collect_recursive(&path, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aethyme-enhance-onboarding-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn committed_git_repo(tag: &str) -> PathBuf {
        let repo = fixture_repo(tag);
        git(&repo, &["init"]);
        git(&repo, &["config", "user.name", "Aethyme Tests"]);
        git(&repo, &["config", "user.email", "aethyme@example.test"]);
        write(&repo.join("README.md"), "fixture\n");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "test: initialize fixture"]);
        repo
    }

    fn linked_worktree(repo: &Path, tag: &str) -> PathBuf {
        let worktree = repo.with_file_name(format!(
            "aethyme-enhance-session-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&worktree);
        let worktree_arg = worktree.to_string_lossy().into_owned();
        git(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("test/{tag}"),
                &worktree_arg,
            ],
        );
        worktree
    }

    fn artifact_paths(artifact: &Value, key: &str) -> Vec<String> {
        artifact
            .get(key)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item.get("path").map(Value::py_str))
            .collect()
    }

    #[test]
    fn tracked_path_parser_is_nul_safe() {
        assert_eq!(
            parse_git_paths_z(b"src/main.rs\0misc\nnode_modules/pkg.js\0tab\tname.txt\0"),
            vec!["src/main.rs", "misc\nnode_modules/pkg.js", "tab\tname.txt"]
        );
    }

    #[test]
    fn tracked_snapshot_does_not_split_newlines_inside_git_paths() {
        let repo = committed_git_repo("nul-safe-paths");
        let unusual_path = "misc\nnode_modules/package/index.js";
        write(&repo.join(unusual_path), "module.exports = {};\n");
        git(&repo, &["add", unusual_path]);

        let snapshot = OnboardingFileSnapshot::discover(&repo).unwrap();
        assert!(snapshot.contains_file(unusual_path));
        assert!(!snapshot.contains_directory("node_modules"));
        let artifact = build_onboarding_artifact(&repo).unwrap();
        assert!(!artifact_paths(&artifact, "caution_zones").contains(&"node_modules".to_string()));

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn repository_name_parses_common_remote_url_forms() {
        for (url, expected) in [
            ("https://github.com/example/alpha.git", "alpha"),
            ("ssh://git@github.com/example/bravo.git", "bravo"),
            ("git@github.com:example/charlie.git", "charlie"),
            ("/srv/git/delta.git", "delta"),
            ("file:///srv/git/echo.git/", "echo"),
        ] {
            assert_eq!(
                repository_name_from_remote_url(url).as_deref(),
                Some(expected),
                "remote URL: {url}"
            );
        }
    }

    #[test]
    fn repository_name_uses_canonical_remote_from_a_linked_worktree() {
        let repo = committed_git_repo("remote-primary");
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:Example/Canonical-Repository.git",
            ],
        );
        let worktree = linked_worktree(&repo, "remote-derived-name");

        let artifact = build_onboarding_artifact(&worktree).unwrap();
        assert_eq!(
            artifact.get("repo").unwrap().get("name").unwrap().as_str(),
            Some("Canonical-Repository")
        );
        assert!(render_onboarding_skill(&artifact)
            .unwrap()
            .contains("# Repo Onboarding: Canonical-Repository\n"));

        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn repository_name_without_remote_uses_primary_worktree() {
        let repo = committed_git_repo("canonical-primary");
        let expected = repo.file_name().unwrap().to_string_lossy().into_owned();
        let worktree = linked_worktree(&repo, "must-not-be-used");

        let artifact = build_onboarding_artifact(&worktree).unwrap();
        assert_eq!(
            artifact.get("repo").unwrap().get("name").unwrap().as_str(),
            Some(expected.as_str())
        );

        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn onboarding_surfaces_use_only_tracked_paths() {
        let repo = committed_git_repo("tracked-surfaces");
        write(
            &repo.join(".gitignore"),
            "node_modules/\nbuild/\ntarget/\ndist/\n",
        );
        write(&repo.join("src/main.ts"), "export const main = true;\n");
        write(&repo.join("tests/main.test.ts"), "export {};\n");
        write(&repo.join("migrations/001.sql"), "select 1;\n");
        write(
            &repo.join("package.json"),
            r#"{"main":"dist/index.js","scripts":{"start":"node dist/index.js"}}"#,
        );
        git(
            &repo,
            &[
                "add",
                ".gitignore",
                "src/main.ts",
                "tests/main.test.ts",
                "migrations/001.sql",
                "package.json",
            ],
        );

        write(&repo.join("app.py"), "print('untracked')\n");
        write(&repo.join("dist/index.js"), "console.log('ignored');\n");
        write(&repo.join("build/generated.js"), "generated\n");
        write(&repo.join("target/debug/local"), "local\n");
        write(
            &repo.join("node_modules/local-dependency/index.js"),
            "module.exports = {};\n",
        );

        let artifact = build_onboarding_artifact(&repo).unwrap();
        let areas = artifact_paths(&artifact, "areas");
        assert!(areas.contains(&"src".to_string()));
        assert!(areas.contains(&"tests".to_string()));
        for excluded in ["node_modules", "build", "target", "dist"] {
            assert!(!areas.contains(&excluded.to_string()), "area: {excluded}");
        }

        let entrypoints = artifact_paths(&artifact, "entrypoints");
        assert!(entrypoints.contains(&"src/main.ts".to_string()));
        assert!(entrypoints.contains(&"tests".to_string()));
        assert!(entrypoints.contains(&"package.json:scripts.start".to_string()));
        assert!(!entrypoints.contains(&"app.py".to_string()));
        assert!(!entrypoints.contains(&"dist/index.js".to_string()));

        let caution = artifact_paths(&artifact, "caution_zones");
        assert!(caution.contains(&"migrations".to_string()));
        for excluded in ["node_modules", "build", "target", "dist"] {
            assert!(
                !caution.contains(&excluded.to_string()),
                "caution: {excluded}"
            );
        }

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn onboarding_is_identical_across_linked_worktree_names() {
        let repo = committed_git_repo("stable-across-worktrees");
        let worktree = linked_worktree(&repo, "different-directory-name");

        assert_eq!(
            expected_onboarding_files(&repo).unwrap(),
            expected_onboarding_files(&worktree).unwrap()
        );

        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn source_digest_tracks_bytes_not_only_dirty_path_names() {
        let repo = committed_git_repo("content-digest");
        write(&repo.join("README.md"), "first dirty contents\n");
        let first = build_onboarding_artifact(&repo).unwrap();
        write(&repo.join("README.md"), "second dirty content\n");
        let second = build_onboarding_artifact(&repo).unwrap();

        assert_ne!(
            first
                .get("freshness")
                .unwrap()
                .get("source_digest")
                .unwrap(),
            second
                .get("freshness")
                .unwrap()
                .get("source_digest")
                .unwrap()
        );

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn generated_outputs_and_commit_identity_do_not_invalidate_onboarding() {
        let repo = committed_git_repo("stable-after-commit");
        let expected = expected_onboarding_files(&repo).unwrap();
        crate::deploy::deploy(&repo, true).unwrap();
        git(&repo, &["add", "--all"]);
        git(&repo, &["commit", "-m", "docs: generate onboarding"]);

        assert_eq!(expected_onboarding_files(&repo).unwrap(), expected);
        let artifact = build_onboarding_artifact(&repo).unwrap();
        let freshness = artifact.get("freshness").unwrap();
        assert_eq!(freshness.get("source_commit"), Some(&Value::Null));
        assert!(freshness.get("generated_at").is_none());
        assert!(freshness.get("snapshot_key").is_none());
        assert!(freshness.get("repo_dirty").is_none());

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn renderer_keeps_optional_source_commit_as_informational_provenance() {
        let repo = committed_git_repo("optional-source-commit");
        let mut artifact = build_onboarding_artifact(&repo).unwrap();
        let mut freshness = artifact.get("freshness").unwrap().clone();
        freshness.set("source_commit", Value::str("0123456789abcdef"));
        artifact.set("freshness", freshness);

        let rendered = render_onboarding_skill(&artifact).unwrap();
        assert!(rendered.contains("- Source commit: `0123456789abcdef`"));

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn rebase_with_the_same_tracked_contents_keeps_onboarding_unchanged() {
        let repo = committed_git_repo("rebase-independent");
        git(&repo, &["checkout", "-b", "topic"]);
        git(&repo, &["commit", "--allow-empty", "-m", "test: topic"]);
        let before = expected_onboarding_files(&repo).unwrap();

        git(&repo, &["checkout", "-b", "upstream", "HEAD~1"]);
        git(&repo, &["commit", "--allow-empty", "-m", "test: upstream"]);
        git(&repo, &["checkout", "topic"]);
        git(&repo, &["rebase", "upstream"]);

        assert_eq!(expected_onboarding_files(&repo).unwrap(), before);

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn onboarding_overrides_can_opt_in_untracked_generated_surfaces() {
        let repo = committed_git_repo("override-local-surface");
        write(&repo.join(".gitignore"), "generated/\n");
        write(&repo.join("generated/local-service.ts"), "export {};\n");
        write(
            &repo.join(ONBOARDING_OVERRIDE_PATH),
            r#"{
  "areas": [{"path": "generated", "role": "generated_or_vendor", "reason": "maintainer-approved local generator output", "confidence": "high"}],
  "entrypoints": [{"path": "generated/local-service.ts", "kind": "file", "role": "app", "reason": "maintainer-approved local service", "confidence": "high"}],
  "caution_zones": [{"path": "generated", "reason": "intentional local generated area"}]
}"#,
        );
        git(&repo, &["add", ".gitignore", ONBOARDING_OVERRIDE_PATH]);

        let artifact = build_onboarding_artifact(&repo).unwrap();
        assert_eq!(artifact_paths(&artifact, "areas"), vec!["generated"]);
        assert_eq!(
            artifact_paths(&artifact, "entrypoints"),
            vec!["generated/local-service.ts"]
        );
        assert_eq!(
            artifact
                .get("primary_entrypoints")
                .unwrap()
                .get("app")
                .unwrap()
                .get("path")
                .unwrap()
                .as_str(),
            Some("generated/local-service.ts")
        );
        assert_eq!(
            artifact_paths(&artifact, "caution_zones"),
            vec!["generated"]
        );

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn bare_repo_artifact_matches_python_shapes() {
        let repo = fixture_repo("bare");
        write(
            &repo.join("README.md"),
            "# Fixture Repo\n\nEnhance target.\n",
        );
        write(&repo.join("src/main.py"), "def entry():\n    return 1\n");

        let artifact = build_onboarding_artifact(&repo).unwrap();
        let json = pyjson::dumps_indent2(&artifact);
        assert!(json.contains("\"schema_version\": \"aethyme-onboarding-v1\""));
        assert!(json.contains("\"kind\": \"repository\""));
        assert!(json.contains("\"package_manager\": \"unknown\""));
        // Entrypoint detection: src/main.py as app entrypoint.
        assert!(json.contains("\"path\": \"src/main.py\""));
        assert!(json.contains("conventional source application entrypoint"));
        // Areas: src as source.
        assert!(json.contains("\"role\": \"source\""));
        // No manifests → empty lists render as [].
        assert!(json.contains("\"commands\": [],"));
        assert!(json.contains("\"generator\": \"deterministic\""));

        let act = build_act_starter_artifact(&artifact).unwrap();
        let act_json = pyjson::dumps_indent2(&act);
        assert!(act_json.contains("\"fast_test\": null"));
        assert!(act_json.contains("\"derived_from\": \".aethyme/generated/onboarding.json\""));

        let skill = render_onboarding_skill(&artifact).unwrap();
        assert!(skill.contains("# Repo Onboarding:"));
        assert!(skill.contains("- Languages: `unknown`"));
        assert!(skill.contains("- Source digest: `"));

        let act_skill = render_act_skill(&act).unwrap();
        assert!(act_skill.contains("# Repo Act:"));
        assert!(act_skill.contains("## Useful Commands"));

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn overrides_replace_lists_and_append_notes() {
        let repo = fixture_repo("overrides");
        write(&repo.join("README.md"), "readme\n");
        write(
            &repo.join(ONBOARDING_OVERRIDE_PATH),
            r#"{"notes": ["Use sandbox creds"], "commands": [{"kind": "test", "command": "./t.sh", "source": "manual-override", "confidence": "high"}]}"#,
        );

        let artifact = build_onboarding_artifact(&repo).unwrap();
        let json = pyjson::dumps_indent2(&artifact);
        assert!(json.contains("\"command\": \"./t.sh\""));
        // notes land after freshness, before telemetry (insertion order).
        let notes_pos = json
            .find("\"notes\": [\n    \"Use sandbox creds\"")
            .unwrap();
        let freshness_pos = json.find("\"freshness\"").unwrap();
        let telemetry_pos = json.find("\"telemetry\"").unwrap();
        assert!(freshness_pos < notes_pos && notes_pos < telemetry_pos);
        assert!(json.contains("\"overrides_applied\": true"));
        assert!(json.contains("\"override_source\": \".aethyme/overrides/onboarding.json\""));

        // Act starter picks the override "test" command as fast_test.
        let act = build_act_starter_artifact(&artifact).unwrap();
        assert_eq!(
            act.get("commands")
                .unwrap()
                .get("fast_test")
                .unwrap()
                .as_str(),
            Some("./t.sh")
        );

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn override_template_matches_python_dumps_shape() {
        let json = pyjson::dumps_indent2(&override_template());
        assert!(json.starts_with("{\n  \"repo\": {\n    \"kind\": \"service\"\n  },"));
        assert!(json.contains("\"command\": \"./scripts/test-fast.sh\""));
        assert!(json.contains("\"Use sandbox credentials from 1Password.\""));
        assert!(json.contains("\"skip_when\": [\n      \"single known file\"\n    ]"));
    }

    #[test]
    fn validate_overrides_matches_python_results() {
        let repo = fixture_repo("validate-ovr");
        // Missing file: ok, exists false.
        let result = validate_overrides(&repo);
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(result.get("exists"), Some(&Value::Bool(false)));

        // Invalid JSON → exc.msg without position.
        write(&repo.join(ONBOARDING_OVERRIDE_PATH), "not json");
        let result = validate_overrides(&repo);
        assert_eq!(result.get("ok"), Some(&Value::Bool(false)));
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert_eq!(errors[0].as_str(), Some("invalid JSON: Expecting value"));

        // Non-object root.
        write(&repo.join(ONBOARDING_OVERRIDE_PATH), "[1, 2]");
        let result = validate_overrides(&repo);
        let errors = result.get("errors").unwrap().as_array().unwrap();
        assert_eq!(
            errors[0].as_str(),
            Some("override root must be a JSON object")
        );

        // Field-level errors; JSON null values are tolerated (Python's
        // `is not None` guard).
        write(
            &repo.join(ONBOARDING_OVERRIDE_PATH),
            r#"{"notes": ["ok", 3], "commands": {}, "summon": [], "extra": null}"#,
        );
        let result = validate_overrides(&repo);
        let errors: Vec<_> = result
            .get("errors")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            errors,
            vec![
                "unknown field $.extra; allowed keys: repo, summon, commands, areas, entrypoints, caution_zones, navigation_recipes, notes",
                "notes must be a list of strings",
                "commands must be a list",
                "summon must be an object",
            ]
        );

        write(
            &repo.join(ONBOARDING_OVERRIDE_PATH),
            r#"{"repo": {"kind": "service", "mystery": true}, "summon": {"other": []}}"#,
        );
        let result = validate_overrides(&repo);
        let errors: Vec<_> = result
            .get("errors")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(errors[0].starts_with("unknown field $.repo.mystery;"));
        assert!(errors[1].starts_with("unknown field $.summon.other;"));

        write(
            &repo.join(ONBOARDING_OVERRIDE_PATH),
            r#"{"notes": null, "commands": null, "summon": null}"#,
        );
        let result = validate_overrides(&repo);
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn invalid_override_marks_telemetry() {
        let repo = fixture_repo("invalid-ovr");
        write(&repo.join(ONBOARDING_OVERRIDE_PATH), "not json");
        let artifact = build_onboarding_artifact(&repo).unwrap();
        let telemetry = artifact.get("telemetry").unwrap();
        assert_eq!(telemetry.get("override_invalid"), Some(&Value::Bool(true)));
        assert_eq!(telemetry.get("overrides_applied"), Some(&Value::Bool(true)));
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
