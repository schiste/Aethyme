"""Filesystem-backed registry of tool manifests.

Tool manifests live under ``packages/aethyme/evals/tools/``. The registry
discovers them lazily: ``get_adapter("graphify")`` finds ``graphify.toml``
in that directory and returns a :class:`ManifestToolAdapter`.

The in-tree Aethyme adapter (when migrated) will live alongside as
``aethyme.toml`` with a sentinel ``[adapter] kind = "native"`` block so the
registry returns the hand-written Python adapter instead of the generic
manifest one. That sentinel-and-dispatch hook is stubbed below but not
wired until task #4 (Aethyme migration).
"""

from __future__ import annotations

from pathlib import Path

from .base import ToolAdapter
from .manifest import (
    ManifestToolAdapter,
    ManifestValidationError,
    ToolManifest,
    load_manifest,
)

_PACKAGE_ROOT = Path(__file__).resolve().parents[3]
_DEFAULT_MANIFEST_DIR = _PACKAGE_ROOT / "evals" / "tools"
_DEFAULT_TOOL_CACHE = _PACKAGE_ROOT / ".eval-cache" / "tools"


def list_tools(manifest_dir: Path | None = None) -> list[str]:
    """Return the names of all manifests found in ``manifest_dir``."""
    directory = (manifest_dir or _DEFAULT_MANIFEST_DIR).resolve()
    if not directory.is_dir():
        return []
    return sorted(p.stem for p in directory.glob("*.toml"))


def get_adapter(
    name: str,
    *,
    manifest_dir: Path | None = None,
    tool_cache_dir: Path | None = None,
) -> ToolAdapter:
    """Resolve a tool name to an adapter instance.

    Looks for ``<manifest_dir>/<name>.toml``. Raises :class:`KeyError` if no
    manifest matches (so the CLI can surface a clean "unknown tool" message).
    """
    directory = (manifest_dir or _DEFAULT_MANIFEST_DIR).resolve()
    cache = (tool_cache_dir or _DEFAULT_TOOL_CACHE).resolve()

    manifest_path = directory / f"{name}.toml"
    if not manifest_path.is_file():
        available = ", ".join(list_tools(directory)) or "(none)"
        raise KeyError(
            f"Unknown tool {name!r}. Available manifests: {available}. "
            f"Looked in {directory}."
        )

    try:
        manifest: ToolManifest = load_manifest(manifest_path)
    except ManifestValidationError:
        raise

    return ManifestToolAdapter(manifest=manifest, tool_cache_dir=cache)
