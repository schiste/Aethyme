"""Graph construction module for building code graphs from indexed data."""

from typing import List, Dict, Any, Optional, Callable, Tuple, Set
from collections import defaultdict
from dataclasses import dataclass
import structlog

from ..models.graph import Node, Edge, EdgeType, NodeKind
from ..graph.store import GraphStore
from .scip_wrapper import SCIPIndexer
from .fallback_indexer import FallbackIndexer

logger = structlog.get_logger(__name__)


@dataclass
class GraphBuilderStats:
    """Statistics from graph building process."""

    nodes_processed: int = 0
    edges_created: int = 0
    definitions_found: int = 0
    references_found: int = 0
    files_processed: int = 0
    errors: List[str] = None

    def __post_init__(self):
        if self.errors is None:
            self.errors = []


class GraphBuilder:
    """Builds and manages code graphs from various sources."""

    def __init__(
        self,
        store: GraphStore,
        batch_size: int = 1000,
        progress_callback: Optional[Callable[[int, int, str], None]] = None,
    ):
        """
        Initialize graph builder.

        Args:
            store: GraphStore instance for persistence
            batch_size: Batch size for database operations
            progress_callback: Optional callback for progress reporting
        """
        self.store = store
        self.batch_size = batch_size
        self.progress_callback = progress_callback

        # Track nodes and edges during building
        self.nodes: List[Node] = []
        self.edges: List[Edge] = []
        self.symbol_to_def: Dict[str, str] = {}  # symbol -> node_id mapping
        self.stats = GraphBuilderStats()

    def build_from_scip(
        self,
        scip_data: Dict[str, Any],
        repository_id: str,
        language: str,
    ) -> GraphBuilderStats:
        """
        Build graph from SCIP indexer output.

        Args:
            scip_data: Output from SCIP indexer
            repository_id: Repository UUID
            language: Programming language

        Returns:
            Statistics from the build process
        """
        logger.info(
            "Building graph from SCIP data",
            repository_id=repository_id,
            language=language,
        )

        # Extract nodes from SCIP data
        for doc in scip_data.get("documents", []):
            self._process_scip_document(doc, language)

        # Build edges based on references
        self._build_reference_edges()

        # Add containment edges (file -> definitions)
        self._build_containment_edges()

        # Persist to database
        self._persist_graph(repository_id)

        logger.info(
            "Graph built from SCIP",
            stats=self.stats,
        )

        return self.stats

    def build_from_fallback(
        self,
        nodes: List[Node],
        edges: List[tuple],
        repository_id: str,
    ) -> GraphBuilderStats:
        """
        Build graph from fallback indexer output.

        Args:
            nodes: List of nodes from fallback indexer
            edges: List of edge tuples
            repository_id: Repository UUID

        Returns:
            Statistics from the build process
        """
        logger.info(
            "Building graph from fallback indexer",
            repository_id=repository_id,
            nodes_count=len(nodes),
            edges_count=len(edges),
        )

        self.nodes = nodes
        self.stats.nodes_processed = len(nodes)

        # Build symbol mapping
        for node in nodes:
            if node.kind in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION, NodeKind.METHOD]:
                self.symbol_to_def[node.symbol] = node.id
                self.stats.definitions_found += 1
            elif node.kind == NodeKind.REFERENCE:
                self.stats.references_found += 1

        # Convert edge tuples to Edge objects
        for from_id, to_id, edge_type in edges:
            edge = Edge.create(
                from_node_id=from_id,
                to_node_id=to_id,
                edge_type=edge_type,
            )
            self.edges.append(edge)
            self.stats.edges_created += 1

        # Add additional edges
        self._build_containment_edges()
        self._build_import_edges()

        # Persist to database
        self._persist_graph(repository_id)

        return self.stats

    def _process_scip_document(self, doc: Dict[str, Any], language: str) -> None:
        """Process a single SCIP document."""
        file_path = doc.get("relative_path", "")
        if not file_path:
            return

        self.stats.files_processed += 1

        for occurrence in doc.get("occurrences", []):
            try:
                node = self._create_node_from_occurrence(
                    occurrence,
                    file_path,
                    language,
                )
                if node:
                    self.nodes.append(node)
                    self.stats.nodes_processed += 1

                    # Track definitions for edge building
                    if node.kind in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION]:
                        self.symbol_to_def[node.symbol] = node.id
                        self.stats.definitions_found += 1
                    elif node.kind == NodeKind.REFERENCE:
                        self.stats.references_found += 1

            except Exception as e:
                logger.warning(
                    "Failed to process occurrence",
                    error=str(e),
                    file=file_path,
                )
                self.stats.errors.append(f"Failed to process occurrence in {file_path}: {e}")

        # Report progress
        if self.progress_callback:
            self.progress_callback(
                self.stats.files_processed,
                len(doc),
                f"Processing {file_path}",
            )

    def _create_node_from_occurrence(
        self,
        occurrence: Dict[str, Any],
        file_path: str,
        language: str,
    ) -> Optional[Node]:
        """Create a Node from a SCIP occurrence."""
        symbol = occurrence.get("symbol", "")
        if not symbol:
            return None

        # Extract position
        range_data = occurrence.get("range", [0, 0, 0, 0])
        line = range_data[0] if len(range_data) > 0 else 0
        column = range_data[1] if len(range_data) > 1 else 0

        # Determine node kind
        symbol_roles = occurrence.get("symbol_roles", 0)
        is_definition = bool(symbol_roles & 1)  # Role 1 = Definition

        # Determine specific kind based on symbol and syntax
        kind = self._determine_node_kind(symbol, is_definition, occurrence)

        # Extract documentation
        documentation = occurrence.get("hover_text", "")
        signature = None

        # Try to extract signature from hover text
        if documentation and "\n" in documentation:
            lines = documentation.split("\n")
            if lines[0].startswith("```"):
                # Code block, extract signature
                signature = lines[1] if len(lines) > 1 else None

        return Node(
            symbol=symbol,
            file_path=file_path,
            line_number=line,
            column_number=column,
            kind=kind,
            language=language,
            signature=signature,
            documentation=documentation,
            metadata={
                "syntax_kind": occurrence.get("syntax_kind"),
                "symbol_roles": symbol_roles,
            },
        )

    def _determine_node_kind(
        self,
        symbol: str,
        is_definition: bool,
        occurrence: Dict[str, Any],
    ) -> str:
        """Determine the specific kind of node."""
        if not is_definition:
            return NodeKind.REFERENCE

        # Use syntax kind if available
        syntax_kind = occurrence.get("syntax_kind", "").lower()

        if "class" in syntax_kind or "interface" in syntax_kind:
            return NodeKind.CLASS
        elif "function" in syntax_kind or "method" in syntax_kind:
            return NodeKind.FUNCTION
        elif "import" in syntax_kind:
            return NodeKind.IMPORT
        elif "variable" in syntax_kind or "const" in syntax_kind:
            return NodeKind.VARIABLE

        # Fall back to analyzing symbol structure
        if "#class" in symbol or "#interface" in symbol:
            return NodeKind.CLASS
        elif "#method" in symbol or "#function" in symbol:
            return NodeKind.FUNCTION
        elif "#import" in symbol:
            return NodeKind.IMPORT

        return NodeKind.DEFINITION

    def _build_reference_edges(self) -> None:
        """Build edges from references to definitions."""
        for node in self.nodes:
            if node.kind == NodeKind.REFERENCE:
                # Find the definition for this reference
                def_node_id = self.symbol_to_def.get(node.symbol)
                if def_node_id:
                    edge = Edge.create(
                        from_node_id=node.id,
                        to_node_id=def_node_id,
                        edge_type=EdgeType.INVOKE,
                    )
                    self.edges.append(edge)
                    self.stats.edges_created += 1

    def _build_containment_edges(self) -> None:
        """Build edges from files to their contained definitions."""
        file_to_nodes: Dict[str, List[str]] = defaultdict(list)

        for node in self.nodes:
            if node.kind in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION, NodeKind.METHOD]:
                file_to_nodes[node.file_path].append(node.id)

        # Create file nodes and containment edges
        for file_path, node_ids in file_to_nodes.items():
            # Create a file node
            file_node = Node(
                symbol=f"file:{file_path}",
                file_path=file_path,
                line_number=0,
                column_number=0,
                kind=NodeKind.FILE,
                language="",
                metadata={"node_count": len(node_ids)},
            )
            self.nodes.append(file_node)

            # Create containment edges
            for node_id in node_ids:
                edge = Edge.create(
                    from_node_id=file_node.id,
                    to_node_id=node_id,
                    edge_type=EdgeType.CONTAIN,
                )
                self.edges.append(edge)
                self.stats.edges_created += 1

    def _build_import_edges(self) -> None:
        """Build edges for import relationships."""
        # Track imports by file
        imports_by_file: Dict[str, Set[str]] = defaultdict(set)

        for node in self.nodes:
            if node.kind == NodeKind.IMPORT:
                imports_by_file[node.file_path].add(node.symbol)

        # Create edges from importing file to imported symbols
        for file_path, imported_symbols in imports_by_file.items():
            # Find file node
            file_node = next(
                (n for n in self.nodes if n.kind == NodeKind.FILE and n.file_path == file_path),
                None
            )

            if file_node:
                for symbol in imported_symbols:
                    # Try to find definition of imported symbol
                    def_node_id = self.symbol_to_def.get(symbol)
                    if def_node_id:
                        edge = Edge.create(
                            from_node_id=file_node.id,
                            to_node_id=def_node_id,
                            edge_type=EdgeType.IMPORT,
                        )
                        self.edges.append(edge)
                        self.stats.edges_created += 1

    def _persist_graph(self, repository_id: str) -> None:
        """Persist nodes and edges to database in batches."""
        logger.info(
            "Persisting graph to database",
            nodes=len(self.nodes),
            edges=len(self.edges),
            repository_id=repository_id,
        )

        # Insert nodes in batches
        for i in range(0, len(self.nodes), self.batch_size):
            batch = self.nodes[i:i + self.batch_size]
            self.store.insert_nodes(batch, repository_id)

            if self.progress_callback:
                self.progress_callback(
                    i + len(batch),
                    len(self.nodes),
                    "Persisting nodes",
                )

        # Insert edges in batches
        for i in range(0, len(self.edges), self.batch_size):
            batch = self.edges[i:i + self.batch_size]
            self.store.insert_edges(batch, repository_id)

            if self.progress_callback:
                self.progress_callback(
                    i + len(batch),
                    len(self.edges),
                    "Persisting edges",
                )

        logger.info("Graph persisted successfully")

    def analyze_graph(self) -> Dict[str, Any]:
        """Analyze the built graph for statistics and patterns."""
        analysis = {
            "total_nodes": len(self.nodes),
            "total_edges": len(self.edges),
            "node_types": defaultdict(int),
            "edge_types": defaultdict(int),
            "top_symbols": [],
            "orphan_nodes": 0,
            "strongly_connected_components": 0,
        }

        # Count node types
        for node in self.nodes:
            analysis["node_types"][node.kind] += 1

        # Count edge types
        for edge in self.edges:
            analysis["edge_types"][edge.edge_type] += 1

        # Find orphan nodes (nodes with no edges)
        node_ids_with_edges = set()
        for edge in self.edges:
            node_ids_with_edges.add(edge.from_node_id)
            node_ids_with_edges.add(edge.to_node_id)

        for node in self.nodes:
            if node.id not in node_ids_with_edges:
                analysis["orphan_nodes"] += 1

        # Find most referenced symbols
        reference_counts = defaultdict(int)
        for edge in self.edges:
            if edge.edge_type == EdgeType.INVOKE:
                reference_counts[edge.to_node_id] += 1

        # Get top 10 most referenced symbols
        top_refs = sorted(
            reference_counts.items(),
            key=lambda x: x[1],
            reverse=True,
        )[:10]

        # Convert node IDs to symbols
        id_to_symbol = {node.id: node.symbol for node in self.nodes}
        analysis["top_symbols"] = [
            {"symbol": id_to_symbol.get(node_id, "unknown"), "references": count}
            for node_id, count in top_refs
        ]

        return analysis


class StreamingGraphBuilder(GraphBuilder):
    """Graph builder that streams data to database to avoid memory issues."""

    def __init__(
        self,
        store: GraphStore,
        batch_size: int = 1000,
        progress_callback: Optional[Callable[[int, int, str], None]] = None,
    ):
        """Initialize streaming graph builder."""
        super().__init__(store, batch_size, progress_callback)
        self.current_batch_nodes: List[Node] = []
        self.current_batch_edges: List[Edge] = []

    def add_node(self, node: Node, repository_id: str) -> None:
        """Add a node, flushing to database when batch is full."""
        self.current_batch_nodes.append(node)
        self.stats.nodes_processed += 1

        if node.kind in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION]:
            self.symbol_to_def[node.symbol] = node.id
            self.stats.definitions_found += 1

        if len(self.current_batch_nodes) >= self.batch_size:
            self._flush_nodes(repository_id)

    def add_edge(self, edge: Edge, repository_id: str) -> None:
        """Add an edge, flushing to database when batch is full."""
        self.current_batch_edges.append(edge)
        self.stats.edges_created += 1

        if len(self.current_batch_edges) >= self.batch_size:
            self._flush_edges(repository_id)

    def _flush_nodes(self, repository_id: str) -> None:
        """Flush current batch of nodes to database."""
        if self.current_batch_nodes:
            self.store.insert_nodes(self.current_batch_nodes, repository_id)
            self.current_batch_nodes = []

    def _flush_edges(self, repository_id: str) -> None:
        """Flush current batch of edges to database."""
        if self.current_batch_edges:
            self.store.insert_edges(self.current_batch_edges, repository_id)
            self.current_batch_edges = []

    def finalize(self, repository_id: str) -> GraphBuilderStats:
        """Flush any remaining data and return statistics."""
        self._flush_nodes(repository_id)
        self._flush_edges(repository_id)
        return self.stats