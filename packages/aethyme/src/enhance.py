"""Deploy and verify Aethyme discoverability files in a target repository.

A repository is "Aethyme-enhanced" when an agent landing in its working
directory can find Aethyme without out-of-band context. We deploy several
files, all derived from canonical templates under
`packages/aethyme/skills/aethyme/`:

    AGENTS.md                                  # cross-product convention, fully generated
    CLAUDE.md                                  # Claude Code project instructions
    .claude/skills/aethyme/SKILL.md            # Claude Skills short runbook
    .codex/skills/aethyme/SKILL.md             # Codex skills short runbook
    .claude/skills/aethyme/references/*.md     # optional detailed workflows
    .codex/skills/aethyme/references/*.md      # optional detailed workflows
    .claude/hooks/aethyme-load-context.sh      # Claude Code SessionStart hook
    .claude/settings.local.json                # wires the hook (merge-aware)

The split between AGENTS.md/CLAUDE.md (root-level announcement),
SKILL.md (per-product short runbook), and references/*.md (optional
detailed workflows) is intentional: agents that auto-load
CLAUDE.md/AGENTS.md see the entry-point; agents that load their
product's skills directory see the bounded operating contract first.

AGENTS.md is now a generated artifact owned by Aethyme. Repo-specific
human policy is supplied through `.aethyme/overrides/agents.json`, not by
editing AGENTS.md directly. Legacy block-managed AGENTS.md files are
migrated forward once into the structured override path.

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
    STATUS_MARKDOWN_PATH,
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
AETHYME_BLOCK_BEGIN = "<!-- AETHYME:BEGIN generated -->"
AETHYME_BLOCK_END = "<!-- AETHYME:END generated -->"
AGENTS_OVERRIDE_PATH = ".aethyme/overrides/agents.json"

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
        "CLAUDE.md",
        _TEMPLATE_DIR / "AGENTS.md",
        "Claude Code project instructions (alias of AGENTS.md)",
    ),
    EnhancementTarget(
        ".claude/skills/aethyme/SKILL.md",
        _TEMPLATE_DIR / "SKILL.md",
        "Claude Skills short runbook",
    ),
    EnhancementTarget(
        ".codex/skills/aethyme/SKILL.md",
        _TEMPLATE_DIR / "SKILL.md",
        "Codex skills short runbook",
    ),
    EnhancementTarget(
        ".claude/skills/aethyme/references/explore.md",
        _TEMPLATE_DIR / "references" / "explore.md",
        "Claude Skills Explore reference",
    ),
    EnhancementTarget(
        ".codex/skills/aethyme/references/explore.md",
        _TEMPLATE_DIR / "references" / "explore.md",
        "Codex skills Explore reference",
    ),
    EnhancementTarget(
        ".claude/skills/aethyme/references/graph-task.md",
        _TEMPLATE_DIR / "references" / "graph-task.md",
        "Claude Skills graph/task reference",
    ),
    EnhancementTarget(
        ".codex/skills/aethyme/references/graph-task.md",
        _TEMPLATE_DIR / "references" / "graph-task.md",
        "Codex skills graph/task reference",
    ),
    EnhancementTarget(
        ".claude/skills/aethyme/references/dead-code.md",
        _TEMPLATE_DIR / "references" / "dead-code.md",
        "Claude Skills dead-code reference",
    ),
    EnhancementTarget(
        ".codex/skills/aethyme/references/dead-code.md",
        _TEMPLATE_DIR / "references" / "dead-code.md",
        "Codex skills dead-code reference",
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

GENERATED_ROOT_TARGETS = {"AGENTS.md", "CLAUDE.md"}

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


def _render_agents_document(root: str, repo: Path | None = None) -> str:
    content = (_TEMPLATE_DIR / "AGENTS.md").read_text(encoding="utf-8").replace(
        PLACEHOLDER, root
    )
    if repo is not None:
        routing = _render_repo_routing(repo)
        if routing:
            content = f"{content.rstrip()}\n\n{routing}"
        broker = _render_broker_protocol(repo, root)
        if broker:
            content = f"{content.rstrip()}\n\n{broker}"
        override_sections = _render_agents_override_sections(repo)
        if override_sections:
            content = f"{content.rstrip()}\n\n{override_sections}"
    return f"{content.rstrip()}\n"


def _render_broker_protocol(repo: Path, root: str) -> str:
    """Agent-facing broker coordination protocol.

    Rendered only when the repo is broker-configured (gates.toml or
    config.toml under .aethyme/), so repos that never adopted the broker
    don't get instructions that fail. Vendor-neutral by design: the
    protocol is plain shell commands + a file-drop convention, the two
    integration surfaces every coding agent already has.
    """
    if not (
        (repo / ".aethyme/gates.toml").exists()
        or (repo / ".aethyme/config.toml").exists()
    ):
        return ""
    return f"""## Broker Coordination (multi-agent repository)

This repository coordinates concurrent agent sessions through the Aethyme
broker. Other agents may be working in sibling worktrees right now. The
`aethyme` binary is installed once with
`cargo install --path "{root}/rust/crates/aethyme-engine"`
(check with `aethyme --version`). Follow this protocol:

1. **Before editing**, check current activity and register yourself:

   ```bash
   aethyme broker status --json    # who is working on what
   aethyme broker adopt --task "<your task>"   # once, from your worktree
   ```

   If `status` shows another session holding leases on the files you plan
   to change, prefer working elsewhere first or say so in your report —
   overlapping edits will conflict at merge time.

2. **While working**: commit early and small. Only committed work can be
   verified and integrated. Never switch branches inside someone else's
   worktree; never edit files outside your own worktree.

3. **When your task is complete**, submit your head commit for verified
   integration instead of merging anything yourself:

   ```bash
   aethyme broker submit --session <your-session-id>
   ```

   This simulates the merge and runs only the checks your diff affects.
   Report the outcome (verified / rejected / conflict) in your summary.
   Afterwards, finish the session with
   `aethyme broker close --session <id>` (state only), or point it at
   a follow-up task with `aethyme broker adopt --reuse --task "..."`.

4. **If a file named `.aethyme/broker-action-required.md` appears in your
   worktree**, read it immediately: your submission conflicted. It names
   the conflicting files, the blocking session, and the exact rebase
   steps. Resolve, commit, and resubmit.

5. **Never** push, merge to the default branch, or touch the
   `aethyme/integration` branch directly — integration and shipping are
   handled through the broker and the human operator."""


def _render_repo_routing(repo: Path) -> str:
    onboarding_path = repo / ONBOARDING_JSON_PATH
    act_path = repo / ACT_STARTER_JSON_PATH
    if not onboarding_path.exists():
        return ""
    onboarding = json.loads(onboarding_path.read_text(encoding="utf-8"))
    act = json.loads(act_path.read_text(encoding="utf-8")) if act_path.exists() else {}
    primary_commands = onboarding.get("primary_commands") or {}
    primary_entrypoints = onboarding.get("primary_entrypoints") or {}
    act_commands = act.get("commands") or {}
    fast_test = primary_commands.get("fast_test") or act_commands.get("fast_test")
    app_entrypoint = primary_entrypoints.get("app") or {}
    lines = [
        "## Aethyme Repo Routing",
        "",
        f"- Onboarding skill: `{ONBOARDING_CODEX_PATH}` or `{ONBOARDING_CLAUDE_PATH}`",
        f"- Act skill: `{ACT_CODEX_PATH}` or `{ACT_CLAUDE_PATH}`",
        f"- Experience status: `{STATUS_MARKDOWN_PATH}`",
    ]
    if fast_test:
        lines.append(f"- Primary fast test: `{fast_test}`")
    if app_entrypoint.get("path"):
        lines.append(f"- Primary app entrypoint: `{app_entrypoint['path']}`")
    return "\n".join(lines)


def _load_agents_overrides(repo: Path) -> dict[str, Any]:
    override_path = repo / AGENTS_OVERRIDE_PATH
    if not override_path.exists():
        return {}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"_invalid_override": True, "_source": AGENTS_OVERRIDE_PATH}
    if not isinstance(payload, dict):
        return {"_invalid_override": True, "_source": AGENTS_OVERRIDE_PATH}
    payload["_source"] = AGENTS_OVERRIDE_PATH
    return payload


def agents_override_template() -> dict[str, Any]:
    return {
        "repo_summary": "One-paragraph repo-specific summary for agents.",
        "hard_constraints": [
            "Never bypass tenant isolation or authorization checks."
        ],
        "validation_rules": [
            "Run the smallest relevant test set before broader suites."
        ],
        "commit_hygiene_notes": [
            "Document domain invariants in the Memory section for substantive commits."
        ],
        "summon_policy_notes": [
            "Load repo-onboarding first for broad or unfamiliar tasks."
        ],
        "maintainer_markdown": "## Domain Notes\n\nAdd compact repo-specific guidance here.",
    }


def validate_agents_overrides(repo: Path) -> dict[str, Any]:
    repo = Path(repo).expanduser().resolve()
    override_path = repo / AGENTS_OVERRIDE_PATH
    if not override_path.exists():
        return {"ok": True, "exists": False, "path": AGENTS_OVERRIDE_PATH, "errors": []}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return {
            "ok": False,
            "exists": True,
            "path": AGENTS_OVERRIDE_PATH,
            "errors": [f"invalid JSON: {exc.msg}"],
        }
    if not isinstance(payload, dict):
        return {
            "ok": False,
            "exists": True,
            "path": AGENTS_OVERRIDE_PATH,
            "errors": ["override root must be a JSON object"],
        }
    errors: list[str] = []
    if "repo_summary" in payload and not isinstance(payload["repo_summary"], str):
        errors.append("repo_summary must be a string")
    for key in (
        "hard_constraints",
        "validation_rules",
        "commit_hygiene_notes",
        "summon_policy_notes",
    ):
        value = payload.get(key)
        if value is not None and not (
            isinstance(value, list) and all(isinstance(item, str) for item in value)
        ):
            errors.append(f"{key} must be a list of strings")
    if "maintainer_markdown" in payload and not isinstance(
        payload["maintainer_markdown"], str
    ):
        errors.append("maintainer_markdown must be a string")
    return {
        "ok": not errors,
        "exists": True,
        "path": AGENTS_OVERRIDE_PATH,
        "errors": errors,
    }


def _render_agents_override_sections(repo: Path) -> str:
    overrides = _load_agents_overrides(repo)
    if overrides.get("_invalid_override"):
        return (
            "## Aethyme Override Status\n\n"
            f"Agents override file `{AGENTS_OVERRIDE_PATH}` is invalid JSON. "
            "Fix it and rerun `aethyme enhance deploy --repo \"$PWD\"`.\n"
        )
    sections: list[str] = []
    repo_summary = overrides.get("repo_summary")
    if isinstance(repo_summary, str) and repo_summary.strip():
        sections.append(f"## Repo Summary\n\n{repo_summary.strip()}")
    sections.extend(
        _render_agents_override_list_section(
            "## Hard Constraints", overrides.get("hard_constraints")
        )
    )
    sections.extend(
        _render_agents_override_list_section(
            "## Validation Rules", overrides.get("validation_rules")
        )
    )
    sections.extend(
        _render_agents_override_list_section(
            "## Commit Hygiene Notes", overrides.get("commit_hygiene_notes")
        )
    )
    sections.extend(
        _render_agents_override_list_section(
            "## Summon Policy Notes", overrides.get("summon_policy_notes")
        )
    )
    maintainer_markdown = overrides.get("maintainer_markdown")
    if (
        isinstance(maintainer_markdown, str)
        and maintainer_markdown.strip()
        and not _looks_like_generated_agents_document(maintainer_markdown.strip())
    ):
        sections.append(f"## Maintainer Notes\n\n{maintainer_markdown.strip()}")
    return "\n\n".join(sections)


def _render_agents_override_list_section(title: str, value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    items = [str(item).strip() for item in value if isinstance(item, str) and item.strip()]
    if not items:
        return []
    return [title, "\n".join(f"- {item}" for item in items)]


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
    cleanup_action = _drop_stale_generated_agents_override(repo)
    if cleanup_action is not None:
        actions.append(cleanup_action)
    migration_action = _migrate_legacy_agents_content(repo)
    if migration_action is not None:
        actions.append(migration_action)

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

    actions.insert(0, _ensure_agents_document(repo, root, force=force))
    for t in TARGETS:
        dest = repo / t.relative_path
        content = _render_agents_document(root, repo) if t.relative_path == "CLAUDE.md" else _render(t, root)
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


def _ensure_agents_document(repo: Path, root: str, *, force: bool) -> DeployAction:
    """Create or refresh the generated AGENTS.md root instruction file."""
    target = EnhancementTarget(
        "AGENTS.md",
        _TEMPLATE_DIR / "AGENTS.md",
        "Cross-product agent instructions (fully generated by Aethyme)",
    )
    dest = repo / target.relative_path
    content = _render_agents_document(root, repo)
    existed = dest.exists()
    if not existed:
        dest.write_text(content, encoding="utf-8")
        return DeployAction(target, "created")

    existing = dest.read_text(encoding="utf-8")
    if not force and existing == content:
        return DeployAction(target, "unchanged")
    dest.write_text(content, encoding="utf-8")
    return DeployAction(target, "updated")


def _migrate_legacy_agents_content(repo: Path) -> DeployAction | None:
    """Migrate legacy or hand-edited AGENTS.md content into structured overrides once."""
    agents_path = repo / "AGENTS.md"
    if not agents_path.exists():
        return None
    existing = agents_path.read_text(encoding="utf-8")
    legacy_content = _extract_legacy_agents_content(existing)
    if not legacy_content:
        return None

    override_path = repo / AGENTS_OVERRIDE_PATH
    overrides = _load_agents_overrides(repo)
    if overrides.get("_invalid_override"):
        return None
    clean_overrides = {
        key: value for key, value in overrides.items() if not key.startswith("_")
    }
    existing_markdown = clean_overrides.get("maintainer_markdown")
    if isinstance(existing_markdown, str) and existing_markdown.strip():
        if legacy_content.strip() in existing_markdown:
            return None
        clean_overrides["maintainer_markdown"] = (
            f"{existing_markdown.rstrip()}\n\n{legacy_content.strip()}"
        )
    else:
        clean_overrides["maintainer_markdown"] = legacy_content.strip()

    override_path.parent.mkdir(parents=True, exist_ok=True)
    existed = override_path.exists()
    override_path.write_text(json.dumps(clean_overrides, indent=2) + "\n", encoding="utf-8")
    target = EnhancementTarget(
        AGENTS_OVERRIDE_PATH,
        _TEMPLATE_DIR / "AGENTS.md",
        "Repo-specific AGENTS override data",
    )
    return DeployAction(target, "updated" if existed else "created")


def _extract_legacy_agents_content(existing: str) -> str:
    stripped = existing.strip()
    if not stripped:
        return ""

    if AETHYME_BLOCK_BEGIN in existing and AETHYME_BLOCK_END in existing:
        before, remainder = existing.split(AETHYME_BLOCK_BEGIN, 1)
        _, after = remainder.split(AETHYME_BLOCK_END, 1)
        pieces = [before.strip(), after.strip()]
        return "\n\n".join(piece for piece in pieces if piece)

    if _looks_like_generated_agents_document(stripped):
        return ""

    rendered_template = (_TEMPLATE_DIR / "AGENTS.md").read_text(encoding="utf-8").replace(
        PLACEHOLDER, aethyme_root()
    )
    if stripped == rendered_template.strip():
        return ""
    return stripped


def _drop_stale_generated_agents_override(repo: Path) -> DeployAction | None:
    """Remove old generated Aethyme boilerplate from maintainer overrides."""
    override_path = repo / AGENTS_OVERRIDE_PATH
    if not override_path.exists():
        return None
    overrides = _load_agents_overrides(repo)
    if overrides.get("_invalid_override"):
        return None
    maintainer_markdown = overrides.get("maintainer_markdown")
    if not (
        isinstance(maintainer_markdown, str)
        and _looks_like_generated_agents_document(maintainer_markdown.strip())
    ):
        return None

    clean_overrides = {
        key: value
        for key, value in overrides.items()
        if not key.startswith("_") and key != "maintainer_markdown"
    }
    if clean_overrides:
        override_path.write_text(json.dumps(clean_overrides, indent=2) + "\n", encoding="utf-8")
    else:
        override_path.unlink()

    target = EnhancementTarget(
        AGENTS_OVERRIDE_PATH,
        _TEMPLATE_DIR / "AGENTS.md",
        "Repo-specific AGENTS override data",
    )
    return DeployAction(target, "updated")


def _looks_like_generated_agents_document(content: str) -> bool:
    """Detect legacy generated Aethyme root guidance, including stale variants."""
    if not content:
        return False
    required_markers = (
        "# Agent Instructions",
        "This repository is **Aethyme-enhanced**",
        "## Quick start (any agent)",
        "## Detailed reference",
        "## Verifying this enhancement",
    )
    return all(marker in content for marker in required_markers)


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
    out.append(_verify_agents_document(repo, root))
    for t in TARGETS:
        dest = repo / t.relative_path
        if not dest.exists():
            out.append(VerifyResult(t, False, False, False))
            continue
        actual = dest.read_text(encoding="utf-8")
        canonical = _render_agents_document(root, repo) if t.relative_path == "CLAUDE.md" else _render(t, root)
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


def _verify_agents_document(repo: Path, root: str) -> VerifyResult:
    target = EnhancementTarget(
        "AGENTS.md",
        _TEMPLATE_DIR / "AGENTS.md",
        "Cross-product agent instructions (fully generated by Aethyme)",
    )
    dest = repo / target.relative_path
    if not dest.exists():
        return VerifyResult(target, False, False, False)
    actual = dest.read_text(encoding="utf-8")
    content = _render_agents_document(root, repo)
    return VerifyResult(
        target=target,
        exists=True,
        placeholder_present=PLACEHOLDER in actual,
        matches_canonical=actual == content,
    )


def is_ok(results: Iterable[VerifyResult]) -> bool:
    """Return True iff every target exists and has its placeholder substituted.

    Content drift is allowed for secondary files, but AGENTS.md and CLAUDE.md
    are generated artifacts and direct edits are unsupported. Missing files and
    leftover {{AETHYME_ROOT}} placeholders are real failures.
    """
    strict_targets = GENERATED_ROOT_TARGETS
    return all(
        r.exists
        and not r.placeholder_present
        and (
            r.matches_canonical
            or r.target.relative_path not in strict_targets
        )
        for r in results
    )


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
