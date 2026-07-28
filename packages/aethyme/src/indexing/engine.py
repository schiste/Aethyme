"""Engine binary bootstrap (build-if-stale) for tests and dev tooling.

All production command paths went native in the router during
python-retirement Phase 1; what remains here is the ensure-built
helper the test fixtures use. Retires in Phase 6.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

ENGINE_MANIFEST_PATH = Path(__file__).resolve().parents[2] / "rust" / "Cargo.toml"
ENGINE_WORKSPACE_PATH = ENGINE_MANIFEST_PATH.parent
ENGINE_BINARY_DEBUG = ENGINE_WORKSPACE_PATH / "target" / "debug" / "aethyme-engine-cli"
ENGINE_BINARY_RELEASE = ENGINE_WORKSPACE_PATH / "target" / "release" / "aethyme-engine-cli"
ENGINE_BINARY_PATH: Path | None = None
CACHE_ROOT = Path(os.getenv("AETHYME_CACHE_DIR", "/tmp/aethyme-cache"))


class EngineError(RuntimeError):
    """Raised when the Rust engine command fails."""


def ensure_engine_binary() -> Path:
    """Build the Rust engine binary if it is missing or stale."""
    binary_path = _configured_engine_binary_path()
    needs_build = not binary_path.exists()
    if not needs_build:
        binary_mtime = binary_path.stat().st_mtime_ns
        for source_path in [ENGINE_MANIFEST_PATH, *ENGINE_WORKSPACE_PATH.rglob("*.rs")]:
            if source_path.stat().st_mtime_ns > binary_mtime:
                needs_build = True
                break

    if needs_build:
        command = [
            "cargo",
            "build",
            "--quiet",
            "--manifest-path",
            str(ENGINE_MANIFEST_PATH),
            "--bin",
            "aethyme-engine-cli",
        ]
        if binary_path == ENGINE_BINARY_RELEASE:
            command.insert(2, "--release")
        result = subprocess.run(command, check=False, capture_output=True, text=True)
        if result.returncode != 0:
            raise EngineError(result.stderr.strip() or result.stdout.strip() or "Rust engine build failed")

    if not binary_path.exists():
        raise EngineError(f"Rust engine binary missing after build: {binary_path}")
    return binary_path


def _preferred_engine_binary() -> Path:
    if ENGINE_BINARY_RELEASE.exists() and ENGINE_BINARY_DEBUG.exists():
        if ENGINE_BINARY_RELEASE.stat().st_mtime_ns >= ENGINE_BINARY_DEBUG.stat().st_mtime_ns:
            return ENGINE_BINARY_RELEASE
        return ENGINE_BINARY_DEBUG
    if ENGINE_BINARY_RELEASE.exists():
        return ENGINE_BINARY_RELEASE
    return ENGINE_BINARY_DEBUG


def _configured_engine_binary_path() -> Path:
    """Return an explicit test/config override or the preferred built binary path."""
    if ENGINE_BINARY_PATH is not None:
        return ENGINE_BINARY_PATH
    return _preferred_engine_binary()
