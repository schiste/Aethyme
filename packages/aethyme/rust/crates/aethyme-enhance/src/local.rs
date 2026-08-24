//! Opt-in repository-local deployment behind a tiny committed bridge.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::PLACEHOLDER;
use crate::agents::render_agents_document;
use crate::deploy::TARGETS;
use crate::onboarding::expected_onboarding_files;

pub const LOCAL_MARKER_PATH: &str = ".aethyme/local/enabled";
pub const LOCAL_POLICY_PATH: &str = ".aethyme/local/AGENTS.md";
pub const BRIDGE_BEGIN: &str = "<!-- aethyme-local:begin -->";
pub const BRIDGE_END: &str = "<!-- aethyme-local:end -->";

pub const BRIDGE_BLOCK: &str = r#"<!-- aethyme-local:begin -->
## Optional local Aethyme activation

Before repository work, check whether `.aethyme/local/enabled` exists. If it
does, read `.aethyme/local/AGENTS.md` and follow that complete policy as
mandatory. If it does not exist, continue normally: do not run Aethyme, probe
for its binary, install it, or mention its absence.
<!-- aethyme-local:end -->"#;

const EXCLUDE_BEGIN: &str = "# aethyme-local:begin";
const EXCLUDE_END: &str = "# aethyme-local:end";
const EXCLUDE_BLOCK: &str = "# aethyme-local:begin\n\
.aethyme/\n\
.codex/skills/aethyme/\n\
.codex/skills/repo-onboarding/\n\
.codex/skills/repo-act/\n\
.claude/skills/aethyme/\n\
.claude/skills/repo-onboarding/\n\
.claude/skills/repo-act/\n\
.claude/hooks/aethyme-load-context.sh\n\
# aethyme-local:end\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAction {
    pub path: String,
    pub action: &'static str,
}

pub fn install_bridge(repo: &Path) -> Result<Vec<LocalAction>, String> {
    ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .map(|relative| {
            let path = repo.join(relative);
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let updated = upsert_block(&existing, BRIDGE_BEGIN, BRIDGE_END, BRIDGE_BLOCK)?;
            let action = if existing == updated {
                "unchanged"
            } else if path.exists() {
                "updated"
            } else {
                "created"
            };
            if existing != updated {
                write_file(&path, &updated)?;
            }
            Ok(LocalAction {
                path: relative.to_string(),
                action,
            })
        })
        .collect()
}

pub fn bridge_installed(repo: &Path) -> bool {
    ["AGENTS.md", "CLAUDE.md"].iter().all(|relative| {
        std::fs::read_to_string(repo.join(relative))
            .map(|text| text.contains(BRIDGE_BLOCK))
            .unwrap_or(false)
    })
}

pub fn prepare(repo: &Path) -> Result<(), String> {
    if !bridge_installed(repo) {
        return Err(
            "the inert AGENTS.md/CLAUDE.md bridge is not installed; run `aethyme deploy bridge --repo .`, review it, and commit it before local activation"
                .into(),
        );
    }
    refuse_tracked_local_targets(repo)?;
    ensure_local_excludes(repo)
}

pub fn deploy(repo: &Path, force: bool) -> Result<Vec<LocalAction>, String> {
    prepare(repo)?;
    let update_owned = force || repo.join(LOCAL_MARKER_PATH).is_file();

    let mut actions = Vec::new();
    for (relative, content) in expected_onboarding_files(repo)? {
        actions.push(write_local_target(
            repo,
            &relative,
            &content,
            update_owned,
            false,
        )?);
    }
    for (relative, content) in TARGETS {
        if *relative == "CLAUDE.md" {
            continue;
        }
        actions.push(write_local_target(
            repo,
            relative,
            content,
            update_owned,
            is_executable(relative),
        )?);
    }

    let policy = render_agents_document(Some(repo))?;
    actions.push(write_local_target(
        repo,
        LOCAL_POLICY_PATH,
        &policy,
        update_owned,
        false,
    )?);
    let marker = format!("schema = 1\nversion = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    actions.push(write_local_target(
        repo,
        LOCAL_MARKER_PATH,
        &marker,
        update_owned,
        false,
    )?);
    Ok(actions)
}

pub fn verify(repo: &Path) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    if !bridge_installed(repo) {
        problems.push("AGENTS.md and CLAUDE.md do not contain the canonical local bridge".into());
    }
    let marker = repo.join(LOCAL_MARKER_PATH);
    if !marker.is_file() {
        problems.push(format!("missing {LOCAL_MARKER_PATH}"));
    }
    let policy = repo.join(LOCAL_POLICY_PATH);
    match std::fs::read_to_string(&policy) {
        Ok(text) if text.contains("Broker Coordination") && !text.contains(PLACEHOLDER) => {}
        Ok(_) => problems.push(format!("{LOCAL_POLICY_PATH} is incomplete")),
        Err(_) => problems.push(format!("missing {LOCAL_POLICY_PATH}")),
    }

    for (relative, _) in expected_onboarding_files(repo)? {
        if !repo.join(&relative).is_file() {
            problems.push(format!("missing {relative}"));
        }
    }
    for (relative, _) in TARGETS {
        if *relative != "CLAUDE.md" && !repo.join(relative).is_file() {
            problems.push(format!("missing {relative}"));
        }
    }
    for relative in [
        LOCAL_MARKER_PATH,
        LOCAL_POLICY_PATH,
        ".codex/skills/aethyme/SKILL.md",
        ".claude/skills/aethyme/SKILL.md",
    ] {
        if repo.join(relative).exists() && !git_ignored(repo, relative)? {
            problems.push(format!("local artifact is not Git-ignored: {relative}"));
        }
    }
    Ok(problems)
}

fn refuse_tracked_local_targets(repo: &Path) -> Result<(), String> {
    let mut targets = expected_onboarding_files(repo)?
        .into_iter()
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    targets.extend(
        TARGETS
            .iter()
            .filter(|(path, _)| *path != "CLAUDE.md")
            .map(|(path, _)| (*path).to_string()),
    );
    targets.extend([LOCAL_MARKER_PATH.into(), LOCAL_POLICY_PATH.into()]);
    let tracked = targets
        .into_iter()
        .filter(|path| git_tracked(repo, path).unwrap_or(false))
        .collect::<Vec<_>>();
    if tracked.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "local-only deployment refuses to overwrite tracked policy: {}; use canonical `aethyme deploy` for this repository",
            tracked.join(", ")
        ))
    }
}

fn ensure_local_excludes(repo: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "info/exclude"])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("resolve Git exclude path: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let reported = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let path = if reported.is_absolute() {
        reported
    } else {
        repo.join(reported)
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert_block(
        &existing,
        EXCLUDE_BEGIN,
        EXCLUDE_END,
        EXCLUDE_BLOCK.trim_end(),
    )?;
    if existing != updated {
        write_file(&path, &updated)?;
    }
    Ok(())
}

fn upsert_block(existing: &str, begin: &str, end: &str, block: &str) -> Result<String, String> {
    match (existing.find(begin), existing.find(end)) {
        (Some(start), Some(end_start)) if end_start >= start => {
            let end_index = end_start + end.len();
            let mut updated = String::with_capacity(existing.len() + block.len());
            updated.push_str(&existing[..start]);
            updated.push_str(block);
            updated.push_str(&existing[end_index..]);
            Ok(normalize_trailing_newline(updated))
        }
        (None, None) => {
            let mut updated = existing.trim_end().to_string();
            if !updated.is_empty() {
                updated.push_str("\n\n");
            }
            updated.push_str(block);
            Ok(normalize_trailing_newline(updated))
        }
        _ => Err(format!(
            "malformed managed block: expected both {begin} and {end}"
        )),
    }
}

fn normalize_trailing_newline(mut text: String) -> String {
    while text.ends_with("\n\n") {
        text.pop();
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn write_local_target(
    repo: &Path,
    relative: &str,
    content: &str,
    force: bool,
    executable: bool,
) -> Result<LocalAction, String> {
    let path = repo.join(relative);
    let existing = std::fs::read_to_string(&path).ok();
    let action = if existing.as_deref() == Some(content) {
        "unchanged"
    } else if path.exists() {
        if !force {
            return Err(format!(
                "refusing to overwrite local artifact {relative}; rerun with --force"
            ));
        }
        write_file(&path, content)?;
        "updated"
    } else {
        write_file(&path, content)?;
        "created"
    };
    if executable {
        ensure_executable(&path)?;
    }
    Ok(LocalAction {
        path: relative.to_string(),
        action,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|error| format!("{}: {error}", path.display()))
}

fn is_executable(relative: &str) -> bool {
    relative.ends_with(".sh") || relative.ends_with("aethyme-explore")
}

fn ensure_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn git_tracked(repo: &Path, relative: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", relative])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("inspect tracked path {relative}: {error}"))?;
    Ok(output.status.success())
}

fn git_ignored(repo: &Path, relative: &str) -> Result<bool, String> {
    let status = Command::new("git")
        .args(["check-ignore", "--quiet", "--", relative])
        .current_dir(repo)
        .status()
        .map_err(|error| format!("inspect ignored path {relative}: {error}"))?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_is_inert_when_marker_is_absent() {
        assert!(BRIDGE_BLOCK.contains("If it does not exist, continue normally"));
        assert!(BRIDGE_BLOCK.contains("do not run Aethyme"));
        assert!(!BRIDGE_BLOCK.contains("command -v"));
    }

    #[test]
    fn managed_blocks_preserve_surrounding_content() {
        let existing =
            "Before\n\n<!-- aethyme-local:begin -->\nold\n<!-- aethyme-local:end -->\n\nAfter\n";
        let updated = upsert_block(existing, BRIDGE_BEGIN, BRIDGE_END, BRIDGE_BLOCK).unwrap();
        assert!(updated.starts_with("Before\n\n"));
        assert!(updated.ends_with("\n\nAfter\n"));
        assert!(updated.contains(BRIDGE_BLOCK));
    }
}
