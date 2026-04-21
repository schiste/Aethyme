"""API Key management endpoints."""

import uuid
from fastapi import APIRouter, Depends, HTTPException, status
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.core.database import get_db
from app.core.deps import get_current_user, get_current_organization
from app.core.api_keys import (
    generate_api_key,
    hash_api_key,
    get_key_prefix,
    calculate_expiration,
)
from app.models.user import User
from app.models.organization import Organization
from app.models.api_key import APIKey
from app.schemas.api_key import (
    APIKeyCreate,
    APIKeyUpdate,
    APIKeyResponse,
    APIKeyCreateResponse,
    APIKeyListResponse,
)

router = APIRouter()


@router.post("/", response_model=APIKeyCreateResponse, status_code=status.HTTP_201_CREATED)
async def create_api_key(
    key_data: APIKeyCreate,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
    organization: Organization = Depends(get_current_organization),
):
    """
    Create a new API key.
    
    The full key is returned ONLY ONCE. Save it immediately!
    """
    # Generate secure API key
    api_key = generate_api_key()
    key_hash = hash_api_key(api_key)
    prefix = get_key_prefix(api_key)
    
    # Calculate expiration
    expires_at = calculate_expiration(key_data.expires_in_days)
    
    # Create API key record
    key_id = str(uuid.uuid4())
    api_key_record = APIKey(
        id=key_id,
        organization_id=organization.id,
        name=key_data.name,
        key_prefix=prefix,
        key_hash=key_hash,
        scopes=",".join(key_data.scopes) if key_data.scopes else None,
        is_active=True,
        expires_at=expires_at,
    )
    db.add(api_key_record)
    
    await db.commit()
    await db.refresh(api_key_record)

    # Return response with full key (only time it's shown)
    # Parse scopes back to list format
    scopes_list = api_key_record.scopes.split(",") if api_key_record.scopes else None

    return APIKeyCreateResponse(
        id=api_key_record.id,
        organization_id=api_key_record.organization_id,
        name=api_key_record.name,
        key_prefix=api_key_record.key_prefix,
        scopes=scopes_list,
        is_active=api_key_record.is_active,
        last_used_at=api_key_record.last_used_at,
        expires_at=api_key_record.expires_at,
        created_at=api_key_record.created_at,
        updated_at=api_key_record.updated_at,
        key=api_key,  # The full key - only shown once
    )


@router.get("/", response_model=APIKeyListResponse)
async def list_api_keys(
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """List all API keys for current organization."""
    result = await db.execute(
        select(APIKey)
        .where(APIKey.organization_id == organization.id)
        .order_by(APIKey.created_at.desc())
    )
    api_keys = result.scalars().all()
    
    # Get total count
    count_result = await db.execute(
        select(func.count(APIKey.id)).where(APIKey.organization_id == organization.id)
    )
    total = count_result.scalar_one()

    # Convert API keys to response objects with proper scope formatting
    items = []
    for key in api_keys:
        scopes_list = key.scopes.split(",") if key.scopes else None
        items.append(APIKeyResponse(
            id=key.id,
            organization_id=key.organization_id,
            name=key.name,
            key_prefix=key.key_prefix,
            scopes=scopes_list,
            is_active=key.is_active,
            last_used_at=key.last_used_at,
            expires_at=key.expires_at,
            created_at=key.created_at,
            updated_at=key.updated_at,
        ))

    return APIKeyListResponse(items=items, total=total)


@router.get("/{key_id}", response_model=APIKeyResponse)
async def get_api_key(
    key_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """Get API key by ID."""
    result = await db.execute(
        select(APIKey).where(
            APIKey.id == key_id,
            APIKey.organization_id == organization.id,
        )
    )
    api_key = result.scalar_one_or_none()
    
    if not api_key:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="API key not found"
        )

    scopes_list = api_key.scopes.split(",") if api_key.scopes else None
    return APIKeyResponse(
        id=api_key.id,
        organization_id=api_key.organization_id,
        name=api_key.name,
        key_prefix=api_key.key_prefix,
        scopes=scopes_list,
        is_active=api_key.is_active,
        last_used_at=api_key.last_used_at,
        expires_at=api_key.expires_at,
        created_at=api_key.created_at,
        updated_at=api_key.updated_at,
    )


@router.patch("/{key_id}", response_model=APIKeyResponse)
async def update_api_key(
    key_id: str,
    key_update: APIKeyUpdate,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """Update API key details."""
    result = await db.execute(
        select(APIKey).where(
            APIKey.id == key_id,
            APIKey.organization_id == organization.id,
        )
    )
    api_key = result.scalar_one_or_none()
    
    if not api_key:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="API key not found"
        )
    
    # Update fields
    if key_update.name is not None:
        api_key.name = key_update.name
    if key_update.scopes is not None:
        api_key.scopes = ",".join(key_update.scopes) if key_update.scopes else None
    if key_update.is_active is not None:
        api_key.is_active = key_update.is_active

    await db.commit()
    await db.refresh(api_key)

    scopes_list = api_key.scopes.split(",") if api_key.scopes else None
    return APIKeyResponse(
        id=api_key.id,
        organization_id=api_key.organization_id,
        name=api_key.name,
        key_prefix=api_key.key_prefix,
        scopes=scopes_list,
        is_active=api_key.is_active,
        last_used_at=api_key.last_used_at,
        expires_at=api_key.expires_at,
        created_at=api_key.created_at,
        updated_at=api_key.updated_at,
    )


@router.delete("/{key_id}", status_code=status.HTTP_204_NO_CONTENT)
async def revoke_api_key(
    key_id: str,
    db: AsyncSession = Depends(get_db),
    organization: Organization = Depends(get_current_organization),
):
    """
    Revoke an API key.
    
    This permanently deletes the key. Active sessions will fail immediately.
    """
    result = await db.execute(
        select(APIKey).where(
            APIKey.id == key_id,
            APIKey.organization_id == organization.id,
        )
    )
    api_key = result.scalar_one_or_none()
    
    if not api_key:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="API key not found"
        )
    
    await db.delete(api_key)
    await db.commit()
    
    return None
