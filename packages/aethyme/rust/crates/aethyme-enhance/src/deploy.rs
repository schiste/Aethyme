//! The `enhance deploy` / `enhance verify` pipeline — byte-parity port
//! of `src/enhance.py`'s deploy(), verify(), summarize(), settings
//! merge, hook deployment, and legacy-content migration.

use std::path::Path;

use crate::agents::{
    extract_legacy_agents_content, generated_policy_receipt, load_agents_overrides,
    looks_like_generated_agents_document, policy_sha256, render_agents_document,
    render_broker_reference,
};
use crate::onboarding::{
    expected_onboarding_files, override_freshness, recommendation_summary, ACT_STARTER_JSON_PATH,
    ONBOARDING_JSON_PATH,
};
use crate::pyjson::{self, Value};
use crate::telemetry::{
    act_summary_payload, append_event, event_payload_from_generated_artifacts,
    onboarding_summary_payload, summarize_events, write_status_artifacts,
};
use crate::templates;
use crate::{AGENTS_OVERRIDE_PATH, PLACEHOLDER};

/// Settings file that gets MERGED rather than templated.
pub const SETTINGS_FILE: &str = ".claude/settings.local.json";
const HOOK_COMMAND: &str = ".claude/hooks/aethyme-load-context.sh";

/// Static template targets, in the Python `TARGETS` tuple order.
/// (relative path, template content). `CLAUDE.md` renders through the
/// agents document instead of its raw template.
pub const TARGETS: &[(&str, &str)] = &[
    ("CLAUDE.md", templates::AGENTS_MD),
    (".claude/skills/aethyme/SKILL.md", templates::SKILL_MD),
    (".codex/skills/aethyme/SKILL.md", templates::SKILL_MD),
    (".claude/skills/aethyme/references/broker.md", ""),
    (".codex/skills/aethyme/references/broker.md", ""),
    (
        ".codex/skills/aethyme/aethyme-explore",
        templates::AETHYME_EXPLORE,
    ),
    (
        ".claude/skills/aethyme/references/explore.md",
        templates::REF_EXPLORE_MD,
    ),
    (
        ".codex/skills/aethyme/references/explore.md",
        templates::REF_EXPLORE_MD,
    ),
    (
        ".claude/skills/aethyme/references/graph-task.md",
        templates::REF_GRAPH_TASK_MD,
    ),
    (
        ".codex/skills/aethyme/references/graph-task.md",
        templates::REF_GRAPH_TASK_MD,
    ),
    (
        ".claude/skills/aethyme/references/dead-code.md",
        templates::REF_DEAD_CODE_MD,
    ),
    (
        ".codex/skills/aethyme/references/dead-code.md",
        templates::REF_DEAD_CODE_MD,
    ),
    (
        ".claude/hooks/aethyme-load-context.sh",
        templates::LOAD_CONTEXT_SH,
    ),
];

fn render_target(repo: &Path, relative_path: &str, template: &str) -> Result<String, String> {
    match relative_path {
        "CLAUDE.md" => render_agents_document(Some(repo)),
        ".claude/skills/aethyme/references/broker.md"
        | ".codex/skills/aethyme/references/broker.md" => Ok(render_broker_reference()),
        _ => Ok(template.to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct DeployAction {
    pub relative_path: String,
    pub action: &'static str, // "created", "updated", "unchanged"
}

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub relative_path: String,
    /// Portable repository policy fails verification when absent. Optional
    /// local integrations are reported without making a clean clone invalid.
    pub required: bool,
    pub exists: bool,
    pub placeholder_present: bool,
    pub matches_canonical: bool,
    pub policy_provenance: Option<PolicyProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyProvenance {
    Current,
    StaleGenerated { generator_version: String },
    ModifiedAfterGeneration { generator_version: String },
    LegacyGenerated,
    Unmanaged,
}

fn is_executable(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .map(|ext| ext == "sh")
        .unwrap_or(false)
        || Path::new(relative_path)
            .file_name()
            .map(|name| name == "aethyme-explore")
            .unwrap_or(false)
}

fn ensure_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

/// Deploy all enhancement files into `repo`. Byte-parity with the Python
/// `deploy()` — action list order, file contents, telemetry event.
pub fn deploy(repo: &Path, force: bool) -> Result<Vec<DeployAction>, String> {
    deploy_inner(repo, force, true)
}

/// Converge generated supporting artifacts without reading or writing the
/// repository's root policy files. Repository upgrades use this lane so
/// AGENTS.md and CLAUDE.md can be migrated under an explicit ownership and
/// resolution contract without a temporary force-replacement window.
pub fn deploy_supporting_artifacts(repo: &Path, force: bool) -> Result<Vec<DeployAction>, String> {
    deploy_inner(repo, force, false)
}

fn deploy_inner(
    repo: &Path,
    force: bool,
    manage_root_policy: bool,
) -> Result<Vec<DeployAction>, String> {
    let mut actions: Vec<DeployAction> = Vec::new();
    if manage_root_policy {
        for relative in ["AGENTS.md", "CLAUDE.md"] {
            let path = repo.join(relative);
            if !path.exists() {
                continue;
            }
            let existing = read_text(&path)?;
            if generated_policy_receipt(&existing)
                .is_some_and(|receipt| receipt.policy_sha256 != policy_sha256(&receipt.policy_body))
            {
                return Err(format!(
                    "customized generated policy {relative} requires reviewed resolution; run `aethyme upgrade plan --repo . --diff`, choose preserve, merge, or replace, then apply the confirmed plan"
                ));
            }
        }
        if let Some(action) = drop_stale_generated_agents_override(repo)? {
            actions.push(action);
        }
        if let Some(action) = migrate_legacy_agents_content(repo)? {
            actions.push(action);
        }
    }

    for (relative_path, content) in expected_onboarding_files(repo)? {
        let dest = repo.join(&relative_path);
        if dest.exists() && !force && read_text(&dest)? == content {
            actions.push(DeployAction {
                relative_path,
                action: "unchanged",
            });
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let existed = dest.exists();
        write_text(&dest, &content)?;
        actions.push(DeployAction {
            relative_path,
            action: if existed { "updated" } else { "created" },
        });
    }

    if manage_root_policy {
        actions.insert(0, ensure_agents_document(repo, force)?);
    }
    for (relative_path, template) in TARGETS {
        if !manage_root_policy && *relative_path == "CLAUDE.md" {
            continue;
        }
        let dest = repo.join(relative_path);
        let content = render_target(repo, relative_path, template)?;
        if dest.exists() && !force && read_text(&dest)? == content {
            // Still ensure the executable bit is right on shell scripts.
            if is_executable(relative_path) {
                ensure_executable(&dest)?;
            }
            actions.push(DeployAction {
                relative_path: relative_path.to_string(),
                action: "unchanged",
            });
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let existed = dest.exists();
        write_text(&dest, &content)?;
        if is_executable(relative_path) {
            ensure_executable(&dest)?;
        }
        actions.push(DeployAction {
            relative_path: relative_path.to_string(),
            action: if existed { "updated" } else { "created" },
        });
    }

    actions.push(ensure_settings_hook(repo)?);

    if manage_root_policy {
        let mut payload = Value::object();
        payload.set("force", Value::Bool(force));
        payload.set(
            "actions",
            Value::Array(
                actions
                    .iter()
                    .map(|action| {
                        let mut entry = Value::object();
                        entry.set("path", Value::str(action.relative_path.clone()));
                        entry.set("action", Value::str(action.action));
                        entry
                    })
                    .collect(),
            ),
        );
        if let Some(entries) = event_payload_from_generated_artifacts(repo)?.as_object() {
            for (key, value) in entries {
                payload.set(key, value.clone());
            }
        }
        append_event(repo, "enhance.deploy", payload)?;
    }

    Ok(actions)
}

fn ensure_agents_document(repo: &Path, force: bool) -> Result<DeployAction, String> {
    let dest = repo.join("AGENTS.md");
    let content = render_agents_document(Some(repo))?;
    let existed = dest.exists();
    if !existed {
        write_text(&dest, &content)?;
        return Ok(DeployAction {
            relative_path: "AGENTS.md".to_string(),
            action: "created",
        });
    }
    let existing = read_text(&dest)?;
    if !force && existing == content {
        return Ok(DeployAction {
            relative_path: "AGENTS.md".to_string(),
            action: "unchanged",
        });
    }
    write_text(&dest, &content)?;
    Ok(DeployAction {
        relative_path: "AGENTS.md".to_string(),
        action: "updated",
    })
}

/// Migrate legacy or hand-edited AGENTS.md content into structured
/// overrides once.
fn migrate_legacy_agents_content(repo: &Path) -> Result<Option<DeployAction>, String> {
    let agents_path = repo.join("AGENTS.md");
    if !agents_path.exists() {
        return Ok(None);
    }
    let existing = read_text(&agents_path)?;
    let legacy_content = extract_legacy_agents_content(&existing)?;
    if legacy_content.is_empty() {
        return Ok(None);
    }

    let override_path = repo.join(AGENTS_OVERRIDE_PATH);
    let overrides = load_agents_overrides(repo);
    if overrides
        .get("_invalid_override")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let mut clean_overrides = Value::Object(
        overrides
            .as_object()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(key, _)| !key.starts_with('_'))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    );
    let existing_markdown = clean_overrides
        .get("maintainer_markdown")
        .and_then(Value::as_str)
        .map(str::to_string);
    match existing_markdown {
        Some(markdown) if !markdown.trim().is_empty() => {
            if markdown.contains(legacy_content.trim()) {
                return Ok(None);
            }
            clean_overrides.set(
                "maintainer_markdown",
                Value::str(format!(
                    "{}\n\n{}",
                    markdown.trim_end(),
                    legacy_content.trim()
                )),
            );
        }
        _ => {
            clean_overrides.set("maintainer_markdown", Value::str(legacy_content.trim()));
        }
    }

    if let Some(parent) = override_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let existed = override_path.exists();
    write_text(
        &override_path,
        &format!("{}\n", pyjson::dumps_indent2(&clean_overrides)),
    )?;
    Ok(Some(DeployAction {
        relative_path: AGENTS_OVERRIDE_PATH.to_string(),
        action: if existed { "updated" } else { "created" },
    }))
}

/// Remove old generated Aethyme boilerplate from maintainer overrides.
fn drop_stale_generated_agents_override(repo: &Path) -> Result<Option<DeployAction>, String> {
    let override_path = repo.join(AGENTS_OVERRIDE_PATH);
    if !override_path.exists() {
        return Ok(None);
    }
    let overrides = load_agents_overrides(repo);
    if overrides
        .get("_invalid_override")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let is_stale = overrides
        .get("maintainer_markdown")
        .and_then(Value::as_str)
        .map(|markdown| looks_like_generated_agents_document(markdown.trim()))
        .unwrap_or(false);
    if !is_stale {
        return Ok(None);
    }

    let clean_overrides = Value::Object(
        overrides
            .as_object()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(key, _)| !key.starts_with('_') && key != "maintainer_markdown")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    );
    if clean_overrides.truthy() {
        write_text(
            &override_path,
            &format!("{}\n", pyjson::dumps_indent2(&clean_overrides)),
        )?;
    } else {
        std::fs::remove_file(&override_path)
            .map_err(|e| format!("{}: {e}", override_path.display()))?;
    }
    Ok(Some(DeployAction {
        relative_path: AGENTS_OVERRIDE_PATH.to_string(),
        action: "updated",
    }))
}

fn settings_has_hook_entry(session_start: &Value) -> bool {
    let Some(entries) = session_start.as_array() else {
        return false;
    };
    entries.iter().any(|entry| {
        entry.is_object()
            && entry
                .get("hooks")
                .filter(|hooks| hooks.truthy())
                .and_then(Value::as_array)
                .map(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.is_object()
                            && hook.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
                    })
                })
                .unwrap_or(false)
    })
}

fn session_start_hook_entry() -> Value {
    let mut hook = Value::object();
    hook.set("type", Value::str("command"));
    hook.set("command", Value::str(HOOK_COMMAND));
    let mut entry = Value::object();
    entry.set("matcher", Value::str(""));
    entry.set("hooks", Value::Array(vec![hook]));
    entry
}

/// Add the SessionStart hook to `.claude/settings.local.json`
/// (merge-aware, idempotent).
pub(crate) fn ensure_settings_hook(repo: &Path) -> Result<DeployAction, String> {
    let settings_path = repo.join(SETTINGS_FILE);
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }

    let mut existed = settings_path.exists();
    let mut settings = Value::object();
    if existed {
        let text = read_text(&settings_path)?;
        match pyjson::loads(&text) {
            Ok(parsed) if parsed.is_object() => settings = parsed,
            // Exists but isn't a JSON object (or isn't JSON) — back up
            // and start fresh, exactly like the Python flow.
            _ => {
                let backup = settings_path.with_extension("json.bak");
                std::fs::rename(&settings_path, &backup)
                    .map_err(|e| format!("{}: {e}", settings_path.display()))?;
                existed = false;
            }
        }
    }

    // settings.setdefault("hooks", {}) with the non-dict repair.
    let hooks_ok = settings.get("hooks").map(Value::is_object).unwrap_or(false);
    if !hooks_ok {
        settings.set("hooks", Value::object());
    }
    let mut hooks = settings.get("hooks").cloned().expect("just ensured");
    let session_ok = hooks
        .get("SessionStart")
        .map(Value::is_array)
        .unwrap_or(false);
    if !session_ok {
        hooks.set("SessionStart", Value::Array(vec![]));
    }
    let mut session_start = hooks.get("SessionStart").cloned().expect("just ensured");

    if settings_has_hook_entry(&session_start) {
        return Ok(DeployAction {
            relative_path: SETTINGS_FILE.to_string(),
            action: "unchanged",
        });
    }

    if let Value::Array(entries) = &mut session_start {
        entries.push(session_start_hook_entry());
    }
    hooks.set("SessionStart", session_start);
    settings.set("hooks", hooks);
    write_text(
        &settings_path,
        &format!("{}\n", pyjson::dumps_indent2(&settings)),
    )?;
    Ok(DeployAction {
        relative_path: SETTINGS_FILE.to_string(),
        action: if existed { "updated" } else { "created" },
    })
}

/// Check each target plus the merged settings hook entry.
pub fn verify(repo: &Path) -> Result<Vec<VerifyResult>, String> {
    let mut out: Vec<VerifyResult> = Vec::new();
    out.push(verify_agents_document(repo)?);
    for (relative_path, template) in TARGETS {
        let dest = repo.join(relative_path);
        if !dest.exists() {
            out.push(VerifyResult {
                relative_path: relative_path.to_string(),
                required: !relative_path.starts_with(".claude/"),
                exists: false,
                placeholder_present: false,
                matches_canonical: false,
                policy_provenance: None,
            });
            continue;
        }
        let actual = read_text(&dest)?;
        let canonical = render_target(repo, relative_path, template)?;
        let (matches_canonical, policy_provenance) = if *relative_path == "CLAUDE.md" {
            let (matches, provenance) = verify_policy_content(&actual, &canonical);
            (matches, Some(provenance))
        } else {
            (actual == canonical, None)
        };
        out.push(VerifyResult {
            relative_path: relative_path.to_string(),
            required: !relative_path.starts_with(".claude/"),
            exists: true,
            placeholder_present: actual.contains(PLACEHOLDER),
            matches_canonical,
            policy_provenance,
        });
    }

    for (relative_path, content) in expected_onboarding_files(repo)? {
        let dest = repo.join(&relative_path);
        if !dest.exists() {
            let required = !relative_path.starts_with(".claude/");
            out.push(VerifyResult {
                relative_path,
                required,
                exists: false,
                placeholder_present: false,
                matches_canonical: false,
                policy_provenance: None,
            });
            continue;
        }
        let actual = read_text(&dest)?;
        out.push(VerifyResult {
            required: !relative_path.starts_with(".claude/"),
            relative_path,
            exists: true,
            placeholder_present: actual.contains(PLACEHOLDER),
            matches_canonical: actual == content,
            policy_provenance: None,
        });
    }

    // Settings hook check.
    let settings_label = format!("{SETTINGS_FILE} (SessionStart hook)");
    let settings_path = repo.join(SETTINGS_FILE);
    if !settings_path.exists() {
        out.push(VerifyResult {
            relative_path: settings_label,
            required: false,
            exists: false,
            placeholder_present: false,
            matches_canonical: false,
            policy_provenance: None,
        });
    } else {
        let text = read_text(&settings_path)?;
        match pyjson::loads(&text) {
            Err(_) => out.push(VerifyResult {
                relative_path: settings_label,
                required: false,
                exists: true,
                placeholder_present: false,
                matches_canonical: false,
                policy_provenance: None,
            }),
            Ok(settings) => {
                let empty = Value::object();
                let hooks = match settings.get("hooks") {
                    Some(v) if v.truthy() => v,
                    _ => &empty,
                };
                let empty_list = Value::Array(vec![]);
                let session_start = match hooks.get("SessionStart") {
                    Some(v) if v.truthy() => v,
                    _ => &empty_list,
                };
                out.push(VerifyResult {
                    relative_path: settings_label,
                    required: false,
                    exists: settings_has_hook_entry(session_start),
                    placeholder_present: false,
                    matches_canonical: true,
                    policy_provenance: None,
                });
            }
        }
    }

    Ok(out)
}

fn verify_agents_document(repo: &Path) -> Result<VerifyResult, String> {
    let dest = repo.join("AGENTS.md");
    if !dest.exists() {
        return Ok(VerifyResult {
            relative_path: "AGENTS.md".to_string(),
            required: true,
            exists: false,
            placeholder_present: false,
            matches_canonical: false,
            policy_provenance: None,
        });
    }
    let actual = read_text(&dest)?;
    let content = render_agents_document(Some(repo))?;
    let (matches_canonical, policy_provenance) = verify_policy_content(&actual, &content);
    Ok(VerifyResult {
        relative_path: "AGENTS.md".to_string(),
        required: true,
        exists: true,
        placeholder_present: actual.contains(PLACEHOLDER),
        matches_canonical,
        policy_provenance: Some(policy_provenance),
    })
}

fn verify_policy_content(actual: &str, canonical: &str) -> (bool, PolicyProvenance) {
    if policy_matches_canonical(actual, canonical) {
        return (true, PolicyProvenance::Current);
    }
    let canonical_receipt = generated_policy_receipt(canonical)
        .expect("rendered canonical policy always carries provenance");
    if let Some(receipt) = generated_policy_receipt(actual) {
        if policy_sha256(&receipt.policy_body) != receipt.policy_sha256 {
            return (
                false,
                PolicyProvenance::ModifiedAfterGeneration {
                    generator_version: receipt.generator_version,
                },
            );
        }
        if receipt.policy_sha256 == canonical_receipt.policy_sha256 {
            return (true, PolicyProvenance::Current);
        }
        return (
            false,
            PolicyProvenance::StaleGenerated {
                generator_version: receipt.generator_version,
            },
        );
    }
    if looks_like_generated_agents_document(actual) {
        let normalized = format!("{}\n", actual.trim_end());
        return (
            policy_sha256(&normalized) == canonical_receipt.policy_sha256,
            PolicyProvenance::LegacyGenerated,
        );
    }
    (false, PolicyProvenance::Unmanaged)
}

fn policy_matches_canonical(actual: &str, canonical: &str) -> bool {
    if actual == canonical {
        return true;
    }
    if actual.matches(crate::BLOCK_BEGIN).count() != 1
        || actual.matches(crate::BLOCK_END).count() != 1
    {
        return false;
    }
    let managed = format!(
        "{}\n{}\n{}",
        crate::BLOCK_BEGIN,
        canonical.trim_end(),
        crate::BLOCK_END
    );
    crate::render::splice_generated_block(actual, &managed) == actual
}

/// True iff every target exists and has its placeholder substituted;
/// content drift is allowed except for the generated root documents.
pub fn is_ok(results: &[VerifyResult]) -> bool {
    results.iter().all(|r| {
        !r.required
            || (r.exists
                && !r.placeholder_present
                && (r.matches_canonical
                    || !matches!(r.relative_path.as_str(), "AGENTS.md" | "CLAUDE.md")))
    })
}

/// Small enhancement summary with recommendations.
pub fn summarize(repo: &Path) -> Result<Value, String> {
    let files = expected_onboarding_files(repo)?;
    let content_of = |path: &str| -> Result<Value, String> {
        let (_, content) = files
            .iter()
            .find(|(rel, _)| rel == path)
            .ok_or_else(|| format!("KeyError: '{path}'"))?;
        pyjson::loads(content)
    };
    let onboarding = content_of(ONBOARDING_JSON_PATH)?;
    let act = content_of(ACT_STARTER_JSON_PATH)?;
    let recommendation = recommendation_summary(repo)?;
    let req = |value: &Value, key: &str| -> Result<Value, String> {
        value
            .get(key)
            .cloned()
            .ok_or_else(|| format!("KeyError: '{key}'"))
    };
    let mut summary = Value::object();
    summary.set(
        "recommended_skill",
        req(&recommendation, "recommended_skill")?,
    );
    summary.set(
        "recommended_mode",
        req(&recommendation, "recommended_mode")?,
    );
    summary.set("reason", req(&recommendation, "reason")?);
    summary.set("onboarding", onboarding_summary_payload(&onboarding)?);
    summary.set("act", act_summary_payload(&act)?);
    summary.set("freshness", override_freshness(repo));
    summary.set("experience_telemetry", summarize_events(repo)?);
    Ok(summary)
}

/// Refresh repo-local experience status artifacts.
pub fn refresh_status(repo: &Path) -> Result<Value, String> {
    write_status_artifacts(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aethyme-enhance-deploy-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "# Fixture\n").unwrap();
        dir
    }

    #[test]
    fn deploy_writes_all_targets_and_settings() {
        let repo = fixture_repo("full");
        let actions = deploy(&repo, true).unwrap();
        // AGENTS.md is inserted first; settings merge is last.
        assert_eq!(actions.first().unwrap().relative_path, "AGENTS.md");
        assert_eq!(actions.last().unwrap().relative_path, SETTINGS_FILE);
        assert!(actions.iter().all(|a| a.action == "created"));
        // 1 AGENTS + 6 onboarding + 13 targets + settings = 21.
        assert_eq!(actions.len(), 21);

        let settings = std::fs::read_to_string(repo.join(SETTINGS_FILE)).unwrap();
        assert_eq!(
            settings,
            "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"matcher\": \"\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \".claude/hooks/aethyme-load-context.sh\"\n          }\n        ]\n      }\n    ]\n  }\n}\n"
        );

        // Hook and Codex wrapper are executable.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(repo.join(".claude/hooks/aethyme-load-context.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        let wrapper_mode = std::fs::metadata(repo.join(".codex/skills/aethyme/aethyme-explore"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(wrapper_mode & 0o111, 0o111);

        // Content-addressed onboarding excludes its own generated outputs,
        // so an unchanged source snapshot makes the whole deploy idempotent.
        let actions = deploy(&repo, false).unwrap();
        let changed: Vec<&str> = actions
            .iter()
            .filter(|a| a.action != "unchanged")
            .map(|a| a.relative_path.as_str())
            .collect();
        assert!(
            changed.is_empty(),
            "unexpected changed targets: {changed:?}"
        );

        let results = verify(&repo).unwrap();
        assert!(is_ok(&results));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn generated_agents_and_claude_share_the_advisory_lifecycle_contract() {
        let repo = fixture_repo("broker-advisory-protocol");
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/gates.toml"), "[[gate]]\n").unwrap();

        deploy(&repo, true).unwrap();

        let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
        let claude = std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap();
        assert_eq!(agents, claude);
        for required in [
            "delivered\nadvisories as work context",
            "aethyme broker status --json",
        ] {
            assert!(
                agents.contains(required),
                "missing protocol clause: {required}"
            );
        }
        let reference =
            std::fs::read_to_string(repo.join(".codex/skills/aethyme/references/broker.md"))
                .unwrap();
        for required in [
            "gitignored persistence projection",
            "Acknowledging a notice stops repeat delivery",
            "selected integration prefix clears only entries contained",
            "Advisories never expand gate selection or block submit",
        ] {
            assert!(
                reference.contains(required),
                "missing reference clause: {required}"
            );
        }

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn supporting_artifact_deploy_never_touches_root_policy() {
        let repo = fixture_repo("supporting-only");
        std::fs::write(repo.join("AGENTS.md"), "maintainer agents policy\n").unwrap();
        std::fs::write(repo.join("CLAUDE.md"), "maintainer claude policy\n").unwrap();

        let actions = deploy_supporting_artifacts(&repo, true).unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.join("AGENTS.md")).unwrap(),
            "maintainer agents policy\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("CLAUDE.md")).unwrap(),
            "maintainer claude policy\n"
        );
        assert!(actions
            .iter()
            .all(|action| { !matches!(action.relative_path.as_str(), "AGENTS.md" | "CLAUDE.md") }));
        assert!(!repo
            .join(".aethyme/generated/experience-telemetry.jsonl")
            .exists());
    }

    #[test]
    fn settings_merge_preserves_user_content() {
        let repo = fixture_repo("settings-merge");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(SETTINGS_FILE),
            "{\"permissions\": {\"allow\": [\"Bash(ls:*)\"]}, \"hooks\": {\"PostToolUse\": []}}\n",
        )
        .unwrap();
        let action = ensure_settings_hook(&repo).unwrap();
        assert_eq!(action.action, "updated");
        let merged = std::fs::read_to_string(repo.join(SETTINGS_FILE)).unwrap();
        // User keys keep their positions; SessionStart appends inside hooks.
        assert!(merged.starts_with("{\n  \"permissions\": {"));
        assert!(merged.contains("\"PostToolUse\": [],"));
        assert!(merged.contains("\"SessionStart\": ["));
        // Idempotent.
        let action = ensure_settings_hook(&repo).unwrap();
        assert_eq!(action.action, "unchanged");
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn non_object_settings_backed_up() {
        let repo = fixture_repo("settings-bak");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(repo.join(SETTINGS_FILE), "[1, 2]\n").unwrap();
        let action = ensure_settings_hook(&repo).unwrap();
        assert_eq!(action.action, "created");
        assert!(repo.join(".claude/settings.local.json.bak").exists());
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn legacy_agents_content_migrates_to_overrides() {
        let repo = fixture_repo("legacy");
        std::fs::write(repo.join("AGENTS.md"), "Hand-written policy.\n").unwrap();
        let actions = deploy(&repo, false).unwrap();
        // Migration action reported, override created.
        assert!(actions
            .iter()
            .any(|a| a.relative_path == AGENTS_OVERRIDE_PATH && a.action == "created"));
        let override_text = std::fs::read_to_string(repo.join(AGENTS_OVERRIDE_PATH)).unwrap();
        assert_eq!(
            override_text,
            "{\n  \"maintainer_markdown\": \"Hand-written policy.\"\n}\n"
        );
        // The regenerated AGENTS.md carries the migrated note.
        let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
        assert!(agents.contains("## Maintainer Notes\n\nHand-written policy."));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn stale_generated_override_is_dropped() {
        let repo = fixture_repo("stale");
        std::fs::create_dir_all(repo.join(".aethyme/overrides")).unwrap();
        // A maintainer_markdown that IS the generated document.
        let generated = render_agents_document(None).unwrap();
        let mut overrides = Value::object();
        overrides.set("maintainer_markdown", Value::str(generated));
        std::fs::write(
            repo.join(AGENTS_OVERRIDE_PATH),
            format!("{}\n", pyjson::dumps_indent2(&overrides)),
        )
        .unwrap();
        let action = drop_stale_generated_agents_override(&repo)
            .unwrap()
            .unwrap();
        assert_eq!(action.action, "updated");
        // Nothing else in the override file → deleted outright.
        assert!(!repo.join(AGENTS_OVERRIDE_PATH).exists());
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn policy_provenance_distinguishes_stale_generation_from_later_edits() {
        let canonical = render_agents_document(None).unwrap();
        let prior_body =
            "This file is generated by Aethyme. Do not edit it directly.\n\nPrior policy.\n";
        let prior = crate::agents::stamp_generated_policy(prior_body, "0.3.0");

        let (matches, provenance) = verify_policy_content(&prior, &canonical);
        assert!(!matches);
        assert_eq!(
            provenance,
            PolicyProvenance::StaleGenerated {
                generator_version: "0.3.0".into()
            }
        );

        let edited = format!("{prior}\nManual direct edit.\n");
        let (matches, provenance) = verify_policy_content(&edited, &canonical);
        assert!(!matches);
        assert_eq!(
            provenance,
            PolicyProvenance::ModifiedAfterGeneration {
                generator_version: "0.3.0".into()
            }
        );
    }

    #[test]
    fn deploy_refuses_modified_generated_policy_before_writing() {
        let repo = fixture_repo("customized-policy");
        deploy(&repo, true).unwrap();
        let before_skill = std::fs::read(repo.join(".codex/skills/aethyme/SKILL.md")).unwrap();
        let mut agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
        agents.push_str("\nmaintainer direct edit\n");
        std::fs::write(repo.join("AGENTS.md"), agents).unwrap();

        let error = deploy(&repo, true).unwrap_err();
        assert!(error.contains("customized generated policy AGENTS.md"));
        assert!(error.contains("upgrade plan --repo . --diff"));
        assert_eq!(
            std::fs::read(repo.join(".codex/skills/aethyme/SKILL.md")).unwrap(),
            before_skill
        );
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
