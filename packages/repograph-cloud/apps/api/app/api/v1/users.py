"""User management endpoints."""

from typing import List
from fastapi import APIRouter, Depends, HTTPException, status
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select

from app.core.database import get_db
from app.core.deps import get_current_user, get_current_superuser
from app.models.user import User
from app.schemas.user import UserResponse, UserUpdate

router = APIRouter()


@router.get("/me", response_model=UserResponse)
async def get_current_user_profile(
    current_user: User = Depends(get_current_user),
):
    """Get current user profile."""
    return UserResponse.model_validate(current_user)


@router.patch("/me", response_model=UserResponse)
async def update_current_user_profile(
    user_update: UserUpdate,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Update current user profile."""
    # Update fields
    if user_update.full_name is not None:
        current_user.full_name = user_update.full_name
    if user_update.avatar_url is not None:
        current_user.avatar_url = user_update.avatar_url

    await db.commit()
    await db.refresh(current_user)

    return UserResponse.model_validate(current_user)


@router.get("/{user_id}", response_model=UserResponse)
async def get_user_by_id(
    user_id: str,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get user by ID.

    Only accessible by superusers or users in the same organization.
    """
    result = await db.execute(select(User).where(User.id == user_id))
    user = result.scalar_one_or_none()

    if not user:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="User not found"
        )

    # Check permissions
    if not current_user.is_superuser and user.organization_id != current_user.organization_id:
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="Not enough permissions"
        )

    return UserResponse.model_validate(user)


@router.get("/", response_model=List[UserResponse])
async def list_users(
    skip: int = 0,
    limit: int = 100,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    List users in current organization.

    Superusers can see all users.
    """
    if current_user.is_superuser:
        # Superuser can see all users
        result = await db.execute(
            select(User).offset(skip).limit(limit)
        )
    else:
        # Regular users can only see users in their organization
        result = await db.execute(
            select(User)
            .where(User.organization_id == current_user.organization_id)
            .offset(skip)
            .limit(limit)
        )

    users = result.scalars().all()
    return [UserResponse.model_validate(user) for user in users]


@router.delete("/{user_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_user(
    user_id: str,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_superuser),
):
    """
    Delete a user (superuser only).

    Soft delete by setting is_active to False.
    """
    result = await db.execute(select(User).where(User.id == user_id))
    user = result.scalar_one_or_none()

    if not user:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="User not found"
        )

    # Soft delete
    user.is_active = False
    await db.commit()

    return None
