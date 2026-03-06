"""API-level proof of the Aethyme core product loop."""

from __future__ import annotations

import time
from pathlib import Path

from fastapi.testclient import TestClient

from src.api.main import app
from tests.support.auth_db import create_token_context, delete_org
from tests.support.repo_builders import build_indexable_core_repo


def test_core_flow_index_search_ego_impact_scorecard(tmp_path: Path) -> None:
    repo_path = build_indexable_core_repo(tmp_path)
    auth_context = create_token_context("core-flow", ["repo:read", "repo:write", "org:admin"])

    try:
        with TestClient(app) as client:
            headers = auth_context["headers"]

            index_response = client.post(
                "/api/v1/index/repositories",
                json={
                    "repository_path": str(repo_path),
                    "repository_name": "core-flow-repo",
                    "languages": ["python"],
                    "use_fallback": True,
                    "clear_existing": True,
                },
                headers=headers,
            )
            assert index_response.status_code == 200, index_response.text
            index_payload = index_response.json()
            repository_id = index_payload["repository_id"]
            assert index_payload["total_nodes"] > 0
            assert index_payload["total_edges"] > 0

            search_response = client.post(
                "/api/v1/search/",
                json={"query": "helper", "limit": 10, "search_type": "hybrid"},
                headers=headers,
            )
            assert search_response.status_code == 200, search_response.text
            search_payload = search_response.json()
            assert search_payload["total_results"] > 0
            helper_symbol = next(
                item["symbol"]
                for item in search_payload["results"]
                if item["symbol"].endswith(":helper")
            )

            ego_response = client.post(
                "/api/v1/ego/",
                json={"symbol": helper_symbol, "depth": 2, "limit": 20},
                headers=headers,
            )
            assert ego_response.status_code == 200, ego_response.text
            ego_payload = ego_response.json()
            assert ego_payload["definition"]["symbol"] == helper_symbol
            assert ego_payload["total_nodes"] >= 2

            impact_response = client.post(
                "/api/v1/impact/",
                json={"symbol": helper_symbol, "max_depth": 4, "limit": 20},
                headers=headers,
            )
            assert impact_response.status_code == 200, impact_response.text
            impact_payload = impact_response.json()
            assert impact_payload["total_impacted"] >= 1

            scan_response = client.post(
                "/api/v1/scorecard/scan",
                json={"repository_id": repository_id},
                headers=headers,
            )
            assert scan_response.status_code == 200, scan_response.text
            scan_id = scan_response.json()["scan_id"]

            result_response = None
            for _ in range(20):
                result_response = client.get(f"/api/v1/scorecard/results/{scan_id}", headers=headers)
                if result_response.status_code == 200:
                    break
                assert result_response.status_code == 202, result_response.text
                time.sleep(0.05)

            assert result_response is not None
            assert result_response.status_code == 200, result_response.text
            scorecard_payload = result_response.json()
            assert scorecard_payload["repository_id"] == repository_id
            assert 0 <= scorecard_payload["score"] <= 100

            summary_response = client.get(
                f"/api/v1/scorecard/summary/{repository_id}",
                headers=headers,
            )
            assert summary_response.status_code == 200, summary_response.text
    finally:
        delete_org(auth_context["org_id"])
