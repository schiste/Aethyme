"""FastAPI dependencies for authentication and authorization."""

from datetime import UTC, datetime
from typing import Optional

from fastapi import Depends, HTTPException, status
from fastapi.security import HTTPBearer, HTTPAuthorizationCredentials, APIKeyHeader
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import get_db
from app.core.security import decode_access_token
from app.core.api_keys import verify_api_key, get_key_prefix
from app.models.user import User
from app.models.organization import Organization
from app.models.api_key import APIKey

# HTTP Bearer token scheme
security = HTTPBearer(auto_error=False)

# API Key header scheme
api_key_header = APIKeyHeader(name="X-API-Key", auto_error=False)


async def get_current_user(
    credentials: Optional[HTTPAuthorizationCredentials] = Depends(security),
    api_key: Optional[str] = Depends(api_key_header),
    db: AsyncSession = Depends(get_db),
) -> User:
    """
    Get the current authenticated user from JWT token or API key.

    Supports two authentication methods:
    1. JWT Bearer token (Authorization: Bearer <token>)
    2. API Key (X-API-Key: <key>)

    Raises:
        HTTPException: If authentication fails or user not found
    """
    credentials_exception = HTTPException(
        status_code=status.HTTP_401_UNAUTHORIZED,
        detail="Not authenticated",
        headers={"WWW-Authenticate": "Bearer"},
    )

    # Try API Key authentication first
    if api_key:
        # Get the key prefix to narrow down search
        key_prefix = get_key_prefix(api_key)

        # Find API keys with matching prefix (more efficient than scanning all)
        result = await db.execute(
            select(APIKey).where(
                APIKey.key_prefix == key_prefix,
                APIKey.is_active.is_(True),
            )
        )
        api_key_records = result.scalars().all()

        # Verify the API key against each candidate
        for api_key_record in api_key_records:
            if verify_api_key(api_key, api_key_record.key_hash):
                # Check if key is expired
                if api_key_record.expires_at and api_key_record.expires_at < datetime.now(UTC):
                    raise HTTPException(
                        status_code=status.HTTP_401_UNAUTHORIZED,
                        detail="API key has expired"
                    )

                # Update last_used_at timestamp
                api_key_record.last_used_at = datetime.now(UTC)
                await db.commit()

                # Get the user associated with this API key's organization
                # For API keys, we need to get the first active user in the organization
                result = await db.execute(
                    select(User).where(
                        User.organization_id == api_key_record.organization_id,
                        User.is_active.is_(True),
                    ).limit(1)
                )
                user = result.scalar_one_or_none()

                if user:
                    return user
                break

    # Try JWT Bearer token authentication
    if credentials:
        # Decode token
        token = credentials.credentials
        payload = decode_access_token(token)

        if payload is not None:
            user_id: str = payload.get("sub")
            if user_id:
                # Get user from database
                result = await db.execute(select(User).where(User.id == user_id))
                user = result.scalar_one_or_none()

                if user and user.is_active:
                    return user

    # Neither authentication method worked
    raise credentials_exception


async def get_current_active_user(
    current_user: User = Depends(get_current_user),
) -> User:
    """Get current active user."""
    if not current_user.is_active:
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="Inactive user"
        )
    return current_user


async def get_current_superuser(
    current_user: User = Depends(get_current_user),
) -> User:
    """Get current superuser."""
    if not current_user.is_superuser:
        raise HTTPException(
            status_code=status.HTTP_403_FORBIDDEN,
            detail="Not enough permissions"
        )
    return current_user


async def get_current_organization(
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
) -> Organization:
    """
    Get the current user's organization.

    Raises:
        HTTPException: If user has no organization
    """
    if not current_user.organization_id:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="User has no organization"
        )

    result = await db.execute(
        select(Organization).where(Organization.id == current_user.organization_id)
    )
    organization = result.scalar_one_or_none()

    if organization is None:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Organization not found"
        )

    return organization
