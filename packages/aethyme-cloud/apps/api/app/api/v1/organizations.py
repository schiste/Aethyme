"""Organization management endpoints."""

from typing import List
from fastapi import APIRouter, Depends, HTTPException, status
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.core.database import get_db
from app.core.deps import get_current_user, get_current_organization, get_current_superuser
from app.models.user import User
from app.models.organization import Organization
from app.models.repository import Repository
from app.schemas.organization import (
    OrganizationResponse,
    OrganizationUpdate,
    OrganizationStats,
)

router = APIRouter()


@router.get("/me", response_model=OrganizationResponse)
async def get_current_organization_details(
    organization: Organization = Depends(get_current_organization),
):
    """Get current user's organization."""
    return OrganizationResponse.model_validate(organization)


@router.patch("/me", response_model=OrganizationResponse)
async def update_current_organization(
    org_update: OrganizationUpdate,
    organization: Organization = Depends(get_current_organization),
    db: AsyncSession = Depends(get_db),
):
    """Update current organization."""
    # Update fields
    if org_update.name is not None:
        organization.name = org_update.name

    await db.commit()
    await db.refresh(organization)

    return OrganizationResponse.model_validate(organization)


@router.get("/me/stats", response_model=OrganizationStats)
async def get_organization_stats(
    organization: Organization = Depends(get_current_organization),
    db: AsyncSession = Depends(get_db),
):
    """Get organization usage statistics."""
    # Count users
    user_count_result = await db.execute(
        select(func.count(User.id)).where(User.organization_id == organization.id)
    )
    total_users = user_count_result.scalar_one()

    # Count repositories
    repo_count_result = await db.execute(
        select(func.count(Repository.id)).where(Repository.organization_id == organization.id)
    )
    total_repositories = repo_count_result.scalar_one()

    return OrganizationStats(
        total_users=total_users,
        total_repositories=total_repositories,
        queries_this_month=organization.current_queries_this_month,
        queries_remaining=organization.max_queries_per_month - organization.current_queries_this_month,
        repositories_remaining=organization.max_repositories - total_repositories,
    )


@router.get("/{organization_id}", response_model=OrganizationResponse)
async def get_organization_by_id(
    organization_id: str,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_superuser),
):
    """Get organization by ID (superuser only)."""
    result = await db.execute(
        select(Organization).where(Organization.id == organization_id)
    )
    organization = result.scalar_one_or_none()

    if not organization:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Organization not found"
        )

    return OrganizationResponse.model_validate(organization)


@router.get("/", response_model=List[OrganizationResponse])
async def list_organizations(
    skip: int = 0,
    limit: int = 100,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_superuser),
):
    """List all organizations (superuser only)."""
    result = await db.execute(
        select(Organization).offset(skip).limit(limit)
    )
    organizations = result.scalars().all()

    return [OrganizationResponse.model_validate(org) for org in organizations]
