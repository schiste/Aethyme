"""Tests for the shared indexing service contract used by API and CLI."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from click.testing import CliRunner
from pytest import MonkeyPatch

from src.api.endpoints.index_status import IndexRepositoryRequest as IndexRepositoryPayload
from src.cli import cli
from src.indexing.service import IndexingResult, ProgressCallback, RepositoryIndexRequest


@dataclass
class StubIndexingUser:
    tenant_id: str
    org_id: str | None


def test_repository_index_request_normalizes_inputs(tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    repo_path.mkdir()

    request = RepositoryIndexRequest(
        repo_path=repo_path,
        repo_name="repo-name",
        languages=[" python ", "typescript", ""],
        tenant_id="tenant-1",
        org_id="org-1",
    )

    assert request.resolved_repo_path() == repo_path.resolve()
    assert request.resolved_languages() == ["python", "typescript"]


def test_api_payload_converts_to_service_request(tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    repo_path.mkdir()

    payload = IndexRepositoryPayload(
        repository_path=str(repo_path),
        repository_name="api-repo",
        languages=["python"],
        use_fallback=True,
        clear_existing=False,
    )
    user = StubIndexingUser(tenant_id="tenant-1", org_id="org-1")

    request = payload.to_service_request(user)

    assert request.repo_name == "api-repo"
    assert request.tenant_id == "tenant-1"
    assert request.org_id == "org-1"
    assert request.use_fallback is True
    assert request.clear_existing is False
    assert request.resolved_repo_path() == repo_path.resolve()


def test_cli_index_uses_shared_service_request(
    monkeypatch: MonkeyPatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    captured: dict[str, RepositoryIndexRequest] = {}

    def fake_run_indexing(
        request: RepositoryIndexRequest,
        progress_callback: ProgressCallback | None = None,
    ) -> IndexingResult:
        del progress_callback
        captured["request"] = request
        return IndexingResult(
            repository_id="repo-1",
            repository_name=request.repo_name or repo_path.name,
            repository_path=str(request.resolved_repo_path()),
            org_id=request.org_id,
            tenant_id=request.tenant_id or "tenant-1",
            languages=request.resolved_languages(),
            total_nodes=1,
            total_edges=0,
            total_files=1,
            graph_statistics={"total_nodes": 1, "total_edges": 0, "total_files": 1},
        )

    monkeypatch.setattr("src.cli.run_indexing", fake_run_indexing)

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "--tenant-id",
            "tenant-1",
            "index",
            str(repo_path),
            "--name",
            "cli-repo",
            "--languages",
            "python,typescript",
            "--use-fallback",
        ],
    )

    assert result.exit_code == 0, result.output
    request = captured["request"]
    assert request.repo_name == "cli-repo"
    assert request.tenant_id == "tenant-1"
    assert request.use_fallback is True
    assert request.resolved_languages() == ["python", "typescript"]
