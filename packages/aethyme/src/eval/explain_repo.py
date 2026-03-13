"""Explain-repo artifact generation and scoring.

Generates prompts, schemas, reference outputs, and navigation context for
the explain-repo benchmark.  Also scores collected agent results.  Actual
eval execution is done externally by launching agent sessions via Chau7 MCP
or equivalent.
"""

from __future__ import annotations

import shlex
from pathlib import Path
from typing import Any

from ..indexing.engine import (
    build_task_pack,
)
from ..rendering.context_pack import render_explain_repo_text
from .control_prompt import build_baseline_prompt, build_leverage_prompt
from .models import EvaluationSide
from .report import EvaluationReport, estimate_report, write_explain_repo_markdown_report
from .runner import CommandEvaluationRunner, EVAL_TOOL_PYTHON, EvaluationRunner, PROJECT_ROOT
from .schemas import explain_repo_output_schema, explain_repo_scoring_rubric
from .scoring import parse_structured_output, score_explain_repo_output

DEFAULT_TASK = "Explain this repo"


def run_explain_repo_evaluation(
    repo_path: Path,
    task: str = DEFAULT_TASK,
    control_runner: EvaluationRunner | None = None,
    explore_runner: EvaluationRunner | None = None,
    leverage_runner: EvaluationRunner | None = None,
    # Backward-compat aliases
    baseline_runner: EvaluationRunner | None = None,
    aethyme_runner: EvaluationRunner | None = None,
) -> dict[str, Any]:
    """Build control/explore/leverage artifacts; optionally execute command backends."""
    # Reconcile legacy parameter names
    if control_runner is None and baseline_runner is not None:
        control_runner = baseline_runner
    if leverage_runner is None and aethyme_runner is not None:
        leverage_runner = aethyme_runner

    pack = build_task_pack(repo_path, task)
    summary = pack.get("summary", {})
    signals = pack.get("signals")
    navigation_context = _build_explain_repo_navigation_context(repo_path, task, pack)
    output_schema = explain_repo_output_schema()

    # --- Build prompts ---
    # Control and Explore get identical vanilla prompts.
    # Explore's advantage comes from the Aethyme skill auto-loaded in its env.
    control_prompt = build_baseline_prompt(repo_path, task)
    explore_prompt = build_baseline_prompt(repo_path, task)
    leverage_prompt = build_leverage_prompt(repo_path, task)
    explanation = render_explain_repo_text(summary, pack)

    # --- Execute conditions via command backends (if provided) ---
    control_run = (
        control_runner.run(
            label="control",
            prompt=control_prompt,
            repo_path=repo_path,
            task=task,
            output_schema=output_schema,
        )
        if control_runner
        else None
    )
    explore_run = (
        explore_runner.run(
            label="explore",
            prompt=explore_prompt,
            repo_path=repo_path,
            task=task,
            navigation_context=None,  # No pre-computed context for explore
            output_schema=output_schema,
        )
        if explore_runner
        else None
    )
    leverage_run = (
        leverage_runner.run(
            label="leverage",
            prompt=leverage_prompt,
            repo_path=repo_path,
            task=task,
            navigation_context=navigation_context,
            output_schema=output_schema,
        )
        if leverage_runner
        else None
    )

    reference_output = _build_explain_repo_reference(summary, pack, explanation)
    control_candidate = parse_structured_output(control_run.stdout) if control_run else None
    explore_candidate = parse_structured_output(explore_run.stdout) if explore_run else None
    leverage_candidate = parse_structured_output(leverage_run.stdout) if leverage_run else None

    prompts = {"control": control_prompt, "explore": explore_prompt, "leverage": leverage_prompt}
    runs = {"control": control_run, "explore": explore_run, "leverage": leverage_run}

    report: EvaluationReport = estimate_report(
        task,
        prompts=prompts,
        runs=runs,
        pack=pack,
    )

    control_assessment = score_explain_repo_output(control_candidate, reference_output, repo_path=str(repo_path))
    explore_assessment = score_explain_repo_output(explore_candidate, reference_output, repo_path=str(repo_path))
    leverage_assessment = score_explain_repo_output(leverage_candidate, reference_output, repo_path=str(repo_path))

    result: dict[str, Any] = {
        "task": task,
        "signals": signals,
        "control": EvaluationSide(
            prompt=control_prompt,
            run=control_run.to_dict() if control_run else None,
            assessment=control_assessment,
        ).to_dict(),
        "explore": EvaluationSide(
            prompt=explore_prompt,
            run=explore_run.to_dict() if explore_run else None,
            assessment=explore_assessment,
        ).to_dict(),
        "leverage": EvaluationSide(
            prompt=leverage_prompt,
            run=leverage_run.to_dict() if leverage_run else None,
            assessment=leverage_assessment,
        ).to_dict(),
        # Legacy keys for backward compat
        "aethyme": EvaluationSide(
            prompt=leverage_prompt,
            run=leverage_run.to_dict() if leverage_run else None,
            assessment=leverage_assessment,
        ).to_dict(),
        "baseline_prompt": control_prompt,
        "aethyme_prompt": leverage_prompt,
        "pack": pack,
        "navigation_context": navigation_context,
        "explanation": explanation,
        "output_schema": output_schema,
        "scoring_rubric": explain_repo_scoring_rubric(),
        "reference_output": reference_output,
        "aethyme_structured_output": reference_output,
        "baseline_run": control_run.to_dict() if control_run else None,
        "aethyme_run": leverage_run.to_dict() if leverage_run else None,
        "baseline_assessment": control_assessment,
        "aethyme_assessment": leverage_assessment,
        "report": report.to_dict(),
    }
    report_path = write_explain_repo_markdown_report(repo_path=repo_path, result=result)
    result["report_path"] = str(report_path)
    return result


def command_runner(command: str, working_directory: Path | None = None) -> CommandEvaluationRunner:
    """Return a command-backed evaluation runner."""
    return CommandEvaluationRunner(command=command, working_directory=working_directory)


def _build_explain_repo_reference(
    summary: dict[str, Any],
    pack: dict[str, Any],
    explanation: str,
) -> dict[str, Any]:
    overview = pack.get("overview", {})
    code_areas = overview.get("code_areas", [])[:3]
    reference_areas = overview.get("reference_areas", [])[:3]
    key_configs = overview.get("key_configs", [])[:3]

    navigation_order = ((
        overview.get("overview_docs", [])[:2]
        + overview.get("code_areas", [])[:2]
        + overview.get("reference_areas", [])[:1]
        + overview.get("entrypoints", [])[:1]
    )[:6] or pack.get("navigation_order", [])[:6])
    navigation_order = list(dict.fromkeys(navigation_order))[:5]

    return {
        "repo_summary": explanation.splitlines()[0] if explanation else "Explain this repo",
        "code_areas": code_areas,
        "reference_areas": reference_areas,
        "entrypoints": overview.get("entrypoints", [])[:3],
        "important_docs": overview.get("overview_docs", [])[:3],
        "key_configs": key_configs,
        "key_languages": summary.get("snapshot", {}).get("languages", []),
        "high_risk_areas": [risk["scope"] for risk in pack.get("risk_flags", [])[:3]],
        "navigation_order": navigation_order,
        "representative_code_files": overview.get("representative_code_files", [])[:3],
        "representative_docs": overview.get("representative_docs", [])[:3],
        "evidence": ((
            overview.get("representative_code_files", [])[:2]
            + overview.get("overview_docs", [])[:1]
        )[:3] or [snippet["file"] for snippet in pack.get("snippets", [])[:3]]),
    }


def _build_cli_commands(
    repo_path: Path,
    pack: dict[str, Any],
) -> list[str]:
    """Build the list of CLI commands available for graph exploration.

    Shared by both explore and leverage conditions.
    """
    cd_prefix = f"cd {shlex.quote(str(PROJECT_ROOT))} &&"
    cli_command = f"{shlex.quote(str(EVAL_TOOL_PYTHON))} -m src.cli"
    quoted_repo = shlex.quote(str(repo_path))
    commands = [
        f"{cd_prefix} {cli_command} repo inspect {quoted_repo} --json-output",
        f"{cd_prefix} {cli_command} graph overview {quoted_repo} --json-output",
    ]
    if pack.get("anchors"):
        commands.append(f"{cd_prefix} {cli_command} graph expand {quoted_repo} <anchor-id> --json-output")
    return commands


def _build_explain_repo_navigation_context(
    repo_path: Path,
    task: str,
    pack: dict[str, Any],
) -> dict[str, Any]:
    commands = _build_cli_commands(repo_path, pack)
    anchors_view = {
        "task": task,
        "anchors": pack.get("anchors", []),
    }
    in_scope = pack.get("in_scope", {})
    out_of_scope = pack.get("out_of_scope", {})
    scope_view = {
        "task": task,
        "navigation_order": pack.get("navigation_order", []),
        "in_scope_files": [item["value"] for item in in_scope.get("files", [])],
        "in_scope_symbols": [item["value"] for item in in_scope.get("symbols", [])],
        "in_scope_areas": [item["value"] for item in in_scope.get("areas", [])],
        "out_of_scope": [item["value"] for item in out_of_scope.get("areas", [])],
        "risks": [risk["scope"] for risk in pack.get("risk_flags", [])],
    }
    return {
        "mode": "iterative_navigation",
        "repo_path": str(repo_path),
        "tool_repo_path": str(PROJECT_ROOT),
        "tool_python": str(EVAL_TOOL_PYTHON),
        "task": task,
        "anchors": anchors_view,
        "scope": scope_view,
        "commands": commands,
    }
