"""Command execution contracts for local Aethyme benchmarks.

These helpers execute backend commands (e.g. ``codex exec``) and collect
structured results.  They do *not* orchestrate evaluations — eval sessions
are launched externally via Chau7 MCP or equivalent terminal multiplexer.
"""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from shutil import which
from typing import Any, Protocol

from src.contracts.run_metadata import build_run_metadata
from src.indexing.repository_snapshot import capture_snapshot

PROJECT_ROOT = Path(__file__).resolve().parents[2]
_VENV_PYTHON = PROJECT_ROOT / ".venv" / "bin" / "python"
# Prefer the project venv interpreter; fall back to the running interpreter
# when no venv exists (e.g. CI installs into the system environment). Local
# behavior is unchanged whenever .venv is present.
EVAL_TOOL_PYTHON = _VENV_PYTHON if _VENV_PYTHON.exists() else Path(sys.executable)


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
    final_output_message: str | None
    structured_output: dict[str, Any] | None
    tool_calls: list[dict[str, str]] | None = None
    run_metadata: dict[str, Any] | None = None

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
            "final_output_message": self.final_output_message,
            "structured_output": self.structured_output,
            "tool_calls": self.tool_calls,
            "run_metadata": self.run_metadata,
        }


class EvaluationRunner(Protocol):
    """Protocol for command execution backends."""

    def run(
        self,
        *,
        label: str,
        prompt: str,
        repo_path: Path,
        task: str,
        navigation_context: dict[str, Any] | None = None,
        output_schema: dict[str, Any] | None = None,
    ) -> EvaluationRunResult:
        """Execute a prompt against an evaluation backend."""


@dataclass(frozen=True)
class CommandEvaluationRunner:
    """Execute a command against a prompt file and env contract, collecting results."""

    command: str
    working_directory: Path | None = None

    def run(
        self,
        *,
        label: str,
        prompt: str,
        repo_path: Path,
        task: str,
        navigation_context: dict[str, Any] | None = None,
        output_schema: dict[str, Any] | None = None,
    ) -> EvaluationRunResult:
        snapshot = capture_snapshot(repo_path)
        run_id = uuid.uuid4().hex
        started_at = datetime.now(UTC).isoformat()
        with tempfile.TemporaryDirectory(prefix="aethyme-eval-") as temp_dir:
            prompt_file = Path(temp_dir) / "prompt.txt"
            prompt_file.write_text(prompt, encoding="utf-8")
            navigation_file = Path(temp_dir) / "navigation-context.json"
            output_schema_file = Path(temp_dir) / "output-schema.json"
            if navigation_context is not None:
                navigation_file.write_text(json.dumps(navigation_context, indent=2), encoding="utf-8")
            if output_schema is not None:
                output_schema_file.write_text(json.dumps(output_schema, indent=2), encoding="utf-8")

            env = os.environ.copy()
            env["AETHYME_EVAL_PROMPT_FILE"] = str(prompt_file)
            env["AETHYME_EVAL_PROMPT"] = prompt
            env["AETHYME_EVAL_REPO"] = str(repo_path)
            env["AETHYME_EVAL_TASK"] = task
            env["AETHYME_EVAL_LABEL"] = label
            env["AETHYME_EVAL_RUN_ID"] = run_id
            env["AETHYME_EVAL_TOOL_REPO"] = str(PROJECT_ROOT)
            env["AETHYME_EVAL_TOOL_PYTHON"] = str(EVAL_TOOL_PYTHON)
            if navigation_context is not None:
                env["AETHYME_EVAL_NAVIGATION_CONTEXT_FILE"] = str(navigation_file)
                env["AETHYME_EVAL_NAVIGATION_CONTEXT"] = json.dumps(navigation_context)
            if output_schema is not None:
                env["AETHYME_EVAL_OUTPUT_SCHEMA_FILE"] = str(output_schema_file)
                env["AETHYME_EVAL_OUTPUT_SCHEMA"] = json.dumps(output_schema)

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
        finished_at = datetime.now(UTC).isoformat()
        run_metadata = build_run_metadata(
            snapshot,
            project_root=PROJECT_ROOT,
            phase=f"eval:{label}",
            status="success" if result.returncode == 0 else "failed",
            run_id=run_id,
            started_at=started_at,
            finished_at=finished_at,
            command=resolved_command,
            config={
                "task": task,
                "label": label,
                "working_directory": str((self.working_directory or repo_path).resolve()),
                "has_navigation_context": navigation_context is not None,
                "has_output_schema": output_schema is not None,
            },
        )
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
            final_output_message=_final_output_message(stdout, metrics.get("structured_output")),
            structured_output=metrics.get("structured_output"),
            tool_calls=metrics.get("tool_calls"),
            run_metadata=run_metadata.to_dict(),
        )


def _parse_metrics(stdout: str) -> dict[str, Any]:
    empty: dict[str, Any] = {
        "input_tokens": None,
        "output_tokens": None,
        "retries": None,
        "review_burden": None,
        "structured_output": None,
        "tool_calls": None,
    }
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return empty
    if not isinstance(payload, dict):
        return empty
    structured_output = payload.get("structured_output")
    if not isinstance(structured_output, dict):
        structured_output = payload if any(
            key in payload for key in ("repo_summary", "config_target", "code_target", "management_area", "relationship_chain")
        ) else None
    tool_calls = payload.get("tool_calls")
    if not isinstance(tool_calls, list):
        tool_calls = None
    return {
        "input_tokens": _as_int(payload.get("input_tokens")),
        "output_tokens": _as_int(payload.get("output_tokens")),
        "retries": _as_int(payload.get("retries")),
        "review_burden": _as_int(payload.get("review_burden")),
        "structured_output": structured_output,
        "tool_calls": tool_calls,
    }


def _final_output_message(stdout: str, structured_output: dict[str, Any] | None) -> str | None:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return stdout or None
    if isinstance(payload, dict):
        message = payload.get("final_output_message")
        if isinstance(message, str) and message.strip():
            return message
        if structured_output is not None:
            return json.dumps(structured_output, indent=2)
    return stdout or None


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
            resolved = [str(cwd_candidate), *args[1:]]
        else:
            project_candidate = (PROJECT_ROOT / executable).resolve(strict=False)
            resolved = [str(project_candidate), *args[1:]]
        return _resolve_local_path_args(resolved)
    if which(executable):
        return _resolve_local_path_args(args)
    return _resolve_local_path_args(args)


def _resolve_local_path_args(args: list[str]) -> list[str]:
    if len(args) < 2:
        return args
    resolved = [args[0]]
    for arg in args[1:]:
        if not ("/" in arg or arg.startswith(".")):
            resolved.append(arg)
            continue
        cwd_candidate = (Path.cwd() / arg).resolve(strict=False)
        if cwd_candidate.exists():
            resolved.append(str(cwd_candidate))
            continue
        project_candidate = (PROJECT_ROOT / arg).resolve(strict=False)
        if project_candidate.exists():
            resolved.append(str(project_candidate))
            continue
        resolved.append(arg)
    return resolved
