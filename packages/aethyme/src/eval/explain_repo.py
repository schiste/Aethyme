"""Local explain-repo evaluation flow."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..indexing.engine import build_task_pack, explain_task
from .control_prompt import build_aethyme_prompt, build_baseline_prompt
from .report import EvaluationReport, estimate_report
from .runner import CommandEvaluationRunner, EvaluationRunner

DEFAULT_TASK = "Explain this repo"


def run_explain_repo_evaluation(
    repo_path: Path,
    task: str = DEFAULT_TASK,
    baseline_runner: EvaluationRunner | None = None,
    aethyme_runner: EvaluationRunner | None = None,
) -> dict[str, Any]:
    """Build baseline and Aethyme artifacts and optionally execute live runners."""
    pack = build_task_pack(repo_path, task)
    baseline_prompt = build_baseline_prompt(repo_path, task)
    aethyme_prompt = build_aethyme_prompt(repo_path, task, pack)
    explanation = explain_task(repo_path, task)

    baseline_run = baseline_runner.run(label="baseline", prompt=baseline_prompt, repo_path=repo_path, task=task) if baseline_runner else None
    aethyme_run = aethyme_runner.run(label="aethyme", prompt=aethyme_prompt, repo_path=repo_path, task=task) if aethyme_runner else None

    report: EvaluationReport = estimate_report(
        task,
        baseline_prompt,
        aethyme_prompt,
        pack,
        baseline_run=baseline_run,
        aethyme_run=aethyme_run,
    )
    return {
        "task": task,
        "baseline_prompt": baseline_prompt,
        "aethyme_prompt": aethyme_prompt,
        "pack": pack,
        "explanation": explanation,
        "baseline_run": baseline_run.to_dict() if baseline_run else None,
        "aethyme_run": aethyme_run.to_dict() if aethyme_run else None,
        "report": report.to_dict(),
    }


def command_runner(command: str, working_directory: Path | None = None) -> CommandEvaluationRunner:
    """Return a command-backed evaluation runner."""
    return CommandEvaluationRunner(command=command, working_directory=working_directory)
