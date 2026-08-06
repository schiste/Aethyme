//! Aethyme skill references must teach the progressive-disclosure ladder.
//!
//! Ported from `tests/local/test_skill_progressive_disclosure.py`
//! (python-retirement Phase 7).
//!
//! The auto-loaded SKILL.md should stay compact. The detailed
//! `## Progressive Disclosure: --depth` pedagogy lives in
//! `references/explore.md`, which agents load only when they need depth
//! selection or retry rules.
//!
//! These tests pin:
//!
//! 1. SKILL.md exists, is concise, and links the references.
//! 2. All four depth values are documented.
//! 3. Both escalation heuristics ("start at 0 unless you know the
//!    symbol" and "escalate one rung at a time") are present.
//! 4. The "when NOT to escalate" guard is present (otherwise agents
//!    default to escalating, defeating the purpose of the ladder).
//!
//! A future contributor adjusting the section will trip these tests if
//! the pedagogy regresses to inventory-only.

use std::path::PathBuf;

use aethyme_testkit::repos::{build_pnpm_demo_repo, read, write};
use aethyme_testkit::{invoke_aethyme, package_root, tmp_dir};

fn skill_path() -> PathBuf {
    package_root().join("skills/aethyme/SKILL.md")
}

fn agents_path() -> PathBuf {
    package_root().join("skills/aethyme/AGENTS.md")
}

fn explore_ref_path() -> PathBuf {
    package_root().join("skills/aethyme/references/explore.md")
}

fn graph_ref_path() -> PathBuf {
    package_root().join("skills/aethyme/references/graph-task.md")
}

fn dead_code_ref_path() -> PathBuf {
    package_root().join("skills/aethyme/references/dead-code.md")
}

fn hook_path() -> PathBuf {
    package_root().join("skills/aethyme/aethyme-load-context.sh")
}

#[test]
fn skill_md_exists() {
    assert!(
        skill_path().exists(),
        "{} must exist — deployed by `aethyme enhance deploy` to \
         .codex/skills/aethyme/SKILL.md in target repos",
        skill_path().display()
    );
}

#[test]
fn skill_md_is_concise_auto_load_card() {
    let text = read(skill_path());
    assert!(
        text.lines().count() <= 90,
        "SKILL.md is auto-loaded and should stay short; move detailed \
         workflows into references/*.md"
    );
    for needle in [
        "one bounded Explore call",
        "mktemp -t aethyme-explore",
        "safe_to_use_as_answer",
        "top_verification_targets",
        "observability.readiness",
        "verify-targets",
        // Phase 5.5 (2026-08-01): the compact projection is a native
        // reader command, not a `.venv/bin/python` heredoc — SKILL.md
        // deploys into user repos, so the product path must not require
        // Python.
        "explore-summary --from",
        "120 output lines / 20k chars",
        "multi-file `sed`",
        "`rg -C` context dumps",
        "broad `rg`",
        "references/explore.md",
        "references/graph-task.md",
        "references/dead-code.md",
    ] {
        assert!(text.contains(needle), "SKILL.md missing {needle:?}");
    }
    assert!(!text.contains(".venv/bin/python"));
    assert!(!text.contains("navigation_hints[]"));
}

#[test]
fn generated_agents_template_uses_projection_contract() {
    let text = read(agents_path());
    for needle in [
        "mktemp -t aethyme-explore",
        "top_verification_targets",
        "observability.readiness",
        "verify-targets",
        "explore-summary --from",
        "120 output lines / 20k chars",
        "multi-file `sed`",
    ] {
        assert!(text.contains(needle), "AGENTS.md template missing {needle:?}");
    }
    assert!(!text.contains(".venv/bin/python"));
    assert!(!text.contains("navigation_hints[]"));
}

#[test]
fn reference_files_exist() {
    for path in [explore_ref_path(), graph_ref_path(), dead_code_ref_path()] {
        assert!(path.exists(), "missing Aethyme skill reference: {}", path.display());
    }
}

#[test]
fn references_use_native_projection_and_no_venv_python() {
    assert!(read(explore_ref_path()).contains("explore-summary --from"));
    for path in [explore_ref_path(), graph_ref_path(), dead_code_ref_path()] {
        assert!(
            !read(&path).contains(".venv/bin/python"),
            "{} still reaches for the Aethyme venv interpreter",
            path.display()
        );
    }
}

/// Phase 6 (2026-08-01): the SessionStart hook template carried the last
/// Python invocation on the product path — a bare `python3` heredoc that
/// JSON-escaped the envelope. It is now `aethyme repo hook-envelope`, so
/// the deployed hook works on a machine with no Python at all.
///
/// Mirrors `verify-playground.sh:check_no_venv_python`, which enforces
/// the same rule against *deployed* copies.
#[test]
fn session_hook_template_emits_envelope_natively() {
    let text = read(hook_path());
    assert!(text.contains("repo hook-envelope"));
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue; // provenance comments may name the retired spelling
        }
        assert!(!line.contains("python3"), "{line}");
        assert!(!line.contains(".venv/bin/python"), "{line}");
    }
}

#[test]
fn enhance_deploys_aethyme_skill_references() {
    let tmp = tmp_dir();
    let repo = tmp.path().join("demo-repo");
    build_pnpm_demo_repo(&repo);
    // The Python fixture omitted the lockfile and the build script here.
    write(
        repo.join("package.json"),
        "{\"name\":\"demo\",\"scripts\":{\"test\":\"vitest\",\"dev\":\"vite\"}}\n",
    );
    std::fs::remove_file(repo.join("pnpm-lock.yaml")).unwrap();

    invoke_aethyme(["enhance", "deploy", "--repo", &repo.display().to_string()]).ok();

    for product in [".codex", ".claude"] {
        for name in ["explore.md", "graph-task.md", "dead-code.md"] {
            let path = repo.join(product).join("skills/aethyme/references").join(name);
            assert!(path.exists(), "missing deployed reference: {}", path.display());
            assert!(!read(&path).contains("{{AETHYME_ROOT}}"));
        }
    }
}

#[test]
fn progressive_disclosure_section_exists() {
    let text = read(explore_ref_path());
    assert!(
        text.contains("## Progressive Disclosure: `--depth`")
            || text.contains("### Progressive Disclosure: `--depth`"),
        "references/explore.md must include a Progressive Disclosure \
         section teaching the --depth ladder."
    );
}

#[test]
fn all_four_depth_values_documented() {
    let text = read(explore_ref_path());
    for depth in ["--depth 0", "--depth 1", "--depth 2", "--depth 3"] {
        assert!(
            text.contains(depth),
            "references/explore.md must document {depth:?}. The cargo-side \
             cap table has 4 rungs; the skill must teach all 4."
        );
    }
}

/// The section's job isn't enumeration; it's teaching when to escalate.
/// Both rules from the design must appear.
#[test]
fn escalation_heuristics_present() {
    let text = read(explore_ref_path()).to_lowercase();
    // Heuristic 1: "Start at 0 unless you already know the symbol."
    assert!(
        text.contains("start at 0") || text.contains("start at `--depth 0`"),
        "references/explore.md missing the 'start at 0 unless you know the \
         symbol' heuristic — without it, agents default to a single rung."
    );
    // Heuristic 2: "Escalate one rung at a time."
    assert!(
        text.contains("escalate") && text.contains("one rung"),
        "references/explore.md missing the 'escalate one rung at a time' heuristic"
    );
}

/// Without an explicit stop rule, agents will escalate by default —
/// defeating the budget purpose.
#[test]
fn when_not_to_escalate_guard_present() {
    let text = read(explore_ref_path());
    assert!(
        text.contains("When NOT to escalate") || text.to_lowercase().contains("when not to escalate"),
        "references/explore.md must include a 'when NOT to escalate' guard. \
         Without it, the ladder degrades into 'always go deeper'."
    );
}

/// We don't break existing callers — the flag coexists with `--detail`.
/// The skill should mention this so a contributor reading it doesn't
/// think the old flag is gone.
#[test]
fn legacy_detail_flag_compatibility_noted() {
    assert!(
        read(explore_ref_path()).contains("--detail"),
        "references/explore.md should still reference --detail for callers \
         using the legacy budget vocabulary."
    );
}

/// The pedagogy must convey the cost gradient — depth=3 isn't just
/// 'more', it's the most expensive rung. Without that signal, agents may
/// default to depth=3 thinking 'more is better'.
#[test]
fn depth_3_documented_as_expensive() {
    let text = read(explore_ref_path()).to_lowercase();
    assert!(
        ["expensive", "commit", "deepest", "deep dive"]
            .iter()
            .any(|word| text.contains(word)),
        "references/explore.md should signal that depth=3 is expensive / \
         commit-level, not just 'more detailed'. Otherwise agents may \
         default to it."
    );
}
