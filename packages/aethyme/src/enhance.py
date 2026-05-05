"""Deploy and verify Aethyme discoverability files in a target repository.

A repository is "Aethyme-enhanced" when an agent landing in its working
directory can find Aethyme without out-of-band context. We deploy four files,
all derived from `packages/aethyme/skills/aethyme/{AGENTS.md,SKILL.md}`:

    AGENTS.md                              # cross-product convention
    CLAUDE.md                              # Claude Code project instructions
    .claude/skills/aethyme/SKILL.md        # Claude Skills detailed runbook
    .codex/skills/aethyme/SKILL.md         # Codex skills detailed runbook

The split is intentional. AGENTS.md and CLAUDE.md (root-level) carry the
"this repo has Aethyme — invoke it like X" announcement. SKILL.md (under the
two product-specific skill directories) carries the full intent catalog,
output schema, and trust-policy semantics.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

PLACEHOLDER = "{{AETHYME_ROOT}}"

# packages/aethyme/, the canonical AETHYME_ROOT during install.
PACKAGE_ROOT = Path(__file__).resolve().parent.parent

_TEMPLATE_DIR = PACKAGE_ROOT / "skills" / "aethyme"


@dataclass(frozen=True)
class EnhancementTarget:
    """One file that gets dropped into a target repository."""

    relative_path: str  # path relative to the target repo root
    source: Path        # source template file
    description: str


TARGETS: tuple[EnhancementTarget, ...] = (
    EnhancementTarget(
        "AGENTS.md",
        _TEMPLATE_DIR / "AGENTS.md",
        "Cross-product agent instructions (industry convention)",
    ),
    EnhancementTarget(
        "CLAUDE.md",
        _TEMPLATE_DIR / "AGENTS.md",
        "Claude Code project instructions (alias of AGENTS.md)",
    ),
    EnhancementTarget(
        ".claude/skills/aethyme/SKILL.md",
        _TEMPLATE_DIR / "SKILL.md",
        "Claude Skills detailed runbook",
    ),
    EnhancementTarget(
        ".codex/skills/aethyme/SKILL.md",
        _TEMPLATE_DIR / "SKILL.md",
        "Codex skills detailed runbook",
    ),
)


def aethyme_root() -> str:
    """The Aethyme tooling package path used for {{AETHYME_ROOT}} substitution."""
    return str(PACKAGE_ROOT)


def _render(target: EnhancementTarget, root: str) -> str:
    return target.source.read_text(encoding="utf-8").replace(PLACEHOLDER, root)


@dataclass(frozen=True)
class DeployAction:
    target: EnhancementTarget
    action: str  # "created", "updated", "unchanged"


def deploy(repo: Path, *, force: bool = False) -> list[DeployAction]:
    """Deploy all enhancement files into `repo`.

    Idempotent. Files whose contents already match the canonical render are
    left untouched (reported as "unchanged"). Pass `force=True` to rewrite
    even unchanged files (useful for restoring permissions or mtime).
    """

    root = aethyme_root()
    actions: list[DeployAction] = []
    for t in TARGETS:
        dest = repo / t.relative_path
        content = _render(t, root)
        if dest.exists() and not force and dest.read_text(encoding="utf-8") == content:
            actions.append(DeployAction(t, "unchanged"))
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        existed = dest.exists()
        dest.write_text(content, encoding="utf-8")
        actions.append(DeployAction(t, "updated" if existed else "created"))
    return actions


@dataclass(frozen=True)
class VerifyResult:
    target: EnhancementTarget
    exists: bool
    placeholder_present: bool   # true means substitution wasn't applied
    matches_canonical: bool     # true means content matches current source


def verify(repo: Path) -> list[VerifyResult]:
    """Check each target. Returns one VerifyResult per target."""

    root = aethyme_root()
    out: list[VerifyResult] = []
    for t in TARGETS:
        dest = repo / t.relative_path
        if not dest.exists():
            out.append(VerifyResult(t, False, False, False))
            continue
        actual = dest.read_text(encoding="utf-8")
        canonical = _render(t, root)
        out.append(
            VerifyResult(
                target=t,
                exists=True,
                placeholder_present=PLACEHOLDER in actual,
                matches_canonical=(actual == canonical),
            )
        )
    return out


def is_ok(results: Iterable[VerifyResult]) -> bool:
    """Return True iff every target exists and has its placeholder substituted.

    Content drift (matches_canonical=False) is allowed — a downstream consumer
    may have legitimately customised one of the files. Missing files and
    leftover {{AETHYME_ROOT}} placeholders are real failures.
    """
    return all(r.exists and not r.placeholder_present for r in results)
