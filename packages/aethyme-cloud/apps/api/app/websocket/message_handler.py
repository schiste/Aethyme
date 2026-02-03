"""WebSocket message handler for routing and processing messages."""

import logging
from typing import Dict, Any, Callable, Awaitable, Optional
from uuid import UUID
from datetime import datetime

logger = logging.getLogger(__name__)


MessageHandlerFunc = Callable[[str, Dict[str, Any], Any], Awaitable[None]]


class MessageHandler:
    """
    Routes WebSocket messages to appropriate handlers based on message type.
    """

    def __init__(self):
        self.handlers: Dict[str, MessageHandlerFunc] = {}
        self._register_default_handlers()

    def register(self, message_type: str, handler: MessageHandlerFunc):
        """
        Register a message handler for a specific message type.

        Args:
            message_type: Type of message (e.g., "presence.join")
            handler: Async function to handle the message
        """
        self.handlers[message_type] = handler
        logger.debug(f"Registered handler for message type: {message_type}")

    async def handle(self, connection_id: str, message: Dict[str, Any], context: Any = None):
        """
        Route a message to the appropriate handler.

        Args:
            connection_id: Connection ID of the sender
            message: Message dictionary with 'type' and 'payload' keys
            context: Optional context (e.g., database session, connection manager)
        """
        message_type = message.get("type")
        if not message_type:
            logger.warning(f"Message without type from {connection_id}: {message}")
            return

        payload = message.get("payload", {})

        handler = self.handlers.get(message_type)
        if not handler:
            logger.warning(f"No handler for message type '{message_type}' from {connection_id}")
            return

        try:
            await handler(connection_id, payload, context)
        except Exception as e:
            logger.error(f"Error handling message type '{message_type}' from {connection_id}: {e}", exc_info=True)

    def _register_default_handlers(self):
        """Register default message handlers."""
        self.register("ping", self._handle_ping)
        self.register("pong", self._handle_pong)

        # Register comment handlers
        from app.websocket.handlers.comment_handlers import register_comment_handlers
        register_comment_handlers(self)

        # Register follow mode handlers
        from app.websocket.handlers.follow_handlers import register_follow_handlers
        register_follow_handlers(self)

        # Register annotation handlers
        from app.websocket.handlers.annotation_handlers import register_annotation_handlers
        register_annotation_handlers(self)

    async def _handle_ping(self, connection_id: str, payload: Dict[str, Any], context: Any):
        """Handle ping message by sending pong."""
        from .connection_manager import connection_manager

        connection = connection_manager.get_connection(connection_id)
        if connection:
            connection.last_pong = datetime.utcnow()
            await connection_manager.send_to_connection(
                connection_id, {"type": "pong", "payload": {"server_time": datetime.utcnow().isoformat()}}
            )

    async def _handle_pong(self, connection_id: str, payload: Dict[str, Any], context: Any):
        """Handle pong message by updating last_pong timestamp."""
        from .connection_manager import connection_manager

        connection = connection_manager.get_connection(connection_id)
        if connection:
            connection.last_pong = datetime.utcnow()


# Global message handler instance
message_handler = MessageHandler()
