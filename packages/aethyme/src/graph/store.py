"""PostgreSQL-backed graph storage with production features."""

from typing import Optional, List, Dict, Any, Tuple
import json
import os
import structlog
from psycopg2.extras import execute_batch, Json

from .connection_pool import db_pool
from ..models.graph import Node, Edge, Repository

logger = structlog.get_logger(__name__)


class GraphStore:
    """PostgreSQL-backed graph storage with multi-tenant support."""

    def __init__(self, tenant_id: Optional[str] = None, scopes: Optional[List[str]] = None):
        """
        Initialize graph store with optional tenant context.

        Args:
            tenant_id: UUID of the tenant for row-level security
        """
        self.tenant_id = tenant_id
        self.db_pool = db_pool
        self.scopes = self._resolve_scopes(scopes)

    def _resolve_scopes(self, scopes: Optional[List[str]]) -> Optional[List[str]]:
        if scopes is not None:
            return scopes
        scopes_env = os.getenv("AETHYME_DB_SCOPES") or os.getenv("REPOGRAPH_DB_SCOPES")
        if scopes_env is None:
            return None
        scopes_env = scopes_env.strip()
        if not scopes_env:
            return None
        try:
            scopes = json.loads(scopes_env)
        except json.JSONDecodeError as exc:
            raise ValueError(
                "Invalid scopes JSON in AETHYME_DB_SCOPES/REPOGRAPH_DB_SCOPES"
            ) from exc
        if not isinstance(scopes, list) or any(not isinstance(s, str) for s in scopes):
            raise ValueError(
                "AETHYME_DB_SCOPES/REPOGRAPH_DB_SCOPES must be a JSON array of strings"
            )
        return scopes

    def set_tenant(self, tenant_id: str) -> None:
        """Set tenant context for all operations."""
        self.tenant_id = tenant_id

    def set_scopes(self, scopes: Optional[List[str]]) -> None:
        """Set scope context for all operations."""
        self.scopes = scopes

    def apply_session(self, cur) -> None:
        """Public wrapper for setting session variables."""
        self._apply_session(cur)

    def _apply_session(self, cur) -> None:
        if self.tenant_id:
            cur.execute("SET app.current_tenant = %s", (self.tenant_id,))
        if self.scopes:
            cur.execute("SET app.current_scopes = %s", (json.dumps(self.scopes),))

    def _execute_with_session(
        self,
        query: str,
        params: Optional[tuple] = None,
        fetch: bool = True,
    ) -> Optional[List[Dict[str, Any]]]:
        with self.db_pool.get_connection() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)
                cur.execute(query, params)
                if fetch:
                    return cur.fetchall()
        return None

    # ============= Repository Management =============

    def create_repository(
        self,
        name: str,
        path: str,
        languages: List[str] = None,
    ) -> Dict[str, Any]:
        """Create or update a repository."""
        query = """
            INSERT INTO aethyme.repositories (tenant_id, name, path, languages)
            VALUES (%s, %s, %s, %s)
            ON CONFLICT (tenant_id, name) DO UPDATE
            SET path = EXCLUDED.path,
                languages = EXCLUDED.languages,
                updated_at = CURRENT_TIMESTAMP
            RETURNING *
        """
        params = (
            self.tenant_id,
            name,
            path,
            languages or ["python", "typescript"],
        )

        with self.db_pool.get_connection() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)
                cur.execute(query, params)
                return cur.fetchone()

    def get_repository(self, name: str) -> Optional[Dict[str, Any]]:
        """Get repository by name."""
        query = """
            SELECT * FROM aethyme.repositories
            WHERE tenant_id = %s AND name = %s
        """
        result = self._execute_with_session(query, (self.tenant_id, name))
        return result[0] if result else None

    # ============= Node Management =============

    def insert_nodes(self, nodes: List[Node], repository_id: str) -> int:
        """
        Bulk insert nodes with conflict resolution.

        Args:
            nodes: List of Node objects
            repository_id: UUID of the repository

        Returns:
            Number of nodes inserted/updated
        """
        query = """
            INSERT INTO aethyme.nodes (
                id, display_id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language,
                signature, documentation, metadata
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (tenant_id, repository_id, symbol, file_path, line_number, column_number)
            DO UPDATE SET
                signature = EXCLUDED.signature,
                documentation = EXCLUDED.documentation,
                metadata = EXCLUDED.metadata,
                indexed_at = CURRENT_TIMESTAMP
        """

        params_list = [
            (
                node.id,
                node.display_id,
                self.tenant_id,
                repository_id,
                node.symbol,
                node.file_path,
                node.line_number,
                node.column_number,
                node.kind,
                node.language,
                node.signature,
                node.documentation,
                Json(node.metadata) if node.metadata else None,
            )
            for node in nodes
        ]

        with self.db_pool.transaction() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)
                execute_batch(cur, query, params_list, page_size=1000)
                affected = cur.rowcount

        logger.info(
            "Nodes inserted",
            count=affected,
            repository_id=repository_id,
            tenant_id=self.tenant_id,
        )
        return affected

    def get_node(self, node_id: str) -> Optional[Dict[str, Any]]:
        """Get a single node by ID."""
        query = """
            SELECT * FROM aethyme.nodes
            WHERE id = %s AND tenant_id = %s
        """
        result = self._execute_with_session(query, (node_id, self.tenant_id))
        return result[0] if result else None

    def find_definition(self, symbol: str) -> Optional[Dict[str, Any]]:
        """Find definition node by symbol name."""
        query = """
            SELECT * FROM aethyme.nodes
            WHERE tenant_id = %s
              AND symbol = %s
              AND kind IN ('def', 'class', 'function', 'method')
            ORDER BY indexed_at DESC
            LIMIT 1
        """
        result = self._execute_with_session(query, (self.tenant_id, symbol))
        return result[0] if result else None

    # ============= Edge Management =============

    def insert_edges(self, edges: List[Edge], repository_id: str) -> int:
        """
        Bulk insert edges with conflict resolution.

        Args:
            edges: List of Edge objects
            repository_id: UUID of the repository

        Returns:
            Number of edges inserted/updated
        """
        query = """
            INSERT INTO aethyme.edges (
                id, tenant_id, repository_id, from_node_id,
                to_node_id, edge_type, weight, metadata
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (tenant_id, repository_id, from_node_id, to_node_id, edge_type)
            DO UPDATE SET
                weight = EXCLUDED.weight,
                metadata = EXCLUDED.metadata
        """

        params_list = [
            (
                edge.id,
                self.tenant_id,
                repository_id,
                edge.from_node_id,
                edge.to_node_id,
                edge.edge_type,
                edge.weight,
                Json(edge.metadata) if edge.metadata else None,
            )
            for edge in edges
        ]

        with self.db_pool.transaction() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)
                execute_batch(cur, query, params_list, page_size=1000)
                affected = cur.rowcount

        logger.info(
            "Edges inserted",
            count=affected,
            repository_id=repository_id,
            tenant_id=self.tenant_id,
        )
        return affected

    # ============= Graph Queries =============

    def ego_graph(
        self,
        symbol: str,
        depth: int = 1,
        limit: int = 100,
    ) -> Dict[str, Any]:
        """
        Get ego graph using recursive CTE.

        Args:
            symbol: Symbol to start from
            depth: Maximum depth to traverse
            limit: Maximum number of nodes to return

        Returns:
            Dictionary with definition and nodes by depth
        """
        query = """
            WITH RECURSIVE ego_graph AS (
                SELECT n.id, n.tenant_id, n.repository_id, n.symbol, n.file_path,
                       n.line_number, n.column_number, n.kind, n.language,
                       n.signature, n.documentation, n.metadata, n.indexed_at,
                       0 as depth, n.id as root_id
                FROM aethyme.nodes n
                WHERE n.tenant_id = %s
                  AND n.symbol = %s
                  AND n.kind IN ('def', 'class', 'function', 'method')
                UNION ALL
                SELECT n.id, n.tenant_id, n.repository_id, n.symbol, n.file_path,
                       n.line_number, n.column_number, n.kind, n.language,
                       n.signature, n.documentation, n.metadata, n.indexed_at,
                       eg.depth + 1, eg.root_id
                FROM ego_graph eg
                JOIN aethyme.edges e ON (
                    (e.from_node_id = eg.id OR e.to_node_id = eg.id)
                    AND e.tenant_id = %s
                )
                JOIN aethyme.nodes n ON (
                    (n.id = e.from_node_id OR n.id = e.to_node_id)
                    AND n.tenant_id = %s
                    AND n.id != eg.id
                )
                WHERE eg.depth < %s
            )
            SELECT * FROM ego_graph
            ORDER BY depth, symbol
            LIMIT %s
        """

        with self.db_pool.get_connection() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)

                cur.execute(
                    query,
                    (self.tenant_id, symbol, self.tenant_id, self.tenant_id, depth, limit)
                )
                nodes = cur.fetchall()

        if not nodes:
            return {"error": "Symbol not found", "symbol": symbol}

        # Structure response by depth
        result = {
            "definition": nodes[0] if nodes[0]["depth"] == 0 else None,
            "nodes_by_depth": {},
            "total_nodes": len(nodes),
        }

        for node in nodes:
            node_depth = node["depth"]
            if node_depth not in result["nodes_by_depth"]:
                result["nodes_by_depth"][node_depth] = []
            result["nodes_by_depth"][node_depth].append(node)

        return result

    def impact_analysis(
        self,
        symbol: str,
        max_depth: int = 10,
        limit: int = 1000,
    ) -> Dict[str, Any]:
        """
        Find all symbols impacted by changes using recursive CTE.

        Args:
            symbol: Symbol to analyze impact for
            max_depth: Maximum depth to traverse
            limit: Maximum number of results

        Returns:
            Dictionary with impact analysis results
        """
        query = """
            WITH RECURSIVE impact_tree AS (
                -- Base: starting symbol
                SELECT n.id, n.symbol, n.file_path, 0 as depth,
                       ARRAY[n.id] as path, false as is_cycle
                FROM aethyme.nodes n
                WHERE n.tenant_id = %s
                  AND n.symbol = %s
                  AND n.kind IN ('def', 'class', 'function', 'method')
                LIMIT 1

                UNION ALL

                -- Find all callers recursively
                SELECT n.id, n.symbol, n.file_path, it.depth + 1,
                       it.path || n.id,
                       n.id = ANY(it.path) as is_cycle
                FROM impact_tree it
                JOIN aethyme.edges e ON (
                    e.to_node_id = it.id
                    AND e.tenant_id = %s
                    AND e.edge_type IN ('invoke', 'import', 'inherit')
                )
                JOIN aethyme.nodes n ON (
                    n.id = e.from_node_id
                    AND n.tenant_id = %s
                )
                WHERE it.depth < %s
                  AND NOT it.is_cycle
            )
            SELECT DISTINCT ON (id) id, symbol, file_path, depth
            FROM impact_tree
            WHERE NOT is_cycle
            ORDER BY id, depth
            LIMIT %s
        """

        with self.db_pool.get_connection() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)

                cur.execute(
                    query,
                    (self.tenant_id, symbol, self.tenant_id, self.tenant_id, max_depth, limit)
                )
                results = cur.fetchall()

        if not results:
            return {"error": "Symbol not found", "symbol": symbol}

        # Group by depth for analysis
        by_depth = {}
        for row in results[1:]:  # Skip the root node
            depth = row["depth"]
            if depth not in by_depth:
                by_depth[depth] = []
            by_depth[depth].append({
                "symbol": row["symbol"],
                "file": row["file_path"],
            })

        return {
            "symbol": symbol,
            "total_impacted": len(results) - 1,
            "max_depth_reached": max(r["depth"] for r in results) if results else 0,
            "by_depth": by_depth,
        }

    def search(
        self,
        query: str,
        limit: int = 20,
        search_type: str = "hybrid",
    ) -> List[Dict[str, Any]]:
        """
        Search for symbols using PostgreSQL full-text search.

        Args:
            query: Search query
            limit: Maximum results
            search_type: 'exact', 'fuzzy', or 'hybrid'

        Returns:
            List of matching nodes with relevance scores
        """
        if search_type == "exact":
            sql = """
                SELECT *, 1.0 as score
                FROM aethyme.nodes
                WHERE tenant_id = %s AND symbol = %s
                LIMIT %s
            """
            params = (self.tenant_id, query, limit)

        elif search_type == "fuzzy":
            sql = """
                SELECT *,
                       similarity(symbol, %s) as score
                FROM aethyme.nodes
                WHERE tenant_id = %s
                  AND symbol %% %s  -- trigram similarity operator (escaped)
                ORDER BY score DESC
                LIMIT %s
            """
            params = (query, self.tenant_id, query, limit)

        else:  # hybrid - combines FTS and fuzzy
            sql = """
                WITH fts_results AS (
                    SELECT id, tenant_id, repository_id, symbol, file_path,
                           line_number, column_number, kind, language,
                           signature, documentation,
                           ts_rank(search_vector, plainto_tsquery('english', %s)) as fts_score,
                           0.0 as fuzzy_score
                    FROM aethyme.nodes
                    WHERE tenant_id = %s
                      AND search_vector @@ plainto_tsquery('english', %s)
                    LIMIT %s
                ),
                fuzzy_results AS (
                    SELECT id, tenant_id, repository_id, symbol, file_path,
                           line_number, column_number, kind, language,
                           signature, documentation,
                           0.0 as fts_score,
                           similarity(symbol, %s) as fuzzy_score
                    FROM aethyme.nodes
                    WHERE tenant_id = %s
                      AND symbol %% %s  -- trigram similarity operator (escaped)
                    LIMIT %s
                )
                SELECT DISTINCT ON (id)
                       id, tenant_id, repository_id, symbol, file_path,
                       line_number, column_number, kind, language,
                       signature, documentation,
                       GREATEST(fts_score, fuzzy_score) as score
                FROM (
                    SELECT * FROM fts_results
                    UNION ALL
                    SELECT * FROM fuzzy_results
                ) combined
                ORDER BY id, score DESC
                LIMIT %s
            """
            params = (
                query, self.tenant_id, query, limit,
                query, self.tenant_id, query, limit,
                limit
            )

        results = self._execute_with_session(sql, params)
        return results or []

    # ============= Statistics =============

    def get_statistics(self) -> Dict[str, Any]:
        """Get graph statistics for the current tenant."""
        stats_query = """
            SELECT
                (SELECT COUNT(*) FROM aethyme.nodes WHERE tenant_id = %s) as total_nodes,
                (SELECT COUNT(*) FROM aethyme.edges WHERE tenant_id = %s) as total_edges,
                (SELECT COUNT(DISTINCT kind) FROM aethyme.nodes WHERE tenant_id = %s) as node_types,
                (SELECT COUNT(DISTINCT edge_type) FROM aethyme.edges WHERE tenant_id = %s) as edge_types,
                (SELECT COUNT(DISTINCT file_path) FROM aethyme.nodes WHERE tenant_id = %s) as total_files,
                (SELECT COUNT(DISTINCT language) FROM aethyme.nodes WHERE tenant_id = %s) as languages
        """

        result = self._execute_with_session(
            stats_query,
            (self.tenant_id,) * 6
        )

        return result[0] if result else {}

    def clear_repository(self, repository_id: str) -> Dict[str, int]:
        """Clear all data for a repository (for re-indexing)."""
        with self.db_pool.transaction() as conn:
            with conn.cursor() as cur:
                self._apply_session(cur)

                # Delete edges first (foreign key constraint)
                cur.execute(
                    "DELETE FROM aethyme.edges WHERE repository_id = %s AND tenant_id = %s",
                    (repository_id, self.tenant_id)
                )
                edges_deleted = cur.rowcount

                # Then delete nodes
                cur.execute(
                    "DELETE FROM aethyme.nodes WHERE repository_id = %s AND tenant_id = %s",
                    (repository_id, self.tenant_id)
                )
                nodes_deleted = cur.rowcount

        logger.info(
            "Repository cleared",
            repository_id=repository_id,
            nodes_deleted=nodes_deleted,
            edges_deleted=edges_deleted,
        )

        return {
            "nodes_deleted": nodes_deleted,
            "edges_deleted": edges_deleted,
        }
