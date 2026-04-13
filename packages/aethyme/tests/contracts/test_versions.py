"""Tests for persisted contract version stamps."""

from __future__ import annotations

from src.contracts.versions import contract_versions


def test_contract_versions_are_defined() -> None:
    versions = contract_versions()

    assert versions["repository_snapshot"] == "1"
    assert versions["run_metadata"] == "1"
    assert versions["graph_export"] == "1"
    assert versions["eval_artifact"] == "1"
    assert versions["ranking"] == "1"
    assert versions["output"] == "1"
