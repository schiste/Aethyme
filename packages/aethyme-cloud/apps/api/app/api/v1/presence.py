"""REST API endpoints for presence management."""

from typing import List, Optional
from uuid import UUID
from fastapi import APIRouter, Depends, HTTPException, Query, status
from sqlalchemy.ext.asyncio import AsyncSession
from pydantic import BaseModel

from app.core.database import get_db
from app.core.auth import get_current_user
from app.models.user import User
from app.services.presence.presence_manager import presence_manager

router = APIRouter()


class PresenceResponse(BaseModel):
    """Presence response model."""

    id: str
    user_id: str
    session_id: str
    status: str
    location: str
    cursor: Optional[dict] = None
    selection: Optional[dict] = None
    last_seen: str
    joined_at: str
    metadata: dict = {}

    class Config:
        from_attributes = True


class LocationUsersResponse(BaseModel):
    """Response model for users at a location."""

    location: str
    users: List[PresenceResponse]
    count: int


@router.get("/online", response_model=List[PresenceResponse])
async def get_online_users(
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get all currently online users.
    """
    presences = await presence_manager.get_online_users(db)
    return [PresenceResponse(**p.to_dict()) for p in presences]


@router.get("/user/{user_id}", response_model=List[PresenceResponse])
async def get_user_presence(
    user_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get presence information for a specific user.
    """
    presences = await presence_manager.get_user_presence(db, user_id)
    return [PresenceResponse(**p.to_dict()) for p in presences]


@router.get("/location", response_model=LocationUsersResponse)
async def get_location_users(
    location: str = Query(..., description="Location string (e.g., /repos/123/files/main.py)"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get all users currently at a specific location.
    """
    presences = await presence_manager.get_users_at_location(db, location)
    presence_list = [PresenceResponse(**p.to_dict()) for p in presences]

    return LocationUsersResponse(location=location, users=presence_list, count=len(presence_list))


@router.post("/cleanup")
async def cleanup_stale_presence(
    timeout_minutes: int = Query(30, ge=1, le=1440),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Manually trigger cleanup of stale presence records.
    Requires authentication.
    """
    # Only allow superusers to trigger cleanup
    if not current_user.is_superuser:
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Insufficient permissions")

    await presence_manager.cleanup_stale_presence(db, timeout_minutes)
    return {"message": "Cleanup completed", "timeout_minutes": timeout_minutes}
