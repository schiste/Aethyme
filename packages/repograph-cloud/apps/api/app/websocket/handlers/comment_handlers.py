"""
WebSocket message handlers for code comments.

Handles real-time comment notifications and updates.
"""

import logging
from typing import Any, Dict
from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import AsyncSessionLocal
from app.services.comments import CommentManager
from app.websocket.distributed_connection_manager import distributed_manager

logger = logging.getLogger(__name__)

comment_manager = CommentManager()


async def handle_comment_subscribe(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Subscribe to comment notifications for a location.

    Message format:
    {
        "type": "comment.subscribe",
        "payload": {
            "location": "path/to/file.py"
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent comment.subscribe without location")
        return

    # User is already subscribed via location tracking
    # This is a no-op but confirms subscription
    await distributed_manager.send_to_connection(
        connection_id=connection_id,
        message={
            "type": "comment.subscribed",
            "payload": {
                "location": location,
                "status": "subscribed",
            },
        },
    )

    logger.info(f"Connection {connection_id} subscribed to comments at {location}")


async def handle_comment_unsubscribe(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Unsubscribe from comment notifications for a location.

    Message format:
    {
        "type": "comment.unsubscribe",
        "payload": {
            "location": "path/to/file.py"
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent comment.unsubscribe without location")
        return

    await distributed_manager.send_to_connection(
        connection_id=connection_id,
        message={
            "type": "comment.unsubscribed",
            "payload": {
                "location": location,
                "status": "unsubscribed",
            },
        },
    )

    logger.info(f"Connection {connection_id} unsubscribed from comments at {location}")


async def handle_comment_load(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Load comments for a location.

    Message format:
    {
        "type": "comment.load",
        "payload": {
            "location": "path/to/file.py",
            "include_resolved": false,
            "line_start": 10,
            "line_end": 50
        }
    }
    """
    location = payload.get("location")
    if not location:
        logger.warning(f"Connection {connection_id} sent comment.load without location")
        return

    include_resolved = payload.get("include_resolved", False)
    line_start = payload.get("line_start")
    line_end = payload.get("line_end")

    line_range = None
    if line_start is not None and line_end is not None:
        line_range = (line_start, line_end)

    try:
        async with AsyncSessionLocal() as db:
            comments = await comment_manager.get_comments_for_location(
                db=db,
                location=location,
                include_resolved=include_resolved,
                line_range=line_range,
            )

            # Convert to dict
            comment_data = []
            for comment in comments:
                comment_dict = {
                    "id": str(comment.id),
                    "user_id": str(comment.user_id),
                    "location": comment.location,
                    "line_start": comment.line_start,
                    "line_end": comment.line_end,
                    "column_start": comment.column_start,
                    "column_end": comment.column_end,
                    "content": comment.content,
                    "parent_id": str(comment.parent_id) if comment.parent_id else None,
                    "resolved": comment.resolved,
                    "resolved_by": str(comment.resolved_by) if comment.resolved_by else None,
                    "resolved_at": comment.resolved_at.isoformat() if comment.resolved_at else None,
                    "created_at": comment.created_at.isoformat(),
                    "updated_at": comment.updated_at.isoformat(),
                    "metadata": comment.metadata,
                    "replies": [
                        {
                            "id": str(reply.id),
                            "user_id": str(reply.user_id),
                            "content": reply.content,
                            "created_at": reply.created_at.isoformat(),
                        }
                        for reply in comment.replies
                    ],
                }
                comment_data.append(comment_dict)

            await distributed_manager.send_to_connection(
                connection_id=connection_id,
                message={
                    "type": "comment.loaded",
                    "payload": {
                        "location": location,
                        "comments": comment_data,
                        "count": len(comment_data),
                    },
                },
            )

            logger.info(f"Sent {len(comment_data)} comments to connection {connection_id}")

    except Exception as e:
        logger.error(f"Failed to load comments for {location}: {e}")
        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "comment.error",
                "payload": {
                    "error": "Failed to load comments",
                    "location": location,
                },
            },
        )


async def handle_comment_typing(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Broadcast typing indicator for comments.

    Message format:
    {
        "type": "comment.typing",
        "payload": {
            "location": "path/to/file.py",
            "line": 42,
            "is_typing": true
        }
    }
    """
    location = payload.get("location")
    line = payload.get("line")
    is_typing = payload.get("is_typing", True)

    if not location or line is None:
        logger.warning(f"Connection {connection_id} sent incomplete comment.typing")
        return

    # Get user info from connection
    connection = distributed_manager.connections.get(connection_id)
    if not connection:
        return

    # Broadcast to other users at this location
    await distributed_manager.broadcast_to_location(
        location=location,
        message={
            "type": "comment.typing",
            "payload": {
                "user_id": str(connection.user_id),
                "location": location,
                "line": line,
                "is_typing": is_typing,
            },
        },
        exclude_connection_id=connection_id,
    )


async def handle_comment_mark_read(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Mark a comment mention as read.

    Message format:
    {
        "type": "comment.mark_read",
        "payload": {
            "comment_id": "uuid"
        }
    }
    """
    comment_id_str = payload.get("comment_id")
    if not comment_id_str:
        logger.warning(f"Connection {connection_id} sent comment.mark_read without comment_id")
        return

    try:
        comment_id = UUID(comment_id_str)

        async with AsyncSessionLocal() as db:
            await comment_manager.mark_mentions_notified(db=db, comment_id=comment_id)

        await distributed_manager.send_to_connection(
            connection_id=connection_id,
            message={
                "type": "comment.marked_read",
                "payload": {
                    "comment_id": comment_id_str,
                    "status": "read",
                },
            },
        )

    except ValueError:
        logger.error(f"Invalid comment_id format: {comment_id_str}")
    except Exception as e:
        logger.error(f"Failed to mark comment {comment_id_str} as read: {e}")


async def handle_comment_request_thread(
    connection_id: str,
    payload: Dict[str, Any],
    context: Any = None,
):
    """
    Request a full comment thread.

    Message format:
    {
        "type": "comment.request_thread",
        "payload": {
            "comment_id": "uuid"
        }
    }
    """
    comment_id_str = payload.get("comment_id")
    if not comment_id_str:
        logger.warning(f"Connection {connection_id} sent comment.request_thread without comment_id")
        return

    try:
        comment_id = UUID(comment_id_str)

        async with AsyncSessionLocal() as db:
            thread = await comment_manager.get_thread(db=db, comment_id=comment_id)

            if not thread:
                await distributed_manager.send_to_connection(
                    connection_id=connection_id,
                    message={
                        "type": "comment.error",
                        "payload": {
                            "error": "Comment not found",
                            "comment_id": comment_id_str,
                        },
                    },
                )
                return

            # Convert to dict
            thread_data = [
                {
                    "id": str(comment.id),
                    "user_id": str(comment.user_id),
                    "location": comment.location,
                    "line_start": comment.line_start,
                    "line_end": comment.line_end,
                    "content": comment.content,
                    "parent_id": str(comment.parent_id) if comment.parent_id else None,
                    "resolved": comment.resolved,
                    "created_at": comment.created_at.isoformat(),
                    "updated_at": comment.updated_at.isoformat(),
                }
                for comment in thread
            ]

            await distributed_manager.send_to_connection(
                connection_id=connection_id,
                message={
                    "type": "comment.thread",
                    "payload": {
                        "comment_id": comment_id_str,
                        "thread": thread_data,
                    },
                },
            )

    except ValueError:
        logger.error(f"Invalid comment_id format: {comment_id_str}")
    except Exception as e:
        logger.error(f"Failed to get thread for comment {comment_id_str}: {e}")


# Register handlers
def register_comment_handlers(message_handler):
    """Register all comment-related WebSocket handlers."""
    message_handler.register("comment.subscribe", handle_comment_subscribe)
    message_handler.register("comment.unsubscribe", handle_comment_unsubscribe)
    message_handler.register("comment.load", handle_comment_load)
    message_handler.register("comment.typing", handle_comment_typing)
    message_handler.register("comment.mark_read", handle_comment_mark_read)
    message_handler.register("comment.request_thread", handle_comment_request_thread)

    logger.info("✅ Comment WebSocket handlers registered")
