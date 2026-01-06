"""WebSocket connection manager for handling multiple concurrent connections."""

import asyncio
import json
import logging
from typing import Dict, Set, Optional, List
from uuid import UUID
from datetime import datetime, timedelta
from fastapi import WebSocket, WebSocketDisconnect
from collections import defaultdict

logger = logging.getLogger(__name__)


class Connection:
    """Represents a single WebSocket connection."""

    def __init__(self, websocket: WebSocket, user_id: UUID, connection_id: str):
        self.websocket = websocket
        self.user_id = user_id
        self.connection_id = connection_id
        self.locations: Set[str] = set()  # Locations user has joined
        self.last_ping = datetime.utcnow()
        self.last_pong = datetime.utcnow()
        self.connected = True
        self.metadata = {}

    async def send_json(self, data: dict):
        """Send JSON message to client."""
        try:
            await self.websocket.send_json(data)
        except Exception as e:
            logger.error(f"Error sending message to {self.connection_id}: {e}")
            self.connected = False

    async def send_text(self, message: str):
        """Send text message to client."""
        try:
            await self.websocket.send_text(message)
        except Exception as e:
            logger.error(f"Error sending text to {self.connection_id}: {e}")
            self.connected = False

    def is_alive(self, timeout_seconds: int = 300) -> bool:
        """Check if connection is still alive based on last pong."""
        if not self.connected:
            return False
        time_since_pong = (datetime.utcnow() - self.last_pong).total_seconds()
        return time_since_pong < timeout_seconds


class ConnectionManager:
    """
    Manages all WebSocket connections and provides methods for
    broadcasting messages to specific users, locations, or all connections.
    """

    def __init__(self):
        # Connection storage
        self.connections: Dict[str, Connection] = {}  # connection_id -> Connection
        self.user_connections: Dict[UUID, Set[str]] = defaultdict(set)  # user_id -> set of connection_ids
        self.location_connections: Dict[str, Set[str]] = defaultdict(
            set
        )  # location -> set of connection_ids

        # Rate limiting
        self.message_counts: Dict[str, int] = defaultdict(int)  # connection_id -> message count
        self.rate_limit_window = 60  # 1 minute
        self.max_messages_per_window = 100

        # Background tasks
        self._cleanup_task: Optional[asyncio.Task] = None
        self._heartbeat_task: Optional[asyncio.Task] = None

    async def connect(self, websocket: WebSocket, user_id: UUID, connection_id: str) -> Connection:
        """
        Accept and register a new WebSocket connection.

        Args:
            websocket: FastAPI WebSocket instance
            user_id: UUID of the authenticated user
            connection_id: Unique connection identifier

        Returns:
            Connection instance
        """
        await websocket.accept()

        connection = Connection(websocket, user_id, connection_id)
        self.connections[connection_id] = connection
        self.user_connections[user_id].add(connection_id)

        logger.info(f"User {user_id} connected with connection {connection_id}")
        logger.info(f"Total connections: {len(self.connections)}")

        # Start background tasks if not running
        if not self._cleanup_task or self._cleanup_task.done():
            self._cleanup_task = asyncio.create_task(self._cleanup_dead_connections())
        if not self._heartbeat_task or self._heartbeat_task.done():
            self._heartbeat_task = asyncio.create_task(self._send_heartbeats())

        return connection

    async def disconnect(self, connection_id: str):
        """
        Disconnect and clean up a connection.

        Args:
            connection_id: Connection identifier to disconnect
        """
        if connection_id not in self.connections:
            return

        connection = self.connections[connection_id]
        user_id = connection.user_id

        # Remove from all locations
        for location in list(connection.locations):
            await self.leave_location(connection_id, location)

        # Remove from user connections
        if user_id in self.user_connections:
            self.user_connections[user_id].discard(connection_id)
            if not self.user_connections[user_id]:
                del self.user_connections[user_id]

        # Remove from connections
        del self.connections[connection_id]
        logger.info(f"User {user_id} disconnected (connection {connection_id})")
        logger.info(f"Total connections: {len(self.connections)}")

    async def join_location(self, connection_id: str, location: str):
        """
        Add a connection to a location (e.g., a file or symbol view).

        Args:
            connection_id: Connection identifier
            location: Location string (e.g., "/repos/123/files/main.py")
        """
        if connection_id not in self.connections:
            return

        connection = self.connections[connection_id]
        connection.locations.add(location)
        self.location_connections[location].add(connection_id)

        logger.debug(f"Connection {connection_id} joined location {location}")

        # Notify other users in this location
        await self.broadcast_to_location(
            location,
            {
                "type": "presence.user_joined",
                "payload": {"user_id": str(connection.user_id), "location": location, "connection_id": connection_id},
            },
            exclude_connection_id=connection_id,
        )

    async def leave_location(self, connection_id: str, location: str):
        """
        Remove a connection from a location.

        Args:
            connection_id: Connection identifier
            location: Location string
        """
        if connection_id not in self.connections:
            return

        connection = self.connections[connection_id]
        connection.locations.discard(location)

        if location in self.location_connections:
            self.location_connections[location].discard(connection_id)
            if not self.location_connections[location]:
                del self.location_connections[location]

        logger.debug(f"Connection {connection_id} left location {location}")

        # Notify other users in this location
        await self.broadcast_to_location(
            location,
            {
                "type": "presence.user_left",
                "payload": {"user_id": str(connection.user_id), "location": location, "connection_id": connection_id},
            },
        )

    async def send_to_connection(self, connection_id: str, message: dict):
        """
        Send a message to a specific connection.

        Args:
            connection_id: Connection identifier
            message: Message dictionary to send
        """
        if connection_id not in self.connections:
            return

        connection = self.connections[connection_id]
        await connection.send_json(message)

    async def send_to_user(self, user_id: UUID, message: dict, exclude_connection_id: Optional[str] = None):
        """
        Send a message to all connections of a specific user.

        Args:
            user_id: User UUID
            message: Message dictionary to send
            exclude_connection_id: Optional connection ID to exclude
        """
        if user_id not in self.user_connections:
            return

        connection_ids = self.user_connections[user_id]
        for conn_id in connection_ids:
            if exclude_connection_id and conn_id == exclude_connection_id:
                continue
            await self.send_to_connection(conn_id, message)

    async def broadcast_to_location(
        self, location: str, message: dict, exclude_connection_id: Optional[str] = None
    ):
        """
        Broadcast a message to all connections in a specific location.

        Args:
            location: Location string
            message: Message dictionary to send
            exclude_connection_id: Optional connection ID to exclude
        """
        if location not in self.location_connections:
            return

        connection_ids = list(self.location_connections[location])
        for conn_id in connection_ids:
            if exclude_connection_id and conn_id == exclude_connection_id:
                continue
            await self.send_to_connection(conn_id, message)

    async def broadcast_to_all(self, message: dict, exclude_connection_id: Optional[str] = None):
        """
        Broadcast a message to all connections.

        Args:
            message: Message dictionary to send
            exclude_connection_id: Optional connection ID to exclude
        """
        for conn_id in list(self.connections.keys()):
            if exclude_connection_id and conn_id == exclude_connection_id:
                continue
            await self.send_to_connection(conn_id, message)

    def get_connection(self, connection_id: str) -> Optional[Connection]:
        """Get a connection by ID."""
        return self.connections.get(connection_id)

    def get_user_connections(self, user_id: UUID) -> List[Connection]:
        """Get all connections for a user."""
        if user_id not in self.user_connections:
            return []
        return [self.connections[conn_id] for conn_id in self.user_connections[user_id] if conn_id in self.connections]

    def get_location_connections(self, location: str) -> List[Connection]:
        """Get all connections at a location."""
        if location not in self.location_connections:
            return []
        return [
            self.connections[conn_id]
            for conn_id in self.location_connections[location]
            if conn_id in self.connections
        ]

    def get_location_users(self, location: str) -> Set[UUID]:
        """Get unique user IDs at a location."""
        connections = self.get_location_connections(location)
        return {conn.user_id for conn in connections}

    def check_rate_limit(self, connection_id: str) -> bool:
        """
        Check if connection has exceeded rate limit.

        Returns:
            True if under limit, False if exceeded
        """
        self.message_counts[connection_id] += 1

        # Clean up old counts periodically
        if len(self.message_counts) > 10000:
            self.message_counts.clear()

        return self.message_counts[connection_id] <= self.max_messages_per_window

    async def _cleanup_dead_connections(self):
        """Background task to clean up dead connections."""
        while True:
            try:
                await asyncio.sleep(60)  # Check every minute

                dead_connections = []
                for conn_id, connection in self.connections.items():
                    if not connection.is_alive():
                        dead_connections.append(conn_id)

                for conn_id in dead_connections:
                    logger.info(f"Cleaning up dead connection {conn_id}")
                    await self.disconnect(conn_id)

                # Reset rate limit counts
                self.message_counts.clear()

            except Exception as e:
                logger.error(f"Error in cleanup task: {e}")

    async def _send_heartbeats(self):
        """Background task to send heartbeats to all connections."""
        while True:
            try:
                await asyncio.sleep(30)  # Send heartbeat every 30 seconds

                message = {"type": "ping", "payload": {"server_time": datetime.utcnow().isoformat()}}

                for conn_id, connection in list(self.connections.items()):
                    try:
                        await connection.send_json(message)
                        connection.last_ping = datetime.utcnow()
                    except Exception as e:
                        logger.debug(f"Failed to send heartbeat to {conn_id}: {e}")

            except Exception as e:
                logger.error(f"Error in heartbeat task: {e}")


# Global connection manager instance
connection_manager = ConnectionManager()
