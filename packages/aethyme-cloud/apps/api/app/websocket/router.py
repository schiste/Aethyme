"""WebSocket API router."""

import logging
import json
from uuid import uuid4
from fastapi import APIRouter, WebSocket, WebSocketDisconnect, Query, status
from typing import Optional

from app.core.auth import get_current_user_ws
from .connection_manager import connection_manager
from .message_handler import message_handler

logger = logging.getLogger(__name__)

websocket_router = APIRouter()


@websocket_router.websocket("/ws")
async def websocket_endpoint(
    websocket: WebSocket,
    token: Optional[str] = Query(None),
):
    """
    Main WebSocket endpoint for real-time collaboration.

    Authentication via query parameter: ws://api/ws?token=<jwt_token>
    """
    connection_id = str(uuid4())

    try:
        # Authenticate user
        user = await get_current_user_ws(websocket, token)
        if not user:
            await websocket.close(code=status.WS_1008_POLICY_VIOLATION, reason="Authentication required")
            return

        # Accept connection
        connection = await connection_manager.connect(websocket, user.id, connection_id)

        # Send welcome message
        await connection.send_json(
            {
                "type": "connection.established",
                "payload": {
                    "connection_id": connection_id,
                    "user_id": str(user.id),
                    "message": "Connected to Aethyme Cloud",
                },
            }
        )

        logger.info(f"WebSocket connection established for user {user.id}")

        # Message loop
        while True:
            try:
                # Receive message
                data = await websocket.receive_text()
                message = json.loads(data)

                # Check rate limit
                if not connection_manager.check_rate_limit(connection_id):
                    logger.warning(f"Rate limit exceeded for connection {connection_id}")
                    await connection.send_json(
                        {"type": "error", "payload": {"message": "Rate limit exceeded. Slow down."}}
                    )
                    continue

                # Handle message
                await message_handler.handle(connection_id, message, context={"user": user, "websocket": websocket})

            except WebSocketDisconnect:
                logger.info(f"WebSocket disconnected: {connection_id}")
                break
            except json.JSONDecodeError as e:
                logger.warning(f"Invalid JSON from {connection_id}: {e}")
                await connection.send_json({"type": "error", "payload": {"message": "Invalid JSON format"}})
            except Exception as e:
                logger.error(f"Error processing message from {connection_id}: {e}", exc_info=True)
                await connection.send_json({"type": "error", "payload": {"message": "Internal server error"}})

    except Exception as e:
        logger.error(f"WebSocket connection error: {e}", exc_info=True)
    finally:
        # Clean up connection
        await connection_manager.disconnect(connection_id)
        logger.info(f"WebSocket connection closed: {connection_id}")
