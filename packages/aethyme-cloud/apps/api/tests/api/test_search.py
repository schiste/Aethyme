"""Tests for search endpoints."""

import pytest
from unittest.mock import Mock, patch, AsyncMock
from httpx import AsyncClient
from fastapi import status


class TestGlobalSearch:
    """Test global code search endpoint."""

    @pytest.mark.asyncio
    async def test_search_code_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test searching code across repositories."""
        # Mock Elasticsearch response
        mock_search_result = {
            "results": [
                {
                    "file_path": "src/main.py",
                    "file_name": "main.py",
                    "language": "Python",
                    "score": 15.5,
                    "highlights": ["def main():"],
                }
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=main"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            assert data["query"] == "main"
            assert data["total"] >= 0
            assert "results" in data
            assert "facets" in data
            assert "took_ms" in data

    @pytest.mark.asyncio
    async def test_search_code_with_filters(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test searching with language and file path filters."""
        mock_search_result = {
            "results": [
                {
                    "file_path": "src/utils.py",
                    "file_name": "utils.py",
                    "language": "Python",
                    "score": 12.0,
                    "highlights": ["def utils():"],
                }
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=function&languages=Python&file_path=src/"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            # Verify filters were applied
            mock_indexer.search_code.assert_called()

    @pytest.mark.asyncio
    async def test_search_code_pagination(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search pagination."""
        # Create multiple results
        mock_results = {
            "results": [
                {
                    "file_path": f"src/file{i}.py",
                    "file_name": f"file{i}.py",
                    "language": "Python",
                    "score": 10.0 - i,
                    "highlights": [f"code {i}"],
                }
                for i in range(25)
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_results

            # First page
            response = await authenticated_async_client.get(
                "/api/v1/search/?q=test&page=1&per_page=10"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            assert data["page"] == 1
            assert data["per_page"] == 10
            assert len(data["results"]) <= 10
            assert data["total_pages"] > 0

    @pytest.mark.asyncio
    async def test_search_code_no_results(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search with no results."""
        mock_search_result = {"results": []}

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=nonexistent"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            assert data["total"] == 0
            assert data["results"] == []

    @pytest.mark.asyncio
    async def test_search_code_unauthorized(
        self,
        async_client: AsyncClient,
    ):
        """Test search without authentication."""
        response = await async_client.get("/api/v1/search/?q=test")

        assert response.status_code == status.HTTP_401_UNAUTHORIZED

    @pytest.mark.asyncio
    async def test_search_code_empty_query(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test search with empty query."""
        response = await authenticated_async_client.get("/api/v1/search/?q=")

        # Should fail validation
        assert response.status_code == status.HTTP_422_UNPROCESSABLE_ENTITY

    @pytest.mark.asyncio
    async def test_search_code_facets(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search returns proper facets."""
        mock_search_result = {
            "results": [
                {
                    "file_path": "src/main.py",
                    "file_name": "main.py",
                    "language": "Python",
                    "score": 15.5,
                    "highlights": ["code"],
                },
                {
                    "file_path": "src/app.js",
                    "file_name": "app.js",
                    "language": "JavaScript",
                    "score": 12.0,
                    "highlights": ["code"],
                },
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=test"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            # Check facets are present
            assert "facets" in data
            assert "languages" in data["facets"]
            assert "repositories" in data["facets"]

            # Should have both languages
            languages = data["facets"]["languages"]
            assert "Python" in languages
            assert "JavaScript" in languages


class TestRepositorySearch:
    """Test single repository search endpoint."""

    @pytest.mark.asyncio
    async def test_search_repository_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test searching within a specific repository."""
        mock_search_result = {
            "results": [
                {
                    "file_path": "README.md",
                    "file_name": "README.md",
                    "language": "Markdown",
                    "score": 20.0,
                    "highlights": ["# Project"],
                }
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                f"/api/v1/search/repositories/{test_repository.id}?q=project"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            assert data["query"] == "project"
            assert len(data["results"]) >= 0

            # Verify it searched the correct repository
            mock_indexer.search_code.assert_called_once()
            call_args = mock_indexer.search_code.call_args
            assert call_args[1]["repository_id"] == test_repository.id

    @pytest.mark.asyncio
    async def test_search_repository_not_found(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test searching non-existent repository."""
        response = await authenticated_async_client.get(
            "/api/v1/search/repositories/non-existent-id?q=test"
        )

        assert response.status_code == status.HTTP_404_NOT_FOUND

    @pytest.mark.asyncio
    async def test_search_repository_with_language_filter(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test repository search with language filter."""
        mock_search_result = {
            "results": [
                {
                    "file_path": "src/main.py",
                    "file_name": "main.py",
                    "language": "Python",
                    "score": 18.0,
                    "highlights": ["def main():"],
                }
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                f"/api/v1/search/repositories/{test_repository.id}?q=main&language=Python"
            )

            assert response.status_code == status.HTTP_200_OK
            data = response.json()

            # Verify language filter was passed
            call_args = mock_indexer.search_code.call_args
            assert call_args[1]["language"] == "Python"

    @pytest.mark.asyncio
    async def test_search_repository_with_file_path_filter(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test repository search with file path filter."""
        mock_search_result = {"results": []}

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                f"/api/v1/search/repositories/{test_repository.id}?q=test&file_path=src/"
            )

            assert response.status_code == status.HTTP_200_OK

            # Verify file path filter was passed
            call_args = mock_indexer.search_code.call_args
            assert call_args[1]["file_path"] == "src/"


class TestSearchPerformance:
    """Test search performance and edge cases."""

    @pytest.mark.asyncio
    async def test_search_response_time(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search responds quickly."""
        import time

        mock_search_result = {
            "results": [
                {
                    "file_path": f"file{i}.py",
                    "file_name": f"file{i}.py",
                    "language": "Python",
                    "score": 10.0,
                    "highlights": ["code"],
                }
                for i in range(50)
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            start = time.time()
            response = await authenticated_async_client.get(
                "/api/v1/search/?q=test"
            )
            elapsed = time.time() - start

            assert response.status_code == status.HTTP_200_OK

            # Should respond quickly (mocked, so very fast)
            assert elapsed < 2.0

            # Check that took_ms is reported
            data = response.json()
            assert "took_ms" in data
            assert data["took_ms"] >= 0

    @pytest.mark.asyncio
    async def test_search_with_multiple_repositories(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test searching across multiple repositories."""
        mock_search_result = {
            "results": [
                {
                    "file_path": "file.py",
                    "file_name": "file.py",
                    "language": "Python",
                    "score": 10.0,
                    "highlights": ["test"],
                }
            ]
        }

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=test"
            )

            assert response.status_code == status.HTTP_200_OK

            # Should have called search for each repository (API searches all, not just indexed)
            assert mock_indexer.search_code.call_count == 5

    @pytest.mark.asyncio
    async def test_search_with_elasticsearch_error(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search handles Elasticsearch errors gracefully."""
        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.side_effect = Exception("ES connection failed")

            response = await authenticated_async_client.get(
                "/api/v1/search/?q=test"
            )

            # Should still return 200 with empty results (graceful degradation)
            assert response.status_code == status.HTTP_200_OK
            data = response.json()
            assert data["total"] == 0

    @pytest.mark.asyncio
    async def test_search_with_special_characters(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test search handles special characters."""
        mock_search_result = {"results": []}

        with patch("app.api.v1.search.CodeIndexer") as MockIndexer:
            mock_indexer = MockIndexer.return_value
            mock_indexer.search_code.return_value = mock_search_result

            # Test with special characters
            response = await authenticated_async_client.get(
                "/api/v1/search/?q=function%20test%28%29"
            )

            assert response.status_code == status.HTTP_200_OK
