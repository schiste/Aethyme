"""Message handlers for presence-related WebSocket messages."""

import logging
from typing import Dict, Any

from app.websocket.connection_manager import connection_manager
from app.websocket.message_handler import message_handler
from app.services.presence.presence_manager import presence_manager
from app.core.database import AsyncSessionLocal

logger = logging.getLogger(__name__)


async def handle_presence_join(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.join message - user joins a location.

    Message format:
    {
        "type": "presence.join",
        "payload": {
            "location": "/repos/123/files/main.py",
            "metadata": { ... }
        }
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    location = payload.get("location")
    if not location:
        logger.warning(f"presence.join without location from {connection_id}")
        return

    metadata = payload.get("metadata", {})

    try:
        # Update database
        async with AsyncSessionLocal() as db:
            presence = await presence_manager.join_location(
                db, connection.user_id, connection_id, location, metadata
            )

            # Join location in connection manager
            await connection_manager.join_location(connection_id, location)

            # Get all users at this location (for broadcasting)
            users_at_location = await presence_manager.get_users_at_location(db, location)

        # Broadcast to all users at this location (except sender)
        await connection_manager.broadcast_to_location(
            location,
            {
                "type": "presence.user_joined",
                "payload": {
                    "user_id": str(connection.user_id),
                    "location": location,
                    "connection_id": connection_id,
                    "timestamp": presence.joined_at.isoformat() if presence.joined_at else None,
                },
            },
            exclude_connection_id=connection_id,
        )

        # Send current users list to the joiner
        await connection_manager.send_to_connection(
            connection_id,
            {
                "type": "presence.users_at_location",
                "payload": {
                    "location": location,
                    "users": [
                        {
                            "user_id": str(p.user_id),
                            "status": p.status,
                            "cursor": (
                                {"line": p.cursor_line, "column": p.cursor_column}
                                if p.cursor_line is not None
                                else None
                            ),
                            "selection": p.to_dict().get("selection"),
                            "joined_at": p.joined_at.isoformat() if p.joined_at else None,
                        }
                        for p in users_at_location
                        if str(p.user_id) != str(connection.user_id)  # Exclude self
                    ],
                },
            },
        )

        logger.info(f"User {connection.user_id} joined location {location}")

    except Exception as e:
        logger.error(f"Error handling presence.join: {e}", exc_info=True)
        await connection_manager.send_to_connection(
            connection_id, {"type": "error", "payload": {"message": "Failed to join location"}}
        )


async def handle_presence_leave(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.leave message - user leaves a location.

    Message format:
    {
        "type": "presence.leave",
        "payload": {
            "location": "/repos/123/files/main.py"
        }
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    location = payload.get("location")
    if not location:
        logger.warning(f"presence.leave without location from {connection_id}")
        return

    try:
        # Update database
        async with AsyncSessionLocal() as db:
            await presence_manager.leave_location(db, connection.user_id, location)

        # Leave location in connection manager
        await connection_manager.leave_location(connection_id, location)

        logger.info(f"User {connection.user_id} left location {location}")

    except Exception as e:
        logger.error(f"Error handling presence.leave: {e}", exc_info=True)


async def handle_presence_cursor(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.cursor message - update cursor position.

    Message format:
    {
        "type": "presence.cursor",
        "payload": {
            "line": 42,
            "column": 10
        }
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    line = payload.get("line")
    column = payload.get("column")

    if line is None or column is None:
        logger.warning(f"presence.cursor with missing line/column from {connection_id}")
        return

    # Get user's current location
    current_location = None
    if connection.locations:
        current_location = list(connection.locations)[0]  # Get first location

    if not current_location:
        logger.warning(f"presence.cursor but user not at any location: {connection_id}")
        return

    try:
        # Update database (async, non-blocking)
        async with AsyncSessionLocal() as db:
            await presence_manager.update_cursor(db, connection.user_id, current_location, line, column)

        # Broadcast to other users at same location
        await connection_manager.broadcast_to_location(
            current_location,
            {
                "type": "presence.cursor_update",
                "payload": {"user_id": str(connection.user_id), "cursor": {"line": line, "column": column}},
            },
            exclude_connection_id=connection_id,
        )

    except Exception as e:
        logger.error(f"Error handling presence.cursor: {e}", exc_info=True)


async def handle_presence_selection(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.selection message - update text selection.

    Message format:
    {
        "type": "presence.selection",
        "payload": {
            "start": {"line": 10, "column": 5},
            "end": {"line": 15, "column": 20}
        }
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    start = payload.get("start")
    end = payload.get("end")

    if not start or not end:
        logger.warning(f"presence.selection with missing start/end from {connection_id}")
        return

    # Get user's current location
    current_location = None
    if connection.locations:
        current_location = list(connection.locations)[0]

    if not current_location:
        logger.warning(f"presence.selection but user not at any location: {connection_id}")
        return

    try:
        # Update database
        async with AsyncSessionLocal() as db:
            await presence_manager.update_selection(
                db,
                connection.user_id,
                current_location,
                start.get("line"),
                start.get("column"),
                end.get("line"),
                end.get("column"),
            )

        # Broadcast to other users at same location
        await connection_manager.broadcast_to_location(
            current_location,
            {
                "type": "presence.selection_update",
                "payload": {"user_id": str(connection.user_id), "selection": {"start": start, "end": end}},
            },
            exclude_connection_id=connection_id,
        )

    except Exception as e:
        logger.error(f"Error handling presence.selection: {e}", exc_info=True)


async def handle_presence_clear_selection(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.clear_selection message - clear text selection.

    Message format:
    {
        "type": "presence.clear_selection",
        "payload": {}
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    # Get user's current location
    current_location = None
    if connection.locations:
        current_location = list(connection.locations)[0]

    if not current_location:
        return

    try:
        # Update database
        async with AsyncSessionLocal() as db:
            await presence_manager.clear_selection(db, connection.user_id, current_location)

        # Broadcast to other users at same location
        await connection_manager.broadcast_to_location(
            current_location,
            {"type": "presence.selection_cleared", "payload": {"user_id": str(connection.user_id)}},
            exclude_connection_id=connection_id,
        )

    except Exception as e:
        logger.error(f"Error handling presence.clear_selection: {e}", exc_info=True)


async def handle_presence_get_users(connection_id: str, payload: Dict[str, Any], context: Any):
    """
    Handle presence.get_users message - get users at a location.

    Message format:
    {
        "type": "presence.get_users",
        "payload": {
            "location": "/repos/123/files/main.py"
        }
    }
    """
    connection = connection_manager.get_connection(connection_id)
    if not connection:
        return

    location = payload.get("location")
    if not location:
        logger.warning(f"presence.get_users without location from {connection_id}")
        return

    try:
        async with AsyncSessionLocal() as db:
            users = await presence_manager.get_users_at_location(db, location)

        await connection_manager.send_to_connection(
            connection_id,
            {
                "type": "presence.users_at_location",
                "payload": {
                    "location": location,
                    "users": [
                        {
                            "user_id": str(u.user_id),
                            "status": u.status,
                            "cursor": (
                                {"line": u.cursor_line, "column": u.cursor_column}
                                if u.cursor_line is not None
                                else None
                            ),
                            "selection": u.to_dict().get("selection"),
                            "joined_at": u.joined_at.isoformat() if u.joined_at else None,
                        }
                        for u in users
                    ],
                },
            },
        )

    except Exception as e:
        logger.error(f"Error handling presence.get_users: {e}", exc_info=True)


# Register all presence message handlers
def register_presence_handlers():
    """Register all presence-related message handlers."""
    message_handler.register("presence.join", handle_presence_join)
    message_handler.register("presence.leave", handle_presence_leave)
    message_handler.register("presence.cursor", handle_presence_cursor)
    message_handler.register("presence.selection", handle_presence_selection)
    message_handler.register("presence.clear_selection", handle_presence_clear_selection)
    message_handler.register("presence.get_users", handle_presence_get_users)
    logger.info("Registered presence message handlers")


# Auto-register handlers on import
register_presence_handlers()
