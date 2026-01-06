"""
Tests for Repository model.
"""
import pytest
from datetime import datetime
from sqlalchemy.exc import IntegrityError
from app.models import Repository, Organization


class TestRepositoryModel:
    """Test Repository model validation and constraints."""

    @pytest.mark.asyncio
    async def test_create_repository(self, db_session, test_organization):
        """Test creating a valid repository."""
        repo = Repository(
            id="repo-123",
            organization_id=test_organization.id,
            name="test-repo",
            full_name="org/test-repo",
            url="https://github.com/org/test-repo",
            provider="github",
            provider_id="1234567",
            default_branch="main",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.id == "repo-123"
        assert repo.organization_id == test_organization.id
        assert repo.name == "test-repo"
        assert repo.full_name == "org/test-repo"
        assert repo.provider == "github"
        assert repo.provider_id == "1234567"

    @pytest.mark.asyncio
    async def test_repository_name_required(self, db_session, test_organization):
        """Test that name is required."""
        repo = Repository(
            id="repo-no-name",
            organization_id=test_organization.id,
            full_name="org/no-name",
            url="https://github.com/org/no-name",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_repository_organization_required(self, db_session):
        """Test that organization_id is required."""
        repo = Repository(
            id="repo-no-org",
            name="no-org-repo",
            full_name="org/no-org-repo",
            url="https://github.com/org/no-org-repo",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_repository_defaults(self, db_session, test_organization):
        """Test default values for repository fields."""
        repo = Repository(
            id="repo-defaults",
            organization_id=test_organization.id,
            name="defaults-repo",
            full_name="org/defaults-repo",
            url="https://github.com/org/defaults-repo",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        # Check defaults
        assert repo.default_branch == "main"
        assert repo.is_private is False
        assert repo.is_indexed is False
        assert repo.indexing_status == "pending"
        assert repo.file_count == 0
        assert repo.line_count == 0
        assert repo.stars == 0
        assert repo.forks == 0
        assert repo.total_files == 0
        assert repo.total_lines == 0
        assert repo.created_at is not None
        assert repo.updated_at is not None

    @pytest.mark.asyncio
    async def test_repository_timestamps(self, db_session, test_organization):
        """Test that timestamps are set correctly."""
        repo = Repository(
            id="repo-timestamps",
            organization_id=test_organization.id,
            name="timestamps-repo",
            full_name="org/timestamps-repo",
            url="https://github.com/org/timestamps-repo",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.created_at is not None
        assert repo.updated_at is not None
        assert repo.created_at == repo.updated_at

    @pytest.mark.asyncio
    async def test_repository_str_representation(self, db_session, test_organization):
        """Test repository string representation."""
        repo = Repository(
            id="repo-str",
            organization_id=test_organization.id,
            name="str-repo",
            full_name="org/str-repo",
            url="https://github.com/org/str-repo",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)
        await db_session.commit()

        # String representation should include full_name
        repo_str = str(repo)
        assert "org/str-repo" in repo_str or "repo-str" in repo_str


class TestRepositoryProviders:
    """Test Repository provider functionality."""

    @pytest.mark.asyncio
    async def test_github_repository(self, db_session, test_organization):
        """Test GitHub repository creation."""
        repo = Repository(
            id="repo-github",
            organization_id=test_organization.id,
            name="github-repo",
            full_name="owner/github-repo",
            url="https://github.com/owner/github-repo",
            provider="github",
            provider_id="gh-123",
            github_id="123456",
            clone_url="https://github.com/owner/github-repo.git",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.provider == "github"
        assert repo.github_id == "123456"
        assert repo.clone_url == "https://github.com/owner/github-repo.git"

    @pytest.mark.asyncio
    async def test_gitlab_repository(self, db_session, test_organization):
        """Test GitLab repository creation."""
        repo = Repository(
            id="repo-gitlab",
            organization_id=test_organization.id,
            name="gitlab-repo",
            full_name="owner/gitlab-repo",
            url="https://gitlab.com/owner/gitlab-repo",
            provider="gitlab",
            provider_id="gl-123",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.provider == "gitlab"

    @pytest.mark.asyncio
    async def test_bitbucket_repository(self, db_session, test_organization):
        """Test Bitbucket repository creation."""
        repo = Repository(
            id="repo-bitbucket",
            organization_id=test_organization.id,
            name="bitbucket-repo",
            full_name="owner/bitbucket-repo",
            url="https://bitbucket.org/owner/bitbucket-repo",
            provider="bitbucket",
            provider_id="bb-123",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.provider == "bitbucket"


class TestRepositoryIndexing:
    """Test Repository indexing functionality."""

    @pytest.mark.asyncio
    async def test_pending_indexing_status(self, db_session, test_organization):
        """Test repository with pending indexing status."""
        repo = Repository(
            id="repo-pending",
            organization_id=test_organization.id,
            name="pending-repo",
            full_name="org/pending-repo",
            url="https://github.com/org/pending-repo",
            provider="github",
            provider_id="1234",
            indexing_status="pending",
            is_indexed=False,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.indexing_status == "pending"
        assert repo.is_indexed is False

    @pytest.mark.asyncio
    async def test_completed_indexing_status(self, db_session, test_organization):
        """Test repository with completed indexing status."""
        now = datetime.utcnow()
        repo = Repository(
            id="repo-indexed",
            organization_id=test_organization.id,
            name="indexed-repo",
            full_name="org/indexed-repo",
            url="https://github.com/org/indexed-repo",
            provider="github",
            provider_id="1234",
            indexing_status="completed",
            is_indexed=True,
            last_indexed_at=now,
            indexed_at=now,
            last_commit_sha="abc123",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.indexing_status == "completed"
        assert repo.is_indexed is True
        assert repo.last_indexed_at is not None
        assert repo.last_commit_sha == "abc123"

    @pytest.mark.asyncio
    async def test_failed_indexing_status(self, db_session, test_organization):
        """Test repository with failed indexing status."""
        repo = Repository(
            id="repo-failed",
            organization_id=test_organization.id,
            name="failed-repo",
            full_name="org/failed-repo",
            url="https://github.com/org/failed-repo",
            provider="github",
            provider_id="1234",
            indexing_status="failed",
            is_indexed=False,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.indexing_status == "failed"
        assert repo.is_indexed is False


class TestRepositoryStatistics:
    """Test Repository statistics functionality."""

    @pytest.mark.asyncio
    async def test_repository_file_counts(self, db_session, test_organization):
        """Test repository file count tracking."""
        repo = Repository(
            id="repo-stats",
            organization_id=test_organization.id,
            name="stats-repo",
            full_name="org/stats-repo",
            url="https://github.com/org/stats-repo",
            provider="github",
            provider_id="1234",
            file_count=150,
            total_files=150,
            line_count=5000,
            total_lines=5000,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.file_count == 150
        assert repo.total_files == 150
        assert repo.line_count == 5000
        assert repo.total_lines == 5000

    @pytest.mark.asyncio
    async def test_repository_language_stats(self, db_session, test_organization):
        """Test repository language statistics."""
        repo = Repository(
            id="repo-lang",
            organization_id=test_organization.id,
            name="lang-repo",
            full_name="org/lang-repo",
            url="https://github.com/org/lang-repo",
            provider="github",
            provider_id="1234",
            primary_language="Python",
            languages={"Python": 70, "JavaScript": 20, "TypeScript": 10},
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.primary_language == "Python"
        assert repo.languages is not None
        assert repo.languages["Python"] == 70
        assert repo.languages["JavaScript"] == 20

    @pytest.mark.asyncio
    async def test_repository_stars_forks(self, db_session, test_organization):
        """Test repository stars and forks tracking."""
        repo = Repository(
            id="repo-social",
            organization_id=test_organization.id,
            name="social-repo",
            full_name="org/social-repo",
            url="https://github.com/org/social-repo",
            provider="github",
            provider_id="1234",
            stars=1500,
            forks=250,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.stars == 1500
        assert repo.forks == 250


class TestRepositoryRelationships:
    """Test Repository model relationships."""

    @pytest.mark.asyncio
    async def test_repository_organization_relationship(self, db_session, test_organization):
        """Test repository-organization relationship."""
        repo = Repository(
            id="repo-org-rel",
            organization_id=test_organization.id,
            name="org-rel-repo",
            full_name="org/org-rel-repo",
            url="https://github.com/org/org-rel-repo",
            provider="github",
            provider_id="1234",
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        # Load relationship
        await db_session.refresh(repo, ["organization"])
        assert repo.organization.id == test_organization.id
        assert repo.organization.name == test_organization.name


class TestRepositoryQueries:
    """Test Repository model queries."""

    @pytest.mark.asyncio
    async def test_query_by_organization(self, db_session, test_organization):
        """Test querying repositories by organization."""
        from sqlalchemy import select

        # Create multiple repositories for the organization
        repo1 = Repository(
            id="repo-org-1",
            organization_id=test_organization.id,
            name="repo1",
            full_name="org/repo1",
            url="https://github.com/org/repo1",
            provider="github",
            provider_id="1",
        )
        repo2 = Repository(
            id="repo-org-2",
            organization_id=test_organization.id,
            name="repo2",
            full_name="org/repo2",
            url="https://github.com/org/repo2",
            provider="github",
            provider_id="2",
        )
        db_session.add_all([repo1, repo2])
        await db_session.commit()

        # Query repositories by organization
        result = await db_session.execute(
            select(Repository).where(Repository.organization_id == test_organization.id)
        )
        repos = result.scalars().all()

        assert len(repos) >= 2
        repo_ids = [r.id for r in repos]
        assert "repo-org-1" in repo_ids
        assert "repo-org-2" in repo_ids

    @pytest.mark.asyncio
    async def test_query_indexed_repositories(self, db_session, test_organization):
        """Test querying indexed repositories."""
        from sqlalchemy import select

        # Create indexed and non-indexed repositories
        repo_indexed = Repository(
            id="repo-indexed-query",
            organization_id=test_organization.id,
            name="indexed",
            full_name="org/indexed",
            url="https://github.com/org/indexed",
            provider="github",
            provider_id="1",
            is_indexed=True,
        )
        repo_not_indexed = Repository(
            id="repo-not-indexed",
            organization_id=test_organization.id,
            name="not-indexed",
            full_name="org/not-indexed",
            url="https://github.com/org/not-indexed",
            provider="github",
            provider_id="2",
            is_indexed=False,
        )
        db_session.add_all([repo_indexed, repo_not_indexed])
        await db_session.commit()

        # Query indexed repositories
        result = await db_session.execute(
            select(Repository).where(Repository.is_indexed == True)
        )
        indexed_repos = result.scalars().all()

        indexed_ids = [r.id for r in indexed_repos]
        assert "repo-indexed-query" in indexed_ids
        assert "repo-not-indexed" not in indexed_ids

    @pytest.mark.asyncio
    async def test_query_by_provider(self, db_session, test_organization):
        """Test querying repositories by provider."""
        from sqlalchemy import select

        # Create repositories with different providers
        repo_github = Repository(
            id="repo-github-query",
            organization_id=test_organization.id,
            name="github-query",
            full_name="org/github-query",
            url="https://github.com/org/github-query",
            provider="github",
            provider_id="1",
        )
        repo_gitlab = Repository(
            id="repo-gitlab-query",
            organization_id=test_organization.id,
            name="gitlab-query",
            full_name="org/gitlab-query",
            url="https://gitlab.com/org/gitlab-query",
            provider="gitlab",
            provider_id="2",
        )
        db_session.add_all([repo_github, repo_gitlab])
        await db_session.commit()

        # Query GitHub repositories
        result = await db_session.execute(
            select(Repository).where(Repository.provider == "github")
        )
        github_repos = result.scalars().all()

        github_ids = [r.id for r in github_repos]
        assert "repo-github-query" in github_ids


class TestRepositoryVisibility:
    """Test Repository visibility functionality."""

    @pytest.mark.asyncio
    async def test_public_repository(self, db_session, test_organization):
        """Test public repository creation."""
        repo = Repository(
            id="repo-public",
            organization_id=test_organization.id,
            name="public-repo",
            full_name="org/public-repo",
            url="https://github.com/org/public-repo",
            provider="github",
            provider_id="1234",
            is_private=False,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.is_private is False

    @pytest.mark.asyncio
    async def test_private_repository(self, db_session, test_organization):
        """Test private repository creation."""
        repo = Repository(
            id="repo-private",
            organization_id=test_organization.id,
            name="private-repo",
            full_name="org/private-repo",
            url="https://github.com/org/private-repo",
            provider="github",
            provider_id="1234",
            is_private=True,
        )
        db_session.add(repo)
        await db_session.commit()
        await db_session.refresh(repo)

        assert repo.is_private is True
