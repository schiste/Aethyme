"""Production-grade connection pooling for PostgreSQL."""

import logging
from contextlib import contextmanager
from typing import Generator, Optional, Any
import psycopg2
from psycopg2 import pool
from psycopg2.extensions import connection as PGConnection
from psycopg2.extras import RealDictCursor
import structlog

from ..config import settings

logger = structlog.get_logger(__name__)


class DatabasePool:
    """Thread-safe PostgreSQL connection pool with automatic management."""

    _instance: Optional['DatabasePool'] = None
    _pool: Optional[pool.ThreadedConnectionPool] = None

    def __new__(cls) -> 'DatabasePool':
        """Singleton pattern for connection pool."""
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._initialize_pool()
        return cls._instance

    def _initialize_pool(self) -> None:
        """Initialize the connection pool."""
        try:
            self._pool = pool.ThreadedConnectionPool(
                minconn=settings.db_pool_min_size,
                maxconn=settings.db_pool_max_size,
                dsn=settings.database_url_sync,
                cursor_factory=RealDictCursor,
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
            raise RuntimeError("Connection pool not initialized")

        conn = None
        try:
            conn = self._pool.getconn()
            yield conn
            conn.commit()
        except Exception as e:
            if conn:
                conn.rollback()
            logger.error("Database operation failed", error=str(e))
            raise
        finally:
            if conn:
                self._pool.putconn(conn)

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
                conn.commit()
            except Exception:
                conn.rollback()
                raise

    def execute(
        self,
        query: str,
        params: Optional[tuple] = None,
        fetch: bool = True
    ) -> Optional[list[dict[str, Any]]]:
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
                    return cur.fetchall()
        return None

    def execute_many(
        self,
        query: str,
        params_list: list[tuple],
        batch_size: int = 1000,
    ) -> int:
        """
        Execute a query with multiple parameter sets efficiently.

        Args:
            query: SQL query to execute
            params_list: List of parameter tuples
            batch_size: Batch size for execution

        Returns:
            Number of affected rows
        """
        total_affected = 0
        with self.get_connection() as conn:
            with conn.cursor() as cur:
                for i in range(0, len(params_list), batch_size):
                    batch = params_list[i:i + batch_size]
                    psycopg2.extras.execute_batch(
                        cur, query, batch, page_size=batch_size
                    )
                    total_affected += cur.rowcount
        return total_affected

    def set_tenant_context(self, tenant_id: str) -> None:
        """
        Set tenant context for row-level security.

        Args:
            tenant_id: UUID of the tenant
        """
        with self.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute("SET app.current_tenant = %s", (tenant_id,))

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

    def get_stats(self) -> dict[str, int]:
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


# Global pool instance
db_pool = DatabasePool()