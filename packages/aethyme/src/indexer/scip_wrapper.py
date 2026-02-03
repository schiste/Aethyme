"""Production SCIP wrapper with validation and error handling."""

import subprocess
import json
import tempfile
from pathlib import Path
from typing import Dict, List, Any, Optional
import structlog
from dataclasses import dataclass

from ..models.graph import Node, NodeKind

logger = structlog.get_logger(__name__)


@dataclass
class SCIPDocument:
    """Represents a SCIP document with occurrences."""

    relative_path: str
    occurrences: List[Dict[str, Any]]
    symbols: Optional[List[Dict[str, Any]]] = None


class SCIPIndexer:
    """Robust SCIP indexer with fallback support."""

    SCIP_BINARIES = {
        "python": "scip-python",
        "typescript": "scip-typescript",
        "javascript": "scip-typescript",
        "tsx": "scip-typescript",
        "jsx": "scip-typescript",
    }

    def __init__(self, language: str, timeout: int = 300):
        """
        Initialize SCIP indexer for a specific language.

        Args:
            language: Programming language to index
            timeout: Timeout in seconds for indexing
        """
        self.language = language.lower()
        self.timeout = timeout
        self.binary = self.SCIP_BINARIES.get(self.language)

        if not self.binary:
            raise ValueError(f"Unsupported language: {language}")

        self._available = self._check_binary_available()

    def _check_binary_available(self) -> bool:
        """Check if SCIP binary is available and functional."""
        try:
            result = subprocess.run(
                [self.binary, "--version"],
                capture_output=True,
                timeout=5,
                text=True,
            )
            if result.returncode == 0:
                logger.info(
                    "SCIP binary available",
                    binary=self.binary,
                    version=result.stdout.strip(),
                )
                return True
            else:
                logger.warning(
                    "SCIP binary not functional",
                    binary=self.binary,
                    stderr=result.stderr,
                )
                return False
        except (subprocess.TimeoutExpired, FileNotFoundError) as e:
            logger.warning(
                "SCIP binary not found",
                binary=self.binary,
                error=str(e),
            )
            return False

    def is_available(self) -> bool:
        """Check if SCIP indexer is available."""
        return self._available

    def index(self, repo_path: Path, project_name: Optional[str] = None) -> Dict[str, Any]:
        """
        Index repository using SCIP.

        Args:
            repo_path: Path to repository
            project_name: Optional project name for SCIP

        Returns:
            Dictionary containing indexed documents

        Raises:
            RuntimeError: If indexing fails
        """
        if not self._available:
            raise RuntimeError(f"SCIP binary {self.binary} not available")

        repo_path = Path(repo_path)
        if not repo_path.exists():
            raise FileNotFoundError(f"Repository path does not exist: {repo_path}")

        # Validate repository has required files
        if self.language in ["typescript", "tsx", "jsx"]:
            tsconfig = repo_path / "tsconfig.json"
            if not tsconfig.exists():
                logger.warning(
                    "tsconfig.json not found, creating minimal config",
                    repo_path=str(repo_path),
                )
                try:
                    self._create_minimal_tsconfig(repo_path)
                except (PermissionError, OSError) as e:
                    logger.error(
                        "Cannot create tsconfig.json, skipping TypeScript indexing",
                        repo_path=str(repo_path),
                        error=str(e),
                    )
                    raise RuntimeError(f"Cannot create tsconfig.json: {e}")

        with tempfile.NamedTemporaryFile(suffix=".scip", delete=False) as tmp:
            output_path = Path(tmp.name)

        try:
            # Build SCIP command
            cmd = [
                self.binary,
                "index",
                "--output", str(output_path),
            ]

            if project_name:
                cmd.extend(["--project-name", project_name])
            else:
                cmd.extend(["--project-name", repo_path.name])

            # Run SCIP indexer
            logger.info(
                "Running SCIP indexer",
                command=" ".join(cmd),
                cwd=str(repo_path),
            )

            result = subprocess.run(
                cmd,
                cwd=repo_path,
                capture_output=True,
                timeout=self.timeout,
                text=True,
            )

            if result.returncode != 0:
                logger.error(
                    "SCIP indexing failed",
                    returncode=result.returncode,
                    stderr=result.stderr[:1000],
                )
                raise RuntimeError(f"SCIP indexing failed: {result.stderr[:500]}")

            # Parse SCIP output
            return self._parse_scip_output(output_path)

        except subprocess.TimeoutExpired:
            logger.error(
                "SCIP indexing timed out",
                timeout=self.timeout,
                repo_path=str(repo_path),
            )
            raise RuntimeError(f"SCIP indexing timed out after {self.timeout}s")
        finally:
            # Cleanup temporary file
            if output_path.exists():
                output_path.unlink()

    def _parse_scip_output(self, output_path: Path) -> Dict[str, Any]:
        """
        Parse SCIP index file.

        Args:
            output_path: Path to SCIP output file

        Returns:
            Dictionary containing parsed documents
        """
        if not output_path.exists():
            raise FileNotFoundError(f"SCIP output not found: {output_path}")

        # Try to parse as JSONL (newer format)
        documents = []
        try:
            with open(output_path, "r") as f:
                for line_num, line in enumerate(f, 1):
                    if line.strip():
                        try:
                            doc = json.loads(line)
                            documents.append(doc)
                        except json.JSONDecodeError as e:
                            logger.warning(
                                "Failed to parse SCIP line",
                                line_number=line_num,
                                error=str(e),
                            )
                            continue

            logger.info(
                "SCIP output parsed",
                documents=len(documents),
                format="jsonl",
            )

            return {
                "metadata": {
                    "language": self.language,
                    "format": "jsonl",
                    "documents_count": len(documents),
                },
                "documents": documents,
            }

        except Exception as e:
            logger.error(
                "Failed to parse SCIP output",
                error=str(e),
                output_path=str(output_path),
            )
            raise ValueError(f"Failed to parse SCIP output: {e}")

    def _create_minimal_tsconfig(self, repo_path: Path) -> None:
        """Create minimal tsconfig.json for TypeScript projects."""
        tsconfig = {
            "compilerOptions": {
                "target": "ES2020",
                "module": "commonjs",
                "lib": ["ES2020"],
                "jsx": "react",
                "strict": False,
                "esModuleInterop": True,
                "skipLibCheck": True,
                "forceConsistentCasingInFileNames": True,
            },
            "include": ["**/*"],
            "exclude": ["node_modules", "dist", "build"],
        }

        tsconfig_path = repo_path / "tsconfig.json"
        with open(tsconfig_path, "w") as f:
            json.dump(tsconfig, f, indent=2)

        logger.info("Created minimal tsconfig.json", path=str(tsconfig_path))

    def extract_nodes(self, scip_data: Dict[str, Any]) -> List[Node]:
        """
        Extract Node objects from SCIP data.

        Args:
            scip_data: Parsed SCIP data

        Returns:
            List of Node objects
        """
        nodes = []

        for doc in scip_data.get("documents", []):
            file_path = doc.get("relative_path", "")

            for occurrence in doc.get("occurrences", []):
                # Parse SCIP occurrence
                symbol = occurrence.get("symbol", "")
                if not symbol:
                    continue

                # Extract position
                range_data = occurrence.get("range", [0, 0, 0, 0])
                line = range_data[0] if len(range_data) > 0 else 0
                column = range_data[1] if len(range_data) > 1 else 0

                # Determine node kind
                symbol_roles = occurrence.get("symbol_roles", 0)
                is_definition = bool(symbol_roles & 1)  # Role 1 = Definition

                kind = NodeKind.DEFINITION if is_definition else NodeKind.REFERENCE

                # Create node
                node = Node(
                    symbol=symbol,
                    file_path=file_path,
                    line_number=line,
                    column_number=column,
                    kind=kind,
                    language=self.language,
                    documentation=occurrence.get("hover_text"),
                    metadata={
                        "syntax_kind": occurrence.get("syntax_kind"),
                        "symbol_roles": symbol_roles,
                    },
                )

                nodes.append(node)

        logger.info(
            "Extracted nodes from SCIP",
            total_nodes=len(nodes),
            definitions=sum(1 for n in nodes if n.kind == NodeKind.DEFINITION),
            references=sum(1 for n in nodes if n.kind == NodeKind.REFERENCE),
        )

        return nodes

    def extract_edges(self, scip_data: Dict[str, Any], nodes: List[Node]) -> List[tuple]:
        """
        Extract edge relationships from SCIP data.

        Args:
            scip_data: Parsed SCIP data
            nodes: List of nodes

        Returns:
            List of (from_node_id, to_node_id, edge_type) tuples
        """
        edges = []

        # Build symbol to node mapping
        symbol_to_def = {}
        for node in nodes:
            if node.kind in [NodeKind.DEFINITION, NodeKind.CLASS, NodeKind.FUNCTION]:
                symbol_to_def[node.symbol] = node.id

        # Create edges from references to definitions
        for node in nodes:
            if node.kind == NodeKind.REFERENCE and node.symbol in symbol_to_def:
                edges.append((
                    node.id,
                    symbol_to_def[node.symbol],
                    "invoke"
                ))

        logger.info(
            "Extracted edges from SCIP",
            total_edges=len(edges),
        )

        return edges