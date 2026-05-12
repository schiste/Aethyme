"""Deterministic repo-onboarding skill generation."""

from __future__ import annotations

import hashlib
import json
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .skills import AETHYME_PACKAGE_ROOT

ONBOARDING_JSON_PATH = ".aethyme/generated/onboarding.json"
ACT_STARTER_JSON_PATH = ".aethyme/generated/act-starter.json"
ONBOARDING_CLAUDE_PATH = ".claude/skills/repo-onboarding/SKILL.md"
ONBOARDING_CODEX_PATH = ".codex/skills/repo-onboarding/SKILL.md"
ACT_CLAUDE_PATH = ".claude/skills/repo-act/SKILL.md"
ACT_CODEX_PATH = ".codex/skills/repo-act/SKILL.md"
ONBOARDING_OVERRIDE_PATH = ".aethyme/overrides/onboarding.json"

_AREA_ROLE_MAP = {
    "src": "source",
    "lib": "source",
    "app": "application",
    "apps": "application",
    "packages": "workspace",
    "services": "workspace",
    "backend": "service",
    "frontend": "service",
    "cmd": "entrypoints",
    "internal": "internal",
    "includes": "source",
    "tests": "tests",
    "test": "tests",
    "__tests__": "tests",
    "spec": "tests",
    "specs": "tests",
    "docs": "docs",
    "doc": "docs",
    "scripts": "tooling",
    "tools": "tooling",
    "tooling": "tooling",
    "bin": "tooling",
    ".github": "automation",
    "infra": "infrastructure",
    "deploy": "infrastructure",
    "deployments": "infrastructure",
    "docker": "infrastructure",
    "terraform": "infrastructure",
    "k8s": "infrastructure",
    "helm": "infrastructure",
    "public": "assets",
    "static": "assets",
    "assets": "assets",
    "vendor": "generated_or_vendor",
    "third_party": "generated_or_vendor",
    "generated": "generated_or_vendor",
    "dist": "generated_or_vendor",
    "build": "generated_or_vendor",
    "coverage": "generated_or_vendor",
    "target": "generated_or_vendor",
    "node_modules": "generated_or_vendor",
}

_CAUTION_ZONE_NAMES = {
    "vendor",
    "third_party",
    "generated",
    "dist",
    "build",
    "coverage",
    "target",
    "node_modules",
    "migrations",
    "fixtures",
    "snapshots",
}


def expected_onboarding_files(
    repo_path: Path,
    *,
    aethyme_root: Path | None = None,
) -> dict[str, str]:
    """Return deterministic onboarding artifacts for *repo_path*."""
    artifact = build_onboarding_artifact(repo_path, aethyme_root=aethyme_root)
    onboarding_markdown = render_onboarding_skill(artifact)
    act_artifact = build_act_starter_artifact(artifact)
    act_markdown = render_act_skill(act_artifact)
    return {
        ONBOARDING_JSON_PATH: json.dumps(artifact, indent=2) + "\n",
        ACT_STARTER_JSON_PATH: json.dumps(act_artifact, indent=2) + "\n",
        ONBOARDING_CLAUDE_PATH: onboarding_markdown,
        ONBOARDING_CODEX_PATH: onboarding_markdown,
        ACT_CLAUDE_PATH: act_markdown,
        ACT_CODEX_PATH: act_markdown,
    }


def build_onboarding_artifact(
    repo_path: Path,
    *,
    aethyme_root: Path | None = None,
) -> dict[str, Any]:
    """Build a deterministic onboarding artifact for a repository."""
    repo_path = Path(repo_path).expanduser().resolve()
    snapshot = _snapshot_metadata(repo_path)
    aethyme_root = (aethyme_root or AETHYME_PACKAGE_ROOT).resolve()

    manifests = _detect_manifests(repo_path)
    package_manager = _primary_package_manager(repo_path, manifests)
    commands = _collect_commands(repo_path, manifests, package_manager)
    areas = _collect_areas(repo_path)
    entrypoints = _collect_entrypoints(repo_path, manifests)
    caution_zones = _collect_caution_zones(repo_path)
    repo_kind = _infer_repo_kind(repo_path, manifests, areas)
    languages = _infer_languages(repo_path, manifests)
    overrides = _load_overrides(repo_path)

    artifact: dict[str, Any] = {
        "schema_version": "aethyme-onboarding-v1",
        "repo": {
            "name": snapshot.repo_name,
            "root": str(snapshot.repo_path),
            "kind": repo_kind,
            "languages": languages,
            "package_manager": package_manager,
            "manifests": manifests,
        },
        "commands": commands,
        "areas": areas,
        "entrypoints": entrypoints,
        "caution_zones": caution_zones,
        "navigation_recipes": _navigation_recipes(aethyme_root),
        "summon": _summon_policy(),
        "freshness": {
            "generated_at": datetime.now(UTC).replace(microsecond=0).isoformat(),
            "snapshot_key": snapshot.cache_key,
            "commit": snapshot.commit,
            "repo_dirty": snapshot.dirty,
            "file_count": snapshot.file_count,
            "generator": "deterministic",
        },
    }
    _apply_overrides(artifact, overrides)
    artifact["telemetry"] = _generation_telemetry(artifact, overrides)
    return artifact


def build_act_starter_artifact(onboarding_artifact: dict[str, Any]) -> dict[str, Any]:
    """Build a deterministic Act starter artifact from onboarding facts."""
    repo = onboarding_artifact["repo"]
    commands = onboarding_artifact.get("commands", [])
    entrypoints = onboarding_artifact.get("entrypoints", [])
    caution_zones = onboarding_artifact.get("caution_zones", [])
    return {
        "schema_version": "aethyme-act-starter-v1",
        "repo": {
            "name": repo["name"],
            "kind": repo["kind"],
        },
        "recommended_mode": "repo-act",
        "starter_checklists": {
            "debugging": [
                "Load repo-onboarding first if the repository or area is unfamiliar.",
                "Run Aethyme Explore on the user task before broad manual search.",
                "Verify answer candidates before concluding when trust_policy is not answer-safe.",
                "Identify the nearest fast test or validation command before editing.",
                "Inspect caution zones before changing generated or vendored areas.",
            ],
            "change_validation": [
                "Run the fastest relevant test command first.",
                "Check likely entrypoints and impacted callers before finishing.",
                "Re-run lint/build only after the focused validation passes.",
                "Document any override-based maintainer note that affected the change plan.",
            ],
        },
        "commands": {
            "fast_test": _first_command_of_kind(commands, "test"),
            "lint": _first_command_of_kind(commands, "lint"),
            "build": _first_command_of_kind(commands, "build"),
        },
        "entrypoints": entrypoints[:3],
        "caution_zones": caution_zones[:5],
        "telemetry": {
            "derived_from": ONBOARDING_JSON_PATH,
            "entrypoint_count": len(entrypoints),
            "caution_zone_count": len(caution_zones),
        },
    }


def render_onboarding_skill(artifact: dict[str, Any]) -> str:
    """Render the onboarding artifact into an agent-facing skill."""
    repo = artifact["repo"]
    commands = artifact["commands"]
    areas = artifact["areas"]
    entrypoints = artifact["entrypoints"]
    caution_zones = artifact["caution_zones"]
    recipes = artifact["navigation_recipes"]
    notes = artifact.get("notes") or []
    summon = artifact["summon"]
    freshness = artifact["freshness"]
    telemetry = artifact["telemetry"]

    lines = [
        "---",
        "name: repo-onboarding",
        f"description: {summon['description']}",
        "---",
        "",
        f"# Repo Onboarding: {repo['name']}",
        "",
        "## When to Use",
        "",
        "- Load this skill first when the repository is unfamiliar or the request is broad.",
        f"- Recommended when: {', '.join(summon['recommended_when'])}.",
        f"- Skip when: {', '.join(summon['skip_when'])}.",
        "- Use `.codex/skills/aethyme/SKILL.md` or `.claude/skills/aethyme/SKILL.md` for the full Aethyme tool runbook after orientation.",
        "",
        "## Repo Identity",
        "",
        f"- Kind: `{repo['kind']}`",
        f"- Languages: `{', '.join(repo['languages']) or 'unknown'}`",
        f"- Package manager: `{repo['package_manager']}`",
    ]

    if repo["manifests"]:
        lines.append(f"- Key manifests: `{', '.join(repo['manifests'])}`")

    lines.extend(
        [
            "",
            "## Start Here",
            "",
        ]
    )
    for command in commands[:5]:
        lines.append(
            f"- `{command['command']}`"
            f" ({command['kind']}; {command['confidence']} confidence from `{command['source']}`)"
        )

    if entrypoints:
        lines.extend(["", "## Entrypoints", ""])
        for entrypoint in entrypoints[:5]:
            lines.append(
                f"- `{entrypoint['path']}`"
                f" ({entrypoint['kind']}; {entrypoint['reason']}; {entrypoint.get('confidence', 'medium')} confidence)"
            )

    if areas:
        lines.extend(["", "## Repo Map", ""])
        for area in areas[:8]:
            lines.append(
                f"- `{area['path']}`"
                f" ({area['role']}; {area['reason']}; {area.get('confidence', 'medium')} confidence)"
            )

    lines.extend(["", "## Aethyme Recipes", ""])
    for recipe in recipes:
        lines.append(f"- `{recipe['command']}`")
        lines.append(f"  Purpose: {recipe['purpose']}")

    if caution_zones:
        lines.extend(["", "## Caution Zones", ""])
        for zone in caution_zones[:6]:
            lines.append(f"- `{zone['path']}` ({zone['reason']})")

    if notes:
        lines.extend(["", "## Maintainer Notes", ""])
        for note in notes[:8]:
            lines.append(f"- {note}")

    lines.extend(
        [
            "",
            "## Freshness",
            "",
            f"- Snapshot key: `{freshness['snapshot_key']}`",
            f"- Commit: `{freshness['commit'] or 'working-tree'}`",
            f"- Repo dirty: `{freshness['repo_dirty']}`",
            f"- Generated at: `{freshness['generated_at']}`",
            f"- Overrides applied: `{telemetry['overrides_applied']}`",
            f"- Sections generated: `{', '.join(telemetry['sections'])}`",
        ]
    )
    return "\n".join(lines) + "\n"


def render_act_skill(artifact: dict[str, Any]) -> str:
    """Render the Act starter artifact into an agent-facing skill."""
    repo = artifact["repo"]
    commands = artifact["commands"]
    entrypoints = artifact["entrypoints"]
    caution_zones = artifact["caution_zones"]
    checklists = artifact["starter_checklists"]

    lines = [
        "---",
        "name: repo-act",
        "description: Use after repo-onboarding or Explore when moving from orientation into execution planning, debugging checklists, and validation steps. Skip when only broad repo orientation is needed.",
        "---",
        "",
        f"# Repo Act: {repo['name']}",
        "",
        "## When to Use",
        "",
        "- Load this after repo-onboarding when the next step is execution or validation planning.",
        "- Skip it for broad repo overview questions with no action plan yet.",
        "",
        "## Debugging Checklist",
        "",
    ]
    for item in checklists["debugging"]:
        lines.append(f"- {item}")
    lines.extend(["", "## Validation Checklist", ""])
    for item in checklists["change_validation"]:
        lines.append(f"- {item}")
    lines.extend(["", "## Useful Commands", ""])
    for label, command in commands.items():
        if command:
            lines.append(f"- `{label}`: `{command}`")
    if entrypoints:
        lines.extend(["", "## Likely Entrypoints", ""])
        for entrypoint in entrypoints:
            lines.append(f"- `{entrypoint['path']}` ({entrypoint['reason']})")
    if caution_zones:
        lines.extend(["", "## Caution Zones", ""])
        for zone in caution_zones:
            lines.append(f"- `{zone['path']}` ({zone['reason']})")
    return "\n".join(lines) + "\n"


def _detect_manifests(repo_path: Path) -> list[str]:
    manifests = [
        "package.json",
        "pnpm-workspace.yaml",
        "pyproject.toml",
        "Cargo.toml",
        "composer.json",
        "go.mod",
        "Makefile",
        "justfile",
    ]
    return [name for name in manifests if (repo_path / name).exists()]


def _primary_package_manager(repo_path: Path, manifests: list[str]) -> str:
    if "pnpm-workspace.yaml" in manifests or (repo_path / "pnpm-lock.yaml").exists():
        return "pnpm"
    if (repo_path / "package-lock.json").exists():
        return "npm"
    if (repo_path / "yarn.lock").exists():
        return "yarn"
    if "Cargo.toml" in manifests:
        return "cargo"
    if "composer.json" in manifests:
        return "composer"
    if "go.mod" in manifests:
        return "go"
    if "pyproject.toml" in manifests:
        return "python"
    if "Makefile" in manifests:
        return "make"
    return "unknown"


def _collect_commands(
    repo_path: Path,
    manifests: list[str],
    package_manager: str,
) -> list[dict[str, str]]:
    commands: list[dict[str, str]] = []
    seen: set[str] = set()

    def add(kind: str, command: str, source: str, confidence: str) -> None:
        if command in seen:
            return
        seen.add(command)
        commands.append(
            {
                "kind": kind,
                "command": command,
                "source": source,
                "confidence": confidence,
            }
        )

    if package_manager in {"pnpm", "npm", "yarn"} and (repo_path / "package.json").exists():
        package_json = json.loads((repo_path / "package.json").read_text(encoding="utf-8"))
        scripts = package_json.get("scripts") or {}
        install_cmd = {
            "pnpm": "pnpm install --frozen-lockfile" if (repo_path / "pnpm-lock.yaml").exists() else "pnpm install",
            "npm": "npm install",
            "yarn": "yarn install --immutable" if (repo_path / ".yarnrc.yml").exists() else "yarn install",
        }[package_manager]
        add("install", install_cmd, "package.json", "high")
        for script_name in ("test", "lint", "dev", "build", "start"):
            if script_name in scripts:
                prefix = "npm run" if package_manager == "npm" else package_manager
                add(script_name, f"{prefix} {script_name}", f"package.json:scripts.{script_name}", "high")
        if scripts:
            primary = _guess_primary_script(scripts)
            if primary:
                add("primary-script", f"{package_manager} {primary}", f"package.json:scripts.{primary}", "medium")

    if "pyproject.toml" in manifests:
        add("test", "pytest", "pyproject.toml", "medium")
        add("lint", "ruff check .", "pyproject.toml", "medium")
        add("install", "python -m pip install -e .", "pyproject.toml", "medium")

    if "Cargo.toml" in manifests:
        add("build", "cargo build", "Cargo.toml", "high")
        add("test", "cargo test", "Cargo.toml", "high")
        add("dev", "cargo run", "Cargo.toml", "medium")

    if "composer.json" in manifests:
        add("install", "composer install", "composer.json", "high")
        composer_json = json.loads((repo_path / "composer.json").read_text(encoding="utf-8"))
        scripts = composer_json.get("scripts") or {}
        for script_name in ("test", "lint"):
            if script_name in scripts:
                add(script_name, f"composer {script_name}", f"composer.json:scripts.{script_name}", "high")

    if "Makefile" in manifests:
        targets = _make_targets(repo_path / "Makefile")
        for script_name in ("install", "test", "lint", "dev", "build"):
            if script_name in targets:
                add(script_name, f"make {script_name}", f"Makefile:{script_name}", "medium")

    for inferred in _commands_from_github_actions(repo_path):
        add(
            inferred["kind"],
            inferred["command"],
            inferred["source"],
            inferred["confidence"],
        )

    return commands[:8]


def _make_targets(makefile_path: Path) -> set[str]:
    targets: set[str] = set()
    for line in makefile_path.read_text(encoding="utf-8").splitlines():
        if ":" not in line or line.startswith("\t") or line.startswith("#"):
            continue
        target = line.split(":", 1)[0].strip()
        if target and " " not in target and not target.startswith("."):
            targets.add(target)
    return targets


def _collect_areas(repo_path: Path) -> list[dict[str, str]]:
    areas: list[dict[str, str]] = []
    for path in sorted(repo_path.iterdir()):
        if not path.is_dir():
            continue
        name = path.name
        role = _AREA_ROLE_MAP.get(name)
        if not role:
            continue
        reason = {
            "source": "conventional source directory",
            "application": "top-level application area",
            "workspace": "workspace-style package container",
            "service": "service or deployable area",
            "tests": "conventional test directory",
            "docs": "documentation area",
            "tooling": "developer tooling or scripts",
            "automation": "automation and CI configuration",
            "infrastructure": "deployment or infrastructure configuration",
            "assets": "public assets or static files",
            "generated_or_vendor": "generated output or vendored dependencies",
            "entrypoints": "binary or service entrypoint area",
            "internal": "internal implementation area",
        }[role]
        areas.append({"path": name, "role": role, "reason": reason, "confidence": "high"})
    return areas


def _collect_entrypoints(repo_path: Path, manifests: list[str]) -> list[dict[str, str]]:
    candidates: list[dict[str, str]] = []
    roots = [
        "main.py",
        "app.py",
        "manage.py",
        "main.go",
        "cmd",
        "src/main.py",
        "src/index.ts",
        "src/index.js",
        "src/main.ts",
        "src/main.rs",
        "public/index.php",
        "index.php",
    ]
    for relative in roots:
        path = repo_path / relative
        if path.exists():
            kind = "directory" if path.is_dir() else "file"
            candidates.append(
                {
                    "path": relative,
                    "kind": kind,
                    "reason": "conventional application entrypoint",
                    "confidence": "medium",
                }
            )
    if "Cargo.toml" in manifests and (repo_path / "src/lib.rs").exists():
        candidates.append(
            {
                "path": "src/lib.rs",
                "kind": "file",
                "reason": "library root for Rust crate",
                "confidence": "medium",
            }
        )
    return candidates[:6]


def _collect_caution_zones(repo_path: Path) -> list[dict[str, str]]:
    caution: list[dict[str, str]] = []
    for path in sorted(repo_path.iterdir()):
        if not path.exists():
            continue
        if path.name in _CAUTION_ZONE_NAMES:
            caution.append(
                {
                    "path": path.name,
                    "reason": "likely generated, vendored, fixture, or migration-heavy area",
                }
            )
    return caution


def _infer_repo_kind(
    repo_path: Path,
    manifests: list[str],
    areas: list[dict[str, str]],
) -> str:
    area_paths = {area["path"] for area in areas}
    if "pnpm-workspace.yaml" in manifests or "packages" in area_paths or "apps" in area_paths:
        return "monorepo"
    if "Cargo.toml" in manifests and (repo_path / "src/lib.rs").exists() and not (repo_path / "src/main.rs").exists():
        return "library"
    if "composer.json" in manifests or "public" in area_paths:
        return "application"
    if "go.mod" in manifests or "cmd" in area_paths:
        return "service"
    if "pyproject.toml" in manifests and "src" in area_paths:
        return "library_or_service"
    return "repository"


def _infer_languages(repo_path: Path, manifests: list[str]) -> list[str]:
    languages: list[str] = []
    if "pyproject.toml" in manifests:
        languages.append("python")
    if "package.json" in manifests:
        if any((repo_path / rel).exists() for rel in ("tsconfig.json", "src/index.ts", "src/main.ts")):
            languages.append("typescript")
        else:
            languages.append("javascript")
    if "Cargo.toml" in manifests:
        languages.append("rust")
    if "composer.json" in manifests:
        languages.append("php")
    if "go.mod" in manifests:
        languages.append("go")
    return languages


def _navigation_recipes(aethyme_root: Path) -> list[dict[str, str]]:
    bin_path = aethyme_root / "rust" / "target" / "release" / "aethyme"
    return [
        {
            "purpose": "Broad repository orientation for a user request",
            "command": f"\"{bin_path}\" explore --repo \"$PWD\" --request \"<task>\" --format answer-json",
        },
        {
            "purpose": "Quick deterministic repo summary",
            "command": f"(cd \"{aethyme_root}\" && .venv/bin/python -m src.cli repo inspect \"$PWD\" --mode brief --json-output)",
        },
        {
            "purpose": "Trace likely impact before editing",
            "command": f"(cd \"{aethyme_root}\" && .venv/bin/python -m src.cli graph callers \"$PWD\" \"<symbol-or-file>\" --json-output)",
        },
    ]


def _guess_primary_script(scripts: dict[str, Any]) -> str | None:
    for candidate in ("dev", "start", "serve", "storybook"):
        if candidate in scripts:
            return candidate
    return None


def _commands_from_github_actions(repo_path: Path) -> list[dict[str, str]]:
    workflows_dir = repo_path / ".github" / "workflows"
    if not workflows_dir.exists():
        return []
    commands: list[dict[str, str]] = []
    lines = []
    for workflow in sorted(workflows_dir.glob("*.y*ml")):
        try:
            lines.extend(workflow.read_text(encoding="utf-8").splitlines())
        except OSError:
            continue
    joined = "\n".join(lines)
    patterns = [
        ("test", "pnpm test", "github-actions", "medium"),
        ("test", "npm test", "github-actions", "medium"),
        ("test", "cargo test", "github-actions", "medium"),
        ("test", "pytest", "github-actions", "medium"),
        ("lint", "ruff check .", "github-actions", "medium"),
        ("lint", "cargo clippy", "github-actions", "medium"),
        ("build", "cargo build", "github-actions", "medium"),
    ]
    for kind, command, source, confidence in patterns:
        if command in joined:
            commands.append(
                {
                    "kind": kind,
                    "command": command,
                    "source": source,
                    "confidence": confidence,
                }
            )
    return commands


def _first_command_of_kind(commands: list[dict[str, str]], kind: str) -> str | None:
    for command in commands:
        if command.get("kind") == kind:
            return str(command.get("command"))
    return None


def _load_overrides(repo_path: Path) -> dict[str, Any]:
    override_path = repo_path / ONBOARDING_OVERRIDE_PATH
    if not override_path.exists():
        return {}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"_invalid_override": True, "_source": ONBOARDING_OVERRIDE_PATH}
    if not isinstance(payload, dict):
        return {"_invalid_override": True, "_source": ONBOARDING_OVERRIDE_PATH}
    payload["_source"] = ONBOARDING_OVERRIDE_PATH
    return payload


def _apply_overrides(artifact: dict[str, Any], overrides: dict[str, Any]) -> None:
    repo_overrides = overrides.get("repo")
    if isinstance(repo_overrides, dict):
        artifact["repo"].update({k: v for k, v in repo_overrides.items() if v is not None})

    summon_overrides = overrides.get("summon")
    if isinstance(summon_overrides, dict):
        artifact["summon"].update({k: v for k, v in summon_overrides.items() if v is not None})

    for key in ("commands", "areas", "entrypoints", "caution_zones", "navigation_recipes"):
        value = overrides.get(key)
        if isinstance(value, list):
            artifact[key] = value

    notes = overrides.get("notes")
    if isinstance(notes, list):
        artifact["notes"] = notes


def _summon_policy() -> dict[str, Any]:
    return {
        "description": (
            "Use when starting work in an unfamiliar repository, when the task asks "
            "for repo overview, setup, architecture, entrypoints, test commands, "
            "or where to begin. Skip for narrow file-scoped edits once the relevant "
            "paths are already known."
        ),
        "recommended_when": [
            "first task in repo",
            "repo overview",
            "setup or run instructions",
            "architecture or entrypoints",
            "where should I start",
            "broad debugging or feature-localization request",
        ],
        "skip_when": [
            "known file-scoped edit",
            "follow-up inside already identified area",
            "task already localized to concrete files",
        ],
        "task_signals": [
            "unfamiliar repository",
            "broad task with no path named",
            "user asks how the repo is organized",
        ],
    }


def _generation_telemetry(artifact: dict[str, Any], overrides: dict[str, Any]) -> dict[str, Any]:
    return {
        "sections": [
            "repo",
            "commands",
            "areas",
            "entrypoints",
            "caution_zones",
            "navigation_recipes",
            "summon",
            "freshness",
        ],
        "counts": {
            "commands": len(artifact.get("commands", [])),
            "areas": len(artifact.get("areas", [])),
            "entrypoints": len(artifact.get("entrypoints", [])),
            "caution_zones": len(artifact.get("caution_zones", [])),
            "navigation_recipes": len(artifact.get("navigation_recipes", [])),
            "notes": len(artifact.get("notes", [])),
        },
        "overrides_applied": bool(overrides),
        "override_source": overrides.get("_source"),
        "override_invalid": bool(overrides.get("_invalid_override")),
    }


def override_template() -> dict[str, Any]:
    """Return a starter onboarding override template."""
    return {
        "repo": {
            "kind": "service",
        },
        "commands": [
            {
                "kind": "test",
                "command": "./scripts/test-fast.sh",
                "source": "manual-override",
                "confidence": "high",
            }
        ],
        "notes": [
            "Use sandbox credentials from 1Password.",
            "Do not edit generated files under src/gen directly.",
        ],
        "summon": {
            "recommended_when": ["first touch", "broad task"],
            "skip_when": ["single known file"],
        },
    }


def validate_overrides(repo_path: Path) -> dict[str, Any]:
    """Return validation status for the onboarding override file."""
    repo_path = Path(repo_path).expanduser().resolve()
    override_path = repo_path / ONBOARDING_OVERRIDE_PATH
    if not override_path.exists():
        return {"ok": True, "exists": False, "path": ONBOARDING_OVERRIDE_PATH, "errors": []}
    try:
        payload = json.loads(override_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return {
            "ok": False,
            "exists": True,
            "path": ONBOARDING_OVERRIDE_PATH,
            "errors": [f"invalid JSON: {exc.msg}"],
        }
    if not isinstance(payload, dict):
        return {
            "ok": False,
            "exists": True,
            "path": ONBOARDING_OVERRIDE_PATH,
            "errors": ["override root must be a JSON object"],
        }
    errors: list[str] = []
    notes = payload.get("notes")
    if notes is not None and not (isinstance(notes, list) and all(isinstance(item, str) for item in notes)):
        errors.append("notes must be a list of strings")
    commands = payload.get("commands")
    if commands is not None and not isinstance(commands, list):
        errors.append("commands must be a list")
    summon = payload.get("summon")
    if summon is not None and not isinstance(summon, dict):
        errors.append("summon must be an object")
    return {
        "ok": not errors,
        "exists": True,
        "path": ONBOARDING_OVERRIDE_PATH,
        "errors": errors,
    }


def override_freshness(repo_path: Path) -> dict[str, Any]:
    """Return whether generated onboarding artifacts are fresh relative to overrides."""
    repo_path = Path(repo_path).expanduser().resolve()
    override_path = repo_path / ONBOARDING_OVERRIDE_PATH
    onboarding_path = repo_path / ONBOARDING_JSON_PATH
    act_path = repo_path / ACT_STARTER_JSON_PATH

    if not override_path.exists():
        return {
            "override_exists": False,
            "generated_exists": {
                "onboarding": onboarding_path.exists(),
                "act": act_path.exists(),
            },
            "regeneration_required": False,
            "stale_targets": [],
        }

    override_mtime = override_path.stat().st_mtime
    stale_targets: list[str] = []
    generated_exists = {
        "onboarding": onboarding_path.exists(),
        "act": act_path.exists(),
    }
    if not onboarding_path.exists() or onboarding_path.stat().st_mtime < override_mtime:
        stale_targets.append("onboarding")
    if not act_path.exists() or act_path.stat().st_mtime < override_mtime:
        stale_targets.append("act")

    return {
        "override_exists": True,
        "override_path": ONBOARDING_OVERRIDE_PATH,
        "override_mtime": override_mtime,
        "generated_exists": generated_exists,
        "generated_mtime": {
            "onboarding": onboarding_path.stat().st_mtime if onboarding_path.exists() else None,
            "act": act_path.stat().st_mtime if act_path.exists() else None,
        },
        "regeneration_required": bool(stale_targets),
        "stale_targets": stale_targets,
    }


def recommendation_summary(repo_path: Path, *, aethyme_root: Path | None = None) -> dict[str, Any]:
    """Return a small deterministic recommendation surface for the repo."""
    artifact = build_onboarding_artifact(repo_path, aethyme_root=aethyme_root)
    return {
        "recommended_skill": "repo-onboarding",
        "recommended_mode": "explore",
        "reason": "broad repo orientation and safe first-step guidance",
        "summon": artifact["summon"],
        "telemetry": artifact["telemetry"],
    }


class _OnboardingSnapshot:
    def __init__(self, repo_path: Path, commit: str | None, dirty: bool, fingerprint: str, file_count: int) -> None:
        self.repo_path = repo_path
        self.repo_name = repo_path.name
        self.commit = commit
        self.dirty = dirty
        self.fingerprint = fingerprint
        self.file_count = file_count

    @property
    def cache_key(self) -> str:
        return self.commit if self.commit and not self.dirty else self.fingerprint


def _snapshot_metadata(repo_path: Path) -> _OnboardingSnapshot:
    if (repo_path / ".git").exists():
        commit, dirty, fingerprint, file_count = _git_snapshot(repo_path)
    else:
        fingerprint, file_count = _filesystem_snapshot(repo_path)
        commit = None
        dirty = False
    return _OnboardingSnapshot(repo_path, commit, dirty, fingerprint, file_count)


def _git_snapshot(repo_path: Path) -> tuple[str | None, bool, str, int]:
    commit_result = subprocess.run(
        ["git", "-C", str(repo_path), "rev-parse", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    commit = commit_result.stdout.strip() or None if commit_result.returncode == 0 else None
    status_result = subprocess.run(
        ["git", "-C", str(repo_path), "status", "--porcelain", "--untracked-files=all"],
        check=False,
        capture_output=True,
        text=True,
    )
    status_lines = status_result.stdout.splitlines() if status_result.returncode == 0 else []
    dirty = bool(status_lines)
    files_result = subprocess.run(
        ["git", "-C", str(repo_path), "ls-files", "--cached", "--others", "--exclude-standard"],
        check=False,
        capture_output=True,
        text=True,
    )
    file_count = len([line for line in files_result.stdout.splitlines() if line]) if files_result.returncode == 0 else 0
    digest = hashlib.sha256()
    digest.update((commit or "no-commit").encode("utf-8"))
    for line in status_lines:
        digest.update(line.encode("utf-8"))
    if not status_lines:
        digest.update(b"clean")
    return commit, dirty, digest.hexdigest(), file_count


def _filesystem_snapshot(repo_path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    file_count = 0
    for path in sorted(repo_path.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(repo_path).as_posix()
        digest.update(relative.encode("utf-8"))
        stat_result = path.stat()
        digest.update(str(stat_result.st_size).encode("utf-8"))
        digest.update(str(stat_result.st_mtime_ns).encode("utf-8"))
        file_count += 1
    return digest.hexdigest(), file_count
