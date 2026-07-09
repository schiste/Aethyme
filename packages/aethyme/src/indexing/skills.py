"""Deploy static Aethyme skill templates to a target repository.

Runtime skills are generic Markdown templates stored at
``<aethyme-package>/skills/``. Deployment copies target-safe skills into
``<target-repo>/.codex/skills/`` with the ``{{AETHYME_ROOT}}`` placeholder
replaced by the actual Aethyme package path.

This module intentionally handles only the static runtime skill. Repo-specific
generated skills such as onboarding are emitted by the enhancement flow.
"""

from __future__ import annotations

import shutil
from pathlib import Path

# Host package root — two levels up from this file (src/indexing/skills.py).
# Historically named AETHYME_PACKAGE_ROOT; renamed 2026-05-19 to make the
# semantics tool-agnostic as part of the AethymeBench extraction (Stage A.1.2).
# The function parameter `aethyme_root` is intentionally NOT renamed — it points
# at the Aethyme package specifically (which owns the skill templates), not at
# the host package. The two concepts will diverge once skill ownership moves to
# the tool adapter in Stage B (plan item 2.4).
HOST_PACKAGE_ROOT = Path(__file__).resolve().parent.parent.parent
SKILLS_SOURCE_DIR = HOST_PACKAGE_ROOT / "skills"
DEFAULT_RUNTIME_SKILLS = ("aethyme",)

PLACEHOLDER = "{{AETHYME_ROOT}}"


def deploy_skills(
    target_repo: Path,
    *,
    aethyme_root: Path | None = None,
    force: bool = False,
) -> list[str]:
    """Copy target-safe skill templates into *target_repo*/.codex/skills/.

    Only runtime navigation skills are deployed by default. Internal skills such
    as the eval workflow must not be exposed to benchmark agent repositories
    because they can leak protocol and scoring context.

    Parameters
    ----------
    target_repo:
        Path to the repository that should receive skills.
    aethyme_root:
        Override for the Aethyme package root stamped into templates.
        Defaults to the package root detected from this file's location.
    force:
        If True, overwrite existing skill directories. Otherwise skip
        skills that already exist.

    Returns
    -------
    List of skill names that were deployed.
    """
    if aethyme_root is None:
        aethyme_root = HOST_PACKAGE_ROOT

    if not SKILLS_SOURCE_DIR.is_dir():
        return []

    dest_base = Path(target_repo).resolve() / ".codex" / "skills"
    dest_base.mkdir(parents=True, exist_ok=True)

    deployed: list[str] = []
    for skill_name in DEFAULT_RUNTIME_SKILLS:
        skill_dir = SKILLS_SOURCE_DIR / skill_name
        if not skill_dir.is_dir():
            continue

        dest_dir = dest_base / skill_name

        if dest_dir.exists():
            if force:
                shutil.rmtree(dest_dir)
            else:
                continue

        shutil.copytree(skill_dir, dest_dir)

        # Replace placeholders in markdown docs and shell wrappers.
        # The `.sh` extension is enforced for pure shells; the `aethyme-explore`
        # convenience wrapper has no extension by convention but is shell.
        for path in dest_dir.rglob("*"):
            if path.is_dir():
                continue
            if path.suffix in (".md", ".sh") or path.name == "aethyme-explore":
                text = path.read_text()
                if PLACEHOLDER in text:
                    path.write_text(text.replace(PLACEHOLDER, str(aethyme_root)))

        deployed.append(skill_name)

    return deployed


def remove_skills(target_repo: Path) -> list[str]:
    """Remove Aethyme-deployed skills from *target_repo*.

    Removes skill directories whose names match source templates, including
    internal skills that may have been deployed by older tooling.
    """
    removed: list[str] = []
    dest_base = Path(target_repo).resolve() / ".codex" / "skills"
    if not dest_base.is_dir() or not SKILLS_SOURCE_DIR.is_dir():
        return removed

    for skill_dir in sorted(SKILLS_SOURCE_DIR.iterdir()):
        if not skill_dir.is_dir():
            continue
        dest_dir = dest_base / skill_dir.name
        if dest_dir.exists():
            shutil.rmtree(dest_dir)
            removed.append(skill_dir.name)

    return removed
