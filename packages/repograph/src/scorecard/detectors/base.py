"""Base detector class for AI-readiness checks."""

from abc import ABC, abstractmethod
from typing import List, Optional
from pathlib import Path
import structlog

from ..models import Finding

logger = structlog.get_logger(__name__)


class BaseDetector(ABC):
    """Base class for all detectors."""

    def __init__(self, repo_path: Path):
        """
        Initialize detector.

        Args:
            repo_path: Path to the repository to scan
        """
        self.repo_path = repo_path
        self.logger = logger.bind(detector=self.name)

    @property
    @abstractmethod
    def name(self) -> str:
        """Detector name."""
        pass

    @property
    @abstractmethod
    def description(self) -> str:
        """Detector description."""
        pass

    @abstractmethod
    def detect(self) -> List[Finding]:
        """
        Run detection and return findings.

        Returns:
            List of findings
        """
        pass

    def should_skip_file(self, file_path: Path) -> bool:
        """
        Check if file should be skipped during scanning.

        Args:
            file_path: Path to check

        Returns:
            True if file should be skipped
        """
        # Common exclusions
        exclude_dirs = {
            'node_modules', '__pycache__', '.git', 'venv', '.venv',
            'dist', 'build', '.pytest_cache', '.mypy_cache',
            'site-packages', '.next', 'coverage', '.tox',
        }

        # Check if any part of path is in exclude list
        parts = set(file_path.parts)
        if parts & exclude_dirs:
            return True

        # Skip hidden files (except specific ones)
        if any(part.startswith('.') and part not in {'.env.example', '.env.template'}
               for part in file_path.parts):
            return True

        return False

    def read_file_safe(self, file_path: Path) -> Optional[str]:
        """
        Safely read file contents.

        Args:
            file_path: Path to file

        Returns:
            File contents or None if error
        """
        try:
            return file_path.read_text(encoding='utf-8')
        except (UnicodeDecodeError, PermissionError, FileNotFoundError) as e:
            self.logger.debug("Failed to read file", path=str(file_path), error=str(e))
            return None
