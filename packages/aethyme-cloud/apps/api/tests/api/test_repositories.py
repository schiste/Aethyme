"""Tests for repository management endpoints."""

import pytest
from httpx import AsyncClient
from fastapi import status


class TestCreateRepository:
    """Test repository creation endpoint."""

    @pytest.mark.asyncio
    async def test_create_repository_success(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test creating a new repository."""
        repo_data = {
            "name": "new-repo",
            "full_name": "testorg/new-repo",
            "url": "https://github.com/testorg/new-repo",
            "description": "A new test repository",
            "provider": "github",
            "provider_id": "54321",
            "default_branch": "main",
            "is_private": False,
        }

        response = await authenticated_async_client.post(
            "/api/v1/repositories/", json=repo_data
        )

        assert response.status_code == status.HTTP_201_CREATED
        data = response.json()

        assert data["name"] == repo_data["name"]
        assert data["full_name"] == repo_data["full_name"]
        assert data["provider"] == repo_data["provider"]
        assert data["indexing_status"] == "pending"
        assert data["is_indexed"] is False
        assert "id" in data
        assert "created_at" in data

    @pytest.mark.asyncio
    async def test_create_repository_duplicate(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test creating a duplicate repository fails."""
        repo_data = {
            "name": test_repository.name,
            "full_name": test_repository.full_name,
            "url": test_repository.url,
            "provider": test_repository.provider,
            "provider_id": test_repository.provider_id,
            "default_branch": "main",
            "is_private": False,
        }

        response = await authenticated_async_client.post(
            "/api/v1/repositories/", json=repo_data
        )

        assert response.status_code == status.HTTP_400_BAD_REQUEST
        assert "already connected" in response.json()["detail"].lower()

    @pytest.mark.asyncio
    async def test_create_repository_unauthorized(
        self,
        async_client: AsyncClient,
    ):
        """Test creating repository without authentication fails."""
        repo_data = {
            "name": "new-repo",
            "full_name": "testorg/new-repo",
            "url": "https://github.com/testorg/new-repo",
            "provider": "github",
            "provider_id": "54321",
            "default_branch": "main",
            "is_private": False,
        }

        response = await async_client.post("/api/v1/repositories/", json=repo_data)

        assert response.status_code == status.HTTP_401_UNAUTHORIZED

    @pytest.mark.asyncio
    async def test_create_repository_invalid_data(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test creating repository with invalid data fails."""
        repo_data = {
            "name": "",  # Empty name
            "provider": "invalid_provider",  # Invalid provider
        }

        response = await authenticated_async_client.post(
            "/api/v1/repositories/", json=repo_data
        )

        assert response.status_code == status.HTTP_422_UNPROCESSABLE_ENTITY


class TestListRepositories:
    """Test repository listing endpoint."""

    @pytest.mark.asyncio
    async def test_list_repositories_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test listing repositories."""
        response = await authenticated_async_client.get("/api/v1/repositories/")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert "items" in data
        assert "total" in data
        assert "page" in data
        assert "page_size" in data

        assert len(data["items"]) == 1
        assert data["total"] == 1
        assert data["items"][0]["id"] == test_repository.id

    @pytest.mark.asyncio
    async def test_list_repositories_pagination(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test repository listing pagination."""
        # First page
        response = await authenticated_async_client.get(
            "/api/v1/repositories/?page=1&page_size=2"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert len(data["items"]) == 2
        assert data["total"] == 5
        assert data["has_next"] is True
        assert data["has_prev"] is False

        # Second page
        response = await authenticated_async_client.get(
            "/api/v1/repositories/?page=2&page_size=2"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert len(data["items"]) == 2
        assert data["has_next"] is True
        assert data["has_prev"] is True

    @pytest.mark.asyncio
    async def test_list_repositories_filter_by_provider(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test filtering repositories by provider."""
        response = await authenticated_async_client.get(
            "/api/v1/repositories/?provider=github"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # All test repos are GitHub
        assert data["total"] == 5

    @pytest.mark.asyncio
    async def test_list_repositories_filter_indexed_only(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test filtering for indexed repositories only."""
        response = await authenticated_async_client.get(
            "/api/v1/repositories/?indexed_only=true"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        # Only first 3 test repos are indexed
        assert data["total"] == 3
        assert all(item["is_indexed"] for item in data["items"])

    @pytest.mark.asyncio
    async def test_list_repositories_empty(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test listing repositories when none exist."""
        response = await authenticated_async_client.get("/api/v1/repositories/")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["items"] == []
        assert data["total"] == 0


class TestGetRepository:
    """Test get single repository endpoint."""

    @pytest.mark.asyncio
    async def test_get_repository_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test getting a repository by ID."""
        response = await authenticated_async_client.get(
            f"/api/v1/repositories/{test_repository.id}"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["id"] == test_repository.id
        assert data["name"] == test_repository.name
        assert data["provider"] == test_repository.provider

    @pytest.mark.asyncio
    async def test_get_repository_not_found(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test getting non-existent repository."""
        response = await authenticated_async_client.get(
            "/api/v1/repositories/non-existent-id"
        )

        assert response.status_code == status.HTTP_404_NOT_FOUND


class TestUpdateRepository:
    """Test repository update endpoint."""

    @pytest.mark.asyncio
    async def test_update_repository_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test updating repository details."""
        update_data = {
            "name": "updated-repo-name",
            "description": "Updated description",
        }

        response = await authenticated_async_client.patch(
            f"/api/v1/repositories/{test_repository.id}", json=update_data
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["name"] == update_data["name"]
        assert data["description"] == update_data["description"]

    @pytest.mark.asyncio
    async def test_update_repository_not_found(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test updating non-existent repository."""
        update_data = {"name": "new-name"}

        response = await authenticated_async_client.patch(
            "/api/v1/repositories/non-existent-id", json=update_data
        )

        assert response.status_code == status.HTTP_404_NOT_FOUND


class TestDeleteRepository:
    """Test repository deletion endpoint."""

    @pytest.mark.asyncio
    async def test_delete_repository_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test deleting a repository."""
        response = await authenticated_async_client.delete(
            f"/api/v1/repositories/{test_repository.id}"
        )

        assert response.status_code == status.HTTP_204_NO_CONTENT

        # Verify it's deleted
        get_response = await authenticated_async_client.get(
            f"/api/v1/repositories/{test_repository.id}"
        )
        assert get_response.status_code == status.HTTP_404_NOT_FOUND

    @pytest.mark.asyncio
    async def test_delete_repository_not_found(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test deleting non-existent repository."""
        response = await authenticated_async_client.delete(
            "/api/v1/repositories/non-existent-id"
        )

        assert response.status_code == status.HTTP_404_NOT_FOUND


class TestReindexRepository:
    """Test repository reindexing endpoint."""

    @pytest.mark.asyncio
    async def test_reindex_repository_success(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test triggering repository reindex."""
        response = await authenticated_async_client.post(
            f"/api/v1/repositories/{test_repository.id}/reindex"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["indexing_status"] == "pending"
        assert data["is_indexed"] is False

    @pytest.mark.asyncio
    async def test_reindex_repository_not_found(
        self,
        authenticated_async_client: AsyncClient,
    ):
        """Test reindexing non-existent repository."""
        response = await authenticated_async_client.post(
            "/api/v1/repositories/non-existent-id/reindex"
        )

        assert response.status_code == status.HTTP_404_NOT_FOUND


class TestRepositoryStats:
    """Test repository statistics endpoints."""

    @pytest.mark.asyncio
    async def test_get_global_stats(
        self,
        authenticated_async_client: AsyncClient,
        multiple_test_repositories,
    ):
        """Test getting global repository statistics."""
        response = await authenticated_async_client.get("/api/v1/repositories/stats")

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["total_repositories"] == 5
        assert data["indexed_repositories"] == 3
        assert data["pending_repositories"] == 2
        assert data["failed_repositories"] == 0
        assert data["total_files"] > 0
        assert data["total_lines"] > 0

    @pytest.mark.asyncio
    async def test_get_single_repository_stats(
        self,
        authenticated_async_client: AsyncClient,
        test_repository,
    ):
        """Test getting statistics for a single repository."""
        response = await authenticated_async_client.get(
            f"/api/v1/repositories/{test_repository.id}/stats"
        )

        assert response.status_code == status.HTTP_200_OK
        data = response.json()

        assert data["repository_id"] == test_repository.id
        assert data["name"] == test_repository.name
        assert data["total_files"] == test_repository.total_files
        assert data["total_lines"] == test_repository.total_lines
        assert data["languages"] == test_repository.languages
        assert data["primary_language"] == test_repository.primary_language
