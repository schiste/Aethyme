"""Activity manager for tracking and broadcasting user activities."""

import logging
from typing import List, Optional
from uuid import UUID
from datetime import datetime, timedelta
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, delete
from sqlalchemy.sql import func

from app.models.collaboration.activity import ActivityEvent

logger = logging.getLogger(__name__)


class ActivityManager:
    """
    Manages activity events including creation, retrieval, and broadcasting.
    """

    async def create_activity(
        self, db: AsyncSession, user_id: UUID, event_type: str, location: Optional[str] = None, payload: Optional[dict] = None
    ) -> ActivityEvent:
        """
        Create a new activity event.

        Args:
            db: Database session
            user_id: User UUID
            event_type: Type of event (e.g., "file.viewed", "search.performed")
            location: Optional location string
            payload: Optional event payload

        Returns:
            Created ActivityEvent
        """
        activity = ActivityEvent(user_id=user_id, event_type=event_type, location=location, payload=payload or {})

        db.add(activity)
        await db.commit()
        await db.refresh(activity)

        logger.debug(f"Created activity: {event_type} for user {user_id}")
        return activity

    async def get_recent_activity(
        self, db: AsyncSession, limit: int = 50, user_id: Optional[UUID] = None, event_type: Optional[str] = None, location: Optional[str] = None
    ) -> List[ActivityEvent]:
        """
        Get recent activity events with optional filters.

        Args:
            db: Database session
            limit: Maximum number of events to return
            user_id: Optional filter by user
            event_type: Optional filter by event type
            location: Optional filter by location

        Returns:
            List of ActivityEvent objects
        """
        query = select(ActivityEvent)

        if user_id:
            query = query.where(ActivityEvent.user_id == user_id)
        if event_type:
            query = query.where(ActivityEvent.event_type == event_type)
        if location:
            query = query.where(ActivityEvent.location == location)

        query = query.order_by(ActivityEvent.created_at.desc()).limit(limit)

        result = await db.execute(query)
        return list(result.scalars().all())

    async def get_activity_by_id(self, db: AsyncSession, activity_id: UUID) -> Optional[ActivityEvent]:
        """
        Get a specific activity event by ID.

        Args:
            db: Database session
            activity_id: Activity UUID

        Returns:
            ActivityEvent or None
        """
        result = await db.execute(select(ActivityEvent).where(ActivityEvent.id == activity_id))
        return result.scalar_one_or_none()

    async def get_user_activity(
        self, db: AsyncSession, user_id: UUID, limit: int = 50, offset: int = 0
    ) -> List[ActivityEvent]:
        """
        Get activity events for a specific user.

        Args:
            db: Database session
            user_id: User UUID
            limit: Maximum number of events
            offset: Pagination offset

        Returns:
            List of ActivityEvent objects
        """
        result = await db.execute(
            select(ActivityEvent)
            .where(ActivityEvent.user_id == user_id)
            .order_by(ActivityEvent.created_at.desc())
            .limit(limit)
            .offset(offset)
        )
        return list(result.scalars().all())

    async def get_location_activity(
        self, db: AsyncSession, location: str, limit: int = 50, offset: int = 0
    ) -> List[ActivityEvent]:
        """
        Get activity events for a specific location.

        Args:
            db: Database session
            location: Location string
            limit: Maximum number of events
            offset: Pagination offset

        Returns:
            List of ActivityEvent objects
        """
        result = await db.execute(
            select(ActivityEvent)
            .where(ActivityEvent.location == location)
            .order_by(ActivityEvent.created_at.desc())
            .limit(limit)
            .offset(offset)
        )
        return list(result.scalars().all())

    async def get_activity_stats(self, db: AsyncSession, user_id: Optional[UUID] = None) -> dict:
        """
        Get activity statistics.

        Args:
            db: Database session
            user_id: Optional filter by user

        Returns:
            Dictionary with statistics
        """
        query = select(func.count(ActivityEvent.id))
        if user_id:
            query = query.where(ActivityEvent.user_id == user_id)

        result = await db.execute(query)
        total_count = result.scalar() or 0

        # Get activity by type
        type_query = select(ActivityEvent.event_type, func.count(ActivityEvent.id)).group_by(ActivityEvent.event_type)
        if user_id:
            type_query = type_query.where(ActivityEvent.user_id == user_id)

        result = await db.execute(type_query)
        by_type = {row[0]: row[1] for row in result.all()}

        # Get recent activity count (last 24 hours)
        cutoff = datetime.utcnow() - timedelta(days=1)
        recent_query = select(func.count(ActivityEvent.id)).where(ActivityEvent.created_at >= cutoff)
        if user_id:
            recent_query = recent_query.where(ActivityEvent.user_id == user_id)

        result = await db.execute(recent_query)
        recent_count = result.scalar() or 0

        return {"total_count": total_count, "recent_count_24h": recent_count, "by_type": by_type}

    async def cleanup_old_activity(self, db: AsyncSession, days: int = 90):
        """
        Clean up old activity events.

        Args:
            db: Database session
            days: Delete events older than this many days
        """
        cutoff = datetime.utcnow() - timedelta(days=days)

        result = await db.execute(delete(ActivityEvent).where(ActivityEvent.created_at < cutoff))
        deleted_count = result.rowcount

        await db.commit()

        if deleted_count > 0:
            logger.info(f"Cleaned up {deleted_count} old activity events")

        return deleted_count


# Global activity manager instance
activity_manager = ActivityManager()
