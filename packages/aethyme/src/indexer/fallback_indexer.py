"""Fallback indexer using simple regex extraction when SCIP is unavailable."""

from __future__ import annotations

import re
from pathlib import Path
from typing import TypeAlias

import structlog

from ..models.graph import Node, NodeKind

logger = structlog.get_logger(__name__)

EdgeTuple: TypeAlias = tuple[str, str, str]
ImportNames: TypeAlias = list[str]


class FallbackIndexer:
    """Regex-based fallback indexer for when SCIP is unavailable."""

    LANGUAGE_EXTENSIONS = {
        "python": [".py"],
        "typescript": [".ts", ".tsx"],
        "javascript": [".js", ".jsx"],
    }

    def __init__(self, language: str):
        self.language = language.lower()

    def index(
        self,
        repo_path: Path,
        exclude_dirs: list[str] | None = None,
    ) -> tuple[list[Node], list[EdgeTuple]]:
        """Index repository using conservative regex extraction."""
        repo_path = Path(repo_path)
        if not repo_path.exists():
            raise FileNotFoundError(f"Repository path does not exist: {repo_path}")

        nodes: list[Node] = []
        edges: list[EdgeTuple] = []
        exclude_dirs = exclude_dirs or [
            "node_modules",
            "__pycache__",
            ".git",
            "venv",
            ".venv",
            "dist",
            "build",
            ".pytest_cache",
            ".mypy_cache",
            "site-packages",
            "proc",
            "sys",
            "dev",
            "run",
            "tmp",
            ".tox",
            "htmlcov",
            "eggs",
            ".eggs",
            "lib",
            "lib64",
        ]

        extensions = self.LANGUAGE_EXTENSIONS.get(self.language, [])
        files: list[Path] = []
        for ext in extensions:
            try:
                for file_path in repo_path.rglob(f"*{ext}"):
                    try:
                        relative_path = file_path.relative_to(repo_path)
                    except (ValueError, OSError):
                        continue
                    if any(part in exclude_dirs for part in relative_path.parts):
                        continue
                    files.append(file_path)
            except (OSError, PermissionError) as exc:
                logger.warning("Error during file discovery", extension=ext, error=str(exc))

        logger.info(
            "Starting fallback indexing",
            language=self.language,
            files_found=len(files),
            repo_path=str(repo_path),
        )

        for file_path in files:
            try:
                relative_path = file_path.relative_to(repo_path)
                file_nodes, file_edges = self._index_file(file_path, str(relative_path))
                nodes.extend(file_nodes)
                edges.extend(file_edges)
            except Exception as exc:
                logger.warning("Failed to index file", file=str(file_path), error=str(exc))

        logger.info(
            "Fallback indexing complete",
            total_nodes=len(nodes),
            total_edges=len(edges),
        )
        return nodes, edges

    def _index_file(self, file_path: Path, relative_path: str) -> tuple[list[Node], list[EdgeTuple]]:
        with open(file_path, encoding="utf-8") as handle:
            content = handle.read()
        return self._index_with_regex(content, relative_path)

    def _index_with_regex(self, content: str, file_path: str) -> tuple[list[Node], list[EdgeTuple]]:
        nodes: list[Node] = []
        edges: list[EdgeTuple] = []
        lines = content.split("\n")

        if self.language == "python":
            nodes.extend(self._extract_python_definitions(lines, file_path))
            nodes.extend(self._extract_python_imports(lines, file_path))
            nodes.extend(self._extract_python_references(lines, file_path))
        elif self.language in ["typescript", "javascript"]:
            nodes.extend(self._extract_typescript_definitions(lines, file_path))
            nodes.extend(self._extract_typescript_imports(lines, file_path))
            nodes.extend(self._extract_typescript_references(lines, file_path))

        return nodes, edges

    def _extract_python_definitions(self, lines: list[str], file_path: str) -> list[Node]:
        nodes: list[Node] = []
        class_pattern = re.compile(r"^class\s+(\w+)(?:\(.*?\))?:")
        function_pattern = re.compile(r"^def\s+(\w+)\s*\(.*?\)\s*(?:->\s*[^:]+)?\s*:")
        method_pattern = re.compile(r"^\s+def\s+(\w+)\s*\(.*?\)\s*(?:->\s*[^:]+)?\s*:")

        current_class = None
        for line_num, line in enumerate(lines, 1):
            class_match = class_pattern.match(line)
            if class_match:
                current_class = class_match.group(1)
                nodes.append(
                    Node.create_definition(
                        symbol=f"{file_path}:{current_class}",
                        file_path=file_path,
                        line=line_num,
                        column=0,
                        language="python",
                        kind=NodeKind.CLASS,
                    )
                )
                continue

            function_match = function_pattern.match(line)
            if function_match:
                nodes.append(
                    Node.create_definition(
                        symbol=f"{file_path}:{function_match.group(1)}",
                        file_path=file_path,
                        line=line_num,
                        column=0,
                        language="python",
                        kind=NodeKind.FUNCTION,
                    )
                )
                current_class = None
                continue

            method_match = method_pattern.match(line)
            if method_match and current_class:
                nodes.append(
                    Node.create_definition(
                        symbol=f"{file_path}:{current_class}.{method_match.group(1)}",
                        file_path=file_path,
                        line=line_num,
                        column=4,
                        language="python",
                        kind=NodeKind.METHOD,
                    )
                )

        return nodes

    def _extract_python_imports(self, lines: list[str], file_path: str) -> list[Node]:
        nodes: list[Node] = []
        from_import_pattern = re.compile(r"^from\s+([.\w]+)\s+import\s+(.+)$")
        direct_import_pattern = re.compile(r"^import\s+(.+)$")

        for line_num, line in enumerate(lines, 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            from_match = from_import_pattern.match(stripped)
            if from_match:
                module = from_match.group(1).strip()
                nodes.append(
                    Node(
                        symbol=module,
                        file_path=file_path,
                        line_number=line_num,
                        column_number=0,
                        kind=NodeKind.IMPORT,
                        language="python",
                        metadata={
                            "imported_names": self._parse_python_import_names(
                                from_match.group(2)
                            ),
                            "import_mode": "named",
                        },
                    )
                )
                continue

            direct_match = direct_import_pattern.match(stripped)
            if not direct_match:
                continue

            for module_entry in direct_match.group(1).split(","):
                module_name = module_entry.split(" as ", 1)[0].strip()
                if not module_name:
                    continue
                nodes.append(
                    Node(
                        symbol=module_name,
                        file_path=file_path,
                        line_number=line_num,
                        column_number=0,
                        kind=NodeKind.IMPORT,
                        language="python",
                        metadata={
                            "imported_names": [module_name.split(".")[-1]],
                            "import_mode": "module",
                        },
                    )
                )

        return nodes

    def _extract_typescript_definitions(self, lines: list[str], file_path: str) -> list[Node]:
        nodes: list[Node] = []
        patterns = [
            (re.compile(r"(?:export\s+)?class\s+(\w+)"), NodeKind.CLASS),
            (re.compile(r"(?:export\s+)?(?:async\s+)?function\s+(\w+)"), NodeKind.FUNCTION),
            (re.compile(r"(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s+)?(?:\(.*?\)|function)"), NodeKind.FUNCTION),
            (re.compile(r"(?:export\s+)?interface\s+(\w+)"), NodeKind.CLASS),
            (re.compile(r"(?:export\s+)?type\s+(\w+)"), NodeKind.CLASS),
        ]

        for line_num, line in enumerate(lines, 1):
            for pattern, kind in patterns:
                match = pattern.search(line)
                if not match:
                    continue
                nodes.append(
                    Node.create_definition(
                        symbol=f"{file_path}:{match.group(1)}",
                        file_path=file_path,
                        line=line_num,
                        column=0,
                        language=self.language,
                        kind=kind,
                    )
                )
                break

        return nodes

    def _extract_typescript_imports(self, lines: list[str], file_path: str) -> list[Node]:
        nodes: list[Node] = []
        import_pattern = re.compile(r"import\s+(.*?)\s+from\s+['\"](.+?)['\"]")

        for line_num, line in enumerate(lines, 1):
            match = import_pattern.search(line)
            if not match:
                continue
            nodes.append(
                Node(
                    symbol=match.group(2),
                    file_path=file_path,
                    line_number=line_num,
                    column_number=0,
                    kind=NodeKind.IMPORT,
                    language=self.language,
                    metadata={
                        "imported_names": self._parse_typescript_import_names(
                            match.group(1)
                        ),
                        "import_mode": self._detect_typescript_import_mode(
                            match.group(1)
                        ),
                    },
                )
            )

        return nodes

    def _extract_python_references(self, lines: list[str], file_path: str) -> list[Node]:
        """Extract probable Python call references for fallback invoke edges."""
        nodes: list[Node] = []
        call_pattern = re.compile(r"\b([A-Za-z_][\w\.]*)\s*\(")
        ignored_names = {
            "def",
            "class",
            "if",
            "for",
            "while",
            "with",
            "return",
            "print",
            "len",
            "str",
            "int",
            "float",
            "bool",
            "list",
            "dict",
            "set",
            "tuple",
        }

        for line_num, line in enumerate(lines, 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#") or stripped.startswith(("@", "from ", "import ", "class ")):
                continue

            scan_line = line
            if stripped.startswith("def "):
                if ":" not in line:
                    continue
                scan_line = line.split(":", 1)[1]

            for match in call_pattern.finditer(scan_line):
                reference_symbol = match.group(1)
                if reference_symbol in ignored_names:
                    continue
                nodes.append(
                    Node.create_reference(
                        symbol=reference_symbol,
                        file_path=file_path,
                        line=line_num,
                        column=line.find(reference_symbol),
                        language="python",
                    )
                )

        return nodes

    def _extract_typescript_references(self, lines: list[str], file_path: str) -> list[Node]:
        """Extract probable JavaScript/TypeScript call references."""
        nodes: list[Node] = []
        call_pattern = re.compile(r"\b([A-Za-z_$][\w$.]*)\s*\(")
        ignored_names = {
            "if",
            "for",
            "while",
            "switch",
            "catch",
            "return",
            "typeof",
            "console.log",
        }

        for line_num, line in enumerate(lines, 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("//") or stripped.startswith("import "):
                continue

            scan_line = line
            if stripped.startswith(("export function ", "function ", "class ")):
                if "{" not in line:
                    continue
                scan_line = line.split("{", 1)[1]

            for match in call_pattern.finditer(scan_line):
                reference_symbol = match.group(1)
                if reference_symbol in ignored_names:
                    continue
                nodes.append(
                    Node.create_reference(
                        symbol=reference_symbol,
                        file_path=file_path,
                        line=line_num,
                        column=line.find(reference_symbol),
                        language=self.language,
                    )
                )

        return nodes

    def _parse_python_import_names(self, raw_imports: str) -> ImportNames:
        """Parse imported names from Python import clauses."""
        cleaned = raw_imports.strip()
        if cleaned == "*":
            return []

        imported_names: ImportNames = []
        for raw_name in cleaned.split(","):
            name = raw_name.strip()
            if not name:
                continue
            base_name = name.split(" as ", 1)[0].strip()
            if base_name:
                imported_names.append(base_name)
        return imported_names

    def _parse_typescript_import_names(self, raw_clause: str) -> ImportNames:
        """Parse imported symbol names from a TypeScript import clause."""
        clause = raw_clause.strip()
        if not clause:
            return []
        if clause.startswith("{") and clause.endswith("}"):
            imported_names: ImportNames = []
            for raw_name in clause[1:-1].split(","):
                name = raw_name.strip()
                if not name:
                    continue
                base_name = name.split(" as ", 1)[0].strip()
                if base_name:
                    imported_names.append(base_name)
            return imported_names
        if clause.startswith("* as "):
            namespace_name = clause[len("* as "):].strip()
            return [namespace_name] if namespace_name else []
        default_name = clause.split(",", 1)[0].strip()
        return [default_name] if default_name else []

    def _detect_typescript_import_mode(self, raw_clause: str) -> str:
        """Classify the TypeScript import clause for downstream edge resolution."""
        clause = raw_clause.strip()
        if not clause:
            return "module"
        if clause.startswith("{") and clause.endswith("}"):
            return "named"
        if clause.startswith("* as "):
            return "namespace"
        return "default"
