"""Dashboard statistics and metrics endpoints."""

from fastapi import APIRouter, Depends
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.core.database import get_db
from app.core.deps import get_current_organization
from app.models.organization import Organization
from app.models.repository import Repository
from app.schemas.dashboard import DashboardStatsResponse, LanguageStats

router = APIRouter()


@router.get("/stats", response_model=DashboardStatsResponse)
async def get_dashboard_stats(
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Get comprehensive dashboard statistics for the organization.

    Returns:
    - Total repositories
    - Total files indexed
    - Total lines of code
    - Language breakdown with percentages
    - Indexing queue status
    """
    # Total repositories
    total_repos_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id
        )
    )
    total_repositories = total_repos_result.scalar_one()

    # Indexed repositories
    indexed_repos_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.is_indexed.is_(True),
        )
    )
    indexed_repositories = indexed_repos_result.scalar_one()

    # Pending/Indexing
    indexing_repos_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.indexing_status.in_(["pending", "in_progress"]),
        )
    )
    indexing_repositories = indexing_repos_result.scalar_one()

    # Failed
    failed_repos_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.indexing_status == "failed",
        )
    )
    failed_repositories = failed_repos_result.scalar_one()

    # Total files - sum from indexed repositories
    total_files_result = await db.execute(
        select(func.sum(Repository.file_count)).where(
            Repository.organization_id == organization.id,
            Repository.is_indexed.is_(True),
        )
    )
    total_files = total_files_result.scalar_one() or 0

    # Total lines - sum from indexed repositories
    total_lines_result = await db.execute(
        select(func.sum(Repository.line_count)).where(
            Repository.organization_id == organization.id,
            Repository.is_indexed.is_(True),
        )
    )
    total_lines = total_lines_result.scalar_one() or 0

    # Get all repositories for language aggregation
    repos_result = await db.execute(
        select(Repository).where(
            Repository.organization_id == organization.id,
            Repository.is_indexed.is_(True),
        )
    )
    repositories = repos_result.scalars().all()

    # Aggregate languages across all repositories
    language_file_counts = {}
    language_line_counts = {}

    for repo in repositories:
        if repo.languages:
            for language, metrics in repo.languages.items():
                # Aggregate file counts
                if language not in language_file_counts:
                    language_file_counts[language] = 0
                    language_line_counts[language] = 0

                # Assuming metrics format is {"files": X, "lines": Y, "percentage": Z}
                # Adjust if different
                if isinstance(metrics, dict):
                    language_file_counts[language] += metrics.get('files', 0)
                    language_line_counts[language] += metrics.get('lines', 0)

    # Calculate language breakdown with percentages
    language_breakdown = []
    if total_files > 0:
        for language, file_count in sorted(
            language_file_counts.items(),
            key=lambda x: x[1],
            reverse=True
        ):
            percentage = (file_count / total_files) * 100
            language_breakdown.append(
                LanguageStats(
                    language=language,
                    file_count=file_count,
                    line_count=language_line_counts[language],
                    percentage=round(percentage, 2)
                )
            )

    return DashboardStatsResponse(
        total_repositories=total_repositories,
        indexed_repositories=indexed_repositories,
        indexing_repositories=indexing_repositories,
        failed_repositories=failed_repositories,
        total_files=total_files,
        total_lines=total_lines,
        languages=language_breakdown[:10],  # Top 10 languages
    )
