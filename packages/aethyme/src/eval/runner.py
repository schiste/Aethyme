"""Evaluation runner contracts for local Aethyme benchmarks."""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from shutil import which
from typing import Any, Protocol

PROJECT_ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class EvaluationRunResult:
    label: str
    command: str
    exit_code: int
    duration_seconds: float
    stdout: str
    stderr: str
    input_tokens: int | None
    output_tokens: int | None
    retries: int | None
    review_burden: int | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "label": self.label,
            "command": self.command,
            "exit_code": self.exit_code,
            "duration_seconds": self.duration_seconds,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "retries": self.retries,
            "review_burden": self.review_burden,
        }


class EvaluationRunner(Protocol):
    """Protocol for executable evaluation runners."""

    def run(self, *, label: str, prompt: str, repo_path: Path, task: str) -> EvaluationRunResult:
        """Execute a prompt against an evaluation backend."""


@dataclass(frozen=True)
class CommandEvaluationRunner:
    """Run a real evaluation command against a prompt file and env contract."""

    command: str
    working_directory: Path | None = None

    def run(self, *, label: str, prompt: str, repo_path: Path, task: str) -> EvaluationRunResult:
        with tempfile.TemporaryDirectory(prefix="aethyme-eval-") as temp_dir:
            prompt_file = Path(temp_dir) / "prompt.txt"
            prompt_file.write_text(prompt, encoding="utf-8")

            env = os.environ.copy()
            env["AETHYME_EVAL_PROMPT_FILE"] = str(prompt_file)
            env["AETHYME_EVAL_PROMPT"] = prompt
            env["AETHYME_EVAL_REPO"] = str(repo_path)
            env["AETHYME_EVAL_TASK"] = task
            env["AETHYME_EVAL_LABEL"] = label

            resolved_command = _resolve_command(self.command)
            start = time.perf_counter()
            result = subprocess.run(
                resolved_command,
                check=False,
                capture_output=True,
                text=True,
                cwd=self.working_directory or repo_path,
                env=env,
            )
            duration = time.perf_counter() - start

        stdout = result.stdout.strip()
        stderr = result.stderr.strip()
        metrics = _parse_metrics(stdout)
        return EvaluationRunResult(
            label=label,
            command=" ".join(resolved_command),
            exit_code=result.returncode,
            duration_seconds=duration,
            stdout=stdout,
            stderr=stderr,
            input_tokens=metrics.get("input_tokens"),
            output_tokens=metrics.get("output_tokens"),
            retries=metrics.get("retries"),
            review_burden=metrics.get("review_burden"),
        )


def _parse_metrics(stdout: str) -> dict[str, int | None]:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return {
            "input_tokens": None,
            "output_tokens": None,
            "retries": None,
            "review_burden": None,
        }
    if not isinstance(payload, dict):
        return {
            "input_tokens": None,
            "output_tokens": None,
            "retries": None,
            "review_burden": None,
        }
    return {
        "input_tokens": _as_int(payload.get("input_tokens")),
        "output_tokens": _as_int(payload.get("output_tokens")),
        "retries": _as_int(payload.get("retries")),
        "review_burden": _as_int(payload.get("review_burden")),
    }


def _as_int(value: object) -> int | None:
    if isinstance(value, int):
        return value
    return None


def _resolve_command(command: str) -> list[str]:
    args = shlex.split(command)
    if not args:
        raise ValueError("Evaluation command must not be empty")
    executable = args[0]
    if os.path.isabs(executable):
        return args
    if "/" in executable or executable.startswith("."):
        cwd_candidate = (Path.cwd() / executable).resolve(strict=False)
        if cwd_candidate.exists():
            return [str(cwd_candidate), *args[1:]]
        project_candidate = (PROJECT_ROOT / executable).resolve(strict=False)
        return [str(project_candidate), *args[1:]]
    if which(executable):
        return args
    return args
