"""Tree-sitter parser service for code analysis and symbol extraction."""

from pathlib import Path
from typing import List, Dict, Optional, Any
from tree_sitter import Language, Parser, Node
import tree_sitter_python
import tree_sitter_javascript
import tree_sitter_typescript
import tree_sitter_go
import tree_sitter_rust
import tree_sitter_java


class Symbol:
    """Represents a code symbol (function, class, variable, etc.)."""

    def __init__(
        self,
        name: str,
        kind: str,
        line_start: int,
        line_end: int,
        col_start: int,
        col_end: int,
        file_path: str,
        signature: Optional[str] = None,
        docstring: Optional[str] = None,
        parent: Optional[str] = None,
    ):
        self.name = name
        self.kind = kind  # function, class, method, variable, import, etc.
        self.line_start = line_start
        self.line_end = line_end
        self.col_start = col_start
        self.col_end = col_end
        self.file_path = file_path
        self.signature = signature
        self.docstring = docstring
        self.parent = parent  # For methods, the class name

    def to_dict(self) -> Dict[str, Any]:
        """Convert symbol to dictionary."""
        return {
            "name": self.name,
            "kind": self.kind,
            "line_start": self.line_start,
            "line_end": self.line_end,
            "col_start": self.col_start,
            "col_end": self.col_end,
            "file_path": self.file_path,
            "signature": self.signature,
            "docstring": self.docstring,
            "parent": self.parent,
        }


class TreeSitterParser:
    """Service for parsing code with Tree-sitter."""

    # Language to Tree-sitter language mapping
    LANGUAGES = {
        "python": tree_sitter_python.language(),
        "javascript": tree_sitter_javascript.language(),
        "typescript": tree_sitter_typescript.language_typescript(),
        "tsx": tree_sitter_typescript.language_tsx(),
        "go": tree_sitter_go.language(),
        "rust": tree_sitter_rust.language(),
        "java": tree_sitter_java.language(),
    }

    # File extension to language mapping
    EXTENSION_MAP = {
        ".py": "python",
        ".js": "javascript",
        ".jsx": "javascript",
        ".ts": "typescript",
        ".tsx": "tsx",
        ".go": "go",
        ".rs": "rust",
        ".java": "java",
    }

    def __init__(self):
        """Initialize parser."""
        self.parsers = {}  # Cache parsers by language

    def _get_parser(self, language_name: str) -> Optional[Parser]:
        """Get or create parser for language.

        Args:
            language_name: Language name

        Returns:
            Parser configured for language or None
        """
        if language_name not in self.parsers:
            language_capsule = self.LANGUAGES.get(language_name)
            if not language_capsule:
                return None
            parser = Parser()
            parser.language = Language(language_capsule)
            self.parsers[language_name] = parser
        return self.parsers.get(language_name)

    def get_language(self, file_path: str) -> Optional[str]:
        """Get language from file extension.

        Args:
            file_path: Path to file

        Returns:
            Language name or None
        """
        ext = Path(file_path).suffix.lower()
        return self.EXTENSION_MAP.get(ext)

    def parse_file(self, file_path: str, content: str) -> Optional[Node]:
        """Parse a file and return AST.

        Args:
            file_path: Path to file (for language detection)
            content: File content as string

        Returns:
            Root AST node or None if language not supported
        """
        language_name = self.get_language(file_path)
        if not language_name:
            return None

        parser = self._get_parser(language_name)
        if not parser:
            return None

        tree = parser.parse(bytes(content, "utf8"))
        return tree.root_node

    def extract_symbols(self, file_path: str, content: str) -> List[Symbol]:
        """Extract symbols from a file.

        Args:
            file_path: Path to file
            content: File content

        Returns:
            List of extracted symbols
        """
        root_node = self.parse_file(file_path, content)
        if not root_node:
            return []

        language = self.get_language(file_path)
        if language == "python":
            return self._extract_python_symbols(file_path, root_node, content)
        elif language in ["javascript", "typescript", "tsx"]:
            return self._extract_js_ts_symbols(file_path, root_node, content)
        elif language == "go":
            return self._extract_go_symbols(file_path, root_node, content)
        elif language == "rust":
            return self._extract_rust_symbols(file_path, root_node, content)
        elif language == "java":
            return self._extract_java_symbols(file_path, root_node, content)

        return []

    def _extract_python_symbols(
        self, file_path: str, node: Node, content: str
    ) -> List[Symbol]:
        """Extract symbols from Python code.

        Args:
            file_path: File path
            node: Root AST node
            content: File content

        Returns:
            List of symbols
        """
        symbols = []

        def traverse(n: Node, parent_class: Optional[str] = None):
            """Traverse AST and extract symbols."""
            # Function definition
            if n.type == "function_definition":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    kind = "method" if parent_class else "function"

                    # Get function signature
                    params_node = n.child_by_field_name("parameters")
                    signature = None
                    if params_node:
                        signature = content[n.start_byte : params_node.end_byte]

                    # Try to get docstring
                    docstring = self._get_python_docstring(n, content)

                    symbols.append(
                        Symbol(
                            name=name,
                            kind=kind,
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                            signature=signature,
                            docstring=docstring,
                            parent=parent_class,
                        )
                    )

            # Class definition
            elif n.type == "class_definition":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]

                    # Get class docstring
                    docstring = self._get_python_docstring(n, content)

                    symbols.append(
                        Symbol(
                            name=name,
                            kind="class",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                            docstring=docstring,
                        )
                    )

                    # Traverse class body for methods
                    for child in n.children:
                        if child.type == "block":
                            for method in child.children:
                                traverse(method, parent_class=name)
                    return  # Don't traverse children again

            # Import statements
            elif n.type in ["import_statement", "import_from_statement"]:
                import_text = content[n.start_byte : n.end_byte]
                symbols.append(
                    Symbol(
                        name=import_text,
                        kind="import",
                        line_start=n.start_point[0] + 1,
                        line_end=n.end_point[0] + 1,
                        col_start=n.start_point[1],
                        col_end=n.end_point[1],
                        file_path=file_path,
                    )
                )

            # Traverse children
            for child in n.children:
                traverse(child, parent_class)

        traverse(node)
        return symbols

    def _get_python_docstring(self, node: Node, content: str) -> Optional[str]:
        """Extract docstring from Python function or class.

        Args:
            node: Function or class node
            content: File content

        Returns:
            Docstring text or None
        """
        # Look for a string as first statement in body
        for child in node.children:
            if child.type == "block":
                for stmt in child.children:
                    if stmt.type == "expression_statement":
                        for expr_child in stmt.children:
                            if expr_child.type == "string":
                                docstring = content[
                                    expr_child.start_byte : expr_child.end_byte
                                ]
                                # Remove quotes
                                return docstring.strip('"""').strip("'''").strip()
                return None
        return None

    def _extract_js_ts_symbols(
        self, file_path: str, node: Node, content: str
    ) -> List[Symbol]:
        """Extract symbols from JavaScript/TypeScript code."""
        symbols = []

        def traverse(n: Node, parent_class: Optional[str] = None):
            # Function declaration
            if n.type in ["function_declaration", "function"]:
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="function",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                            parent=parent_class,
                        )
                    )

            # Class declaration
            elif n.type == "class_declaration":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="class",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

                    # Traverse for methods
                    for child in n.children:
                        traverse(child, parent_class=name)
                    return

            # Method definition
            elif n.type == "method_definition":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="method",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                            parent=parent_class,
                        )
                    )

            # Arrow functions assigned to variables
            elif n.type == "variable_declarator":
                name_node = n.child_by_field_name("name")
                value_node = n.child_by_field_name("value")
                if name_node and value_node and value_node.type == "arrow_function":
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="function",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

            # Import statements
            elif n.type in ["import_statement", "import_specifier"]:
                import_text = content[n.start_byte : n.end_byte]
                symbols.append(
                    Symbol(
                        name=import_text,
                        kind="import",
                        line_start=n.start_point[0] + 1,
                        line_end=n.end_point[0] + 1,
                        col_start=n.start_point[1],
                        col_end=n.end_point[1],
                        file_path=file_path,
                    )
                )

            for child in n.children:
                traverse(child, parent_class)

        traverse(node)
        return symbols

    def _extract_go_symbols(
        self, file_path: str, node: Node, content: str
    ) -> List[Symbol]:
        """Extract symbols from Go code."""
        symbols = []

        def traverse(n: Node):
            # Function declaration
            if n.type == "function_declaration":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="function",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

            # Type declaration (structs, interfaces)
            elif n.type == "type_declaration":
                for spec in n.children:
                    if spec.type == "type_spec":
                        name_node = spec.child_by_field_name("name")
                        if name_node:
                            name = content[name_node.start_byte : name_node.end_byte]
                            symbols.append(
                                Symbol(
                                    name=name,
                                    kind="type",
                                    line_start=spec.start_point[0] + 1,
                                    line_end=spec.end_point[0] + 1,
                                    col_start=spec.start_point[1],
                                    col_end=spec.end_point[1],
                                    file_path=file_path,
                                )
                            )

            for child in n.children:
                traverse(child)

        traverse(node)
        return symbols

    def _extract_rust_symbols(
        self, file_path: str, node: Node, content: str
    ) -> List[Symbol]:
        """Extract symbols from Rust code."""
        symbols = []

        def traverse(n: Node):
            # Function definition
            if n.type == "function_item":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="function",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

            # Struct definition
            elif n.type == "struct_item":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="struct",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

            for child in n.children:
                traverse(child)

        traverse(node)
        return symbols

    def _extract_java_symbols(
        self, file_path: str, node: Node, content: str
    ) -> List[Symbol]:
        """Extract symbols from Java code."""
        symbols = []

        def traverse(n: Node, parent_class: Optional[str] = None):
            # Class declaration
            if n.type == "class_declaration":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="class",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                        )
                    )

                    # Traverse for methods
                    for child in n.children:
                        traverse(child, parent_class=name)
                    return

            # Method declaration
            elif n.type == "method_declaration":
                name_node = n.child_by_field_name("name")
                if name_node:
                    name = content[name_node.start_byte : name_node.end_byte]
                    symbols.append(
                        Symbol(
                            name=name,
                            kind="method",
                            line_start=n.start_point[0] + 1,
                            line_end=n.end_point[0] + 1,
                            col_start=n.start_point[1],
                            col_end=n.end_point[1],
                            file_path=file_path,
                            parent=parent_class,
                        )
                    )

            for child in n.children:
                traverse(child, parent_class)

        traverse(node)
        return symbols
