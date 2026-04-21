"""
WebSocket message handlers for code annotations (Phase 14.3).

Handles real-time annotation notifications.
"""

import logging
from typing import Any, Dict


from app.core.database import AsyncSessionLocal
from app.services.annotations import AnnotationManager
from app.websocket.distributed_connection_manager import distributed_manager

logger = logging.getLogger(__name__)

annotation_manager = AnnotationManager()


async def handle_annotation_load(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Load annotations for a location via WebSocket.

    Message format:
    {
        "type": "annotation.load",
        "payload": {
            "location": "path/to/file.py",
            "include_private": false,
            "line_start": 10,
            "line_end": 50,
            "color": "yellow"
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent annotation.load without location")
        return

    include_private = payload.get("include_private", False)
    line_start = payload.get("line_start")
    line_end = payload.get("line_end")
    color = payload.get("color")

    # Get user from connection
    connection = distributed_manager.connections.get(connection_id)
    if not connection:
        return

    user_id = connection.user_id

    try:
        async with AsyncSessionLocal() as db:
            line_range = None
            if line_start is not None and line_end is not None:
                line_range = (line_start, line_end)

            annotations = await annotation_manager.get_annotations_for_location(
                db=db,
                location=location,
                include_private=include_private,
                user_id=user_id if include_private else None,
                line_range=line_range,
                color=color,
            )

            # Convert to dict
            annotation_data = [ann.to_dict() for ann in annotations]

            await distributed_manager.send_to_connection(
                connection_id=connection_id,
                message={
                    "type": "annotation.loaded",
                    "payload": {
                        "location": location,
                        "annotations": annotation_data,
                        "count": len(annotation_data),
                    },
                },
            )

            logger.debug(f"Sent {len(annotation_data)} annotations to connection {connection_id}")

    except Exception as e:
        logger.error(f"Failed to load annotations for {location}: {e}")
        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "annotation.error",
                "payload": {
                    "error": "Failed to load annotations",
                    "location": location,
                },
            },
        )


async def handle_annotation_subscribe(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Subscribe to annotation notifications for a location.

    Message format:
    {
        "type": "annotation.subscribe",
        "payload": {
            "location": "path/to/file.py"
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent annotation.subscribe without location")
        return

    # User is already subscribed via location tracking
    await distributed_manager.send_to_connection(
        connection_id=connection_id,
        message={
            "type": "annotation.subscribed",
            "payload": {
                "location": location,
                "status": "subscribed",
            },
        },
    )

    logger.info(f"Connection {connection_id} subscribed to annotations at {location}")


async def handle_annotation_unsubscribe(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Unsubscribe from annotation notifications.

    Message format:
    {
        "type": "annotation.unsubscribe",
        "payload": {
            "location": "path/to/file.py"
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent annotation.unsubscribe without location")
        return

    await distributed_manager.send_to_connection(
        connection_id=connection_id,
        message={
            "type": "annotation.unsubscribed",
            "payload": {
                "location": location,
                "status": "unsubscribed",
            },
        },
    )

    logger.info(f"Connection {connection_id} unsubscribed from annotations at {location}")


# Register handlers
def register_annotation_handlers(message_handler):
    """Register all annotation WebSocket handlers."""
    message_handler.register("annotation.load", handle_annotation_load)
    message_handler.register("annotation.subscribe", handle_annotation_subscribe)
    message_handler.register("annotation.unsubscribe", handle_annotation_unsubscribe)

    logger.info("✅ Annotation WebSocket handlers registered")
