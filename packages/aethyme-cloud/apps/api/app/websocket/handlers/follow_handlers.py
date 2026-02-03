"""
WebSocket message handlers for follow mode (Phase 14.2).

Handles viewport tracking and synchronized scrolling.
"""

import logging
from typing import Any, Dict
from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import AsyncSessionLocal
from app.services.presence.presence_manager import PresenceManager
from app.websocket.distributed_connection_manager import distributed_manager

logger = logging.getLogger(__name__)

presence_manager = PresenceManager()


async def handle_viewport_update(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Handle viewport position updates.

    Message format:
    {
        "type": "presence.viewport",
        "payload": {
            "location": "path/to/file.py",
            "start_line": 10,
            "end_line": 50,
            "scroll_top": 240
        }
    }
    """
    location = payload.get("location")
    start_line = payload.get("start_line")
    end_line = payload.get("end_line")
    scroll_top = payload.get("scroll_top")

    if not location or start_line is None or end_line is None:
        logger.warning(f"Connection {connection_id} sent incomplete viewport update")
        return

    # Get user from connection
    connection = distributed_manager.connections.get(connection_id)
    if not connection:
        return

    user_id = connection.user_id

    try:
        async with AsyncSessionLocal() as db:
            # Update viewport in presence
            presence = await presence_manager.update_viewport(
                db=db,
                user_id=user_id,
                location=location,
                start_line=start_line,
                end_line=end_line,
                scroll_top=scroll_top,
            )

            if presence:
                # Broadcast viewport update to other users at this location
                await distributed_manager.broadcast_to_location(
                    location=location,
                    message={
                        "type": "presence.viewport_updated",
                        "payload": {
                            "user_id": str(user_id),
                            "location": location,
                            "viewport": {
                                "start_line": start_line,
                                "end_line": end_line,
                                "scroll_top": scroll_top,
                            },
                        },
                    },
                    exclude_connection_id=connection_id,
                )

                logger.debug(f"User {user_id} viewport updated at {location}: lines {start_line}-{end_line}")

    except Exception as e:
        logger.error(f"Failed to update viewport for {user_id}: {e}")
        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "presence.error",
                "payload": {
                    "error": "Failed to update viewport",
                },
            },
        )


async def handle_follow_user(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Handle follow user request.

    Message format:
    {
        "type": "follow.start",
        "payload": {
            "user_id": "uuid-to-follow"
        }
    }
    """
    follow_user_id_str = payload.get("user_id")

    if not follow_user_id_str:
        logger.warning(f"Connection {connection_id} sent follow.start without user_id")
        return

    try:
        follow_user_id = UUID(follow_user_id_str)

        # Get current user from connection
        connection = distributed_manager.connections.get(connection_id)
        if not connection:
            return

        # Send confirmation
        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "follow.started",
                "payload": {
                    "following_user_id": str(follow_user_id),
                },
            },
        )

        # Notify the followed user (optional)
        user_connections = distributed_manager.user_connections.get(follow_user_id)
        if user_connections:
            for conn_id in user_connections:
                await distributed_manager.send_to_connection(
                    connection_id=conn_id,
                    message={
                        "type": "follow.follower_added",
                        "payload": {
                            "follower_user_id": str(connection.user_id),
                        },
                    },
                )

        logger.info(f"User {connection.user_id} started following {follow_user_id}")

    except ValueError:
        logger.error(f"Invalid user_id format: {follow_user_id_str}")


async def handle_unfollow_user(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Handle unfollow user request.

    Message format:
    {
        "type": "follow.stop",
        "payload": {
            "user_id": "uuid-to-unfollow"
        }
    }
    """
    unfollow_user_id_str = payload.get("user_id")

    if not unfollow_user_id_str:
        logger.warning(f"Connection {connection_id} sent follow.stop without user_id")
        return

    try:
        unfollow_user_id = UUID(unfollow_user_id_str)

        # Get current user from connection
        connection = distributed_manager.connections.get(connection_id)
        if not connection:
            return

        # Send confirmation
        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "follow.stopped",
                "payload": {
                    "unfollowed_user_id": str(unfollow_user_id),
                },
            },
        )

        # Notify the unfollowed user (optional)
        user_connections = distributed_manager.user_connections.get(unfollow_user_id)
        if user_connections:
            for conn_id in user_connections:
                await distributed_manager.send_to_connection(
                    connection_id=conn_id,
                    message={
                        "type": "follow.follower_removed",
                        "payload": {
                            "follower_user_id": str(connection.user_id),
                        },
                    },
                )

        logger.info(f"User {connection.user_id} stopped following {unfollow_user_id}")

    except ValueError:
        logger.error(f"Invalid user_id format: {unfollow_user_id_str}")


async def handle_request_viewport(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Request current viewport of a user.

    Message format:
    {
        "type": "follow.request_viewport",
        "payload": {
            "user_id": "uuid"
        }
    }
    """
    target_user_id_str = payload.get("user_id")

    if not target_user_id_str:
        logger.warning(f"Connection {connection_id} sent follow.request_viewport without user_id")
        return

    try:
        target_user_id = UUID(target_user_id_str)

        async with AsyncSessionLocal() as db:
            # Get the target user's presence
            from sqlalchemy import select
            from app.models.collaboration.presence import UserPresence

            result = await db.execute(
                select(UserPresence).where(
                    UserPresence.user_id == target_user_id,
                    UserPresence.status.in_(["online", "idle"]),
                )
            )
            presence = result.scalar_one_or_none()

            if presence and presence.viewport_start_line is not None:
                # Send viewport info
                await distributed_manager.send_to_connection(
                    connection_id=connection_id,
                    message={
                        "type": "follow.viewport_info",
                        "payload": {
                            "user_id": str(target_user_id),
                            "location": presence.location,
                            "viewport": {
                                "start_line": presence.viewport_start_line,
                                "end_line": presence.viewport_end_line,
                                "scroll_top": presence.viewport_scroll_top,
                            },
                        },
                    },
                )
            else:
                # No viewport info available
                await distributed_manager.send_to_connection(
                    connection_id=connection_id,
                    message={
                        "type": "follow.error",
                        "payload": {
                            "error": "Viewport not available",
                            "user_id": str(target_user_id),
                        },
                    },
                )

    except ValueError:
        logger.error(f"Invalid user_id format: {target_user_id_str}")
    except Exception as e:
        logger.error(f"Failed to get viewport for user {target_user_id_str}: {e}")


# Register handlers
def register_follow_handlers(message_handler):
    """Register all follow mode WebSocket handlers."""
    message_handler.register("presence.viewport", handle_viewport_update)
    message_handler.register("follow.start", handle_follow_user)
    message_handler.register("follow.stop", handle_unfollow_user)
    message_handler.register("follow.request_viewport", handle_request_viewport)

    logger.info("✅ Follow mode WebSocket handlers registered")
