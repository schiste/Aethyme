"""
Optimized search queries with database indexing and connection pooling.

Performance optimizations:
- GIN indexes for full-text search and trigram similarity
- Parameterized queries (SQL injection prevention)
- Connection pooling (asyncpg)
- Query timeout (5s max)
- Batch loading for N+1 prevention
"""

from typing import List, Dict, Any, Optional
import asyncio
import asyncpg
import structlog
from contextlib import asynccontextmanager

logger = structlog.get_logger(__name__)


class OptimizedSearchService:
    """High-performance search service with connection pooling and indexing."""

    def __init__(self, pool: asyncpg.Pool):
        """
        Initialize search service with connection pool.

        Args:
            pool: AsyncPG connection pool
        """
        self.pool = pool
        self.query_timeout = 5.0  # 5 seconds max

    async def search_exact(
        self,
        tenant_id: str,
        query: str,
        limit: int = 20
    ) -> List[Dict[str, Any]]:
        """
        Exact symbol name matching.

        Uses indexed lookup for O(log n) performance.

        Args:
            tenant_id: Tenant UUID for RLS
            query: Exact symbol name
            limit: Maximum results

        Returns:
            List of matching symbols
        """
        sql = """
            SELECT id, symbol, file_path, line_number, column_number,
                   kind, language, signature, documentation,
                   1.0 as score
            FROM aethyme.nodes
            WHERE tenant_id = $1 AND symbol = $2
            LIMIT $3
        """

        try:
            async with self.pool.acquire() as conn:
                # Set query timeout
                await conn.execute("SET statement_timeout = $1", int(self.query_timeout * 1000))

                # Set tenant for RLS
                await conn.execute("SET app.current_tenant = $1", tenant_id)

                # Execute query with parameters
                rows = await conn.fetch(sql, tenant_id, query, limit)

                return [dict(row) for row in rows]

        except asyncio.TimeoutError:
            logger.error("Search query timeout", query=query, tenant_id=tenant_id)
            raise
        except Exception as e:
            logger.error("Search error", error=str(e), query=query)
            raise

    async def search_fuzzy(
        self,
        tenant_id: str,
        query: str,
        limit: int = 20,
        min_similarity: float = 0.3
    ) -> List[Dict[str, Any]]:
        """
        Fuzzy search using trigram similarity.

        Uses GIN index on symbol field for fast trigram matching.

        Args:
            tenant_id: Tenant UUID
            query: Search query
            limit: Maximum results
            min_similarity: Minimum similarity score (0-1)

        Returns:
            List of matching symbols ordered by similarity
        """
        sql = """
            SELECT id, symbol, file_path, line_number, column_number,
                   kind, language, signature, documentation,
                   similarity(symbol, $2) as score
            FROM aethyme.nodes
            WHERE tenant_id = $1
              AND symbol % $2  -- Trigram similarity operator
              AND similarity(symbol, $2) >= $3
            ORDER BY score DESC, symbol
            LIMIT $4
        """

        try:
            async with self.pool.acquire() as conn:
                await conn.execute("SET statement_timeout = $1", int(self.query_timeout * 1000))
                await conn.execute("SET app.current_tenant = $1", tenant_id)

                rows = await conn.fetch(sql, tenant_id, query, min_similarity, limit)

                return [dict(row) for row in rows]

        except asyncio.TimeoutError:
            logger.error("Fuzzy search timeout", query=query)
            raise
        except Exception as e:
            logger.error("Fuzzy search error", error=str(e))
            raise

    async def search_hybrid(
        self,
        tenant_id: str,
        query: str,
        limit: int = 20
    ) -> List[Dict[str, Any]]:
        """
        Hybrid search combining full-text search and fuzzy matching.

        Uses both GIN indexes:
        - search_vector for full-text search
        - symbol gin_trgm_ops for fuzzy matching

        Args:
            tenant_id: Tenant UUID
            query: Search query
            limit: Maximum results

        Returns:
            Combined and deduplicated results
        """
        sql = """
            WITH fts_results AS (
                -- Full-text search on symbol and documentation
                SELECT DISTINCT ON (id)
                       id, symbol, file_path, line_number, column_number,
                       kind, language, signature, documentation,
                       ts_rank(search_vector, plainto_tsquery('english', $2)) as fts_score,
                       0.0 as fuzzy_score
                FROM aethyme.nodes
                WHERE tenant_id = $1
                  AND search_vector @@ plainto_tsquery('english', $2)
                LIMIT $3
            ),
            fuzzy_results AS (
                -- Fuzzy matching on symbol name
                SELECT DISTINCT ON (id)
                       id, symbol, file_path, line_number, column_number,
                       kind, language, signature, documentation,
                       0.0 as fts_score,
                       similarity(symbol, $2) as fuzzy_score
                FROM aethyme.nodes
                WHERE tenant_id = $1
                  AND symbol % $2
                LIMIT $3
            )
            -- Combine and deduplicate
            SELECT DISTINCT ON (id)
                   id, symbol, file_path, line_number, column_number,
                   kind, language, signature, documentation,
                   GREATEST(fts_score, fuzzy_score) as score
            FROM (
                SELECT * FROM fts_results
                UNION ALL
                SELECT * FROM fuzzy_results
            ) combined
            ORDER BY id, score DESC
            LIMIT $3
        """

        try:
            async with self.pool.acquire() as conn:
                await conn.execute("SET statement_timeout = $1", int(self.query_timeout * 1000))
                await conn.execute("SET app.current_tenant = $1", tenant_id)

                rows = await conn.fetch(sql, tenant_id, query, limit)

                # Sort by score descending
                results = [dict(row) for row in rows]
                results.sort(key=lambda x: x["score"], reverse=True)

                return results

        except asyncio.TimeoutError:
            logger.error("Hybrid search timeout", query=query)
            raise
        except Exception as e:
            logger.error("Hybrid search error", error=str(e))
            raise

    async def search_with_filters(
        self,
        tenant_id: str,
        query: str,
        kinds: Optional[List[str]] = None,
        languages: Optional[List[str]] = None,
        repository_id: Optional[str] = None,
        limit: int = 20
    ) -> List[Dict[str, Any]]:
        """
        Advanced search with filters.

        Args:
            tenant_id: Tenant UUID
            query: Search query
            kinds: Filter by node kinds (class, function, method)
            languages: Filter by languages (python, typescript)
            repository_id: Filter by specific repository
            limit: Maximum results

        Returns:
            Filtered search results
        """
        # Build dynamic query with filters
        conditions = ["tenant_id = $1"]
        params = [tenant_id, query]
        param_idx = 3

        # Add full-text search condition
        conditions.append("search_vector @@ plainto_tsquery('english', $2)")

        # Add filters
        if kinds:
            conditions.append(f"kind = ANY(${param_idx})")
            params.append(kinds)
            param_idx += 1

        if languages:
            conditions.append(f"language = ANY(${param_idx})")
            params.append(languages)
            param_idx += 1

        if repository_id:
            conditions.append(f"repository_id = ${param_idx}")
            params.append(repository_id)
            param_idx += 1

        # Build final query
        sql = f"""
            SELECT id, symbol, file_path, line_number, column_number,
                   kind, language, signature, documentation,
                   ts_rank(search_vector, plainto_tsquery('english', $2)) as score
            FROM aethyme.nodes
            WHERE {' AND '.join(conditions)}
            ORDER BY score DESC, symbol
            LIMIT ${param_idx}
        """
        params.append(limit)

        try:
            async with self.pool.acquire() as conn:
                await conn.execute("SET statement_timeout = $1", int(self.query_timeout * 1000))
                await conn.execute("SET app.current_tenant = $1", tenant_id)

                rows = await conn.fetch(sql, *params)

                return [dict(row) for row in rows]

        except Exception as e:
            logger.error("Filtered search error", error=str(e), filters={
                "kinds": kinds,
                "languages": languages,
                "repository_id": repository_id
            })
            raise

    async def batch_get_symbols(
        self,
        tenant_id: str,
        symbol_ids: List[str]
    ) -> Dict[str, Dict[str, Any]]:
        """
        Batch load symbols by IDs (N+1 query prevention).

        Args:
            tenant_id: Tenant UUID
            symbol_ids: List of symbol IDs to load

        Returns:
            Dict mapping symbol ID to symbol data
        """
        if not symbol_ids:
            return {}

        sql = """
            SELECT id, symbol, file_path, line_number, column_number,
                   kind, language, signature, documentation
            FROM aethyme.nodes
            WHERE tenant_id = $1 AND id = ANY($2)
        """

        try:
            async with self.pool.acquire() as conn:
                await conn.execute("SET app.current_tenant = $1", tenant_id)
                rows = await conn.fetch(sql, tenant_id, symbol_ids)

                return {row["id"]: dict(row) for row in rows}

        except Exception as e:
            logger.error("Batch get error", error=str(e), count=len(symbol_ids))
            raise


# Connection pool management

_pool: Optional[asyncpg.Pool] = None


async def get_pool() -> asyncpg.Pool:
    """Get or create connection pool."""
    global _pool

    if _pool is None:
        # Read connection config from environment
        import os
        _pool = await asyncpg.create_pool(
            host=os.getenv("DB_HOST", "localhost"),
            port=int(os.getenv("DB_PORT", "5432")),
            database=os.getenv("DB_NAME", "aethyme"),
            user=os.getenv("DB_USER", "aethyme"),
            password=os.getenv("DB_PASSWORD", ""),
            min_size=5,
            max_size=20,
            command_timeout=5.0,
            # Connection pool optimizations
            max_queries=50000,
            max_inactive_connection_lifetime=300.0,
        )

    return _pool


async def close_pool():
    """Close connection pool."""
    global _pool
    if _pool:
        await _pool.close()
        _pool = None


@asynccontextmanager
async def get_search_service():
    """Get optimized search service instance."""
    pool = await get_pool()
    yield OptimizedSearchService(pool)
