"""Tests for versioned graph export envelopes."""

from __future__ import annotations

from src.contracts.graph_export import GraphExportEnvelope


def test_graph_export_envelope_includes_schema_version() -> None:
    envelope = GraphExportEnvelope(
        repository={"name": "repo"},
        repository_snapshot={"schema_version": "1", "snapshot_key": "abc"},
        stats={"nodes": 1, "edges": 0},
        nodes=[{"id": "n1"}],
        edges=[],
        generated_at="2026-04-13T00:00:00+00:00",
        engine_version="0.1.0",
    ).to_dict()

    assert envelope["schema_version"] == "1"
    assert envelope["repository_snapshot"]["schema_version"] == "1"
    assert envelope["stats"]["nodes"] == 1
