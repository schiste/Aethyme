//! Static runtime-skill deployment — port of `src/indexing/skills.py`.
//!
//! The Python original copied `skills/aethyme/` from the source checkout;
//! the port writes the same tree from the embedded templates (the Phase 2
//! packaging change). Runtime commands resolve `aethyme` through PATH, so
//! deployment never embeds a source-checkout path. Executable bits are set explicitly for
//! the shell wrappers (the Python `copytree` inherited them from the
//! checkout). `repo deploy-skills` / `compile-skills` dispatch here via
//! `repo_cli` since the Phase 3 flip.

use std::path::Path;

use crate::templates::AETHYME_SKILL_FILES;

const DEFAULT_RUNTIME_SKILLS: &[&str] = &["aethyme"];

/// Skill names `remove_skills` sweeps. The Python original removed every
/// target directory matching a `skills/` source-template name — at
/// deletion time that set was `{aethyme, eval}` (the internal eval skill
/// is never deployed by current tooling but may exist in repos enhanced
/// by older versions). Sorted order preserved.
const REMOVABLE_SKILLS: &[&str] = &["aethyme", "eval"];

/// Copy target-safe skill templates into `<target_repo>/.codex/skills/`.
/// Returns the deployed skill names.
pub fn deploy_skills(target_repo: &Path, force: bool) -> Result<Vec<String>, String> {
    let dest_base = crate::util::resolve_path(target_repo)
        .join(".codex")
        .join("skills");
    std::fs::create_dir_all(&dest_base).map_err(|e| format!("{}: {e}", dest_base.display()))?;

    let mut deployed = Vec::new();
    for skill_name in DEFAULT_RUNTIME_SKILLS {
        let dest_dir = dest_base.join(skill_name);
        if dest_dir.exists() {
            if force {
                std::fs::remove_dir_all(&dest_dir)
                    .map_err(|e| format!("{}: {e}", dest_dir.display()))?;
            } else {
                continue;
            }
        }
        for (relative_path, content, executable) in AETHYME_SKILL_FILES {
            let dest = dest_dir.join(relative_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&dest, content).map_err(|e| format!("{}: {e}", dest.display()))?;
            if *executable {
                use std::os::unix::fs::PermissionsExt;
                let metadata =
                    std::fs::metadata(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
                let mut permissions = metadata.permissions();
                permissions.set_mode(permissions.mode() | 0o111);
                std::fs::set_permissions(&dest, permissions)
                    .map_err(|e| format!("{}: {e}", dest.display()))?;
            }
        }
        let broker_reference = dest_dir.join("references/broker.md");
        std::fs::write(&broker_reference, crate::agents::render_broker_reference())
            .map_err(|e| format!("{}: {e}", broker_reference.display()))?;
        deployed.push(skill_name.to_string());
    }
    Ok(deployed)
}

/// Remove Aethyme-deployed skills from `target_repo`. Removes skill
/// directories whose names match source templates.
pub fn remove_skills(target_repo: &Path) -> Result<Vec<String>, String> {
    let mut removed = Vec::new();
    let dest_base = crate::util::resolve_path(target_repo)
        .join(".codex")
        .join("skills");
    if !dest_base.is_dir() {
        return Ok(removed);
    }
    for skill_name in REMOVABLE_SKILLS {
        let dest_dir = dest_base.join(skill_name);
        if dest_dir.exists() {
            std::fs::remove_dir_all(&dest_dir)
                .map_err(|e| format!("{}: {e}", dest_dir.display()))?;
            removed.push(skill_name.to_string());
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aethyme-enhance-skills-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn deploys_full_skill_tree_with_substitution() {
        let repo = fixture_repo("deploy");
        let deployed = deploy_skills(&repo, false).unwrap();
        assert_eq!(deployed, vec!["aethyme".to_string()]);
        let skill_md =
            std::fs::read_to_string(repo.join(".codex/skills/aethyme/SKILL.md")).unwrap();
        assert!(skill_md.contains("AETHYME_BIN=\"${AETHYME_BIN:-aethyme}\""));
        assert!(!skill_md.contains("{{AETHYME_ROOT}}"));
        assert!(repo
            .join(".codex/skills/aethyme/references/explore.md")
            .exists());
        assert!(repo
            .join(".codex/skills/aethyme/references/broker.md")
            .exists());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(repo.join(".codex/skills/aethyme/aethyme-explore"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);

        // Existing skill dir is skipped without force.
        assert!(deploy_skills(&repo, false).unwrap().is_empty());
        assert_eq!(
            deploy_skills(&repo, true).unwrap(),
            vec!["aethyme".to_string()]
        );

        assert_eq!(remove_skills(&repo).unwrap(), vec!["aethyme".to_string()]);
        assert!(!repo.join(".codex/skills/aethyme").exists());
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn remove_sweeps_legacy_eval_skill_too() {
        let repo = fixture_repo("remove-eval");
        deploy_skills(&repo, false).unwrap();
        std::fs::create_dir_all(repo.join(".codex/skills/eval")).unwrap();
        std::fs::write(repo.join(".codex/skills/eval/SKILL.md"), "old\n").unwrap();
        assert_eq!(
            remove_skills(&repo).unwrap(),
            vec!["aethyme".to_string(), "eval".to_string()]
        );
        assert!(!repo.join(".codex/skills/eval").exists());
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
