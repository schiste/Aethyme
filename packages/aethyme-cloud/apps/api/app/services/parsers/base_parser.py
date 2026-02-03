"""Base parser interface for all language parsers."""

from abc import ABC, abstractmethod
from typing import List, Dict, Any, Optional
from dataclasses import dataclass
from enum import Enum


class SymbolKind(str, Enum):
    """Types of code symbols."""

    FILE = "file"
    MODULE = "module"
    NAMESPACE = "namespace"
    PACKAGE = "package"
    CLASS = "class"
    INTERFACE = "interface"
    STRUCT = "struct"
    ENUM = "enum"
    FUNCTION = "function"
    METHOD = "method"
    CONSTRUCTOR = "constructor"
    FIELD = "field"
    PROPERTY = "property"
    VARIABLE = "variable"
    CONSTANT = "constant"
    PARAMETER = "parameter"


@dataclass
class ParsedSymbol:
    """Represents a parsed code symbol."""

    name: str
    kind: SymbolKind
    file_path: str
    line: int
    column: int
    end_line: int
    end_column: int
    signature: Optional[str] = None
    docstring: Optional[str] = None
    modifiers: List[str] = None  # public, private, static, etc.
    return_type: Optional[str] = None
    parameters: List[Dict[str, str]] = None
    parent: Optional[str] = None  # Parent symbol (e.g., class for a method)
    children: List[str] = None  # Child symbols
    metadata: Dict[str, Any] = None

    def __post_init__(self):
        """Initialize mutable default values."""
        if self.modifiers is None:
            self.modifiers = []
        if self.parameters is None:
            self.parameters = []
        if self.children is None:
            self.children = []
        if self.metadata is None:
            self.metadata = {}


@dataclass
class ParsedRelationship:
    """Represents a relationship between code symbols."""

    source_symbol: str
    target_symbol: str
    relationship_type: str  # imports, calls, inherits, implements, etc.
    source_file: str
    target_file: str
    line: int
    column: int


@dataclass
class ParseResult:
    """Result of parsing a source file."""

    file_path: str
    language: str
    symbols: List[ParsedSymbol]
    relationships: List[ParsedRelationship]
    imports: List[str]
    errors: List[str]
    metadata: Dict[str, Any] = None

    def __post_init__(self):
        """Initialize metadata if None."""
        if self.metadata is None:
            self.metadata = {}


class BaseParser(ABC):
    """Abstract base class for language parsers."""

    def __init__(self, language: str):
        """Initialize parser.

        Args:
            language: Programming language name
        """
        self.language = language

    @abstractmethod
    def parse_file(self, file_path: str, content: str) -> ParseResult:
        """Parse a source code file.

        Args:
            file_path: Path to the source file
            content: File content as string

        Returns:
            ParseResult containing symbols, relationships, and metadata
        """
        pass

    @abstractmethod
    def extract_symbols(self, content: str, file_path: str) -> List[ParsedSymbol]:
        """Extract symbols from source code.

        Args:
            content: Source code content
            file_path: Path to the source file

        Returns:
            List of parsed symbols
        """
        pass

    @abstractmethod
    def extract_relationships(
        self, content: str, file_path: str, symbols: List[ParsedSymbol]
    ) -> List[ParsedRelationship]:
        """Extract relationships between symbols.

        Args:
            content: Source code content
            file_path: Path to the source file
            symbols: Previously extracted symbols

        Returns:
            List of relationships
        """
        pass

    def extract_imports(self, content: str) -> List[str]:
        """Extract import statements.

        Args:
            content: Source code content

        Returns:
            List of imported modules/packages
        """
        # Override in subclasses for language-specific import extraction
        return []

    def calculate_complexity(self, symbol: ParsedSymbol, content: str) -> int:
        """Calculate cyclomatic complexity for a symbol.

        Args:
            symbol: The symbol to analyze
            content: Source code content

        Returns:
            Cyclomatic complexity score
        """
        # Override in subclasses for accurate complexity calculation
        # This is a simple default implementation
        if symbol.kind not in (SymbolKind.FUNCTION, SymbolKind.METHOD):
            return 1

        # Count decision points (simple heuristic)
        decision_keywords = ["if", "else", "elif", "for", "while", "case", "catch", "&&", "||"]
        complexity = 1  # Base complexity

        # Get symbol content (this is simplified - subclasses should extract actual symbol content)
        for keyword in decision_keywords:
            complexity += content.count(keyword)

        return min(complexity, 100)  # Cap at 100 for sanity

    def extract_docstring(self, content: str, symbol: ParsedSymbol) -> Optional[str]:
        """Extract docstring/documentation for a symbol.

        Args:
            content: Source code content
            symbol: The symbol to extract docs for

        Returns:
            Docstring text or None
        """
        # Override in subclasses for language-specific docstring extraction
        return None

    def get_symbol_full_name(self, symbol: ParsedSymbol) -> str:
        """Get the fully qualified name of a symbol.

        Args:
            symbol: The symbol

        Returns:
            Fully qualified name (e.g., 'package.Class.method')
        """
        if symbol.parent:
            return f"{symbol.parent}.{symbol.name}"
        return symbol.name

    def validate_parse_result(self, result: ParseResult) -> bool:
        """Validate a parse result.

        Args:
            result: The parse result to validate

        Returns:
            True if valid, False otherwise
        """
        if not result.file_path:
            return False
        if not result.language:
            return False
        # Symbols can be empty for files with no code
        return True
