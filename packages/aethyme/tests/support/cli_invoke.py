"""Implementation-blind CLI invocation for tests (retirement plan Phase 0).

Tests invoke the `aethyme` router binary as a subprocess instead of
importing the Click app in-process. The same assertions then verify
whichever implementation answers — today the router delegates most
commands to `python -m src.cli`; as migration phases land, commands
flip to native Rust with zero test changes.

The router resolves the Python package via AETHYME_ROOT, which we pin
to this checkout so tests never depend on pointer files or upward
walks from tmp directories.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[2]
RUST_MANIFEST = PACKAGE_ROOT / "rust" / "Cargo.toml"


@dataclass
class InvokeResult:
    """Mirrors the click.testing.Result fields our assertions use."""

    exit_code: int
    output: str  # stdout + stderr merged, matching CliRunner's default


@lru_cache(maxsize=1)
def aethyme_binary() -> Path:
    """Resolve the `aethyme` router binary, building it if missing.

    Deliberately does NOT skip when the build fails: an absent binary
    must fail tests loudly (environment-dependent skips are a known
    gate blind spot — see python-retirement-plan.md, cross-phase risks).
    """
    target_dir = RUST_MANIFEST.parent / "target"
    candidates = [
        target_dir / "release" / "aethyme",
        target_dir / "debug" / "aethyme",
    ]
    existing = [path for path in candidates if path.exists()]
    if existing:
        return max(existing, key=lambda path: path.stat().st_mtime_ns)
    result = subprocess.run(
        [
            "cargo",
            "build",
            "--quiet",
            "--manifest-path",
            str(RUST_MANIFEST),
            "--bin",
            "aethyme",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    debug_binary = candidates[1]
    if result.returncode != 0 or not debug_binary.exists():
        raise AssertionError(
            f"aethyme router binary unavailable and build failed:\n{result.stderr.strip()}"
        )
    return debug_binary


def invoke_aethyme(args: list[str], cwd: Path | None = None) -> InvokeResult:
    """Run `aethyme <args>` and return exit code + merged output."""
    env = dict(os.environ)
    env["AETHYME_ROOT"] = str(PACKAGE_ROOT)
    completed = subprocess.run(
        [str(aethyme_binary()), *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(cwd) if cwd else None,
    )
    return InvokeResult(
        exit_code=completed.returncode,
        output=completed.stdout + completed.stderr,
    )
