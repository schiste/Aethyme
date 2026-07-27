"""Aethyme skill references must teach the progressive-disclosure ladder.

The auto-loaded SKILL.md should stay compact. The detailed
`## Progressive Disclosure: --depth` pedagogy lives in
`references/explore.md`, which agents load only when they need depth
selection or retry rules.

These tests pin:

1. SKILL.md exists, is concise, and links the references.
2. All four depth values are documented.
3. Both escalation heuristics ("start at 0 unless you know the
   symbol" and "escalate one rung at a time") are present.
4. The "when NOT to escalate" guard is present (otherwise agents
   default to escalating, defeating the purpose of the ladder).

A future contributor adjusting the section will trip these tests
if the pedagogy regresses to inventory-only.
"""

from __future__ import annotations

from pathlib import Path

from src.enhance import deploy

REPO_ROOT = Path(__file__).resolve().parents[2]
SKILL_PATH = REPO_ROOT / "skills" / "aethyme" / "SKILL.md"
EXPLORE_REF_PATH = REPO_ROOT / "skills" / "aethyme" / "references" / "explore.md"
GRAPH_REF_PATH = REPO_ROOT / "skills" / "aethyme" / "references" / "graph-task.md"
DEAD_CODE_REF_PATH = REPO_ROOT / "skills" / "aethyme" / "references" / "dead-code.md"


def test_skill_md_exists():
    assert SKILL_PATH.exists(), (
        f"{SKILL_PATH} must exist — deployed by `enhance.py` to "
        ".codex/skills/aethyme/SKILL.md in target repos"
    )


def test_skill_md_is_concise_auto_load_card():
    text = SKILL_PATH.read_text()
    lines = text.splitlines()
    assert len(lines) <= 90, (
        "SKILL.md is auto-loaded and should stay short; move detailed "
        "workflows into references/*.md"
    )
    assert "one bounded Explore call" in text
    assert "safe_to_use_as_answer" in text
    assert "observability" in text
    assert "references/explore.md" in text
    assert "references/graph-task.md" in text
    assert "references/dead-code.md" in text


def test_reference_files_exist():
    for path in (EXPLORE_REF_PATH, GRAPH_REF_PATH, DEAD_CODE_REF_PATH):
        assert path.exists(), f"missing Aethyme skill reference: {path}"


def test_enhance_deploys_aethyme_skill_references(tmp_path: Path) -> None:
    repo = tmp_path / "demo-repo"
    repo.mkdir()
    (repo / "package.json").write_text(
        '{"name":"demo","scripts":{"test":"vitest","dev":"vite"}}\n',
        encoding="utf-8",
    )
    (repo / "src").mkdir()
    (repo / "src" / "main.ts").write_text("console.log('demo')\n", encoding="utf-8")

    deploy(repo)

    for product in (".codex", ".claude"):
        for name in ("explore.md", "graph-task.md", "dead-code.md"):
            path = repo / product / "skills" / "aethyme" / "references" / name
            assert path.exists(), f"missing deployed reference: {path}"
            assert "{{AETHYME_ROOT}}" not in path.read_text(encoding="utf-8")


def test_progressive_disclosure_section_exists():
    text = EXPLORE_REF_PATH.read_text()
    assert "## Progressive Disclosure: `--depth`" in text or \
           "### Progressive Disclosure: `--depth`" in text, (
        "references/explore.md must include a Progressive Disclosure "
        "section teaching the --depth ladder."
    )


def test_all_four_depth_values_documented():
    text = EXPLORE_REF_PATH.read_text()
    for depth in ("--depth 0", "--depth 1", "--depth 2", "--depth 3"):
        assert depth in text, (
            f"SKILL.md must document {depth!r}. The cargo-side cap "
            "table has 4 rungs; the skill must teach all 4."
        )


def test_escalation_heuristics_present():
    """The section's job isn't enumeration; it's teaching when to
    escalate. Both rules from the design must appear."""
    text = EXPLORE_REF_PATH.read_text().lower()
    # Heuristic 1: "Start at 0 unless you already know the symbol."
    assert "start at 0" in text or "start at `--depth 0`" in text, (
        "SKILL.md missing the 'start at 0 unless you know the symbol' "
        "heuristic — without it, agents default to a single rung."
    )
    # Heuristic 2: "Escalate one rung at a time."
    assert "escalate" in text and "one rung" in text, (
        "SKILL.md missing the 'escalate one rung at a time' heuristic"
    )


def test_when_not_to_escalate_guard_present():
    """Without an explicit stop rule, agents will escalate by default —
    defeating the budget purpose."""
    text = EXPLORE_REF_PATH.read_text()
    assert "When NOT to escalate" in text or \
           "when not to escalate" in text.lower(), (
        "SKILL.md must include a 'when NOT to escalate' guard. "
        "Without it, the ladder degrades into 'always go deeper'."
    )


def test_legacy_detail_flag_compatibility_noted():
    """We don't break existing callers — the flag coexists with
    --detail. The skill should mention this so a contributor reading
    the skill doesn't think the old flag is gone."""
    text = EXPLORE_REF_PATH.read_text()
    assert "--detail" in text, (
        "SKILL.md should still reference --detail for callers using "
        "the legacy budget vocabulary."
    )


def test_depth_3_documented_as_expensive():
    """The pedagogy must convey the cost gradient — depth=3 isn't
    just 'more', it's the most expensive rung. Without that signal,
    agents may default to depth=3 thinking 'more is better'."""
    text = EXPLORE_REF_PATH.read_text().lower()
    # Look for cost-signaling words near depth=3 mentions.
    assert any(
        word in text
        for word in ("expensive", "commit", "deepest", "deep dive")
    ), (
        "SKILL.md should signal that depth=3 is expensive / commit-"
        "level, not just 'more detailed'. Otherwise agents may "
        "default to it."
    )
