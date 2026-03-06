"""Graph construction module for building code graphs from indexed data."""

from collections import defaultdict
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, TypeAlias, TypedDict, cast

import structlog

from ..graph.store import GraphStore
from ..models.graph import Edge, EdgeType, Node, NodeKind

logger = structlog.get_logger(__name__)

EdgeTuple: TypeAlias = tuple[str, str, str]


class TopSymbol(TypedDict):
    """Most referenced symbol entry."""

    symbol: str
    references: int


class GraphAnalysis(TypedDict):
    """Shape returned by analyze_graph."""

    total_nodes: int
    total_edges: int
    node_types: dict[str, int]
    edge_types: dict[str, int]
    top_symbols: list[TopSymbol]
    orphan_nodes: int
    strongly_connected_components: int


def _new_error_list() -> list[str]:
    """Return a fresh error list for dataclass defaults."""
    return []


@dataclass
class GraphBuilderStats:
    """Statistics from graph building process."""

    nodes_processed: int = 0
    edges_created: int = 0
    definitions_found: int = 0
    references_found: int = 0
    files_processed: int = 0
    errors: list[str] = field(default_factory=_new_error_list)


class GraphBuilder:
    """Builds and manages code graphs from various sources."""

    def __init__(
        self,
        store: GraphStore,
        batch_size: int = 1000,
        progress_callback: Callable[[int, int, str], None] | None = None,
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
        self.nodes: list[Node] = []
        self.edges: list[Edge] = []
        self.symbol_to_def: dict[str, str] = {}  # symbol -> node_id mapping
        self.stats = GraphBuilderStats()

    def _reset_state(self) -> None:
        """Reset per-build state so one builder can safely handle multiple runs."""
        self.nodes = []
        self.edges = []
        self.symbol_to_def = {}
        self.stats = GraphBuilderStats()

    def build_from_scip(
        self,
        scip_data: dict[str, Any],
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

        self._reset_state()

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
        nodes: list[Node],
        edges: list[EdgeTuple],
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

        self._reset_state()

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
        self._build_reference_edges()
        self._build_containment_edges()
        self._build_import_edges()

        # Persist to database
        self._persist_graph(repository_id)

        return self.stats

    def _process_scip_document(self, doc: dict[str, Any], language: str) -> None:
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
        occurrence: dict[str, Any],
        file_path: str,
        language: str,
    ) -> Node | None:
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
        occurrence: dict[str, Any],
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
        imports_by_file: dict[str, list[Node]] = defaultdict(list)
        for node in self.nodes:
            if node.kind == NodeKind.IMPORT:
                imports_by_file[node.file_path].append(node)

        seen_edges: set[tuple[str, str, str]] = set()
        for node in self.nodes:
            if node.kind == NodeKind.REFERENCE:
                for def_node_id in self._resolve_reference_targets(
                    node, imports_by_file.get(node.file_path, [])
                ):
                    edge_key = (node.id, def_node_id, EdgeType.INVOKE)
                    if edge_key in seen_edges:
                        continue
                    seen_edges.add(edge_key)
                    edge = Edge.create(
                        from_node_id=node.id,
                        to_node_id=def_node_id,
                        edge_type=EdgeType.INVOKE,
                    )
                    self.edges.append(edge)
                    self.stats.edges_created += 1

    def _resolve_reference_targets(
        self,
        reference_node: Node,
        import_nodes: list[Node],
    ) -> set[str]:
        """Resolve a fallback reference node to candidate definitions."""
        exact_match = self.symbol_to_def.get(reference_node.symbol)
        if exact_match:
            return {exact_match}

        local_name = self._reference_local_name(reference_node.symbol)
        qualifier = self._reference_qualifier(reference_node.symbol)
        matches = self._match_local_definitions(reference_node.file_path, local_name)

        for import_node in import_nodes:
            import_mode = self._extract_import_mode(import_node)
            imported_names = self._extract_imported_names(import_node)

            if import_mode == "named":
                if imported_names and local_name not in imported_names:
                    continue
            elif import_mode in {"module", "namespace"}:
                if qualifier is None or (imported_names and qualifier not in imported_names):
                    continue
            elif import_mode == "default":
                if imported_names and reference_node.symbol not in imported_names and qualifier not in imported_names:
                    continue

            import_targets = self._resolve_import_targets(import_node)
            if import_mode == "default" and qualifier is None:
                matches.update(import_targets)
                continue

            matches.update(self._filter_definition_targets(import_targets, local_name))

        if matches:
            return matches

        global_matches = self._match_definitions_by_local_name(local_name)
        if len(global_matches) == 1:
            return global_matches
        return set()

    def _build_containment_edges(self) -> None:
        """Build edges from files to their contained definitions."""
        file_to_nodes: dict[str, list[str]] = defaultdict(list)

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
        imports_by_file: dict[str, list[Node]] = defaultdict(list)

        for node in self.nodes:
            if node.kind == NodeKind.IMPORT:
                imports_by_file[node.file_path].append(node)

        # Create edges from importing file to imported symbols
        for file_path, import_nodes in imports_by_file.items():
            # Find file node
            file_node = next(
                (n for n in self.nodes if n.kind == NodeKind.FILE and n.file_path == file_path),
                None
            )

            if file_node:
                for import_node in import_nodes:
                    for def_node_id in self._resolve_import_targets(import_node):
                        edge = Edge.create(
                            from_node_id=file_node.id,
                            to_node_id=def_node_id,
                            edge_type=EdgeType.IMPORT,
                        )
                        self.edges.append(edge)
                        self.stats.edges_created += 1

    def _resolve_import_targets(self, import_node: Node) -> set[str]:
        """Resolve import nodes to definitions using module-path heuristics."""
        symbol = import_node.symbol
        direct_match = self.symbol_to_def.get(symbol)
        if direct_match:
            return {direct_match}

        normalized = self._normalize_import_symbol(symbol)
        normalized_stem = Path(normalized).with_suffix("").as_posix()
        module_name = Path(normalized_stem).name
        imported_names = self._extract_imported_names(import_node)
        import_mode = self._extract_import_mode(import_node)

        matches: set[str] = set()
        for node in self.nodes:
            if node.kind not in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION, NodeKind.METHOD]:
                continue
            node_stem = Path(node.file_path).with_suffix("").as_posix()
            symbol_name = node.symbol.rsplit(":", 1)[-1]
            local_name = symbol_name.split(".")[-1]
            if (
                import_mode == "named"
                and imported_names
                and local_name not in imported_names
                and symbol_name not in imported_names
            ):
                continue
            if (
                node_stem == normalized_stem
                or node_stem.endswith(normalized_stem)
                or Path(node_stem).name == module_name
            ):
                matches.add(node.id)

        return matches

    def _normalize_import_symbol(self, symbol: str) -> str:
        """Normalize import symbols to repository-relative module paths."""
        normalized = symbol.strip().strip("\"'")
        while normalized.startswith("."):
            normalized = normalized[1:]
        normalized = normalized.removeprefix("./").removeprefix("/")
        normalized = normalized.replace(".", "/")
        return normalized

    def _extract_imported_names(self, import_node: Node) -> list[str]:
        """Return imported symbol names if present on an import node."""
        metadata = import_node.metadata or {}
        raw_names = metadata.get("imported_names", [])
        if not isinstance(raw_names, list):
            return []

        names: list[str] = []
        for raw_name in cast(list[object], raw_names):
            if isinstance(raw_name, str) and raw_name:
                names.append(raw_name)
        return names

    def _extract_import_mode(self, import_node: Node) -> str:
        """Return the import mode carried by the fallback import node."""
        metadata = import_node.metadata or {}
        raw_mode = metadata.get("import_mode", "module")
        if isinstance(raw_mode, str) and raw_mode:
            return raw_mode
        return "module"

    def _reference_local_name(self, symbol: str) -> str:
        """Return the local symbol name from a reference expression."""
        return symbol.rsplit(".", 1)[-1]

    def _reference_qualifier(self, symbol: str) -> str | None:
        """Return the qualifier portion of a dotted reference expression."""
        if "." not in symbol:
            return None
        return symbol.split(".", 1)[0]

    def _definition_local_name(self, node: Node) -> str:
        """Return the local symbol name for a definition node."""
        symbol_name = node.symbol.rsplit(":", 1)[-1]
        return symbol_name.split(".")[-1]

    def _match_local_definitions(self, file_path: str, local_name: str) -> set[str]:
        """Match definitions in the same file by local symbol name."""
        matches: set[str] = set()
        for node in self.nodes:
            if node.file_path != file_path:
                continue
            if node.kind not in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION, NodeKind.METHOD]:
                continue
            if self._definition_local_name(node) == local_name:
                matches.add(node.id)
        return matches

    def _match_definitions_by_local_name(self, local_name: str) -> set[str]:
        """Match definitions anywhere in the graph by local symbol name."""
        matches: set[str] = set()
        for node in self.nodes:
            if node.kind not in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION, NodeKind.METHOD]:
                continue
            if self._definition_local_name(node) == local_name:
                matches.add(node.id)
        return matches

    def _filter_definition_targets(self, candidate_ids: set[str], local_name: str) -> set[str]:
        """Filter candidate definition IDs by local symbol name."""
        matches: set[str] = set()
        candidate_set = set(candidate_ids)
        for node in self.nodes:
            if node.id not in candidate_set:
                continue
            if self._definition_local_name(node) == local_name:
                matches.add(node.id)
        return matches

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

    def analyze_graph(self) -> GraphAnalysis:
        """Analyze the built graph for statistics and patterns."""
        analysis: GraphAnalysis = {
            "total_nodes": len(self.nodes),
            "total_edges": len(self.edges),
            "node_types": {},
            "edge_types": {},
            "top_symbols": [],
            "orphan_nodes": 0,
            "strongly_connected_components": 0,
        }

        # Count node types
        for node in self.nodes:
            analysis["node_types"][node.kind] = analysis["node_types"].get(node.kind, 0) + 1

        # Count edge types
        for edge in self.edges:
            analysis["edge_types"][edge.edge_type] = analysis["edge_types"].get(edge.edge_type, 0) + 1

        # Find orphan nodes (nodes with no edges)
        node_ids_with_edges: set[str] = set()
        for edge in self.edges:
            node_ids_with_edges.add(edge.from_node_id)
            node_ids_with_edges.add(edge.to_node_id)

        for node in self.nodes:
            if node.id not in node_ids_with_edges:
                analysis["orphan_nodes"] += 1

        # Find most referenced symbols
        reference_counts: dict[str, int] = {}
        for edge in self.edges:
            if edge.edge_type == EdgeType.INVOKE:
                reference_counts[edge.to_node_id] = reference_counts.get(edge.to_node_id, 0) + 1

        # Get top 10 most referenced symbols
        top_refs: list[tuple[str, int]] = sorted(
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
        progress_callback: Callable[[int, int, str], None] | None = None,
    ):
        """Initialize streaming graph builder."""
        super().__init__(store, batch_size, progress_callback)
        self.current_batch_nodes: list[Node] = []
        self.current_batch_edges: list[Edge] = []

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
