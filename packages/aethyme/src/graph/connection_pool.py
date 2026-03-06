"""Production-grade connection pooling for PostgreSQL."""

import os
from collections.abc import Generator, Sequence
from contextlib import contextmanager
from typing import Any, Protocol, Self, TypeAlias, cast

import psycopg2
import structlog
from psycopg2 import pool
from psycopg2.extensions import connection as PGConnection
from psycopg2.extras import RealDictCursor, RealDictRow

from ..config import settings

logger = structlog.get_logger(__name__)

DatabaseRow: TypeAlias = dict[str, Any]
SQLParams: TypeAlias = Sequence[Any]


class ConnectionPoolProtocol(Protocol):
    """Typed subset of ThreadedConnectionPool used by this module."""

    minconn: int
    maxconn: int

    def getconn(self, key: object | None = None) -> PGConnection: ...

    def putconn(
        self,
        conn: PGConnection,
        key: object | None = None,
        close: bool = False,
    ) -> None: ...

    def closeall(self) -> None: ...


class DatabasePool:
    """Thread-safe PostgreSQL connection pool with automatic management."""

    _instance: Self | None = None
    _pool: ConnectionPoolProtocol | None = None

    def __new__(cls) -> Self:
        """Singleton pattern for connection pool."""
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def _initialize_pool(self) -> None:
        """Initialize the connection pool."""
        try:
            self._pool = cast(
                ConnectionPoolProtocol,
                pool.ThreadedConnectionPool(
                    minconn=settings.db_pool_min_size,
                    maxconn=settings.db_pool_max_size,
                    dsn=os.getenv("DATABASE_URL", settings.database_url_sync),
                    cursor_factory=RealDictCursor,
                ),
            )
            logger.info(
                "Database connection pool initialized",
                min_size=settings.db_pool_min_size,
                max_size=settings.db_pool_max_size,
            )
        except psycopg2.Error as e:
            logger.error("Failed to initialize connection pool", error=str(e))
            raise

    @contextmanager
    def get_connection(self) -> Generator[PGConnection, None, None]:
        """
        Get a connection from the pool with automatic return.

        Usage:
            with db_pool.get_connection() as conn:
                with conn.cursor() as cur:
                    cur.execute("SELECT * FROM nodes")
        """
        if not self._pool:
            self._initialize_pool()

        conn: PGConnection | None = None
        try:
            if self._pool is None:
                raise RuntimeError("Database pool failed to initialize")
            pool_ref = self._pool
            conn = pool_ref.getconn()
            yield conn
            conn.commit()
        except Exception as e:
            if conn:
                conn.rollback()
            logger.error("Database operation failed", error=str(e))
            raise
        finally:
            if conn:
                try:
                    with conn.cursor() as cur:
                        cur.execute("RESET ROLE")
                        cur.execute("RESET ALL")
                    conn.commit()
                    if self._pool is not None:
                        self._pool.putconn(conn)
                except Exception:
                    try:
                        conn.close()
                    finally:
                        if self._pool is not None:
                            self._pool.putconn(conn, close=True)

    @contextmanager
    def transaction(self) -> Generator[PGConnection, None, None]:
        """
        Execute operations in a transaction with automatic rollback on error.

        Usage:
            with db_pool.transaction() as conn:
                with conn.cursor() as cur:
                    cur.execute("INSERT INTO nodes ...")
                    cur.execute("INSERT INTO edges ...")
        """
        with self.get_connection() as conn:
            try:
                yield conn
            except Exception:
                conn.rollback()
                raise

    def execute(
        self,
        query: str,
        params: SQLParams | None = None,
        fetch: bool = True,
    ) -> list[DatabaseRow] | None:
        """
        Execute a query and return results.

        Args:
            query: SQL query to execute
            params: Query parameters
            fetch: Whether to fetch results

        Returns:
            Query results as list of dicts, or None if fetch=False
        """
        with self.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute(query, params)
                if fetch:
                    rows = cast(list[RealDictRow], cur.fetchall())
                    return [dict(row) for row in rows]
        return None

    def health_check(self) -> dict[str, Any]:
        """
        Check database connection health.

        Returns:
            Health status dictionary
        """
        try:
            result = self.execute("SELECT 1 as health", fetch=True)
            return {
                "status": "healthy",
                "pool_size": self._pool.maxconn if self._pool else 0,
                "database": "connected" if result else "error",
            }
        except Exception as e:
            return {
                "status": "unhealthy",
                "error": str(e),
            }

    def close_all(self) -> None:
        """Close all connections in the pool."""
        if self._pool:
            self._pool.closeall()
            logger.info("All database connections closed")
            self._pool = None

    def get_stats(self) -> dict[str, int] | dict[str, str]:
        """
        Get connection pool statistics.

        Returns:
            Dictionary with pool statistics
        """
        if not self._pool:
            return {"error": "Pool not initialized"}

        # Note: psycopg2 pool doesn't expose these directly,
        # we'd need to track them manually for production
        return {
            "min_connections": self._pool.minconn,
            "max_connections": self._pool.maxconn,
            # These would need custom tracking:
            "active_connections": 0,  # Would need to track
            "idle_connections": 0,    # Would need to track
        }


# Global pool instance; actual connections are initialized lazily on first use.
db_pool = DatabasePool()
