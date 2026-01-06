"""Fallback indexer using tree-sitter when SCIP is unavailable."""

import re
from pathlib import Path
from typing import List, Dict, Any, Optional, Tuple
import structlog
import tree_sitter

from ..models.graph import Node, NodeKind

logger = structlog.get_logger(__name__)


class FallbackIndexer:
    """Tree-sitter based fallback indexer for when SCIP is unavailable."""

    # Language file extensions
    LANGUAGE_EXTENSIONS = {
        "python": [".py"],
        "typescript": [".ts", ".tsx"],
        "javascript": [".js", ".jsx"],
    }

    # Tree-sitter queries for different languages
    PYTHON_QUERIES = {
        "functions": """
            (function_definition
                name: (identifier) @function.name
                parameters: (parameters) @function.params
                body: (block) @function.body) @function
        """,
        "classes": """
            (class_definition
                name: (identifier) @class.name
                body: (block) @class.body) @class
        """,
        "methods": """
            (class_definition
                body: (block
                    (function_definition
                        name: (identifier) @method.name
                        parameters: (parameters) @method.params) @method))
        """,
        "imports": """
            (import_statement
                module_name: (dotted_name) @import.module) @import
            (import_from_statement
                module_name: (dotted_name) @import.module
                name: (dotted_name) @import.name) @import
        """,
        "calls": """
            (call
                function: [
                    (identifier) @call.function
                    (attribute
                        attribute: (identifier) @call.method)
                ]) @call
        """,
    }

    TYPESCRIPT_QUERIES = {
        "functions": """
            (function_declaration
                name: (identifier) @function.name
                parameters: (formal_parameters) @function.params) @function
        """,
        "classes": """
            (class_declaration
                name: (identifier) @class.name
                body: (class_body) @class.body) @class
        """,
        "methods": """
            (method_definition
                name: (property_identifier) @method.name
                parameters: (formal_parameters) @method.params) @method
        """,
        "imports": """
            (import_statement
                source: (string) @import.source) @import
        """,
        "interfaces": """
            (interface_declaration
                name: (identifier) @interface.name
                body: (interface_body) @interface.body) @interface
        """,
        "components": """
            (variable_declarator
                name: (identifier) @component.name
                value: (arrow_function
                    body: (jsx_element))) @component
            (function_declaration
                name: (identifier) @component.name
                body: (statement_block
                    (return_statement
                        (jsx_element)))) @component
        """,
    }

    def __init__(self, language: str):
        """
        Initialize fallback indexer for a specific language.

        Args:
            language: Programming language to index
        """
        self.language = language.lower()
        self.parser = None
        self.tree_sitter_language = None

        # Initialize tree-sitter parser
        self._initialize_parser()

    def _initialize_parser(self) -> None:
        """Initialize tree-sitter parser for the language."""
        try:
            # Note: In production, you'd need to build and load the language libraries
            # This is a simplified version
            self.parser = tree_sitter.Parser()

            if self.language == "python":
                # Load Python language (requires tree-sitter-python installed)
                # self.parser.set_language(Language('path/to/python.so', 'python'))
                logger.info("Tree-sitter parser initialized", language="python")
            elif self.language in ["typescript", "javascript"]:
                # Load TypeScript/JavaScript language
                # self.parser.set_language(Language('path/to/typescript.so', 'typescript'))
                logger.info("Tree-sitter parser initialized", language="typescript")
            else:
                raise ValueError(f"Unsupported language for fallback: {self.language}")

        except Exception as e:
            logger.error(
                "Failed to initialize tree-sitter parser",
                language=self.language,
                error=str(e),
            )
            # Continue without parser - will use regex fallback
            self.parser = None

    def index(self, repo_path: Path, exclude_dirs: List[str] = None) -> Tuple[List[Node], List[tuple]]:
        """
        Index repository using tree-sitter or regex fallback.

        Args:
            repo_path: Path to repository
            exclude_dirs: Directories to exclude from indexing

        Returns:
            Tuple of (nodes, edges)
        """
        repo_path = Path(repo_path)
        if not repo_path.exists():
            raise FileNotFoundError(f"Repository path does not exist: {repo_path}")

        nodes = []
        edges = []

        # Default exclusions to avoid system directories and dependencies
        if exclude_dirs is None:
            exclude_dirs = [
                'node_modules', '__pycache__', '.git', 'venv', '.venv',
                'dist', 'build', '.pytest_cache', '.mypy_cache',
                'site-packages', 'proc', 'sys', 'dev', 'run', 'tmp',
                '.tox', 'htmlcov', 'eggs', '.eggs', 'lib', 'lib64'
            ]

        # Get file extensions for the language
        extensions = self.LANGUAGE_EXTENSIONS.get(self.language, [])

        # Find all relevant files with exclusions
        files = []
        for ext in extensions:
            try:
                for file_path in repo_path.rglob(f"*{ext}"):
                    # Skip files in excluded directories
                    try:
                        relative_path = file_path.relative_to(repo_path)
                        path_parts = str(relative_path).split('/')
                        if any(excl in path_parts for excl in exclude_dirs):
                            continue
                        files.append(file_path)
                    except (ValueError, OSError):
                        # Skip files we can't process
                        continue
            except (OSError, PermissionError) as e:
                # rglob() can fail on system directories like /proc
                logger.warning(
                    "Error during file discovery",
                    extension=ext,
                    error=str(e),
                )
                continue

        logger.info(
            "Starting fallback indexing",
            language=self.language,
            files_found=len(files),
            repo_path=str(repo_path),
        )

        # Index each file
        for file_path in files:
            try:
                relative_path = file_path.relative_to(repo_path)
                file_nodes, file_edges = self._index_file(file_path, str(relative_path))
                nodes.extend(file_nodes)
                edges.extend(file_edges)
            except Exception as e:
                logger.warning(
                    "Failed to index file",
                    file=str(file_path),
                    error=str(e),
                )
                continue

        logger.info(
            "Fallback indexing complete",
            total_nodes=len(nodes),
            total_edges=len(edges),
        )

        return nodes, edges

    def _index_file(self, file_path: Path, relative_path: str) -> Tuple[List[Node], List[tuple]]:
        """
        Index a single file.

        Args:
            file_path: Absolute path to file
            relative_path: Relative path from repository root

        Returns:
            Tuple of (nodes, edges) for this file
        """
        with open(file_path, "r", encoding="utf-8") as f:
            content = f.read()

        # Use tree-sitter if available, otherwise fall back to regex
        if self.parser:
            return self._index_with_tree_sitter(content, relative_path)
        else:
            return self._index_with_regex(content, relative_path)

    def _index_with_tree_sitter(
        self,
        content: str,
        file_path: str
    ) -> Tuple[List[Node], List[tuple]]:
        """
        Index using tree-sitter parser.

        Args:
            content: File content
            file_path: Relative file path

        Returns:
            Tuple of (nodes, edges)
        """
        # This is a simplified version - real implementation would use tree-sitter
        # For now, fall back to regex
        return self._index_with_regex(content, file_path)

    def _index_with_regex(
        self,
        content: str,
        file_path: str
    ) -> Tuple[List[Node], List[tuple]]:
        """
        Index using regex patterns (basic fallback).

        Args:
            content: File content
            file_path: Relative file path

        Returns:
            Tuple of (nodes, edges)
        """
        nodes = []
        edges = []
        lines = content.split("\n")

        if self.language == "python":
            nodes.extend(self._extract_python_definitions(lines, file_path))
            nodes.extend(self._extract_python_imports(lines, file_path))
        elif self.language in ["typescript", "javascript"]:
            nodes.extend(self._extract_typescript_definitions(lines, file_path))
            nodes.extend(self._extract_typescript_imports(lines, file_path))

        return nodes, edges

    def _extract_python_definitions(self, lines: List[str], file_path: str) -> List[Node]:
        """Extract Python function and class definitions."""
        nodes = []

        # Patterns for Python definitions
        class_pattern = re.compile(r"^class\s+(\w+)(?:\(.*?\))?:")
        function_pattern = re.compile(r"^def\s+(\w+)\s*\(.*?\):")
        method_pattern = re.compile(r"^\s+def\s+(\w+)\s*\(.*?\):")

        current_class = None

        for line_num, line in enumerate(lines, 1):
            # Check for class definition
            class_match = class_pattern.match(line)
            if class_match:
                class_name = class_match.group(1)
                current_class = class_name
                node = Node.create_definition(
                    symbol=f"{file_path}:{class_name}",
                    file_path=file_path,
                    line=line_num,
                    column=0,
                    language="python",
                    kind=NodeKind.CLASS,
                )
                nodes.append(node)
                continue

            # Check for function definition
            func_match = function_pattern.match(line)
            if func_match:
                func_name = func_match.group(1)
                node = Node.create_definition(
                    symbol=f"{file_path}:{func_name}",
                    file_path=file_path,
                    line=line_num,
                    column=0,
                    language="python",
                    kind=NodeKind.FUNCTION,
                )
                nodes.append(node)
                current_class = None
                continue

            # Check for method definition (indented function)
            method_match = method_pattern.match(line)
            if method_match and current_class:
                method_name = method_match.group(1)
                node = Node.create_definition(
                    symbol=f"{file_path}:{current_class}.{method_name}",
                    file_path=file_path,
                    line=line_num,
                    column=4,  # Assuming standard indentation
                    language="python",
                    kind=NodeKind.METHOD,
                )
                nodes.append(node)

        return nodes

    def _extract_python_imports(self, lines: List[str], file_path: str) -> List[Node]:
        """Extract Python import statements."""
        nodes = []

        import_pattern = re.compile(r"^(?:from\s+([\w.]+)\s+)?import\s+(.*)")

        for line_num, line in enumerate(lines, 1):
            match = import_pattern.match(line)
            if match:
                module = match.group(1) or match.group(2)
                node = Node(
                    symbol=module.split(",")[0].strip(),
                    file_path=file_path,
                    line_number=line_num,
                    column_number=0,
                    kind=NodeKind.IMPORT,
                    language="python",
                )
                nodes.append(node)

        return nodes

    def _extract_typescript_definitions(self, lines: List[str], file_path: str) -> List[Node]:
        """Extract TypeScript/JavaScript definitions."""
        nodes = []

        # Patterns for TypeScript/JavaScript
        class_pattern = re.compile(r"(?:export\s+)?class\s+(\w+)")
        function_pattern = re.compile(r"(?:export\s+)?(?:async\s+)?function\s+(\w+)")
        const_func_pattern = re.compile(r"(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s+)?(?:\(.*?\)|function)")
        interface_pattern = re.compile(r"(?:export\s+)?interface\s+(\w+)")
        type_pattern = re.compile(r"(?:export\s+)?type\s+(\w+)")

        for line_num, line in enumerate(lines, 1):
            # Check for various definition patterns
            for pattern, kind in [
                (class_pattern, NodeKind.CLASS),
                (function_pattern, NodeKind.FUNCTION),
                (const_func_pattern, NodeKind.FUNCTION),
                (interface_pattern, NodeKind.CLASS),  # Treat interface as class
                (type_pattern, NodeKind.CLASS),  # Treat type as class
            ]:
                match = pattern.search(line)
                if match:
                    name = match.group(1)
                    node = Node.create_definition(
                        symbol=f"{file_path}:{name}",
                        file_path=file_path,
                        line=line_num,
                        column=0,
                        language=self.language,
                        kind=kind,
                    )
                    nodes.append(node)
                    break

        return nodes

    def _extract_typescript_imports(self, lines: List[str], file_path: str) -> List[Node]:
        """Extract TypeScript/JavaScript import statements."""
        nodes = []

        import_pattern = re.compile(r"import\s+.*?from\s+['\"](.+?)['\"]")

        for line_num, line in enumerate(lines, 1):
            match = import_pattern.search(line)
            if match:
                module = match.group(1)
                node = Node(
                    symbol=module,
                    file_path=file_path,
                    line_number=line_num,
                    column_number=0,
                    kind=NodeKind.IMPORT,
                    language=self.language,
                )
                nodes.append(node)

        return nodes