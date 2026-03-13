"""Navigation-CTF artifact generation and scoring.

Builds a directed graph navigation challenge from real graph relations,
generates prompts/schemas/reference outputs, and scores collected results.
Actual eval execution is done externally by launching agent sessions via
Chau7 MCP or equivalent.
"""

from __future__ import annotations

import shlex
from pathlib import Path
from typing import Any

from ..indexing.engine import build_task_pack, graph_expand, inspect_repository
from .control_prompt import build_baseline_prompt, build_leverage_prompt
from .models import EvaluationSide
from .report import EvaluationReport, estimate_report, write_navigation_ctf_markdown_report
from .runner import CommandEvaluationRunner, EVAL_TOOL_PYTHON, EvaluationRunner, PROJECT_ROOT
from .schemas import navigation_ctf_output_schema, navigation_ctf_scoring_rubric
from .scoring import parse_structured_output, score_navigation_ctf_output

DEFAULT_TASK = "Find the managing config, owned area, and code entrypoint for the main runtime path."


def run_navigation_ctf_evaluation(
    repo_path: Path,
    task: str = DEFAULT_TASK,
    control_runner: EvaluationRunner | None = None,
    explore_runner: EvaluationRunner | None = None,
    leverage_runner: EvaluationRunner | None = None,
    # Backward-compat aliases
    baseline_runner: EvaluationRunner | None = None,
    aethyme_runner: EvaluationRunner | None = None,
) -> dict[str, Any]:
    # Reconcile legacy parameter names
    if control_runner is None and baseline_runner is not None:
        control_runner = baseline_runner
    if leverage_runner is None and aethyme_runner is not None:
        leverage_runner = aethyme_runner

    inspect = inspect_repository(repo_path)

    # Hard separation: task_spec is agent-safe, reference is scoring-only.
    task_spec, reference = _build_navigation_case(inspect)

    task_pack = build_task_pack(repo_path, task_spec["task"])
    output_schema = navigation_ctf_output_schema()
    anchors_view = {
        "task": task_spec["task"],
        "anchors": task_pack.get("anchors", []),
    }
    in_scope = task_pack.get("in_scope", {})
    out_of_scope = task_pack.get("out_of_scope", {})
    scope_view = {
        "task": task_spec["task"],
        "navigation_order": task_pack.get("navigation_order", []),
        "in_scope_files": [item["value"] for item in in_scope.get("files", [])],
        "in_scope_symbols": [item["value"] for item in in_scope.get("symbols", [])],
        "in_scope_areas": [item["value"] for item in in_scope.get("areas", [])],
        "out_of_scope": [item["value"] for item in out_of_scope.get("areas", [])],
        "risks": [risk["scope"] for risk in task_pack.get("risk_flags", [])],
    }
    navigation_context = _build_navigation_context(repo_path, task_spec, anchors_view, scope_view)

    # --- Build prompts (agent-facing — no reference data) ---
    # Control and Explore get identical vanilla prompts.
    # Explore's advantage comes solely from the Aethyme skill being
    # auto-loaded in its runtime environment (Playground Aethyme).
    control_prompt = build_baseline_prompt(repo_path, task_spec["task"])
    explore_prompt = build_baseline_prompt(repo_path, task_spec["task"])
    leverage_prompt = build_leverage_prompt(repo_path, task_spec["task"])

    # --- Execute conditions via command backends (if provided) ---
    control_run = (
        control_runner.run(
            label="control",
            prompt=control_prompt,
            repo_path=repo_path,
            task=task_spec["task"],
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
            task=task_spec["task"],
            navigation_context=None,
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
            task=task_spec["task"],
            navigation_context=navigation_context,
            output_schema=output_schema,
        )
        if leverage_runner
        else None
    )

    control_candidate = parse_structured_output(control_run.stdout) if control_run else None
    explore_candidate = parse_structured_output(explore_run.stdout) if explore_run else None
    leverage_candidate = parse_structured_output(leverage_run.stdout) if leverage_run else None

    prompts = {"control": control_prompt, "explore": explore_prompt, "leverage": leverage_prompt}
    runs = {"control": control_run, "explore": explore_run, "leverage": leverage_run}
    pack_data = {"anchors": anchors_view, "scope": scope_view, "task_pack": task_pack}

    report: EvaluationReport = estimate_report(
        task_spec["task"],
        prompts=prompts,
        runs=runs,
        pack=pack_data,
    )

    # --- Scoring (reference used here only) ---
    control_assessment = score_navigation_ctf_output(control_candidate, reference, repo_path=str(repo_path))
    explore_assessment = score_navigation_ctf_output(explore_candidate, reference, repo_path=str(repo_path))
    leverage_assessment = score_navigation_ctf_output(leverage_candidate, reference, repo_path=str(repo_path))

    # Reconstruct combined challenge for archival (result storage, not agent-facing).
    challenge = {**task_spec, "reference_output": reference}

    result: dict[str, Any] = {
        "task": task_spec["task"],
        "signals": inspect.get("signals"),
        "task_spec": task_spec,
        "reference": reference,
        "challenge": challenge,
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
        "pack": pack_data,
        "navigation_context": navigation_context,
        "output_schema": output_schema,
        "scoring_rubric": navigation_ctf_scoring_rubric(),
        "reference_output": reference,
        "aethyme_structured_output": reference,
        "baseline_run": control_run.to_dict() if control_run else None,
        "aethyme_run": leverage_run.to_dict() if leverage_run else None,
        "baseline_assessment": control_assessment,
        "aethyme_assessment": leverage_assessment,
        "report": report.to_dict(),
    }
    report_path = write_navigation_ctf_markdown_report(repo_path=repo_path, result=result)
    result["report_path"] = str(report_path)
    return result


def command_runner(command: str, working_directory: Path | None = None) -> CommandEvaluationRunner:
    return CommandEvaluationRunner(command=command, working_directory=working_directory)


def _build_navigation_case(
    inspect: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Build the navigation challenge from graph inspection data.

    Returns a ``(task_spec, reference)`` tuple with a hard separation
    boundary:

    * **task_spec** — agent-visible metadata: ``kind``, ``task`` (the
      prompt text), and ``management_area`` (already mentioned in the
      task text, safe to share).  This is the *only* dict that may flow
      into prompts, navigation contexts, or CLI commands.
    * **reference** — the scoring-only answer key.  Must *never* be
      included in any agent-facing artifact.
    """
    files_by_id = {item["id"]: item["path"] for item in inspect.get("files", []) if "id" in item}
    areas_by_id = {item["id"]: item["name"] for item in inspect.get("areas", []) if "id" in item}

    for config in inspect.get("configs", []):
        config_id = config["id"]
        area_name = areas_by_id.get(config.get("area_id", ""))
        code_target: str | None = None
        chain: list[dict[str, str]] = []

        # Collect entrypoint_for edges to files, preferring high-confidence (direct) over low (transitive).
        ep_edges = [
            e for e in inspect.get("edges", [])
            if e.get("from") == config_id and e.get("kind") == "entrypoint_for" and e.get("to") in files_by_id
        ]
        ep_edges.sort(key=lambda e: -e.get("confidence", 0))

        if ep_edges:
            best = ep_edges[0]
            code_target = files_by_id[best["to"]]
            chain.append({"from": config["path"], "to": code_target, "relation": "entrypoint_for"})

        for edge in inspect.get("edges", []):
            if edge.get("from") != config_id:
                continue
            if edge.get("kind") == "configures" and edge.get("to") in areas_by_id:
                area_name = areas_by_id[edge["to"]]
                chain.append({"from": config["path"], "to": area_name, "relation": "configures"})

        if code_target and area_name:
            task_spec = {
                "kind": "navigation_ctf",
                "task": (
                    f"Find the manifest that manages the main code entrypoint in the {area_name} area, "
                    "identify the entrypoint file it controls, and name the top-level area that owns both."
                ),
                "management_area": area_name,
            }
            reference = {
                "config_target": {
                    "path": config["path"],
                    "why": "manifest/config linked to the runtime entrypoint",
                },
                "code_target": {
                    "path": code_target,
                    "why": "entrypoint file linked by the configuration graph",
                },
                "management_area": {
                    "name": area_name,
                    "why": "top-level area linked by the configuration graph",
                },
                "relationship_chain": chain,
                "rejected_candidates": [],
                "confidence": "high",
            }
            return task_spec, reference

    doc_path = next((doc["path"] for doc in inspect.get("docs", []) if doc.get("doc_type") == "readme"), "README.md")
    file_path = next((item["path"] for item in inspect.get("files", []) if item.get("language")), "")
    area_name = inspect.get("snapshot", {}).get("top_level_dirs", ["root"])[0]
    task_spec = {
        "kind": "navigation_ctf",
        "task": (
            f"Find the main documentation file, a representative code file, and the top-level area "
            f"that owns the runtime path for {area_name}."
        ),
        "management_area": area_name,
    }
    reference = {
        "config_target": {"path": doc_path, "why": "fallback documentation target"},
        "code_target": {"path": file_path, "why": "fallback representative code target"},
        "management_area": {"name": area_name, "why": "fallback top-level area"},
        "relationship_chain": [],
        "rejected_candidates": [],
        "confidence": "medium",
    }
    return task_spec, reference


def _build_cli_commands(
    repo_path: Path,
    task_spec: dict[str, Any],
    anchors_view: dict[str, Any],
) -> list[str]:
    """Build the list of CLI commands for graph exploration (shared by explore + leverage).

    Takes *task_spec* (agent-safe) — never the reference answer.
    """
    cd_prefix = f"cd {shlex.quote(str(PROJECT_ROOT))} &&"
    cli_command = f"{shlex.quote(str(EVAL_TOOL_PYTHON))} -m src.cli"
    quoted_repo = shlex.quote(str(repo_path))
    commands = [
        f"{cd_prefix} {cli_command} task anchors --repo {quoted_repo} --task <task> --json-output",
        f"{cd_prefix} {cli_command} task scope --repo {quoted_repo} --task <task> --json-output",
    ]
    management_area = task_spec["management_area"]
    commands.append(f"{cd_prefix} {cli_command} graph configs {quoted_repo} {shlex.quote(management_area)} --json-output")
    if anchors_view.get("anchors"):
        commands.append(f"{cd_prefix} {cli_command} graph expand {quoted_repo} <anchor-id> --json-output")
    return commands


def _build_navigation_context(
    repo_path: Path,
    task_spec: dict[str, Any],
    anchors_view: dict[str, Any],
    scope_view: dict[str, Any],
) -> dict[str, Any]:
    """Build the navigation context given to the leverage condition agent.

    Takes *task_spec* (agent-safe) — the reference answer is never
    included.  Any future additions to this dict must go through the
    same gate: if it comes from scoring data, it does not belong here.
    """
    commands = _build_cli_commands(repo_path, task_spec, anchors_view)

    # Pre-compute expansions for top anchors so the leverage agent can
    # compare graph neighborhoods without additional tool calls.
    anchor_expansions: dict[str, Any] = {}
    for anchor in anchors_view.get("anchors", [])[:3]:
        anchor_id = anchor.get("id", "")
        if anchor_id:
            try:
                anchor_expansions[anchor_id] = graph_expand(repo_path, anchor_id)
            except Exception:
                pass

    # Derive the primary top-level area from the scope.  The task pack
    # marks this explicitly; surfacing it here prevents the agent from
    # falling back to "nearest containing area" reasoning.
    in_scope_areas = scope_view.get("in_scope_areas", [])
    primary_area = in_scope_areas[0] if in_scope_areas else task_spec.get("management_area")

    return {
        "mode": "iterative_navigation",
        "repo_path": str(repo_path),
        "tool_repo_path": str(PROJECT_ROOT),
        "tool_python": str(EVAL_TOOL_PYTHON),
        "task": task_spec["task"],
        "primary_area": primary_area,
        "anchors": anchors_view,
        "anchor_expansions": anchor_expansions,
        "scope": scope_view,
        "commands": commands,
    }
