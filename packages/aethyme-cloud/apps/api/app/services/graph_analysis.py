"""
Graph Analysis Service

Analyzes code relationships and generates graph data:
- Call graphs (function caller/callee relationships)
- Class hierarchies (inheritance trees)
- Dependency graphs (import relationships)
- Impact analysis (what breaks if we change X)
"""

from typing import List, Dict, Any, Optional, Set
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, and_, or_, func
from collections import deque

from app.models.code_graph import CodeRelationship, SymbolMetadata, RelationshipType


class GraphAnalysisService:
    """Service for analyzing code relationships and generating graphs."""

    def __init__(self, db: AsyncSession):
        self.db = db

    async def get_call_graph(
        self,
        symbol_id: str,
        max_depth: int = 3,
        direction: str = "both"  # "callers", "callees", "both"
    ) -> Dict[str, Any]:
        """
        Generate call graph for a function/method.

        Args:
            symbol_id: The symbol to analyze
            max_depth: Maximum traversal depth (default 3)
            direction: "callers" (who calls this), "callees" (what this calls), or "both"

        Returns:
            Graph data structure with nodes and edges
        """
        nodes = {}
        edges = []
        visited = set()

        # Get the root symbol
        root_metadata = await self._get_symbol_metadata(symbol_id)
        if not root_metadata:
            return {"nodes": [], "edges": [], "root": symbol_id}

        nodes[symbol_id] = {
            "id": symbol_id,
            "label": symbol_id.split(".")[-1],  # Last part of qualified name
            "type": "root",
            "depth": 0,
        }

        # Traverse callers (incoming edges)
        if direction in ["callers", "both"]:
            await self._traverse_callers(
                symbol_id, nodes, edges, visited, max_depth, current_depth=0
            )

        # Traverse callees (outgoing edges)
        if direction in ["callees", "both"]:
            await self._traverse_callees(
                symbol_id, nodes, edges, visited, max_depth, current_depth=0
            )

        return {
            "nodes": list(nodes.values()),
            "edges": edges,
            "root": symbol_id,
            "stats": {
                "total_nodes": len(nodes),
                "total_edges": len(edges),
                "max_depth": max_depth,
            }
        }

    async def _traverse_callers(
        self,
        symbol_id: str,
        nodes: Dict,
        edges: List,
        visited: Set,
        max_depth: int,
        current_depth: int
    ):
        """Traverse upward (find all callers of this symbol)."""
        if current_depth >= max_depth or symbol_id in visited:
            return

        visited.add(symbol_id)

        # Find all symbols that CALL this symbol
        stmt = select(CodeRelationship).where(
            and_(
                CodeRelationship.target_symbol_id == symbol_id,
                CodeRelationship.relationship_type == RelationshipType.CALLS
            )
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        for rel in relationships:
            caller_id = rel.source_symbol_id

            # Add caller node
            if caller_id not in nodes:
                nodes[caller_id] = {
                    "id": caller_id,
                    "label": rel.source_name,
                    "type": "caller",
                    "depth": current_depth + 1,
                    "file": rel.source_file_path,
                    "kind": rel.source_kind,
                }

            # Add edge (caller -> current)
            edges.append({
                "source": caller_id,
                "target": symbol_id,
                "type": "calls",
                "file": rel.source_file_path,
                "line": rel.line_number,
            })

            # Recursively traverse
            await self._traverse_callers(
                caller_id, nodes, edges, visited, max_depth, current_depth + 1
            )

    async def _traverse_callees(
        self,
        symbol_id: str,
        nodes: Dict,
        edges: List,
        visited: Set,
        max_depth: int,
        current_depth: int
    ):
        """Traverse downward (find all functions this symbol calls)."""
        if current_depth >= max_depth or symbol_id in visited:
            return

        visited.add(symbol_id)

        # Find all symbols that this symbol CALLS
        stmt = select(CodeRelationship).where(
            and_(
                CodeRelationship.source_symbol_id == symbol_id,
                CodeRelationship.relationship_type == RelationshipType.CALLS
            )
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        for rel in relationships:
            callee_id = rel.target_symbol_id

            # Add callee node
            if callee_id not in nodes:
                nodes[callee_id] = {
                    "id": callee_id,
                    "label": rel.target_name,
                    "type": "callee",
                    "depth": current_depth + 1,
                    "file": rel.target_file_path,
                    "kind": rel.target_kind,
                }

            # Add edge (current -> callee)
            edges.append({
                "source": symbol_id,
                "target": callee_id,
                "type": "calls",
                "file": rel.source_file_path,
                "line": rel.line_number,
            })

            # Recursively traverse
            await self._traverse_callees(
                callee_id, nodes, edges, visited, max_depth, current_depth + 1
            )

    async def get_class_hierarchy(
        self,
        symbol_id: str,
        direction: str = "both"  # "parents", "children", "both"
    ) -> Dict[str, Any]:
        """
        Generate class inheritance hierarchy.

        Args:
            symbol_id: The class symbol to analyze
            direction: "parents" (superclasses), "children" (subclasses), or "both"

        Returns:
            Hierarchy tree with inheritance relationships
        """
        nodes = {}
        edges = []

        # Root node
        nodes[symbol_id] = {
            "id": symbol_id,
            "label": symbol_id.split(".")[-1],
            "type": "root",
            "level": 0,
        }

        # Get parent classes
        if direction in ["parents", "both"]:
            await self._get_parent_classes(symbol_id, nodes, edges, level=1)

        # Get child classes
        if direction in ["children", "both"]:
            await self._get_child_classes(symbol_id, nodes, edges, level=1)

        return {
            "nodes": list(nodes.values()),
            "edges": edges,
            "root": symbol_id,
        }

    async def _get_parent_classes(
        self,
        symbol_id: str,
        nodes: Dict,
        edges: List,
        level: int
    ):
        """Get all parent classes (superclasses)."""
        stmt = select(CodeRelationship).where(
            and_(
                CodeRelationship.source_symbol_id == symbol_id,
                or_(
                    CodeRelationship.relationship_type == RelationshipType.INHERITS,
                    CodeRelationship.relationship_type == RelationshipType.IMPLEMENTS
                )
            )
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        for rel in relationships:
            parent_id = rel.target_symbol_id

            if parent_id not in nodes:
                nodes[parent_id] = {
                    "id": parent_id,
                    "label": rel.target_name,
                    "type": "parent",
                    "level": level,
                    "file": rel.target_file_path,
                }

            edges.append({
                "source": symbol_id,
                "target": parent_id,
                "type": rel.relationship_type.value,
            })

            # Recursively get grandparents
            await self._get_parent_classes(parent_id, nodes, edges, level + 1)

    async def _get_child_classes(
        self,
        symbol_id: str,
        nodes: Dict,
        edges: List,
        level: int
    ):
        """Get all child classes (subclasses)."""
        stmt = select(CodeRelationship).where(
            and_(
                CodeRelationship.target_symbol_id == symbol_id,
                or_(
                    CodeRelationship.relationship_type == RelationshipType.INHERITS,
                    CodeRelationship.relationship_type == RelationshipType.IMPLEMENTS
                )
            )
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        for rel in relationships:
            child_id = rel.source_symbol_id

            if child_id not in nodes:
                nodes[child_id] = {
                    "id": child_id,
                    "label": rel.source_name,
                    "type": "child",
                    "level": -level,  # Negative for children
                    "file": rel.source_file_path,
                }

            edges.append({
                "source": child_id,
                "target": symbol_id,
                "type": rel.relationship_type.value,
            })

            # Recursively get grandchildren
            await self._get_child_classes(child_id, nodes, edges, level + 1)

    async def get_dependencies(
        self,
        file_path: str,
        repository_id: str,
        direction: str = "both"  # "imports", "imported_by", "both"
    ) -> Dict[str, Any]:
        """
        Get import dependencies for a file.

        Args:
            file_path: Path to the file
            repository_id: Repository ID
            direction: "imports" (what this file imports), "imported_by" (who imports this), or "both"

        Returns:
            Dependency graph
        """
        nodes = {}
        edges = []

        # Root node (the file)
        nodes[file_path] = {
            "id": file_path,
            "label": file_path.split("/")[-1],  # filename only
            "type": "root",
        }

        # Get imports (what this file imports)
        if direction in ["imports", "both"]:
            stmt = select(CodeRelationship).where(
                and_(
                    CodeRelationship.repository_id == repository_id,
                    CodeRelationship.source_file_path == file_path,
                    CodeRelationship.relationship_type == RelationshipType.IMPORTS
                )
            )
            result = await self.db.execute(stmt)
            relationships = result.scalars().all()

            for rel in relationships:
                target_file = rel.target_file_path

                if target_file not in nodes:
                    nodes[target_file] = {
                        "id": target_file,
                        "label": target_file.split("/")[-1],
                        "type": "import",
                    }

                edges.append({
                    "source": file_path,
                    "target": target_file,
                    "type": "imports",
                })

        # Get imported_by (who imports this file)
        if direction in ["imported_by", "both"]:
            stmt = select(CodeRelationship).where(
                and_(
                    CodeRelationship.repository_id == repository_id,
                    CodeRelationship.target_file_path == file_path,
                    CodeRelationship.relationship_type == RelationshipType.IMPORTS
                )
            )
            result = await self.db.execute(stmt)
            relationships = result.scalars().all()

            for rel in relationships:
                source_file = rel.source_file_path

                if source_file not in nodes:
                    nodes[source_file] = {
                        "id": source_file,
                        "label": source_file.split("/")[-1],
                        "type": "imported_by",
                    }

                edges.append({
                    "source": source_file,
                    "target": file_path,
                    "type": "imports",
                })

        return {
            "nodes": list(nodes.values()),
            "edges": edges,
            "root": file_path,
        }

    async def get_impact_analysis(
        self,
        symbol_id: str,
        max_depth: int = 5
    ) -> Dict[str, Any]:
        """
        Analyze what would break if we change this symbol.

        Uses BFS to find all symbols that transitively depend on this one.

        Args:
            symbol_id: The symbol to analyze
            max_depth: Maximum traversal depth

        Returns:
            Impact analysis with affected symbols
        """
        affected_symbols = set()
        queue = deque([(symbol_id, 0)])  # (symbol_id, depth)
        visited = set()

        while queue:
            current_id, depth = queue.popleft()

            if depth >= max_depth or current_id in visited:
                continue

            visited.add(current_id)

            # Find all symbols that depend on this one
            stmt = select(CodeRelationship).where(
                and_(
                    CodeRelationship.target_symbol_id == current_id,
                    or_(
                        CodeRelationship.relationship_type == RelationshipType.CALLS,
                        CodeRelationship.relationship_type == RelationshipType.REFERENCES,
                        CodeRelationship.relationship_type == RelationshipType.INHERITS
                    )
                )
            )
            result = await self.db.execute(stmt)
            relationships = result.scalars().all()

            for rel in relationships:
                dependent_id = rel.source_symbol_id
                affected_symbols.add(dependent_id)
                queue.append((dependent_id, depth + 1))

        # Get metadata for affected symbols
        affected_list = []
        for affected_id in affected_symbols:
            metadata = await self._get_symbol_metadata(affected_id)
            if metadata:
                affected_list.append(metadata)

        return {
            "symbol_id": symbol_id,
            "total_affected": len(affected_symbols),
            "affected_symbols": affected_list,
            "max_depth_searched": max_depth,
        }

    async def _get_symbol_metadata(self, symbol_id: str) -> Optional[Dict[str, Any]]:
        """Get symbol metadata."""
        stmt = select(SymbolMetadata).where(SymbolMetadata.symbol_id == symbol_id)
        result = await self.db.execute(stmt)
        metadata = result.scalar_one_or_none()

        if metadata:
            return {
                "symbol_id": metadata.symbol_id,
                "inbound_calls": metadata.inbound_call_count,
                "outbound_calls": metadata.outbound_call_count,
                "dependencies": metadata.dependency_count,
                "dependents": metadata.dependent_count,
                "impact_score": metadata.impact_score,
            }

        return None

    async def get_symbol_statistics(self, repository_id: str) -> Dict[str, Any]:
        """
        Get repository-wide symbol statistics.

        Returns:
            Statistics about symbols, relationships, and graph structure
        """
        # Count relationships by type
        stmt = select(
            CodeRelationship.relationship_type,
            func.count(CodeRelationship.id).label("count")
        ).where(
            CodeRelationship.repository_id == repository_id
        ).group_by(
            CodeRelationship.relationship_type
        )
        result = await self.db.execute(stmt)
        relationship_counts = {row[0].value: row[1] for row in result.all()}

        # Count total symbols
        stmt = select(func.count(SymbolMetadata.id)).where(
            SymbolMetadata.repository_id == repository_id
        )
        result = await self.db.execute(stmt)
        total_symbols = result.scalar() or 0

        # Find most connected symbols (high impact)
        stmt = select(SymbolMetadata).where(
            SymbolMetadata.repository_id == repository_id
        ).order_by(
            SymbolMetadata.impact_score.desc()
        ).limit(10)
        result = await self.db.execute(stmt)
        top_symbols = result.scalars().all()

        return {
            "repository_id": repository_id,
            "total_symbols": total_symbols,
            "relationship_counts": relationship_counts,
            "total_relationships": sum(relationship_counts.values()),
            "top_impact_symbols": [
                {
                    "symbol_id": s.symbol_id,
                    "impact_score": s.impact_score,
                    "inbound_calls": s.inbound_call_count,
                    "outbound_calls": s.outbound_call_count,
                }
                for s in top_symbols
            ],
        }
