"""Production SCIP wrapper with validation and error handling."""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import structlog

logger = structlog.get_logger(__name__)


class SCIPIndexer:
    """Robust SCIP indexer with fallback support."""

    SCIP_BINARIES: dict[str, str] = {
        "python": "scip-python",
        "typescript": "scip-typescript",
        "javascript": "scip-typescript",
        "tsx": "scip-typescript",
        "jsx": "scip-typescript",
    }

    def __init__(self, language: str, timeout: int = 300):
        self.language = language.lower()
        self.timeout = timeout
        binary = self.SCIP_BINARIES.get(self.language)
        if binary is None:
            raise ValueError(f"Unsupported language: {language}")
        self.binary = binary
        self._available = self._check_binary_available()

    def _check_binary_available(self) -> bool:
        try:
            result: subprocess.CompletedProcess[str] = subprocess.run(
                [self.binary, "--version"],
                capture_output=True,
                timeout=5,
                text=True,
            )
        except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
            logger.warning("SCIP binary not found", binary=self.binary, error=str(exc))
            return False

        if result.returncode == 0:
            logger.info(
                "SCIP binary available",
                binary=self.binary,
                version=result.stdout.strip(),
            )
            return True

        logger.warning(
            "SCIP binary not functional",
            binary=self.binary,
            stderr=result.stderr,
        )
        return False

    def is_available(self) -> bool:
        return self._available

    def index(self, repo_path: Path, project_name: str | None = None) -> dict[str, Any]:
        if not self._available:
            raise RuntimeError(f"SCIP binary {self.binary} not available")

        repo_path = Path(repo_path)
        if not repo_path.exists():
            raise FileNotFoundError(f"Repository path does not exist: {repo_path}")

        if self.language in ["typescript", "tsx", "jsx"]:
            tsconfig = repo_path / "tsconfig.json"
            if not tsconfig.exists():
                logger.warning("tsconfig.json not found, creating minimal config", repo_path=str(repo_path))
                self._create_minimal_tsconfig(repo_path)

        with tempfile.NamedTemporaryFile(suffix=".scip", delete=False) as tmp:
            output_path = Path(tmp.name)

        try:
            cmd: list[str] = [self.binary, "index", "--output", str(output_path)]
            cmd.extend(["--project-name", project_name or repo_path.name])
            logger.info("Running SCIP indexer", command=" ".join(cmd), cwd=str(repo_path))
            result: subprocess.CompletedProcess[str] = subprocess.run(
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
            return self._parse_scip_output(output_path)
        except subprocess.TimeoutExpired as exc:
            logger.error("SCIP indexing timed out", timeout=self.timeout, repo_path=str(repo_path))
            raise RuntimeError(f"SCIP indexing timed out after {self.timeout}s") from exc
        finally:
            if output_path.exists():
                output_path.unlink()

    def _parse_scip_output(self, output_path: Path) -> dict[str, Any]:
        if not output_path.exists():
            raise FileNotFoundError(f"SCIP output not found: {output_path}")

        documents: list[dict[str, Any]] = []
        try:
            with open(output_path, encoding="utf-8") as handle:
                for line_num, line in enumerate(handle, 1):
                    if not line.strip():
                        continue
                    try:
                        documents.append(json.loads(line))
                    except json.JSONDecodeError as exc:
                        logger.warning("Failed to parse SCIP line", line_number=line_num, error=str(exc))
            logger.info("SCIP output parsed", documents=len(documents), format="jsonl")
            return {
                "metadata": {
                    "language": self.language,
                    "format": "jsonl",
                    "documents_count": len(documents),
                },
                "documents": documents,
            }
        except Exception as exc:
            logger.error("Failed to parse SCIP output", error=str(exc), output_path=str(output_path))
            raise ValueError(f"Failed to parse SCIP output: {exc}") from exc

    def _create_minimal_tsconfig(self, repo_path: Path) -> None:
        tsconfig_path = repo_path / "tsconfig.json"
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
        with open(tsconfig_path, "w", encoding="utf-8") as handle:
            json.dump(tsconfig, handle, indent=2)
        logger.info("Created minimal tsconfig.json", path=str(tsconfig_path))
