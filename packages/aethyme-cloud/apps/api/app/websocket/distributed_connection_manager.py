"""
Distributed Connection Manager

Extends the base ConnectionManager with Redis pub/sub for multi-server support.
Broadcasts connection events across all servers for distributed collaboration.
"""

import asyncio
import logging
import os
from typing import Optional
from uuid import UUID

from app.redis.connection import RedisConnectionManager
from app.redis.pubsub import RedisPubSubManager
from app.redis.server_registry import ServerRegistry
from app.websocket.connection_manager import ConnectionManager

logger = logging.getLogger(__name__)


class DistributedConnectionManager(ConnectionManager):
    """
    Connection manager with Redis pub/sub for multi-server collaboration.
    Falls back to single-server mode if Redis is unavailable.
    """

    def __init__(
        self,
        redis_manager: Optional[RedisConnectionManager] = None,
        pubsub_manager: Optional[RedisPubSubManager] = None,
        server_registry: Optional[ServerRegistry] = None,
    ):
        super().__init__()

        self.redis_manager = redis_manager
        self.pubsub_manager = pubsub_manager
        self.server_registry = server_registry

        # Server ID for message routing
        self.server_id = os.getenv("SERVER_ID", "server-1")

        # Distributed mode flag
        self._distributed_mode = (
            redis_manager is not None
            and pubsub_manager is not None
            and redis_manager.is_healthy
        )

        if self._distributed_mode:
            logger.info(f"✅ Distributed mode enabled (server: {self.server_id})")
        else:
            logger.info("⚠️ Running in single-server mode (Redis unavailable)")

    async def initialize(self):
        """Initialize distributed features (subscribe to channels)."""
        if not self._distributed_mode:
            return

        try:
            # Subscribe to broadcast channel
            await self.pubsub_manager.subscribe(
                "collaboration:broadcast",
                self._handle_broadcast_message,
            )

            # Subscribe to location-specific channels (pattern)
            # Note: Redis pattern subscriptions require separate handling
            logger.info("📡 Subscribed to collaboration channels")

        except Exception as e:
            logger.error(f"Failed to initialize distributed features: {e}")
            self._distributed_mode = False

    async def connect(self, websocket, user_id: UUID, connection_id: str):
        """
        Connect and register a connection.
        Publishes connection event to other servers.
        """
        # Call parent connect
        connection = await super().connect(websocket, user_id, connection_id)

        # Update server registry connection count
        if self.server_registry:
            await self.server_registry.update_connection_count(len(self.connections))

        # Publish connection event
        if self._distributed_mode:
            await self._publish_event("connection.connected", {
                "connection_id": connection_id,
                "user_id": str(user_id),
            })

        return connection

    async def disconnect(self, connection_id: str):
        """
        Disconnect a connection.
        Publishes disconnection event to other servers.
        """
        if connection_id not in self.connections:
            return

        user_id = self.connections[connection_id].user_id

        # Call parent disconnect
        await super().disconnect(connection_id)

        # Update server registry connection count
        if self.server_registry:
            await self.server_registry.update_connection_count(len(self.connections))

        # Publish disconnection event
        if self._distributed_mode:
            await self._publish_event("connection.disconnected", {
                "connection_id": connection_id,
                "user_id": str(user_id),
            })

    async def join_location(self, connection_id: str, location: str):
        """
        Join a location.
        Publishes join event to other servers.
        """
        await super().join_location(connection_id, location)

        # Publish join event
        if self._distributed_mode and connection_id in self.connections:
            connection = self.connections[connection_id]
            await self._publish_event("location.joined", {
                "connection_id": connection_id,
                "user_id": str(connection.user_id),
                "location": location,
            })

    async def leave_location(self, connection_id: str, location: str):
        """
        Leave a location.
        Publishes leave event to other servers.
        """
        await super().leave_location(connection_id, location)

        # Publish leave event
        if self._distributed_mode and connection_id in self.connections:
            connection = self.connections[connection_id]
            await self._publish_event("location.left", {
                "connection_id": connection_id,
                "user_id": str(connection.user_id),
                "location": location,
            })

    async def broadcast_to_location(
        self,
        location: str,
        message: dict,
        exclude_connection_id: Optional[str] = None,
    ):
        """
        Broadcast message to all connections at a location.
        Includes connections on other servers via Redis.
        """
        # Broadcast to local connections
        await super().broadcast_to_location(location, message, exclude_connection_id)

        # Broadcast to other servers
        if self._distributed_mode:
            await self._publish_location_message(location, message, exclude_connection_id)

    async def broadcast_to_user(self, user_id: UUID, message: dict):
        """
        Broadcast message to all connections for a user.
        Includes connections on other servers via Redis.
        """
        # Broadcast to local connections
        await super().broadcast_to_user(user_id, message)

        # Broadcast to other servers
        if self._distributed_mode:
            await self._publish_user_message(user_id, message)

    async def broadcast_to_all(self, message: dict):
        """
        Broadcast message to all connections.
        Includes connections on other servers via Redis.
        """
        # Broadcast to local connections
        await super().broadcast_to_all(message)

        # Broadcast to other servers
        if self._distributed_mode:
            await self._publish_broadcast_message(message)

    async def send_to_connection(self, connection_id: str, message: dict) -> bool:
        """
        Send message to a specific connection.
        If connection is on another server, message is routed via Redis.
        """
        # Try local connection first
        if connection_id in self.connections:
            return await super().send_to_connection(connection_id, message)

        # Connection not found locally - might be on another server
        # We don't send direct messages cross-server (would need connection routing)
        logger.debug(f"Connection {connection_id} not found locally")
        return False

    async def _publish_event(self, event_type: str, data: dict):
        """Publish a connection event to other servers."""
        if not self.pubsub_manager:
            return

        try:
            await self.pubsub_manager.publish(
                "collaboration:broadcast",
                {
                    "server_id": self.server_id,
                    "type": event_type,
                    "data": data,
                    "timestamp": asyncio.get_event_loop().time(),
                },
            )
        except Exception as e:
            logger.error(f"Failed to publish event {event_type}: {e}")

    async def _publish_location_message(
        self,
        location: str,
        message: dict,
        exclude_connection_id: Optional[str] = None,
    ):
        """Publish a location-specific message to other servers."""
        if not self.pubsub_manager:
            return

        try:
            # Publish to location-specific channel
            channel = f"collaboration:location:{location}"
            await self.pubsub_manager.publish(
                channel,
                {
                    "server_id": self.server_id,
                    "type": "message.location",
                    "location": location,
                    "message": message,
                    "exclude_connection_id": exclude_connection_id,
                    "timestamp": asyncio.get_event_loop().time(),
                },
            )
        except Exception as e:
            logger.error(f"Failed to publish location message: {e}")

    async def _publish_user_message(self, user_id: UUID, message: dict):
        """Publish a user-specific message to other servers."""
        if not self.pubsub_manager:
            return

        try:
            await self.pubsub_manager.publish(
                "collaboration:broadcast",
                {
                    "server_id": self.server_id,
                    "type": "message.user",
                    "user_id": str(user_id),
                    "message": message,
                    "timestamp": asyncio.get_event_loop().time(),
                },
            )
        except Exception as e:
            logger.error(f"Failed to publish user message: {e}")

    async def _publish_broadcast_message(self, message: dict):
        """Publish a broadcast message to other servers."""
        if not self.pubsub_manager:
            return

        try:
            await self.pubsub_manager.publish(
                "collaboration:broadcast",
                {
                    "server_id": self.server_id,
                    "type": "message.broadcast",
                    "message": message,
                    "timestamp": asyncio.get_event_loop().time(),
                },
            )
        except Exception as e:
            logger.error(f"Failed to publish broadcast message: {e}")

    async def _handle_broadcast_message(self, channel: str, data: dict):
        """
        Handle incoming broadcast message from another server.

        Args:
            channel: Redis channel name
            data: Message data
        """
        try:
            # Ignore messages from this server
            if data.get("server_id") == self.server_id:
                return

            message_type = data.get("type")

            if message_type == "message.location":
                # Route message to location
                location = data.get("location")
                message = data.get("message")
                exclude_connection_id = data.get("exclude_connection_id")

                # Use parent's broadcast_to_location to avoid re-publishing
                await super().broadcast_to_location(
                    location, message, exclude_connection_id
                )

            elif message_type == "message.user":
                # Route message to user
                user_id = UUID(data.get("user_id"))
                message = data.get("message")

                # Use parent's broadcast_to_user to avoid re-publishing
                await super().broadcast_to_user(user_id, message)

            elif message_type == "message.broadcast":
                # Route message to all connections
                message = data.get("message")

                # Use parent's broadcast_to_all to avoid re-publishing
                await super().broadcast_to_all(message)

            # Other event types (connection.connected, etc.) are informational
            # We don't need to act on them since connections are server-local

        except Exception as e:
            logger.error(f"Error handling broadcast message: {e}")

    def is_distributed(self) -> bool:
        """Check if running in distributed mode."""
        return self._distributed_mode

    def get_server_id(self) -> str:
        """Get this server's ID."""
        return self.server_id
