"""Deploy and verify Aethyme discoverability files in a target repository.

A repository is "Aethyme-enhanced" when an agent landing in its working
directory can find Aethyme without out-of-band context. We deploy several
files, all derived from canonical templates under
`packages/aethyme/skills/aethyme/`:

    AGENTS.md                                  # cross-product convention
    CLAUDE.md                                  # Claude Code project instructions
    .claude/skills/aethyme/SKILL.md            # Claude Skills detailed runbook
    .codex/skills/aethyme/SKILL.md             # Codex skills detailed runbook
    .claude/hooks/aethyme-load-context.sh      # Claude Code SessionStart hook
    .claude/settings.local.json                # wires the hook (merge-aware)

The split between AGENTS.md/CLAUDE.md (root-level announcement) and
SKILL.md (per-product detailed runbook) is intentional: agents that
auto-load CLAUDE.md/AGENTS.md see the entry-point; agents that load
their product's skills directory see the full reference.

The SessionStart hook covers a real Claude Code limitation: when launched
in headless mode (`--dangerously-skip-permissions`, e.g. by an eval
harness), Claude Code does NOT auto-load CLAUDE.md from CWD. The hook
re-injects AGENTS.md/CLAUDE.md content as `additionalContext` so the
agent still sees the discoverability surface.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from src.indexing.experience_telemetry import (
    append_event,
    event_payload_from_generated_artifacts,
    summarize_events,
    write_status_artifacts,
)
from src.indexing.onboarding import (
    ACT_CLAUDE_PATH,
    ACT_CODEX_PATH,
    ACT_STARTER_JSON_PATH,
    ONBOARDING_CLAUDE_PATH,
    ONBOARDING_CODEX_PATH,
    ONBOARDING_JSON_PATH,
    expected_onboarding_files,
    override_freshness,
    recommendation_summary,
)

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
    EnhancementTarget(
        ".claude/hooks/aethyme-load-context.sh",
        _TEMPLATE_DIR / "aethyme-load-context.sh",
        "Claude Code SessionStart hook: re-inject CLAUDE.md/AGENTS.md content "
        "as additionalContext (works around headless-launch limitation)",
    ),
)

GENERATED_TARGET_DESCRIPTIONS: dict[str, str] = {
    ONBOARDING_JSON_PATH: "Deterministic repo-onboarding facts",
    ACT_STARTER_JSON_PATH: "Deterministic repo Act starter facts",
    ONBOARDING_CLAUDE_PATH: "Claude Skills repo-specific onboarding runbook",
    ONBOARDING_CODEX_PATH: "Codex skills repo-specific onboarding runbook",
    ACT_CLAUDE_PATH: "Claude Skills repo-specific Act starter runbook",
    ACT_CODEX_PATH: "Codex skills repo-specific Act starter runbook",
}

# Settings.local.json gets MERGED rather than written from a template, so
# user customisations survive re-deployment. The hook entry below is what
# we ensure is present in `hooks.SessionStart`.
SETTINGS_FILE = ".claude/settings.local.json"
SESSION_START_HOOK_ENTRY: dict[str, Any] = {
    "matcher": "",
    "hooks": [
        {
            "type": "command",
            "command": ".claude/hooks/aethyme-load-context.sh",
        }
    ],
}


def aethyme_root() -> str:
    """The Aethyme tooling package path used for {{AETHYME_ROOT}} substitution."""
    return str(PACKAGE_ROOT)


def _render(target: EnhancementTarget, root: str) -> str:
    return target.source.read_text(encoding="utf-8").replace(PLACEHOLDER, root)


@dataclass(frozen=True)
class DeployAction:
    target: EnhancementTarget
    action: str  # "created", "updated", "unchanged"


def _is_executable(path: Path) -> bool:
    return path.suffix == ".sh"


def deploy(repo: Path, *, force: bool = False) -> list[DeployAction]:
    """Deploy all enhancement files into `repo`.

    Idempotent. Files whose contents already match the canonical render are
    left untouched (reported as "unchanged"). Pass `force=True` to rewrite
    even unchanged files (useful for restoring permissions or mtime).

    Also merges `.claude/settings.local.json` to include the SessionStart
    hook entry. Existing hooks and other settings keys are preserved.
    """

    root = aethyme_root()
    actions: list[DeployAction] = []
    for t in TARGETS:
        dest = repo / t.relative_path
        content = _render(t, root)
        if dest.exists() and not force and dest.read_text(encoding="utf-8") == content:
            # Still ensure the executable bit is right on shell scripts.
            if _is_executable(dest):
                _ensure_executable(dest)
            actions.append(DeployAction(t, "unchanged"))
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        existed = dest.exists()
        dest.write_text(content, encoding="utf-8")
        if _is_executable(dest):
            _ensure_executable(dest)
        actions.append(DeployAction(t, "updated" if existed else "created"))

    for relative_path, content in expected_onboarding_files(repo).items():
        target = EnhancementTarget(
            relative_path,
            _TEMPLATE_DIR / "AGENTS.md",
            GENERATED_TARGET_DESCRIPTIONS.get(relative_path, "Generated onboarding artifact"),
        )
        dest = repo / relative_path
        if dest.exists() and not force and dest.read_text(encoding="utf-8") == content:
            actions.append(DeployAction(target, "unchanged"))
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        existed = dest.exists()
        dest.write_text(content, encoding="utf-8")
        actions.append(DeployAction(target, "updated" if existed else "created"))

    # Merge-aware settings.local.json deployment.
    settings_action = _ensure_settings_hook(repo)
    actions.append(settings_action)

    append_event(
        repo,
        "enhance.deploy",
        {
            "force": force,
            "actions": [
                {"path": action.target.relative_path, "action": action.action}
                for action in actions
            ],
            **event_payload_from_generated_artifacts(repo),
        },
    )

    return actions


def _ensure_executable(path: Path) -> None:
    import stat

    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _ensure_settings_hook(repo: Path) -> DeployAction:
    """Add the SessionStart hook to `.claude/settings.local.json`.

    Idempotent: if the entry is already present, returns "unchanged".
    Preserves any other keys / hooks the user has configured.
    """

    settings_path = repo / SETTINGS_FILE
    settings_path.parent.mkdir(parents=True, exist_ok=True)

    settings: dict[str, Any]
    existed = settings_path.exists()
    if existed:
        try:
            settings = json.loads(settings_path.read_text(encoding="utf-8"))
            if not isinstance(settings, dict):
                # File exists but isn't a JSON object — back up and start fresh.
                settings_path.rename(settings_path.with_suffix(".json.bak"))
                settings = {}
                existed = False
        except json.JSONDecodeError:
            settings_path.rename(settings_path.with_suffix(".json.bak"))
            settings = {}
            existed = False
    else:
        settings = {}

    hooks = settings.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        hooks = {}
        settings["hooks"] = hooks
    session_start = hooks.setdefault("SessionStart", [])
    if not isinstance(session_start, list):
        session_start = []
        hooks["SessionStart"] = session_start

    # Match by command path so we don't add duplicates on re-deploy.
    cmd = SESSION_START_HOOK_ENTRY["hooks"][0]["command"]
    has_entry = any(
        isinstance(entry, dict)
        and any(
            isinstance(h, dict) and h.get("command") == cmd
            for h in (entry.get("hooks") or [])
        )
        for entry in session_start
    )

    fake_target = EnhancementTarget(
        SETTINGS_FILE,
        _TEMPLATE_DIR / "AGENTS.md",  # source field unused for this entry
        "Claude Code project settings (merge-aware)",
    )

    if has_entry:
        return DeployAction(fake_target, "unchanged")

    session_start.append(SESSION_START_HOOK_ENTRY)
    settings_path.write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    return DeployAction(fake_target, "updated" if existed else "created")


@dataclass(frozen=True)
class VerifyResult:
    target: EnhancementTarget
    exists: bool
    placeholder_present: bool   # true means substitution wasn't applied
    matches_canonical: bool     # true means content matches current source


def verify(repo: Path) -> list[VerifyResult]:
    """Check each target plus the merged settings hook entry.

    Returns one VerifyResult per file target and one for the settings hook
    presence (relative_path = `.claude/settings.local.json (SessionStart hook)`).
    """

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

    for relative_path, content in expected_onboarding_files(repo).items():
        target = EnhancementTarget(
            relative_path,
            _TEMPLATE_DIR / "AGENTS.md",
            GENERATED_TARGET_DESCRIPTIONS.get(relative_path, "Generated onboarding artifact"),
        )
        dest = repo / relative_path
        if not dest.exists():
            out.append(VerifyResult(target, False, False, False))
            continue
        actual = dest.read_text(encoding="utf-8")
        out.append(
            VerifyResult(
                target=target,
                exists=True,
                placeholder_present=PLACEHOLDER in actual,
                matches_canonical=(actual == content),
            )
        )

    # Settings hook check.
    settings_target = EnhancementTarget(
        f"{SETTINGS_FILE} (SessionStart hook)",
        _TEMPLATE_DIR / "AGENTS.md",  # unused
        "Claude Code SessionStart hook entry merged into settings",
    )
    settings_path = repo / SETTINGS_FILE
    cmd = SESSION_START_HOOK_ENTRY["hooks"][0]["command"]
    if not settings_path.exists():
        out.append(VerifyResult(settings_target, False, False, False))
    else:
        try:
            settings = json.loads(settings_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            out.append(VerifyResult(settings_target, True, False, False))
        else:
            session_start = (settings.get("hooks") or {}).get("SessionStart") or []
            has_entry = any(
                isinstance(entry, dict)
                and any(
                    isinstance(h, dict) and h.get("command") == cmd
                    for h in (entry.get("hooks") or [])
                )
                for entry in session_start
            )
            out.append(
                VerifyResult(
                    target=settings_target,
                    exists=has_entry,
                    placeholder_present=False,
                    matches_canonical=True,
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


def summarize(repo: Path) -> dict[str, Any]:
    """Return a small enhancement summary with recommendations."""
    files = expected_onboarding_files(repo)
    onboarding = json.loads(files[ONBOARDING_JSON_PATH])
    act = json.loads(files[ACT_STARTER_JSON_PATH])
    recommendation = recommendation_summary(repo)
    return {
        "recommended_skill": recommendation["recommended_skill"],
        "recommended_mode": recommendation["recommended_mode"],
        "reason": recommendation["reason"],
        "onboarding": {
            "commands": onboarding["telemetry"]["counts"]["commands"],
            "areas": onboarding["telemetry"]["counts"]["areas"],
            "entrypoints": onboarding["telemetry"]["counts"]["entrypoints"],
            "notes": onboarding["telemetry"]["counts"]["notes"],
            "overrides_applied": onboarding["telemetry"]["overrides_applied"],
            "override_invalid": onboarding["telemetry"]["override_invalid"],
        },
        "act": {
            "has_fast_test": bool(act["commands"].get("fast_test")),
            "entrypoints": act["telemetry"]["entrypoint_count"],
            "caution_zones": act["telemetry"]["caution_zone_count"],
        },
        "freshness": override_freshness(repo),
        "experience_telemetry": summarize_events(repo),
    }


def refresh_status(repo: Path) -> dict[str, Any]:
    """Refresh repo-local experience status artifacts."""
    return write_status_artifacts(repo)
