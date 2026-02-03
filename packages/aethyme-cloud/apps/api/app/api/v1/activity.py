"""REST API endpoints for activity feed."""

from typing import List, Optional
from uuid import UUID
from fastapi import APIRouter, Depends, HTTPException, Query, status
from sqlalchemy.ext.asyncio import AsyncSession
from pydantic import BaseModel

from app.core.database import get_db
from app.core.auth import get_current_user
from app.models.user import User
from app.services.activity.activity_manager import activity_manager

router = APIRouter()


class ActivityResponse(BaseModel):
    """Activity event response model."""

    id: str
    user_id: str
    event_type: str
    location: Optional[str] = None
    payload: dict = {}
    created_at: str

    class Config:
        from_attributes = True


class ActivityStatsResponse(BaseModel):
    """Activity statistics response."""

    total_count: int
    recent_count_24h: int
    by_type: dict


@router.get("/recent", response_model=List[ActivityResponse])
async def get_recent_activity(
    limit: int = Query(50, ge=1, le=200),
    user_id: Optional[UUID] = Query(None),
    event_type: Optional[str] = Query(None),
    location: Optional[str] = Query(None),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get recent activity events with optional filters.
    """
    activities = await activity_manager.get_recent_activity(
        db, limit=limit, user_id=user_id, event_type=event_type, location=location
    )
    return [ActivityResponse(**a.to_dict()) for a in activities]


@router.get("/user/{user_id}", response_model=List[ActivityResponse])
async def get_user_activity(
    user_id: UUID,
    limit: int = Query(50, ge=1, le=200),
    offset: int = Query(0, ge=0),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get activity events for a specific user.
    """
    activities = await activity_manager.get_user_activity(db, user_id, limit=limit, offset=offset)
    return [ActivityResponse(**a.to_dict()) for a in activities]


@router.get("/location", response_model=List[ActivityResponse])
async def get_location_activity(
    location: str = Query(...),
    limit: int = Query(50, ge=1, le=200),
    offset: int = Query(0, ge=0),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get activity events for a specific location.
    """
    activities = await activity_manager.get_location_activity(db, location, limit=limit, offset=offset)
    return [ActivityResponse(**a.to_dict()) for a in activities]


@router.get("/stats", response_model=ActivityStatsResponse)
async def get_activity_stats(
    user_id: Optional[UUID] = Query(None),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get activity statistics with optional user filter.
    """
    stats = await activity_manager.get_activity_stats(db, user_id=user_id)
    return ActivityStatsResponse(**stats)


@router.post("/cleanup")
async def cleanup_old_activity(
    days: int = Query(90, ge=1, le=365),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Manually trigger cleanup of old activity events.
    Requires superuser permissions.
    """
    if not current_user.is_superuser:
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Insufficient permissions")

    deleted_count = await activity_manager.cleanup_old_activity(db, days=days)
    return {"message": "Cleanup completed", "days": days, "deleted_count": deleted_count}
