"""Pluggable tool adapters for eval Option A (swap-in comparisons).

Each tool that an eval can be run against (Aethyme, Graphify, future entries)
is described by a declarative TOML manifest under ``evals/tools/`` and loaded
through this package.  The :class:`ToolAdapter` Protocol is the surface the
orchestrator consumes — concrete implementations are either:

* manifest-driven (:class:`ManifestToolAdapter`) for arbitrary CLI tools, or
* hand-written Python adapters for in-tree tools with deep integration
  (currently only Aethyme itself).

See ``packages/aethyme/docs/architecture/tool-adapters.md`` for the design.
"""

from __future__ import annotations

from .base import ConditionResult, ToolAdapter
from .manifest import ManifestToolAdapter, ToolManifest, load_manifest
from .registry import get_adapter, list_tools

__all__ = [
    "ConditionResult",
    "ManifestToolAdapter",
    "ToolAdapter",
    "ToolManifest",
    "get_adapter",
    "list_tools",
    "load_manifest",
]
