"""Runner interface for future live eval execution.

The v0 sentinel compares run artifacts. This module is the explicit seam for
later live execution so the comparison package does not grow a Chau7-specific
core again.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol


@dataclass(frozen=True)
class RunRequest:
    command: tuple[str, ...]
    cwd: Path
    results_path: Path


@dataclass(frozen=True)
class RunResult:
    returncode: int
    results_path: Path
    stdout: str
    stderr: str


class Runner(Protocol):
    def run(self, request: RunRequest) -> RunResult:
        """Execute a run and return the path containing produced artifacts."""


class CommandRunner:
    """Minimal subprocess-backed runner for local scripts."""

    def run(self, request: RunRequest) -> RunResult:
        result = subprocess.run(
            request.command,
            cwd=request.cwd,
            check=False,
            capture_output=True,
            text=True,
        )
        return RunResult(
            returncode=result.returncode,
            results_path=request.results_path,
            stdout=result.stdout,
            stderr=result.stderr,
        )
