"""Eval orchestrator — generates deterministic run plans for Chau7 MCP.

``generate_run_plan()`` is a **pure function**: no side effects, no Chau7 calls,
no file I/O.  It returns a structured dict with 8 phases that the orchestrating
agent (Claude) reads and runs mechanically via Chau7 MCP tools.

Usage::

    plan = generate_run_plan(eval_type="bug-fix", target="grc", model="haiku")
    # Claude reads plan["phases"] and runs each step via MCP
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .report import CONDITION_ORDER, get_aethyme_commit
from .runner import PROJECT_ROOT
from .targets import EvalTarget, get_target


# ---------------------------------------------------------------------------
# Model configuration
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ModelConfig:
    """Backend configuration for an agent session."""

    name: str
    provider: str  # "anthropic" | "openai"
    backend: str  # "claude" | "codex"
    backend_args: tuple[str, ...]
    reasoning: str = "default"

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "provider": self.provider,
            "backend": self.backend,
            "reasoning": self.reasoning,
        }


MODELS: dict[str, ModelConfig] = {
    "haiku": ModelConfig(
        "haiku", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
    ),
    "sonnet": ModelConfig(
        "sonnet", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
    ),
    "opus": ModelConfig(
        "opus", "anthropic", "claude",
        ("--dangerously-skip-permissions",),
    ),
    "gpt-5.4": ModelConfig(
        "gpt-5.4", "openai", "codex",
        ("--sandbox", "workspace-write"),
    ),
}


def get_model(name: str, reasoning: str = "default") -> ModelConfig:
    """Look up a model config by name.  Raises KeyError if unknown."""
    key = name.lower().strip()
    if key not in MODELS:
        available = ", ".join(sorted(MODELS))
        raise KeyError(f"Unknown model {name!r}. Available: {available}")
    cfg = MODELS[key]
    if reasoning != "default" and reasoning != cfg.reasoning:
        return ModelConfig(
            cfg.name, cfg.provider, cfg.backend, cfg.backend_args, reasoning,
        )
    return cfg


# ---------------------------------------------------------------------------
# Condition specification
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ConditionSpec:
    """Configuration for one eval condition."""

    name: str
    cto_override: str | None  # "forceOff" or None (default CTO)
    uses_aethyme: bool
    repo_selector: str  # "control" | "aethyme"
    prompt_variant: str  # "baseline" | "leverage"


CONDITIONS: tuple[ConditionSpec, ...] = (
    ConditionSpec("control-cto-off", "forceOff", False, "control", "baseline"),
    ConditionSpec("control-cto-on", None, False, "control", "baseline"),
    ConditionSpec("explore", None, True, "aethyme", "baseline"),
    ConditionSpec("leverage", None, True, "aethyme", "leverage"),
)


# ---------------------------------------------------------------------------
# Eval-type defaults
# ---------------------------------------------------------------------------

_EVAL_TYPE_DEFAULTS: dict[str, dict[str, str]] = {
    "bug-fix": {
        "task": (
            "Fix failing test: manage permission does not imply share "
            "in ability-implications.test.ts"
        ),
        "prepare_function": "src.eval.bug_fix.prepare_bug_fix_benchmark",
        "score_function": "src.eval.bug_fix.assemble_bug_fix_result",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "bug-fix-1": {
        "task": (
            "Bug report (T419918): Viewing a diff/revision on a watchlisted "
            "page marks all revisions as 'seen' instead of only the one viewed. "
            "Identify which files need editing and explain how you would fix "
            "this bug. Do NOT apply the fix — only report your analysis."
        ),
        "prepare_function": "src.eval.schemas.mediawiki_bug_fix_1_reference",
        "score_function": "src.eval.scoring.score_mediawiki_bug_fix_1",
        "report_function": "src.eval.report.finalize_eval_run",
        "target_restriction": "mediawiki",
    },
    "explain-repo": {
        "task": "Explain this repo",
        "prepare_function": "src.eval.explain_repo.run_explain_repo_evaluation",
        "score_function": "src.eval.scoring.score_explain_repo_output",
        "report_function": "src.eval.report.finalize_eval_run",
    },
    "navigation-ctf": {
        "task": "Find the manifest that manages the main code entrypoint",
        "prepare_function": "src.eval.navigation_ctf.run_navigation_ctf_evaluation",
        "score_function": "src.eval.scoring.score_navigation_ctf_output",
        "report_function": "src.eval.report.finalize_eval_run",
    },
}

_AETHYME_PKG = str(PROJECT_ROOT)
_AETHYME_VENV_PYTHON = str(PROJECT_ROOT / ".venv" / "bin" / "python")


# ---------------------------------------------------------------------------
# Core plan generator
# ---------------------------------------------------------------------------


def generate_run_plan(
    *,
    eval_type: str,
    target: str,
    model: str,
    scenario: str | None = None,
    reasoning: str = "default",
    dest_dir: str | None = None,
) -> dict[str, Any]:
    """Generate a complete eval run plan.

    PURE FUNCTION — no side effects, no Chau7 calls, no file writes.
    Returns a structured dict that the orchestrating agent (Claude)
    runs mechanically via Chau7 MCP.
    """
    if eval_type not in _EVAL_TYPE_DEFAULTS:
        raise ValueError(
            f"Unknown eval_type {eval_type!r}. "
            f"Available: {', '.join(sorted(_EVAL_TYPE_DEFAULTS))}"
        )

    eval_target = get_target(target)
    model_config = get_model(model, reasoning)

    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S")
    slug = f"{eval_target.name}-{eval_type}"
    if scenario:
        slug = f"{eval_target.name}-{scenario}-{eval_type}"

    # Default dest_dir for bug-fix clones
    if eval_type == "bug-fix" and dest_dir is None:
        dest_dir = str(
            Path("/tmp").resolve() / f"benchmark-{eval_target.name}-{timestamp}"
        )

    run_dir_name = f"{timestamp}-{slug}"
    paths = _build_paths(eval_type, dest_dir, run_dir_name)

    meta = {
        "eval_type": eval_type,
        "target": eval_target.name,
        "target_display": eval_target.display_name,
        "scenario": scenario,
        "model": model_config.to_dict(),
        "aethyme_commit": get_aethyme_commit(),
        "aethyme_root": _AETHYME_PKG,
        "timestamp": datetime.now(UTC).isoformat(),
        "conditions": [c.name for c in CONDITIONS],
    }

    phases = [
        _build_validate_phase(eval_target),
        _build_prepare_phase(eval_type, eval_target, scenario, dest_dir, paths),
        _build_launch_phase(eval_type, eval_target, model_config, dest_dir, paths),
        _build_monitor_phase(),
        _build_collect_phase(model_config, paths),
        _build_score_phase(eval_type, scenario, model_config),
        _build_report_phase(eval_type, paths),
        _build_cleanup_phase(),
    ]

    return {"meta": meta, "paths": paths, "phases": phases}


# ---------------------------------------------------------------------------
# Path computation
# ---------------------------------------------------------------------------


def _build_paths(
    eval_type: str,
    dest_dir: str | None,
    run_dir_name: str,
) -> dict[str, Any]:
    """Compute all artifact paths up front."""
    tmp = str(Path("/tmp").resolve())

    prompt_files = {
        c.name: f"{tmp}/aethyme-eval-{c.name}-prompt.txt" for c in CONDITIONS
    }
    result_files = {
        c.name: f"{tmp}/aethyme-eval-{c.name}-result.json" for c in CONDITIONS
    }

    paths: dict[str, Any] = {
        "aethyme_root": _AETHYME_PKG,
        "venv_python": _AETHYME_VENV_PYTHON,
        "run_dir": f"eval-runs/{run_dir_name}",
        "prompt_files": prompt_files,
        "result_files": result_files,
        "schema_file": f"{tmp}/aethyme-eval-output-schema.json",
        "reference_file": f"{tmp}/aethyme-eval-reference.json",
        "rubric_file": f"{tmp}/aethyme-eval-scoring-rubric.json",
        "nav_context_file": f"{tmp}/aethyme-eval-navigation-context.json",
    }

    if eval_type == "bug-fix" and dest_dir:
        paths["dest_dir"] = dest_dir
        paths["condition_repos"] = {
            c.name: f"{dest_dir}/{c.name}" for c in CONDITIONS
        }

    return paths


# ---------------------------------------------------------------------------
# Phase builders
# ---------------------------------------------------------------------------


def _build_validate_phase(target: EvalTarget) -> dict[str, Any]:
    checks = [
        {
            "check": "control_repo_exists",
            "path": str(target.control_path),
            "expected": True,
        },
        {
            "check": "control_is_git_repo",
            "path": str(target.control_path / ".git"),
            "expected": True,
        },
        {
            "check": "control_no_skill_contamination",
            "path": str(target.control_path / ".codex" / "skills"),
            "expected": False,
        },
        {
            "check": "aethyme_repo_exists",
            "path": str(target.aethyme_path),
            "expected": True,
        },
        {
            "check": "aethyme_is_git_repo",
            "path": str(target.aethyme_path / ".git"),
            "expected": True,
        },
        {
            "check": "aethyme_has_skill",
            "path": str(target.aethyme_path / ".codex" / "skills"),
            "expected": True,
        },
        {
            "check": "engine_binary_exists",
            "path": str(
                PROJECT_ROOT / "rust" / "target" / "release" / "aethyme-engine-cli"
            ),
            "expected": True,
        },
        {
            "check": "global_skill_absent",
            "path": str(
                Path.home() / ".codex" / "skills" / "aethyme-navigation"
            ),
            "expected": False,
        },
    ]
    return {
        "name": "validate",
        "description": "Check repos, tooling, and engine binary",
        "checks": checks,
    }


def _build_prepare_phase(
    eval_type: str,
    target: EvalTarget,
    scenario: str | None,
    dest_dir: str | None,
    paths: dict[str, Any],
) -> dict[str, Any]:
    """Build the artifact generation phase."""
    venv = paths["venv_python"]
    pkg = paths["aethyme_root"]

    if eval_type == "bug-fix":
        scenario_flag = f" --scenario {scenario}" if scenario else ""
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval bug-fix prepare"
            f" --source '{target.control_path}'"
            f" --dest '{dest_dir}'"
            f"{scenario_flag}"
            f" --json-output"
        )
        description = (
            f"Clone 4 repos from {target.display_name} control, "
            f"plant bug, generate all artifacts"
        )
    elif eval_type == "bug-fix-1":
        # Read-only diagnostic eval — no cloning, just write prompts + schema
        defaults = _EVAL_TYPE_DEFAULTS["bug-fix-1"]
        cli_cmd = (
            f"cd {pkg} && {venv} -c \""
            f"from pathlib import Path; import json; "
            f"from src.eval.schemas import mediawiki_bug_fix_1_output_schema, "
            f"mediawiki_bug_fix_1_reference, mediawiki_bug_fix_1_scoring_rubric; "
            f"ref = mediawiki_bug_fix_1_reference(); "
            f"schema = mediawiki_bug_fix_1_output_schema(); "
            f"rubric = mediawiki_bug_fix_1_scoring_rubric(); "
            f"task = {defaults['task']!r}; "
            f"ctrl = task + chr(10) + chr(10) + 'Repository path: {target.control_path}' + chr(10) + 'Explore the repository and produce a structured analysis.'; "
            f"lev = 'Use Aethyme tools to navigate the repository graph.' + chr(10) + task + chr(10) + chr(10) + 'Repository path: {target.aethyme_path}' + chr(10) + 'Explore the repository and produce a structured analysis.'; "
            f"Path('{paths['prompt_files']['control-cto-off']}').write_text(ctrl); "
            f"Path('{paths['prompt_files']['control-cto-on']}').write_text(ctrl); "
            f"Path('{paths['prompt_files']['explore']}').write_text(ctrl.replace(str('{target.control_path}'), str('{target.aethyme_path}'))); "
            f"Path('{paths['prompt_files']['leverage']}').write_text(lev); "
            f"Path('{paths['schema_file']}').write_text(json.dumps(schema, indent=2)); "
            f"Path('{paths['reference_file']}').write_text(json.dumps(ref, indent=2)); "
            f"Path('{paths['rubric_file']}').write_text(json.dumps(rubric, indent=2)); "
            f"print('bug-fix-1 artifacts written')"
            f"\""
        )
        description = f"Generate bug-fix-1 (T419918) artifacts for {target.display_name}"
    elif eval_type == "explain-repo":
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval explain-repo"
            f" --repo '{target.aethyme_path}'"
            f" --json-output"
        )
        description = f"Generate explain-repo artifacts for {target.display_name}"
    elif eval_type == "navigation-ctf":
        cli_cmd = (
            f"cd {pkg} && {venv} -m src.cli eval navigation-ctf"
            f" --repo '{target.aethyme_path}'"
            f" --json-output"
        )
        description = f"Generate navigation-ctf artifacts for {target.display_name}"
    else:
        raise ValueError(f"Unknown eval_type: {eval_type}")

    return {
        "name": "prepare",
        "description": description,
        "cli_cmd": cli_cmd,
        "writes_to": list(paths["prompt_files"].values()) + [
            paths["schema_file"],
            paths["reference_file"],
            paths["nav_context_file"],
        ],
    }


def _build_launch_phase(
    eval_type: str,
    target: EvalTarget,
    model_config: ModelConfig,
    dest_dir: str | None,
    paths: dict[str, Any],
) -> dict[str, Any]:
    """Build per-condition launch instructions."""
    conditions_launch: list[dict[str, Any]] = []

    for cond in CONDITIONS:
        directory = _resolve_condition_dir(eval_type, cond, target, dest_dir)
        prompt_file = paths["prompt_files"][cond.name]
        result_file = paths["result_files"][cond.name]
        schema_file = paths["schema_file"]

        entry: dict[str, Any] = {
            "name": cond.name,
            "directory": directory,
            "prompt_file": prompt_file,
            "result_file": result_file,
            "cto_override": cond.cto_override,
        }

        if model_config.backend == "claude":
            args = list(model_config.backend_args)
            entry["launch_method"] = "runtime_session_create"
            entry["session_create_params"] = {
                "backend": "claude",
                "model": model_config.name,
                "directory": directory,
                "auto_approve": True,
                "backend_args": args,
            }
            entry["prompt_source"] = "file"
            entry["note"] = (
                "Read prompt from prompt_file, pass as initial_prompt "
                "to runtime_session_create"
            )
        elif model_config.backend == "codex":
            codex_args = ["codex", "exec"] + list(model_config.backend_args) + [
                "--output-schema", schema_file,
                "--output-last-message", result_file,
            ]
            if model_config.reasoning != "default":
                codex_args += ["-c", f'reasoning_effort="{model_config.reasoning}"']

            entry["launch_method"] = "tab_create_then_exec"
            entry["tab_create_params"] = {"directory": directory}
            entry["tab_exec_args"] = codex_args
            entry["note"] = (
                "Create tab with tab_create(directory=...), "
                "then run codex via tab_exec. "
                "Prompt is read from prompt_file and appended to command."
            )

        conditions_launch.append(entry)

    return {
        "name": "launch",
        "description": (
            f"Launch 4 agent sessions "
            f"({model_config.backend}/{model_config.name})"
        ),
        "backend": model_config.backend,
        "model": model_config.name,
        "conditions": conditions_launch,
    }


def _build_monitor_phase() -> dict[str, Any]:
    return {
        "name": "monitor",
        "description": "Poll sessions until all complete",
        "poll_interval_seconds": 15,
        "timeout_seconds": 1800,
        "instructions": (
            "For claude backend: poll runtime_turn_status(session_id) — "
            "complete when status is 'ready' or session is stopped. "
            "For codex backend: poll tab_status(tab_id) — "
            "complete when is_at_prompt is true."
        ),
    }


def _build_collect_phase(
    model_config: ModelConfig,
    paths: dict[str, Any],
) -> dict[str, Any]:
    per_condition: list[dict[str, Any]] = []

    for cond in CONDITIONS:
        result_file = paths["result_files"][cond.name]

        collect_entry: dict[str, Any] = {
            "name": cond.name,
            "result_file": result_file,
            "chau7_calls": [
                {"tool": "tab_output", "params": {"lines": 5000}},
                {
                    "tool": (
                        "runtime_session_get"
                        if model_config.backend == "claude"
                        else "tab_status"
                    ),
                },
            ],
            "telemetry_calls": [
                {"tool": "run_get"},
                {"tool": "run_transcript"},
                {"tool": "run_tool_calls"},
            ],
        }

        if model_config.backend == "claude":
            collect_entry["result_extraction"] = (
                "Extract structured output from the agent's final message "
                "in the transcript or session JSONL"
            )
        else:
            collect_entry["result_extraction"] = (
                f"Read structured output from {result_file}"
            )

        per_condition.append(collect_entry)

    return {
        "name": "collect",
        "description": "Gather results, telemetry, and outputs per condition",
        "per_condition": per_condition,
        "storage": [
            "store_condition_raw(run_dir, condition, stdout, structured_output)",
            "store_condition_chau7(run_dir, condition, run_id, transcript, tool_calls)",
        ],
    }


def _build_score_phase(
    eval_type: str,
    scenario: str | None,
    model_config: ModelConfig,
) -> dict[str, Any]:
    defaults = _EVAL_TYPE_DEFAULTS[eval_type]

    return {
        "name": "score",
        "description": f"Score and assemble result via {defaults['score_function']}",
        "eval_type": eval_type,
        "scenario": scenario,
        "score_function": defaults["score_function"],
        "model_metadata": model_config.to_dict(),
        "instructions": (
            "For bug-fix: call assemble_bug_fix_result(conditions={...}, "
            f"task=..., repo_path=..., scenario={scenario!r}, model=...). "
            "For other types: score each condition individually, "
            "then build result dict with EvaluationSide per condition."
        ),
    }


def _build_report_phase(
    eval_type: str,
    paths: dict[str, Any],
) -> dict[str, Any]:
    return {
        "name": "report",
        "description": "Generate report via code pipeline (never hand-write)",
        "report_function": _EVAL_TYPE_DEFAULTS[eval_type]["report_function"],
        "run_dir": paths["run_dir"],
        "instructions": (
            "Call finalize_eval_run(run_dir, result, repo_path=..., eval_type=...) "
            "then print_scorecard(result) to display in chat. "
            "NEVER hand-write markdown reports."
        ),
    }


def _build_cleanup_phase() -> dict[str, Any]:
    return {
        "name": "cleanup",
        "description": "Close all agent sessions and tabs",
        "instructions": (
            "For claude backend: "
            "runtime_session_stop(session_id, close_tab=True) per condition. "
            "For codex backend: "
            "tab_close(tab_id, force=True) per condition. "
            "CRITICAL: Always close tabs — Chau7 has a limited tab pool."
        ),
    }


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _resolve_condition_dir(
    eval_type: str,
    condition: ConditionSpec,
    target: EvalTarget,
    dest_dir: str | None,
) -> str:
    """Return the working directory for a condition's agent session."""
    if eval_type == "bug-fix":
        if dest_dir is None:
            raise ValueError("dest_dir required for bug-fix evals")
        return str(Path(dest_dir) / condition.name)

    # Read-only evals: control → control repo, aethyme → aethyme repo
    if condition.repo_selector == "control":
        return str(target.control_path.resolve())
    return str(target.aethyme_path.resolve())
