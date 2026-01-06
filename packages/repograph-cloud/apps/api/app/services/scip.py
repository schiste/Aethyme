"""SCIP (Source Code Intelligence Protocol) index generation service.

SCIP is a language-agnostic protocol for representing code intelligence data
such as symbols, references, and relationships.

Spec: https://github.com/sourcegraph/scip
"""

import hashlib
from typing import List, Dict, Any, Optional
from dataclasses import dataclass, field
from app.services.tree_sitter import Symbol


@dataclass
class SCIPSymbol:
    """SCIP symbol representation."""

    # Symbol identifier (fully qualified name)
    symbol_id: str

    # Symbol kind (function, class, method, variable, etc.)
    kind: str

    # Display name
    name: str

    # File path
    file_path: str

    # Position in file
    line_start: int
    line_end: int
    col_start: int
    col_end: int

    # Language
    language: Optional[str] = None

    # Documentation
    documentation: Optional[str] = None

    # Signature
    signature: Optional[str] = None

    # Content (full symbol source code)
    content: Optional[str] = None

    # Parent symbol (for methods, nested classes)
    parent_symbol_id: Optional[str] = None

    # References to this symbol
    references: List[Dict[str, Any]] = field(default_factory=list)

    # Relationships
    relationships: List[Dict[str, str]] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "symbol_id": self.symbol_id,
            "kind": self.kind,
            "name": self.name,
            "file_path": self.file_path,
            "line_start": self.line_start,
            "line_end": self.line_end,
            "col_start": self.col_start,
            "col_end": self.col_end,
            "documentation": self.documentation,
            "signature": self.signature,
            "parent_symbol_id": self.parent_symbol_id,
            "references": self.references,
            "relationships": self.relationships,
        }


@dataclass
class SCIPDocument:
    """SCIP document (one per file)."""

    # File path
    file_path: str

    # Language
    language: str

    # Repository
    repository_id: str

    # Symbols defined in this file
    symbols: List[SCIPSymbol] = field(default_factory=list)

    # Occurrences (symbol usages)
    occurrences: List[Dict[str, Any]] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "file_path": self.file_path,
            "language": self.language,
            "repository_id": self.repository_id,
            "symbols": [s.to_dict() for s in self.symbols],
            "occurrences": self.occurrences,
        }


class SCIPGenerator:
    """Service for generating SCIP index from Tree-sitter symbols."""

    def __init__(self, repository_id: str, repository_name: str):
        """Initialize SCIP generator.

        Args:
            repository_id: Repository UUID
            repository_name: Repository name (e.g., "owner/repo")
        """
        self.repository_id = repository_id
        self.repository_name = repository_name
        self.documents: Dict[str, SCIPDocument] = {}

    def generate_symbol_id(
        self,
        file_path: str,
        symbol_name: str,
        kind: str,
        parent: Optional[str] = None,
    ) -> str:
        """Generate unique symbol identifier.

        Format: repo:path:kind:name[:parent]

        Args:
            file_path: File path
            symbol_name: Symbol name
            kind: Symbol kind
            parent: Parent symbol name (for methods)

        Returns:
            Unique symbol ID
        """
        # Normalize file path (remove leading slash, use forward slashes)
        normalized_path = file_path.lstrip("/").replace("\\", "/")

        # Build symbol ID
        if parent:
            symbol_id = f"{self.repository_name}:{normalized_path}:{kind}:{parent}.{symbol_name}"
        else:
            symbol_id = f"{self.repository_name}:{normalized_path}:{kind}:{symbol_name}"

        return symbol_id

    def add_file(
        self,
        file_path: str,
        language: str,
        symbols: List[Symbol],
    ) -> SCIPDocument:
        """Add a file to the SCIP index.

        Args:
            file_path: Path to file
            language: Programming language
            symbols: Extracted symbols from Tree-sitter

        Returns:
            SCIP document
        """
        # Create or get document
        if file_path not in self.documents:
            self.documents[file_path] = SCIPDocument(
                file_path=file_path,
                language=language,
                repository_id=self.repository_id,
            )

        doc = self.documents[file_path]

        # Convert Tree-sitter symbols to SCIP symbols
        for symbol in symbols:
            # Skip imports for now (they're references, not definitions)
            if symbol.kind == "import":
                continue

            # Generate symbol ID
            symbol_id = self.generate_symbol_id(
                file_path=file_path,
                symbol_name=symbol.name,
                kind=symbol.kind,
                parent=symbol.parent,
            )

            # Create SCIP symbol
            scip_symbol = SCIPSymbol(
                symbol_id=symbol_id,
                kind=symbol.kind,
                name=symbol.name,
                file_path=file_path,
                line_start=symbol.line_start,
                line_end=symbol.line_end,
                col_start=symbol.col_start,
                col_end=symbol.col_end,
                language=language,
                documentation=symbol.docstring,
                signature=symbol.signature,
                content=None,  # Can be populated from file content if needed
                parent_symbol_id=self._get_parent_symbol_id(file_path, symbol)
                if symbol.parent
                else None,
            )

            # Add relationships
            if symbol.parent:
                # Method belongs to class
                scip_symbol.relationships.append(
                    {
                        "type": "member_of",
                        "target": self.generate_symbol_id(
                            file_path, symbol.parent, "class"
                        ),
                    }
                )

            doc.symbols.append(scip_symbol)

        return doc

    def _get_parent_symbol_id(self, file_path: str, symbol: Symbol) -> Optional[str]:
        """Get parent symbol ID.

        Args:
            file_path: File path
            symbol: Symbol with parent

        Returns:
            Parent symbol ID or None
        """
        if not symbol.parent:
            return None

        return self.generate_symbol_id(
            file_path=file_path, symbol_name=symbol.parent, kind="class"
        )

    def get_all_symbols(self) -> List[SCIPSymbol]:
        """Get all symbols across all documents.

        Returns:
            List of all SCIP symbols
        """
        all_symbols = []
        for doc in self.documents.values():
            all_symbols.extend(doc.symbols)
        return all_symbols

    def get_document(self, file_path: str) -> Optional[SCIPDocument]:
        """Get SCIP document for a file.

        Args:
            file_path: File path

        Returns:
            SCIP document or None
        """
        return self.documents.get(file_path)

    def get_symbols_by_kind(self, kind: str) -> List[SCIPSymbol]:
        """Get all symbols of a specific kind.

        Args:
            kind: Symbol kind (function, class, method, etc.)

        Returns:
            List of matching symbols
        """
        symbols = []
        for doc in self.documents.values():
            symbols.extend([s for s in doc.symbols if s.kind == kind])
        return symbols

    def find_symbol(self, symbol_name: str) -> List[SCIPSymbol]:
        """Find symbols by name.

        Args:
            symbol_name: Symbol name to search for

        Returns:
            List of matching symbols
        """
        symbols = []
        for doc in self.documents.values():
            symbols.extend([s for s in doc.symbols if s.name == symbol_name])
        return symbols

    def to_dict(self) -> Dict[str, Any]:
        """Export entire index as dictionary.

        Returns:
            Dictionary representation of SCIP index
        """
        return {
            "repository_id": self.repository_id,
            "repository_name": self.repository_name,
            "documents": {
                path: doc.to_dict() for path, doc in self.documents.items()
            },
            "statistics": self.get_statistics(),
        }

    def get_statistics(self) -> Dict[str, Any]:
        """Get index statistics.

        Returns:
            Statistics dictionary
        """
        all_symbols = self.get_all_symbols()

        # Count by kind
        kind_counts = {}
        for symbol in all_symbols:
            kind_counts[symbol.kind] = kind_counts.get(symbol.kind, 0) + 1

        return {
            "total_documents": len(self.documents),
            "total_symbols": len(all_symbols),
            "symbols_by_kind": kind_counts,
            "files_by_language": self._get_language_distribution(),
        }

    def _get_language_distribution(self) -> Dict[str, int]:
        """Get distribution of files by language.

        Returns:
            Language distribution
        """
        lang_dist = {}
        for doc in self.documents.values():
            lang_dist[doc.language] = lang_dist.get(doc.language, 0) + 1
        return lang_dist


class SCIPIndex:
    """In-memory SCIP index for fast lookups."""

    def __init__(self):
        """Initialize SCIP index."""
        self.symbols_by_id: Dict[str, SCIPSymbol] = {}
        self.symbols_by_name: Dict[str, List[SCIPSymbol]] = {}
        self.symbols_by_file: Dict[str, List[SCIPSymbol]] = {}
        self.symbols_by_kind: Dict[str, List[SCIPSymbol]] = {}

    def add_symbol(self, symbol: SCIPSymbol):
        """Add symbol to index.

        Args:
            symbol: SCIP symbol
        """
        # By ID
        self.symbols_by_id[symbol.symbol_id] = symbol

        # By name
        if symbol.name not in self.symbols_by_name:
            self.symbols_by_name[symbol.name] = []
        self.symbols_by_name[symbol.name].append(symbol)

        # By file
        if symbol.file_path not in self.symbols_by_file:
            self.symbols_by_file[symbol.file_path] = []
        self.symbols_by_file[symbol.file_path].append(symbol)

        # By kind
        if symbol.kind not in self.symbols_by_kind:
            self.symbols_by_kind[symbol.kind] = []
        self.symbols_by_kind[symbol.kind].append(symbol)

    def get_symbol(self, symbol_id: str) -> Optional[SCIPSymbol]:
        """Get symbol by ID.

        Args:
            symbol_id: Symbol identifier

        Returns:
            SCIP symbol or None
        """
        return self.symbols_by_id.get(symbol_id)

    def search_symbols(self, query: str) -> List[SCIPSymbol]:
        """Search for symbols by name (fuzzy match).

        Args:
            query: Search query

        Returns:
            List of matching symbols
        """
        query_lower = query.lower()
        matches = []

        for name, symbols in self.symbols_by_name.items():
            if query_lower in name.lower():
                matches.extend(symbols)

        return matches

    def get_symbols_in_file(self, file_path: str) -> List[SCIPSymbol]:
        """Get all symbols defined in a file.

        Args:
            file_path: File path

        Returns:
            List of symbols
        """
        return self.symbols_by_file.get(file_path, [])

    def get_symbols_by_kind(self, kind: str) -> List[SCIPSymbol]:
        """Get all symbols of a specific kind.

        Args:
            kind: Symbol kind

        Returns:
            List of symbols
        """
        return self.symbols_by_kind.get(kind, [])

    def get_statistics(self) -> Dict[str, Any]:
        """Get index statistics.

        Returns:
            Statistics dictionary
        """
        return {
            "total_symbols": len(self.symbols_by_id),
            "total_files": len(self.symbols_by_file),
            "symbols_by_kind": {
                kind: len(symbols) for kind, symbols in self.symbols_by_kind.items()
            },
        }
