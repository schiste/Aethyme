"""Tests for run metadata and repository snapshot serialization."""

from __future__ import annotations

from pathlib import Path

from src.contracts.run_metadata import build_run_metadata
from src.indexing.repository_snapshot import LocalRepositorySnapshot


def test_repository_snapshot_to_dict_includes_version_and_snapshot_key() -> None:
    snapshot = LocalRepositorySnapshot(
        repo_path=Path("/tmp/repo"),
        repo_name="repo",
        commit="abc123",
        fingerprint="fingerprint",
        file_count=5,
        dirty=False,
    )

    data = snapshot.to_dict()

    assert data["schema_version"] == "1"
    assert data["repo_name"] == "repo"
    assert data["snapshot_key"] == "abc123"


def test_build_run_metadata_captures_snapshot_and_versions() -> None:
    snapshot = LocalRepositorySnapshot(
        repo_path=Path("/tmp/repo"),
        repo_name="repo",
        commit=None,
        fingerprint="fingerprint",
        file_count=5,
        dirty=True,
    )

    metadata = build_run_metadata(
        snapshot,
        project_root=Path(__file__).resolve().parents[2],
        phase="query",
        status="success",
        command=["aethyme-engine-cli", "inspect"],
        config={"mode": "brief"},
    ).to_dict()

    assert metadata["schema_version"] == "1"
    assert metadata["repo_snapshot_key"] == "fingerprint"
    assert metadata["phase"] == "query"
    assert metadata["status"] == "success"
    assert metadata["command"] == ["aethyme-engine-cli", "inspect"]
    assert metadata["config_hash"] is not None
