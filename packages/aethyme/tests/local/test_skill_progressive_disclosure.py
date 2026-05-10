"""SKILL.md must teach the progressive-disclosure ladder.

The skill's `## Progressive Disclosure: --depth` section is the
agent-facing pedagogy for the depth ladder added in 0d5be82. The
section's job is *escalation pedagogy* — when to call which depth
— not just enumerating what each depth returns.

These tests pin:

1. The section exists.
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

REPO_ROOT = Path(__file__).resolve().parents[2]
SKILL_PATH = REPO_ROOT / "skills" / "aethyme" / "SKILL.md"


def test_skill_md_exists():
    assert SKILL_PATH.exists(), (
        f"{SKILL_PATH} must exist — deployed by `enhance.py` to "
        ".codex/skills/aethyme/SKILL.md in target repos"
    )


def test_progressive_disclosure_section_exists():
    text = SKILL_PATH.read_text()
    assert "## Progressive Disclosure: `--depth`" in text or \
           "### Progressive Disclosure: `--depth`" in text, (
        "SKILL.md must include a Progressive Disclosure section "
        "teaching the --depth ladder. Without it, agents won't "
        "discover the budget knob and will default to bulk-loading."
    )


def test_all_four_depth_values_documented():
    text = SKILL_PATH.read_text()
    for depth in ("--depth 0", "--depth 1", "--depth 2", "--depth 3"):
        assert depth in text, (
            f"SKILL.md must document {depth!r}. The cargo-side cap "
            "table has 4 rungs; the skill must teach all 4."
        )


def test_escalation_heuristics_present():
    """The section's job isn't enumeration; it's teaching when to
    escalate. Both rules from the design must appear."""
    text = SKILL_PATH.read_text().lower()
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
    text = SKILL_PATH.read_text()
    assert "When NOT to escalate" in text or \
           "when not to escalate" in text.lower(), (
        "SKILL.md must include a 'when NOT to escalate' guard. "
        "Without it, the ladder degrades into 'always go deeper'."
    )


def test_legacy_detail_flag_compatibility_noted():
    """We don't break existing callers — the flag coexists with
    --detail. The skill should mention this so a contributor reading
    the skill doesn't think the old flag is gone."""
    text = SKILL_PATH.read_text()
    assert "--detail" in text, (
        "SKILL.md should still reference --detail for callers using "
        "the legacy budget vocabulary."
    )


def test_depth_3_documented_as_expensive():
    """The pedagogy must convey the cost gradient — depth=3 isn't
    just 'more', it's the most expensive rung. Without that signal,
    agents may default to depth=3 thinking 'more is better'."""
    text = SKILL_PATH.read_text().lower()
    # Look for cost-signaling words near depth=3 mentions.
    assert any(
        word in text
        for word in ("expensive", "commit", "deepest", "deep dive")
    ), (
        "SKILL.md should signal that depth=3 is expensive / commit-"
        "level, not just 'more detailed'. Otherwise agents may "
        "default to it."
    )
