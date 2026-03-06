"""Base class for all fixers."""

from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any

from .._log import get_logger

logger = get_logger()


class BaseFixer(ABC):
    """Base class for all autofix implementations."""

    def __init__(self, repo_path: Path):
        self.repo_path = Path(repo_path)
        self.fixes_applied = 0
        self.files_processed = 0
        self.errors: list[str] = []

    @abstractmethod
    def get_fix_type(self) -> str:
        """Return the type identifier for this fixer."""
        pass

    @abstractmethod
    def can_fix(self, file_path: Path) -> bool:
        """Determine if this fixer can process the given file."""
        pass

    @abstractmethod
    def fix(self, file_path: Path, content: str) -> str | None:
        """Apply fix to content and return new content, or None if no fix needed."""
        pass

    def process_file(self, file_path: Path) -> dict[str, Any] | None:
        """Process a single file."""
        if not self.can_fix(file_path):
            return None

        try:
            with open(file_path, encoding='utf-8') as f:
                original_content = f.read()

            new_content = self.fix(file_path, original_content)

            if new_content is None or new_content == original_content:
                return None

            self.files_processed += 1
            self.fixes_applied += 1

            return {
                "file_path": file_path,
                "original_content": original_content,
                "new_content": new_content,
                "fix_type": self.get_fix_type(),
            }

        except Exception as e:
            error_msg = f"Error processing {file_path}: {e}"
            self.errors.append(error_msg)
            logger.error("Fix failed", file=str(file_path), error=str(e))
            return None

    def process_directory(self, directory: Path | None = None) -> list[dict[str, Any]]:
        """Process all applicable files in a directory."""
        if directory is None:
            directory = self.repo_path

        fixes: list[dict[str, Any]] = []
        for file_path in directory.rglob("*"):
            if file_path.is_file():
                fix_result = self.process_file(file_path)
                if fix_result:
                    fixes.append(fix_result)

        logger.info(
            "Directory processing complete",
            fixer=self.get_fix_type(),
            files_processed=self.files_processed,
            fixes_applied=self.fixes_applied,
            errors=len(self.errors),
        )

        return fixes

    def get_stats(self) -> dict[str, Any]:
        """Get statistics for this fixer."""
        return {
            "fix_type": self.get_fix_type(),
            "files_processed": self.files_processed,
            "fixes_applied": self.fixes_applied,
            "errors": len(self.errors),
            "error_messages": self.errors,
        }
