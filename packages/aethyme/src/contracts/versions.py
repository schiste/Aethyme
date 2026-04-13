"""Canonical version constants for persisted Aethyme contracts."""

from __future__ import annotations

REPOSITORY_SNAPSHOT_SCHEMA_VERSION = "1"
RUN_METADATA_SCHEMA_VERSION = "1"
GRAPH_EXPORT_SCHEMA_VERSION = "1"
EVAL_ARTIFACT_SCHEMA_VERSION = "1"
RANKING_SCHEMA_VERSION = "1"
OUTPUT_SCHEMA_VERSION = "1"


def contract_versions() -> dict[str, str]:
    """Return the current persisted contract versions."""
    return {
        "repository_snapshot": REPOSITORY_SNAPSHOT_SCHEMA_VERSION,
        "run_metadata": RUN_METADATA_SCHEMA_VERSION,
        "graph_export": GRAPH_EXPORT_SCHEMA_VERSION,
        "eval_artifact": EVAL_ARTIFACT_SCHEMA_VERSION,
        "ranking": RANKING_SCHEMA_VERSION,
        "output": OUTPUT_SCHEMA_VERSION,
    }
