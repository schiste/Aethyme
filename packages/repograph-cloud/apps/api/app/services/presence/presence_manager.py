"""Presence manager for tracking user presence, locations, and cursor positions."""

import logging
from typing import Dict, List, Optional, Set
from uuid import UUID
from datetime import datetime, timedelta
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, delete, update
from sqlalchemy.sql import func

from app.models.collaboration.presence import UserPresence
from app.models.collaboration.session import CollaborationSession
from app.models.collaboration.activity import ActivityEvent

logger = logging.getLogger(__name__)


class PresenceManager:
    """
    Manages user presence including online status, location,
    cursor position, and selection tracking.
    """

    def __init__(self):
        # In-memory cache for fast lookups
        self.online_users: Set[UUID] = set()
        self.user_locations: Dict[UUID, str] = {}  # user_id -> current location
        self.location_users: Dict[str, Set[UUID]] = {}  # location -> set of user_ids

    async def join_location(
        self,
        db: AsyncSession,
        user_id: UUID,
        connection_id: str,
        location: str,
        metadata: Optional[dict] = None,
    ) -> UserPresence:
        """
        User joins a location (file, symbol view, etc.).

        Args:
            db: Database session
            user_id: User UUID
            connection_id: Connection identifier
            location: Location string (e.g., "/repos/123/files/main.py")
            metadata: Optional metadata (viewport, user agent, etc.)

        Returns:
            UserPresence object
        """
        # Get or create session for this location
        session = await self._get_or_create_session(db, location)

        # Check if user already has presence at this location
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id,
                UserPresence.session_id == session.id,
                UserPresence.status.in_(["online", "idle"]),
            )
        )
        presence = result.scalar_one_or_none()

        if presence:
            # Update existing presence
            presence.status = "online"
            presence.last_seen = datetime.utcnow()
            presence.metadata = metadata or {}
        else:
            # Create new presence
            presence = UserPresence(
                user_id=user_id,
                session_id=session.id,
                status="online",
                location=location,
                metadata=metadata or {},
            )
            db.add(presence)

            # Increment participant count
            session.participant_count += 1

        # Update in-memory cache
        self.online_users.add(user_id)
        self.user_locations[user_id] = location
        if location not in self.location_users:
            self.location_users[location] = set()
        self.location_users[location].add(user_id)

        # Update session activity
        session.last_activity = datetime.utcnow()

        # Record activity
        activity = ActivityEvent(
            user_id=user_id, event_type="user.joined_location", location=location, payload={"connection_id": connection_id}
        )
        db.add(activity)

        await db.commit()
        await db.refresh(presence)

        logger.info(f"User {user_id} joined location {location}")
        return presence

    async def leave_location(self, db: AsyncSession, user_id: UUID, location: str) -> bool:
        """
        User leaves a location.

        Args:
            db: Database session
            user_id: User UUID
            location: Location string

        Returns:
            True if presence was updated, False otherwise
        """
        # Find and update presence
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status.in_(["online", "idle"])
            )
        )
        presence = result.scalar_one_or_none()

        if not presence:
            return False

        # Mark as offline
        presence.status = "offline"
        presence.last_seen = datetime.utcnow()

        # Update session participant count
        result = await db.execute(select(CollaborationSession).where(CollaborationSession.id == presence.session_id))
        session = result.scalar_one_or_none()
        if session and session.participant_count > 0:
            session.participant_count -= 1
            session.last_activity = datetime.utcnow()

        # Update in-memory cache
        if location in self.location_users:
            self.location_users[location].discard(user_id)
            if not self.location_users[location]:
                del self.location_users[location]

        if user_id in self.user_locations and self.user_locations[user_id] == location:
            del self.user_locations[user_id]

        # Check if user is still online anywhere
        result = await db.execute(
            select(UserPresence).where(UserPresence.user_id == user_id, UserPresence.status == "online")
        )
        if not result.scalar_one_or_none():
            self.online_users.discard(user_id)

        # Record activity
        activity = ActivityEvent(user_id=user_id, event_type="user.left_location", location=location, payload={})
        db.add(activity)

        await db.commit()

        logger.info(f"User {user_id} left location {location}")
        return True

    async def update_cursor(
        self, db: AsyncSession, user_id: UUID, location: str, line: int, column: int
    ) -> Optional[UserPresence]:
        """
        Update user's cursor position.

        Args:
            db: Database session
            user_id: User UUID
            location: Current location
            line: Line number
            column: Column number

        Returns:
            Updated UserPresence or None
        """
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status.in_(["online", "idle"])
            )
        )
        presence = result.scalar_one_or_none()

        if not presence:
            logger.warning(f"Cannot update cursor for user {user_id} at {location} - not present")
            return None

        presence.cursor_line = line
        presence.cursor_column = column
        presence.last_seen = datetime.utcnow()
        presence.status = "online"  # Activity resets idle state

        await db.commit()
        await db.refresh(presence)

        return presence

    async def update_selection(
        self, db: AsyncSession, user_id: UUID, location: str, start_line: int, start_column: int, end_line: int, end_column: int
    ) -> Optional[UserPresence]:
        """
        Update user's text selection.

        Args:
            db: Database session
            user_id: User UUID
            location: Current location
            start_line: Selection start line
            start_column: Selection start column
            end_line: Selection end line
            end_column: Selection end column

        Returns:
            Updated UserPresence or None
        """
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status.in_(["online", "idle"])
            )
        )
        presence = result.scalar_one_or_none()

        if not presence:
            logger.warning(f"Cannot update selection for user {user_id} at {location} - not present")
            return None

        presence.selection_start_line = start_line
        presence.selection_start_column = start_column
        presence.selection_end_line = end_line
        presence.selection_end_column = end_column
        presence.last_seen = datetime.utcnow()
        presence.status = "online"

        await db.commit()
        await db.refresh(presence)

        return presence

    async def clear_selection(self, db: AsyncSession, user_id: UUID, location: str) -> Optional[UserPresence]:
        """
        Clear user's text selection.

        Args:
            db: Database session
            user_id: User UUID
            location: Current location

        Returns:
            Updated UserPresence or None
        """
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status.in_(["online", "idle"])
            )
        )
        presence = result.scalar_one_or_none()

        if not presence:
            return None

        presence.selection_start_line = None
        presence.selection_start_column = None
        presence.selection_end_line = None
        presence.selection_end_column = None
        presence.last_seen = datetime.utcnow()

        await db.commit()
        await db.refresh(presence)

        return presence

    async def update_viewport(
        self, db: AsyncSession, user_id: UUID, location: str, start_line: int, end_line: int, scroll_top: Optional[int] = None
    ) -> Optional[UserPresence]:
        """
        Update user's viewport position for follow mode.

        Args:
            db: Database session
            user_id: User UUID
            location: Current location
            start_line: First visible line
            end_line: Last visible line
            scroll_top: Scroll position in pixels

        Returns:
            Updated UserPresence or None
        """
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status.in_(["online", "idle"])
            )
        )
        presence = result.scalar_one_or_none()

        if not presence:
            logger.warning(f"Cannot update viewport for user {user_id} at {location} - not present")
            return None

        presence.viewport_start_line = start_line
        presence.viewport_end_line = end_line
        presence.viewport_scroll_top = scroll_top
        presence.last_seen = datetime.utcnow()
        presence.status = "online"  # Activity resets idle state

        await db.commit()
        await db.refresh(presence)

        return presence

    async def get_users_at_location(self, db: AsyncSession, location: str) -> List[UserPresence]:
        """
        Get all users currently at a location.

        Args:
            db: Database session
            location: Location string

        Returns:
            List of UserPresence objects
        """
        result = await db.execute(
            select(UserPresence)
            .where(UserPresence.location == location, UserPresence.status.in_(["online", "idle"]))
            .order_by(UserPresence.joined_at)
        )
        return list(result.scalars().all())

    async def get_user_presence(self, db: AsyncSession, user_id: UUID) -> List[UserPresence]:
        """
        Get all presence records for a user.

        Args:
            db: Database session
            user_id: User UUID

        Returns:
            List of UserPresence objects
        """
        result = await db.execute(
            select(UserPresence).where(UserPresence.user_id == user_id, UserPresence.status.in_(["online", "idle"])).order_by(UserPresence.last_seen.desc())
        )
        return list(result.scalars().all())

    async def get_online_users(self, db: AsyncSession) -> List[UserPresence]:
        """
        Get all online users.

        Args:
            db: Database session

        Returns:
            List of UserPresence objects
        """
        result = await db.execute(
            select(UserPresence).where(UserPresence.status == "online").order_by(UserPresence.last_seen.desc())
        )
        return list(result.scalars().all())

    async def mark_idle(self, db: AsyncSession, user_id: UUID, location: str) -> Optional[UserPresence]:
        """
        Mark user as idle at a location.

        Args:
            db: Database session
            user_id: User UUID
            location: Location string

        Returns:
            Updated UserPresence or None
        """
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.user_id == user_id, UserPresence.location == location, UserPresence.status == "online"
            )
        )
        presence = result.scalar_one_or_none()

        if presence:
            presence.status = "idle"
            presence.last_seen = datetime.utcnow()
            await db.commit()
            await db.refresh(presence)
            logger.info(f"User {user_id} marked as idle at {location}")

        return presence

    async def cleanup_stale_presence(self, db: AsyncSession, timeout_minutes: int = 30):
        """
        Clean up stale presence records.

        Args:
            db: Database session
            timeout_minutes: Minutes of inactivity before marking offline
        """
        cutoff_time = datetime.utcnow() - timedelta(minutes=timeout_minutes)

        # Find stale presence records
        result = await db.execute(
            select(UserPresence).where(
                UserPresence.status.in_(["online", "idle"]), UserPresence.last_seen < cutoff_time
            )
        )
        stale_presence = list(result.scalars().all())

        for presence in stale_presence:
            presence.status = "offline"

            # Update session participant count
            result = await db.execute(
                select(CollaborationSession).where(CollaborationSession.id == presence.session_id)
            )
            session = result.scalar_one_or_none()
            if session and session.participant_count > 0:
                session.participant_count -= 1

            # Update in-memory cache
            self.online_users.discard(presence.user_id)
            if presence.location in self.location_users:
                self.location_users[presence.location].discard(presence.user_id)

        if stale_presence:
            await db.commit()
            logger.info(f"Cleaned up {len(stale_presence)} stale presence records")

    async def _get_or_create_session(self, db: AsyncSession, location: str) -> CollaborationSession:
        """
        Get existing session or create new one for a location.

        Args:
            db: Database session
            location: Location string

        Returns:
            CollaborationSession object
        """
        # Try to find active session
        result = await db.execute(select(CollaborationSession).where(CollaborationSession.location == location))
        session = result.scalar_one_or_none()

        if not session:
            # Create new session
            session = CollaborationSession(location=location, participant_count=0)
            db.add(session)
            await db.flush()
            logger.info(f"Created new collaboration session for location {location}")

        return session

    def get_cached_location_users(self, location: str) -> Set[UUID]:
        """
        Get users at location from in-memory cache (fast).

        Args:
            location: Location string

        Returns:
            Set of user UUIDs
        """
        return self.location_users.get(location, set()).copy()

    def get_cached_online_users(self) -> Set[UUID]:
        """
        Get all online users from in-memory cache (fast).

        Returns:
            Set of user UUIDs
        """
        return self.online_users.copy()

    def is_user_online(self, user_id: UUID) -> bool:
        """
        Check if user is online (from cache).

        Args:
            user_id: User UUID

        Returns:
            True if online, False otherwise
        """
        return user_id in self.online_users

    def get_user_location(self, user_id: UUID) -> Optional[str]:
        """
        Get user's current location (from cache).

        Args:
            user_id: User UUID

        Returns:
            Location string or None
        """
        return self.user_locations.get(user_id)


# Global presence manager instance
presence_manager = PresenceManager()
