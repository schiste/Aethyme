"""Graph data models for Aethyme."""

import hashlib
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum, StrEnum
from typing import Any, TypeAlias
from urllib.parse import quote


class NodeKind(StrEnum):
    """Types of nodes in the code graph."""
    DEFINITION = "def"
    REFERENCE = "ref"
    FILE = "file"
    CLASS = "class"
    FUNCTION = "function"
    METHOD = "method"
    VARIABLE = "variable"
    IMPORT = "import"


class EdgeType(StrEnum):
    """Types of edges (relationships) in the code graph."""
    INVOKE = "invoke"  # Function/method call
    IMPORT = "import"  # Import relationship
    CONTAIN = "contain"  # File contains symbol
    INHERIT = "inherit"  # Class inheritance
    IMPLEMENT = "implement"  # Interface implementation
    PROPS_FLOW = "props_flow"  # React props flow
    RETURN = "return"  # Return type
    PARAMETER = "parameter"  # Parameter type


EdgeTuple: TypeAlias = tuple[str, str, str]


def _default_languages() -> list[str]:
    return ["python", "typescript"]


def _empty_language_list() -> list[str]:
    return []


def _empty_count_map() -> dict[str, int]:
    return {}


@dataclass
class Node:
    """Represents a code symbol (definition or reference)."""

    symbol: str  # Fully qualified symbol name
    file_path: str  # Relative file path
    line_number: int
    column_number: int
    kind: str  # NodeKind value
    language: str  # Programming language

    # Optional fields
    signature: str | None = None  # Function/method signature
    documentation: str | None = None  # Docstring or JSDoc
    metadata: dict[str, Any] | None = None  # Additional metadata

    # Computed fields
    id: str = field(init=False)
    display_id: str = field(init=False)

    def __post_init__(self) -> None:
        """Normalize fields and generate unique IDs for the node."""
        if isinstance(self.kind, Enum):
            self.kind = self.kind.value
        self.metadata = self.metadata or {}
        self.id = self._generate_id()
        self.display_id = self._generate_display_id()

    def _canonical_str(self) -> str:
        """Create a canonical string from node properties."""
        return f"{self.symbol}:{self.file_path}:{self.line_number}:{self.column_number}:{self.kind}"

    def _generate_id(self) -> str:
        """Generate a unique ID based on node properties."""
        unique_str = self._canonical_str()
        return hashlib.sha256(unique_str.encode()).hexdigest()[:64]

    def _generate_display_id(self) -> str:
        """Generate a deterministic, human-readable ID for display."""
        safe_symbol = quote(self.symbol, safe="/._-")
        safe_path = quote(self.file_path, safe="/._-")
        short_hash = self.id[:10]
        return (
            f"{self.kind}:{safe_symbol}@{safe_path}:"
            f"{self.line_number}:{self.column_number}~{short_hash}"
        )

    @classmethod
    def create_definition(
        cls,
        symbol: str,
        file_path: str,
        line: int,
        column: int,
        language: str,
        kind: str = NodeKind.DEFINITION,
        signature: str | None = None,
        documentation: str | None = None,
    ) -> "Node":
        """Factory method to create a definition node."""
        return cls(
            symbol=symbol,
            file_path=file_path,
            line_number=line,
            column_number=column,
            kind=kind,
            language=language,
            signature=signature,
            documentation=documentation,
        )

    @classmethod
    def create_reference(
        cls,
        symbol: str,
        file_path: str,
        line: int,
        column: int,
        language: str,
    ) -> "Node":
        """Factory method to create a reference node."""
        return cls(
            symbol=symbol,
            file_path=file_path,
            line_number=line,
            column_number=column,
            kind=NodeKind.REFERENCE,
            language=language,
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert node to dictionary."""
        return {
            "id": self.id,
            "display_id": self.display_id,
            "symbol": self.symbol,
            "file_path": self.file_path,
            "line_number": self.line_number,
            "column_number": self.column_number,
            "kind": self.kind,
            "language": self.language,
            "signature": self.signature,
            "documentation": self.documentation,
            "metadata": self.metadata,
        }


@dataclass
class Edge:
    """Represents a relationship between two nodes."""

    from_node_id: str  # Source node ID
    to_node_id: str  # Target node ID
    edge_type: str  # EdgeType value

    # Optional fields
    weight: float = 1.0  # Edge weight for ranking
    metadata: dict[str, Any] | None = None  # Additional metadata

    # Computed fields
    id: str = field(init=False)

    def __post_init__(self) -> None:
        """Normalize fields and generate unique ID for the edge."""
        if isinstance(self.edge_type, Enum):
            self.edge_type = self.edge_type.value
        self.metadata = self.metadata or {}
        self.id = self._generate_id()

    def _generate_id(self) -> str:
        """Generate a unique ID based on edge properties."""
        unique_str = f"{self.from_node_id}:{self.to_node_id}:{self.edge_type}"
        return hashlib.sha256(unique_str.encode()).hexdigest()[:64]

    @classmethod
    def create(
        cls,
        from_node_id: str,
        to_node_id: str,
        edge_type: str,
        weight: float = 1.0,
        metadata: dict[str, Any] | None = None,
    ) -> "Edge":
        """Factory method to create an edge."""
        return cls(
            from_node_id=from_node_id,
            to_node_id=to_node_id,
            edge_type=edge_type,
            weight=weight,
            metadata=metadata,
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert edge to dictionary."""
        return {
            "id": self.id,
            "from_node_id": self.from_node_id,
            "to_node_id": self.to_node_id,
            "edge_type": self.edge_type,
            "weight": self.weight,
            "metadata": self.metadata,
        }


@dataclass
class Repository:
    """Represents a code repository to index."""

    name: str
    path: str
    languages: list[str] = field(default_factory=_default_languages)

    # Optional fields
    id: str | None = None
    org_id: str | None = None
    tenant_id: str | None = None
    last_indexed_at: datetime | None = None
    index_status: str = "pending"
    metadata: dict[str, Any] | None = None

    def __post_init__(self) -> None:
        self.metadata = self.metadata or {}

    def to_dict(self) -> dict[str, Any]:
        """Convert repository to dictionary."""
        return {
            "id": self.id,
            "org_id": self.org_id,
            "tenant_id": self.tenant_id,
            "name": self.name,
            "path": self.path,
            "languages": self.languages,
            "last_indexed_at": self.last_indexed_at.isoformat() if self.last_indexed_at else None,
            "index_status": self.index_status,
            "metadata": self.metadata,
        }


@dataclass
class GraphStatistics:
    """Statistics about the graph."""

    total_nodes: int = 0
    total_edges: int = 0
    total_files: int = 0
    languages: list[str] = field(default_factory=_empty_language_list)
    node_types: dict[str, int] = field(default_factory=_empty_count_map)
    edge_types: dict[str, int] = field(default_factory=_empty_count_map)
    last_updated: datetime | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert statistics to dictionary."""
        return {
            "total_nodes": self.total_nodes,
            "total_edges": self.total_edges,
            "total_files": self.total_files,
            "languages": self.languages,
            "node_types": self.node_types,
            "edge_types": self.edge_types,
            "last_updated": self.last_updated.isoformat() if self.last_updated else None,
        }
