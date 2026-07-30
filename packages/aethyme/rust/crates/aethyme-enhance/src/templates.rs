//! Canonical templates, embedded at build time from
//! `packages/aethyme/skills/aethyme/` (paths are relative to this source
//! file: crate `src/` → up four levels → the package root).
//!
//! Embedding is the sanctioned packaging change of the Phase 2 port: a
//! `cargo install`ed binary deploys without a source checkout, while the
//! rendered bytes stay identical to Python reading the same files
//! (verified by `scripts/migration/enhance-golden.sh`). Template CONTENT
//! flipped to `aethyme ...` spellings on 2026-07-30 (Class 3 change,
//! in lockstep with verify-playground.sh greps and the consumer
//! registry); the wrapper scripts' `record-wrapper-invocation` calls
//! flipped to the native `aethyme` router (guarded by `command -v
//! aethyme`) in the Phase 3 hook flip, same lockstep protocol.

pub const AGENTS_MD: &str = include_str!("../../../../skills/aethyme/AGENTS.md");
pub const SKILL_MD: &str = include_str!("../../../../skills/aethyme/SKILL.md");
pub const REF_EXPLORE_MD: &str = include_str!("../../../../skills/aethyme/references/explore.md");
pub const REF_GRAPH_TASK_MD: &str =
    include_str!("../../../../skills/aethyme/references/graph-task.md");
pub const REF_DEAD_CODE_MD: &str =
    include_str!("../../../../skills/aethyme/references/dead-code.md");
pub const LOAD_CONTEXT_SH: &str =
    include_str!("../../../../skills/aethyme/aethyme-load-context.sh");
pub const AETHYME_EXPLORE: &str = include_str!("../../../../skills/aethyme/aethyme-explore");

/// The full `skills/aethyme/` template directory as (relative path within
/// the skill dir, content, executable) — the source tree
/// `deploy_skills` copies. Order matches a sorted directory walk.
pub const AETHYME_SKILL_FILES: &[(&str, &str, bool)] = &[
    ("AGENTS.md", AGENTS_MD, false),
    ("SKILL.md", SKILL_MD, false),
    ("aethyme-explore", AETHYME_EXPLORE, true),
    ("aethyme-load-context.sh", LOAD_CONTEXT_SH, true),
    ("references/dead-code.md", REF_DEAD_CODE_MD, false),
    ("references/explore.md", REF_EXPLORE_MD, false),
    ("references/graph-task.md", REF_GRAPH_TASK_MD, false),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_templates_resolve_and_carry_placeholders() {
        assert!(AGENTS_MD.contains("{{AETHYME_ROOT}}"));
        assert!(SKILL_MD.starts_with("---"));
        assert!(LOAD_CONTEXT_SH.starts_with("#!"));
        assert!(!REF_EXPLORE_MD.is_empty());
        assert!(!REF_GRAPH_TASK_MD.is_empty());
        assert!(!REF_DEAD_CODE_MD.is_empty());
        assert!(!AETHYME_EXPLORE.is_empty());
    }
}
