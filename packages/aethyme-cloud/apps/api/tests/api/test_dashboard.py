"""Tests for dashboard endpoints."""

import pytest
from httpx import AsyncClient
from fastapi import status


class TestDashboardStats:
    """Test dashboard statistics endpoint."""

    @pytest.mark.asyncio
    async def test_get_dashboard_stats_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test getting dashboard stats with valid authentication."""
        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Verify response structure
        assert "total_repositories" in data
        assert "indexed_repositories" in data
        assert "indexing_repositories" in data
        assert "failed_repositories" in data
        assert "total_files" in data
        assert "total_lines" in data
        assert "languages" in data

        # Verify data types
        assert isinstance(data["total_repositories"], int)
        assert isinstance(data["indexed_repositories"], int)
        assert isinstance(data["indexing_repositories"], int)
        assert isinstance(data["failed_repositories"], int)
        assert isinstance(data["total_files"], int)
        assert isinstance(data["total_lines"], int)
        assert isinstance(data["languages"], list)

        # Verify counts with test repository
        assert data["total_repositories"] == 1
        assert data["indexed_repositories"] == 1

    @pytest.mark.asyncio
    async def test_get_dashboard_stats_multiple_repos(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test dashboard stats with multiple repositories."""
        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Verify counts
        assert data["total_repositories"] == 5
        assert data["indexed_repositories"] == 3  # Only first 3 are indexed

        # Verify aggregated counts
        assert data["total_files"] > 0
        assert data["total_lines"] > 0

        # Verify languages aggregation (languages is a list of LanguageStats)
        languages = data["languages"]
        assert isinstance(languages, list)
        if languages:
            # Check that we have language stats
            assert any(lang["language"] == "Python" for lang in languages)

    @pytest.mark.asyncio
    async def test_get_dashboard_stats_no_repositories(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test dashboard stats with no repositories."""
        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Should return zero counts
        assert data["total_repositories"] == 0
        assert data["indexed_repositories"] == 0
        assert data["indexing_repositories"] == 0
        assert data["failed_repositories"] == 0
        assert data["total_files"] == 0
        assert data["total_lines"] == 0
        assert data["languages"] == []

    @pytest.mark.asyncio
    async def test_get_dashboard_stats_unauthorized(
        self,
        async_client: AsyncClient,
    ):
        """Test dashboard stats without authentication."""
        response = await async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_401_UNAUTHORIZED

    @pytest.mark.asyncio
    async def test_dashboard_stats_language_aggregation(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test that languages are properly aggregated across repositories."""
        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        languages = data["languages"]

        # Languages should be a list of LanguageStats objects
        assert isinstance(languages, list)

        # If there are languages, verify structure
        if languages:
            lang_stat = languages[0]
            assert "language" in lang_stat
            assert "file_count" in lang_stat
            assert "line_count" in lang_stat
            assert "percentage" in lang_stat

    @pytest.mark.asyncio
    async def test_dashboard_stats_caching(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test that dashboard stats can be called multiple times."""
        # First request
        response1 = await authenticated_async_client.get("/api/v1/dashboard/stats")
        assert response1.status_code == status.HTTP_200_OK

        # Second request should also work
        response2 = await authenticated_async_client.get("/api/v1/dashboard/stats")
        assert response2.status_code == status.HTTP_200_OK

        # Data should be consistent
        assert response1.json() == response2.json()


class TestDashboardPerformance:
    """Test dashboard endpoint performance."""

    @pytest.mark.asyncio
    async def test_dashboard_stats_response_time(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test that dashboard stats responds quickly even with multiple repos."""
        import time

        start = time.time()
        response = await authenticated_async_client.get("/api/v1/dashboard/stats")
        elapsed = time.time() - start

        assert response.status_code == status.HTTP_200_OK

        # Should respond in under 1 second even with 5 repos
        assert elapsed < 1.0

    @pytest.mark.asyncio
    async def test_dashboard_stats_with_large_repository(
        self,
        authenticated_async_client: AsyncClient,
        db_session,
        test_organization,
    ):
        """Test dashboard stats with a large repository."""
        from app.models.repository import Repository
        from datetime import datetime

        # Create a large repository
        large_repo = Repository(
            id="large-repo-123",
            organization_id=test_organization.id,
            name="large-repo",
            full_name="testorg/large-repo",
            url="https://github.com/testorg/large-repo",
            provider="github",
            provider_id="99999",
            default_branch="main",
            is_private=False,
            indexing_status="completed",
            is_indexed=True,
            file_count=10000,
            line_count=1000000,
            total_files=10000,
            total_lines=1000000,
            languages={"Python": 0.6, "JavaScript": 0.4},
            primary_language="Python",
            indexed_at=datetime.utcnow(),
        )
        db_session.add(large_repo)
        await db_session.commit()

        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Verify large numbers are handled correctly
        assert data["total_files"] >= 10000
        assert data["total_lines"] >= 1000000


class TestDashboardEdgeCases:
    """Test dashboard edge cases and error handling."""

    @pytest.mark.asyncio
    async def test_dashboard_with_failed_repositories(
        self,
        authenticated_async_client: AsyncClient,
        db_session,
        test_organization,
    ):
        """Test dashboard stats with repositories in failed state."""
        from app.models.repository import Repository

        failed_repo = Repository(
            id="failed-repo-123",
            organization_id=test_organization.id,
            name="failed-repo",
            full_name="testorg/failed-repo",
            url="https://github.com/testorg/failed-repo",
            provider="github",
            provider_id="88888",
            default_branch="main",
            is_private=False,
            indexing_status="failed",
            is_indexed=False,
        )
        db_session.add(failed_repo)
        await db_session.commit()

        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Should count total but not indexed
        assert data["total_repositories"] == 1
        assert data["indexed_repositories"] == 0

    @pytest.mark.asyncio
    async def test_dashboard_with_null_language_data(
        self,
        authenticated_async_client: AsyncClient,
        db_session,
        test_organization,
    ):
        """Test dashboard stats with repository lacking language data."""
        from app.models.repository import Repository

        repo_no_lang = Repository(
            id="no-lang-repo-123",
            organization_id=test_organization.id,
            name="no-lang-repo",
            full_name="testorg/no-lang-repo",
            url="https://github.com/testorg/no-lang-repo",
            provider="github",
            provider_id="77777",
            default_branch="main",
            is_private=False,
            indexing_status="completed",
            is_indexed=True,
            languages=None,  # No language data
            primary_language=None,
        )
        db_session.add(repo_no_lang)
        await db_session.commit()

        response = await authenticated_async_client.get("/api/v1/dashboard/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Should handle null languages gracefully
        assert isinstance(data["languages"], list)
