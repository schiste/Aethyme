//! AGENTS.md / CLAUDE.md generation with overrides and legacy-content
//! migration — byte-parity port of the agents-document half of
//! `src/enhance.py`.

use std::path::Path;

use crate::pyjson::{self, Value};
use crate::render::substitute_root;
use crate::templates;
use crate::{AGENTS_OVERRIDE_PATH, BLOCK_BEGIN, BLOCK_END};

/// `_load_agents_overrides`: missing → `{}`, unreadable/invalid/non-object
/// → `{_invalid_override: true, _source: …}`, else payload + `_source`
/// appended — the exact Python tolerances.
pub fn load_agents_overrides(repo: &Path) -> Value {
    let override_path = repo.join(AGENTS_OVERRIDE_PATH);
    if !override_path.exists() {
        return Value::object();
    }
    let invalid = || {
        let mut marker = Value::object();
        marker.set("_invalid_override", Value::Bool(true));
        marker.set("_source", Value::str(AGENTS_OVERRIDE_PATH));
        marker
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
    payload.set("_source", Value::str(AGENTS_OVERRIDE_PATH));
    payload
}

/// `_render_agents_document`: template + routing + broker protocol +
/// override sections, normalized to a single trailing newline.
pub fn render_agents_document(root: &str, repo: Option<&Path>) -> Result<String, String> {
    let mut content = substitute_root(templates::AGENTS_MD, root);
    if let Some(repo) = repo {
        let routing = render_repo_routing(repo)?;
        if !routing.is_empty() {
            content = format!("{}\n\n{routing}", content.trim_end());
        }
        let broker = render_broker_protocol(repo, root);
        if !broker.is_empty() {
            content = format!("{}\n\n{broker}", content.trim_end());
        }
        let override_sections = render_agents_override_sections(repo);
        if !override_sections.is_empty() {
            content = format!("{}\n\n{override_sections}", content.trim_end());
        }
    }
    Ok(format!("{}\n", content.trim_end()))
}

/// Agent-facing broker coordination protocol, rendered only when the repo
/// is broker-configured.
fn render_broker_protocol(repo: &Path, root: &str) -> String {
    if !(repo.join(".aethyme/gates.toml").exists() || repo.join(".aethyme/config.toml").exists()) {
        return String::new();
    }
    format!(
        r#"## Broker Coordination (multi-agent repository)

This repository coordinates concurrent agent sessions through the Aethyme
broker. Other agents may be working in sibling worktrees right now. The
`aethyme` binary is installed once with
`cargo install --path "{root}/rust/crates/aethyme-engine"`
(check with `aethyme --version`). Follow this protocol:

1. **Before editing**, check current activity and register yourself:

   ```bash
   aethyme broker status --json    # who is working on what
   aethyme broker adopt --task "<your task>"   # once, from your worktree
   ```

   If `status` shows another session holding leases on the files you plan
   to change, prefer working elsewhere first or say so in your report —
   overlapping edits will conflict at merge time.

2. **While working**: commit early and small. Only committed work can be
   verified and integrated. Never switch branches inside someone else's
   worktree; never edit files outside your own worktree.

3. **When your task is complete**, submit your head commit for verified
   integration instead of merging anything yourself:

   ```bash
   aethyme broker submit --session <your-session-id>
   ```

   This simulates the merge and runs only the checks your diff affects.
   Report the outcome (verified / rejected / conflict) in your summary.
   Afterwards, finish the session with
   `aethyme broker close --session <id>` (state only), or point it at
   a follow-up task with `aethyme broker adopt --reuse --task "..."`.

4. **If a file named `.aethyme/broker-action-required.md` appears in your
   worktree**, read it immediately: your submission conflicted. It names
   the conflicting files, the blocking session, and the exact rebase
   steps. Resolve, commit, and resubmit.

5. **Never** push, merge to the default branch, or touch the
   `aethyme/integration` branch directly — integration and shipping are
   handled through the broker and the human operator."#
    )
}

fn render_repo_routing(repo: &Path) -> Result<String, String> {
    use crate::onboarding::{
        ACT_CLAUDE_PATH, ACT_CODEX_PATH, ACT_STARTER_JSON_PATH, ONBOARDING_CLAUDE_PATH,
        ONBOARDING_CODEX_PATH, ONBOARDING_JSON_PATH,
    };
    use crate::telemetry::STATUS_MARKDOWN_PATH;

    let onboarding_path = repo.join(ONBOARDING_JSON_PATH);
    let act_path = repo.join(ACT_STARTER_JSON_PATH);
    if !onboarding_path.exists() {
        return Ok(String::new());
    }
    let onboarding_text = std::fs::read_to_string(&onboarding_path)
        .map_err(|e| format!("{}: {e}", onboarding_path.display()))?;
    let onboarding = pyjson::loads(&onboarding_text)
        .map_err(|e| format!("{}: {e}", onboarding_path.display()))?;
    let act = if act_path.exists() {
        let act_text = std::fs::read_to_string(&act_path)
            .map_err(|e| format!("{}: {e}", act_path.display()))?;
        pyjson::loads(&act_text).map_err(|e| format!("{}: {e}", act_path.display()))?
    } else {
        Value::object()
    };
    let empty = Value::object();
    let primary_commands = match onboarding.get("primary_commands") {
        Some(v) if v.truthy() => v,
        _ => &empty,
    };
    let primary_entrypoints = match onboarding.get("primary_entrypoints") {
        Some(v) if v.truthy() => v,
        _ => &empty,
    };
    let act_commands = match act.get("commands") {
        Some(v) if v.truthy() => v,
        _ => &empty,
    };
    let fast_test = match primary_commands.get("fast_test") {
        Some(v) if v.truthy() => Some(v.clone()),
        _ => match act_commands.get("fast_test") {
            Some(v) => Some(v.clone()),
            None => None,
        },
    };
    let app_entrypoint = match primary_entrypoints.get("app") {
        Some(v) if v.truthy() => v.clone(),
        _ => Value::object(),
    };
    let mut lines: Vec<String> = vec![
        "## Aethyme Repo Routing".to_string(),
        String::new(),
        format!("- Onboarding skill: `{ONBOARDING_CODEX_PATH}` or `{ONBOARDING_CLAUDE_PATH}`"),
        format!("- Act skill: `{ACT_CODEX_PATH}` or `{ACT_CLAUDE_PATH}`"),
        format!("- Experience status: `{STATUS_MARKDOWN_PATH}`"),
    ];
    if let Some(fast_test) = fast_test {
        if fast_test.truthy() {
            lines.push(format!("- Primary fast test: `{}`", fast_test.py_str()));
        }
    }
    if let Some(path) = app_entrypoint.get("path") {
        if path.truthy() {
            lines.push(format!("- Primary app entrypoint: `{}`", path.py_str()));
        }
    }
    Ok(lines.join("\n"))
}

fn render_agents_override_sections(repo: &Path) -> String {
    let overrides = load_agents_overrides(repo);
    if overrides
        .get("_invalid_override")
        .map(Value::truthy)
        .unwrap_or(false)
    {
        return format!(
            "## Aethyme Override Status\n\nAgents override file `{AGENTS_OVERRIDE_PATH}` is invalid JSON. Fix it and rerun `aethyme enhance deploy --repo \"$PWD\"`.\n"
        );
    }
    let mut sections: Vec<String> = Vec::new();
    if let Some(Value::Str(repo_summary)) = overrides.get("repo_summary") {
        if !repo_summary.trim().is_empty() {
            sections.push(format!("## Repo Summary\n\n{}", repo_summary.trim()));
        }
    }
    sections.extend(render_override_list_section(
        "## Hard Constraints",
        overrides.get("hard_constraints"),
    ));
    sections.extend(render_override_list_section(
        "## Validation Rules",
        overrides.get("validation_rules"),
    ));
    sections.extend(render_override_list_section(
        "## Commit Hygiene Notes",
        overrides.get("commit_hygiene_notes"),
    ));
    sections.extend(render_override_list_section(
        "## Summon Policy Notes",
        overrides.get("summon_policy_notes"),
    ));
    if let Some(Value::Str(maintainer_markdown)) = overrides.get("maintainer_markdown") {
        let trimmed = maintainer_markdown.trim();
        if !trimmed.is_empty() && !looks_like_generated_agents_document(trimmed) {
            sections.push(format!("## Maintainer Notes\n\n{trimmed}"));
        }
    }
    sections.join("\n\n")
}

fn render_override_list_section(title: &str, value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let items: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            Value::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    vec![
        title.to_string(),
        items
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ]
}

/// Detect legacy generated Aethyme root guidance, including stale variants.
pub fn looks_like_generated_agents_document(content: &str) -> bool {
    if content.is_empty() {
        return false;
    }
    [
        "# Agent Instructions",
        "This repository is **Aethyme-enhanced**",
        "## Quick start (any agent)",
        "## Detailed reference",
        "## Verifying this enhancement",
    ]
    .iter()
    .all(|marker| content.contains(marker))
}

/// `_extract_legacy_agents_content`.
pub fn extract_legacy_agents_content(existing: &str, root: &str) -> Result<String, String> {
    let stripped = existing.trim();
    if stripped.is_empty() {
        return Ok(String::new());
    }

    if existing.contains(BLOCK_BEGIN) && existing.contains(BLOCK_END) {
        let (before, remainder) = existing
            .split_once(BLOCK_BEGIN)
            .expect("checked contains BEGIN");
        let Some((_, after)) = remainder.split_once(BLOCK_END) else {
            // Python: remainder.split(END, 1) raises ValueError when END
            // precedes BEGIN — same failure surface.
            return Err("AETHYME:END marker precedes AETHYME:BEGIN".to_string());
        };
        let pieces: Vec<&str> = [before.trim(), after.trim()]
            .into_iter()
            .filter(|piece| !piece.is_empty())
            .collect();
        return Ok(pieces.join("\n\n"));
    }

    if looks_like_generated_agents_document(stripped) {
        return Ok(String::new());
    }

    let rendered_template = substitute_root(templates::AGENTS_MD, root);
    if stripped == rendered_template.trim() {
        return Ok(String::new());
    }
    Ok(stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aethyme-enhance-agents-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn document_without_repo_is_template_with_root() {
        let doc = render_agents_document("/opt/ae", None).unwrap();
        assert!(doc.contains("AETHYME_ROOT=\"/opt/ae\""));
        assert!(!doc.contains("{{AETHYME_ROOT}}"));
        assert!(doc.ends_with("placeholders.\n"));
    }

    #[test]
    fn broker_section_gated_on_config() {
        let repo = fixture_repo("broker");
        let doc = render_agents_document("/opt/ae", Some(&repo)).unwrap();
        assert!(!doc.contains("## Broker Coordination"));
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/gates.toml"), "[[gate]]\n").unwrap();
        let doc = render_agents_document("/opt/ae", Some(&repo)).unwrap();
        assert!(doc.contains("## Broker Coordination (multi-agent repository)"));
        assert!(doc.contains("aethyme broker submit --session"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn override_sections_render_known_keys_and_ignore_unknown() {
        let repo = fixture_repo("overrides");
        std::fs::create_dir_all(repo.join(".aethyme/overrides")).unwrap();
        std::fs::write(
            repo.join(AGENTS_OVERRIDE_PATH),
            r#"{"repo_summary": " Summary here. ", "hard_constraints": ["Never break tenancy", "", 42], "unknown_key": ["ignored"]}"#,
        )
        .unwrap();
        let doc = render_agents_document("/opt/ae", Some(&repo)).unwrap();
        assert!(doc.contains("## Repo Summary\n\nSummary here."));
        assert!(doc.contains("## Hard Constraints\n\n- Never break tenancy"));
        assert!(!doc.contains("ignored"));
        assert!(!doc.contains("42"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn invalid_override_renders_status_section() {
        let repo = fixture_repo("invalid");
        std::fs::create_dir_all(repo.join(".aethyme/overrides")).unwrap();
        std::fs::write(repo.join(AGENTS_OVERRIDE_PATH), "{broken").unwrap();
        let doc = render_agents_document("/opt/ae", Some(&repo)).unwrap();
        assert!(doc.contains("## Aethyme Override Status"));
        assert!(doc.contains("is invalid JSON"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn legacy_content_extraction_matches_python() {
        // Block-managed file: keep surroundings.
        let existing = format!("intro\n\n{BLOCK_BEGIN}\ngen\n{BLOCK_END}\n\noutro\n");
        assert_eq!(
            extract_legacy_agents_content(&existing, "/opt/ae").unwrap(),
            "intro\n\noutro"
        );
        // Generated document: nothing to migrate.
        let generated = render_agents_document("/opt/ae", None).unwrap();
        assert_eq!(
            extract_legacy_agents_content(&generated, "/opt/ae").unwrap(),
            ""
        );
        // Hand-written content: migrated verbatim (stripped).
        assert_eq!(
            extract_legacy_agents_content("  my notes\n", "/opt/ae").unwrap(),
            "my notes"
        );
    }
}
