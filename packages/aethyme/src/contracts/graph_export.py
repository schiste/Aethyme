"""Versioned envelope for exported graph payloads."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from .versions import GRAPH_EXPORT_SCHEMA_VERSION


@dataclass(frozen=True)
class GraphExportEnvelope:
    """Stable top-level shape for exported graph snapshots."""

    repository: dict[str, Any]
    repository_snapshot: dict[str, Any]
    stats: dict[str, Any]
    nodes: list[dict[str, Any]]
    edges: list[dict[str, Any]]
    generated_at: str
    engine_version: str
    schema_version: str = GRAPH_EXPORT_SCHEMA_VERSION

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "generated_at": self.generated_at,
            "engine_version": self.engine_version,
            "repository": self.repository,
            "repository_snapshot": self.repository_snapshot,
            "stats": self.stats,
            "nodes": self.nodes,
            "edges": self.edges,
        }
