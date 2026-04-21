"""Repository management endpoints."""

import uuid
from typing import Optional
from fastapi import APIRouter, Depends, HTTPException, status, Query
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.core.database import get_db
from app.core.deps import get_current_user, get_current_organization
from app.models.user import User
from app.models.organization import Organization
from app.models.repository import Repository
from app.schemas.repository import (
    RepositoryCreate,
    RepositoryUpdate,
    RepositoryResponse,
    RepositoryStats,
    RepositoryListResponse,
    RepositoryTreeResponse,
    RepositoryStatsResponse,
)
from app.core.elasticsearch import CodeIndexer
from app.utils.tree_builder import TreeBuilder

router = APIRouter()


@router.post("/", response_model=RepositoryResponse, status_code=status.HTTP_201_CREATED)
async def create_repository(
    repo_data: RepositoryCreate,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
    organization: Organization = Depends(get_current_organization),
):
    """
    Create a new repository connection.
    
    Links an external repository (GitHub/GitLab/Bitbucket) to the organization.
    """
    # Check if repository already exists
    result = await db.execute(
        select(Repository).where(
            Repository.organization_id == organization.id,
            Repository.provider == repo_data.provider,
            Repository.provider_id == repo_data.provider_id,
        )
    )
    existing_repo = result.scalar_one_or_none()
    
    if existing_repo:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Repository already connected to this organization"
        )
    
    # Create repository
    repo_id = str(uuid.uuid4())
    repository = Repository(
        id=repo_id,
        organization_id=organization.id,
        name=repo_data.name,
        full_name=repo_data.full_name,
        url=str(repo_data.url),
        description=repo_data.description,
        provider=repo_data.provider,
        provider_id=repo_data.provider_id,
        default_branch=repo_data.default_branch,
        is_private=repo_data.is_private,
        indexing_status="pending",
    )
    db.add(repository)
    
    await db.commit()
    await db.refresh(repository)
    
    # TODO: Trigger indexing worker (Week 5)
    
    return RepositoryResponse.model_validate(repository)


@router.get("/", response_model=RepositoryListResponse)
async def list_repositories(
    page: int = Query(1, ge=1),
    page_size: int = Query(50, ge=1, le=100),
    provider: Optional[str] = Query(None, pattern="^(github|gitlab|bitbucket)$"),
    indexed_only: bool = Query(False),
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    List all repositories for current organization.
    
    Supports pagination and filtering.
    """
    # Build query
    query = select(Repository).where(Repository.organization_id == organization.id)
    
    if provider:
        query = query.where(Repository.provider == provider)
    
    if indexed_only:
        query = query.where(Repository.is_indexed.is_(True))
    
    # Get total count
    count_query = select(func.count(Repository.id)).where(
        Repository.organization_id == organization.id
    )
    if provider:
        count_query = count_query.where(Repository.provider == provider)
    if indexed_only:
        count_query = count_query.where(Repository.is_indexed.is_(True))
    
    total_result = await db.execute(count_query)
    total = total_result.scalar_one()
    
    # Apply pagination
    offset = (page - 1) * page_size
    query = query.offset(offset).limit(page_size).order_by(Repository.created_at.desc())
    
    result = await db.execute(query)
    repositories = result.scalars().all()
    
    return RepositoryListResponse(
        items=[RepositoryResponse.model_validate(repo) for repo in repositories],
        total=total,
        page=page,
        page_size=page_size,
        has_next=offset + page_size < total,
        has_prev=page > 1,
    )


@router.get("/stats", response_model=RepositoryStats)
async def get_repository_stats(
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """Get repository statistics for current organization."""
    # Total repositories
    total_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id
        )
    )
    total = total_result.scalar_one()
    
    # Indexed repositories
    indexed_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.is_indexed.is_(True),
        )
    )
    indexed = indexed_result.scalar_one()
    
    # Pending repositories
    pending_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.indexing_status == "pending",
        )
    )
    pending = pending_result.scalar_one()
    
    # Failed repositories
    failed_result = await db.execute(
        select(func.count(Repository.id)).where(
            Repository.organization_id == organization.id,
            Repository.indexing_status == "failed",
        )
    )
    failed = failed_result.scalar_one()
    
    # Total files and lines
    files_result = await db.execute(
        select(func.sum(Repository.file_count)).where(
            Repository.organization_id == organization.id
        )
    )
    total_files = files_result.scalar_one() or 0
    
    lines_result = await db.execute(
        select(func.sum(Repository.line_count)).where(
            Repository.organization_id == organization.id
        )
    )
    total_lines = lines_result.scalar_one() or 0
    
    return RepositoryStats(
        total_repositories=total,
        indexed_repositories=indexed,
        pending_repositories=pending,
        failed_repositories=failed,
        total_files=total_files,
        total_lines=total_lines,
    )


@router.get("/{repository_id}", response_model=RepositoryResponse)
async def get_repository(
    repository_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """Get repository by ID."""
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()
    
    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )
    
    return RepositoryResponse.model_validate(repository)


@router.patch("/{repository_id}", response_model=RepositoryResponse)
async def update_repository(
    repository_id: str,
    repo_update: RepositoryUpdate,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """Update repository details."""
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()
    
    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )
    
    # Update fields
    if repo_update.name is not None:
        repository.name = repo_update.name
    if repo_update.description is not None:
        repository.description = repo_update.description
    if repo_update.default_branch is not None:
        repository.default_branch = repo_update.default_branch
    
    await db.commit()
    await db.refresh(repository)
    
    return RepositoryResponse.model_validate(repository)


@router.delete("/{repository_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_repository(
    repository_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Disconnect and delete a repository.
    
    This will remove the repository connection and all indexed data.
    """
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()
    
    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )
    
    await db.delete(repository)
    await db.commit()
    
    # TODO: Delete indexed data from database (Week 5)
    
    return None


@router.post("/{repository_id}/reindex", response_model=RepositoryResponse)
async def reindex_repository(
    repository_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Trigger re-indexing of a repository.

    Useful when code has changed or initial indexing failed.
    """
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()

    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )

    # Update status to pending
    repository.indexing_status = "pending"
    repository.is_indexed = False

    await db.commit()
    await db.refresh(repository)

    # TODO: Trigger indexing worker (Week 5)

    return RepositoryResponse.model_validate(repository)


@router.get("/{repository_id}/tree", response_model=RepositoryTreeResponse)
async def get_repository_tree(
    repository_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Get repository file tree structure.

    Returns a hierarchical tree of all files and folders in the repository.
    """
    # Verify repository access
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()

    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )

    if not repository.is_indexed:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Repository has not been indexed yet"
        )

    # Get all files from Elasticsearch
    indexer = CodeIndexer()
    try:
        files_data = indexer.get_all_files(repository_id)
    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to retrieve file tree: {str(e)}"
        )

    # Build tree structure
    tree_builder = TreeBuilder()
    tree = tree_builder.build_tree(files_data)

    # Count folders and files
    total_folders = tree_builder.count_folders(tree)
    total_files = tree_builder.count_files(tree)

    return RepositoryTreeResponse(
        repository_id=repository_id,
        repository_name=repository.name,
        total_files=total_files,
        total_folders=total_folders,
        tree=tree,
        indexed_at=repository.indexed_at
    )


@router.get("/{repository_id}/stats", response_model=RepositoryStatsResponse)
async def get_single_repository_stats(
    repository_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Get detailed statistics for a specific repository.

    Includes file counts, line counts, languages, and metadata.
    """
    result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == organization.id,
        )
    )
    repository = result.scalar_one_or_none()

    if not repository:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )

    return RepositoryStatsResponse(
        repository_id=repository.id,
        name=repository.name,
        full_name=repository.full_name,
        url=repository.url,
        description=repository.description,
        stars=repository.stars or 0,
        forks=repository.forks or 0,
        total_files=repository.total_files or 0,
        total_lines=repository.total_lines or 0,
        languages=repository.languages or {},
        primary_language=repository.primary_language,
        indexed_at=repository.indexed_at,
        created_at=repository.created_at,
        updated_at=repository.updated_at
    )
