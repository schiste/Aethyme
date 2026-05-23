"""Eval target registry — playground repositories for benchmarking.

Each target is a pair of repos: a vanilla *control* (no Aethyme tooling)
and an *aethyme* repo with ``.codex/skills/`` deployed.  The registry is
the canonical source of truth for where playground repos live.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ._self import self_tool_name

_PLAYGROUND_ROOT = Path.home() / "Downloads" / "Repositories" / "Playground"


@dataclass(frozen=True)
class EvalTarget:
    """A playground repository pair for evaluation."""

    name: str
    display_name: str
    control_path: Path
    aethyme_path: Path
    description: str = ""
    setup_source: str | None = None
    setup_commit: str | None = None
    setup_control_dir_name: str | None = None
    setup_aethyme_dir_name: str | None = None
    # Default tool for this target's "tool-using" conditions. Override at
    # the CLI with --tool to swap in a competitor (e.g. graphify). The
    # tool name must match a manifest under evals/tools/<name>.toml.
    #
    # Factory (not literal default) so each EvalTarget construction re-reads
    # ``AETHYMEBENCH_SELF_TOOL`` — a test that monkeypatches the env var
    # before constructing a target gets the patched value. A literal
    # ``= self_tool_name()`` default would freeze the env at import time.
    default_tool: str = field(default_factory=self_tool_name)

    def validate(self) -> list[str]:
        """Return validation error strings.  Empty list means valid."""
        errors: list[str] = []

        # Control repo
        if not self.control_path.exists():
            errors.append(f"control repo missing: {self.control_path}")
        elif not (self.control_path / ".git").exists():
            errors.append(f"control repo is not a git repo: {self.control_path}")

        # Control must NOT have Aethyme skill (contamination check)
        skill_dirs = (
            self.control_path / ".codex" / "skills",
            self.control_path / ".codex" / "skills" / "aethyme",
            self.control_path / ".codex" / "skills" / "aethyme-navigation",
        )
        for sd in skill_dirs:
            if sd.exists():
                errors.append(f"control repo has skill contamination: {sd}")

        # Aethyme repo
        if not self.aethyme_path.exists():
            errors.append(f"aethyme repo missing: {self.aethyme_path}")
        elif not (self.aethyme_path / ".git").exists():
            errors.append(f"aethyme repo is not a git repo: {self.aethyme_path}")
        else:
            skills_root = self.aethyme_path / ".codex" / "skills"
            if not skills_root.exists():
                errors.append(f"aethyme repo has no .codex/skills/: {self.aethyme_path}")

        return errors

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "display_name": self.display_name,
            "control_path": str(self.control_path),
            "aethyme_path": str(self.aethyme_path),
            "description": self.description,
            "setup_source": self.setup_source,
            "setup_commit": self.setup_commit,
            "setup_control_dir_name": self.setup_control_dir_name,
            "setup_aethyme_dir_name": self.setup_aethyme_dir_name,
        }


TARGETS: dict[str, EvalTarget] = {
    "grc": EvalTarget(
        name="grc",
        display_name="GRC",
        # The TypeScript monorepo previously lived at GRC/Playground{Control,Aethyme}.
        # It was reorganized under Playground/Mockup/Mockup\ -\ {Control,Aethyme}.
        # Slug stays "grc" for tooling stability; paths sync to current filesystem.
        control_path=_PLAYGROUND_ROOT / "Mockup" / "Mockup - Control",
        aethyme_path=_PLAYGROUND_ROOT / "Mockup" / "Mockup - Aethyme",
        description="TypeScript monorepo (enterprise GRC platform)",
        setup_control_dir_name="Mockup - Control",
        setup_aethyme_dir_name="Mockup - Aethyme",
    ),
    "mediawiki": EvalTarget(
        name="mediawiki",
        display_name="MediaWiki",
        control_path=_PLAYGROUND_ROOT / "Mediawiki" / "Mediawiki - Control",
        aethyme_path=_PLAYGROUND_ROOT / "Mediawiki" / "Mediawiki - Aethyme",
        description="PHP monolith (~12.5K files)",
        setup_source="https://github.com/wikimedia/mediawiki.git",
        setup_commit="8b6613f3996",
        setup_control_dir_name="Mediawiki - Control",
        setup_aethyme_dir_name="Mediawiki - Aethyme",
    ),
}


def get_target(name: str) -> EvalTarget:
    """Look up a target by name (case-insensitive).  Raises KeyError if unknown."""
    key = name.lower().strip()
    if key not in TARGETS:
        available = ", ".join(sorted(TARGETS))
        raise KeyError(f"Unknown target {name!r}. Available: {available}")
    return TARGETS[key]


def list_targets() -> list[EvalTarget]:
    """Return all registered targets."""
    return list(TARGETS.values())
