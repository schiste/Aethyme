"""Detector for missing FOLDER.md documentation."""

from typing import List
from pathlib import Path

from .base import BaseDetector
from ..models import Finding, Severity


class FolderDocsDetector(BaseDetector):
    """Detect missing FOLDER.md documentation in directories."""

    @property
    def name(self) -> str:
        return "folder-docs"

    @property
    def description(self) -> str:
        return "Checks for missing FOLDER.md documentation in directories"

    def detect(self) -> List[Finding]:
        """Detect directories without FOLDER.md."""
        findings = []

        # Directories that should have FOLDER.md
        important_dirs = {
            'src', 'components', 'pages', 'api', 'utils', 'lib',
            'services', 'models', 'controllers', 'views', 'routes',
            'middleware', 'config', 'tests', 'docs'
        }

        # Walk through directories
        for dir_path in self.repo_path.rglob('*'):
            if not dir_path.is_dir():
                continue

            if self.should_skip_file(dir_path):
                continue

            # Check if directory name matches important patterns
            dir_name = dir_path.name.lower()
            should_have_docs = dir_name in important_dirs

            # Also check if directory has multiple Python/TS files
            if not should_have_docs:
                code_files = list(dir_path.glob('*.py')) + list(dir_path.glob('*.ts')) + list(dir_path.glob('*.tsx'))
                if len(code_files) >= 3:
                    should_have_docs = True

            if should_have_docs:
                folder_md = dir_path / 'FOLDER.md'
                readme_md = dir_path / 'README.md'

                if not folder_md.exists() and not readme_md.exists():
                    findings.append(Finding(
                        detector=self.name,
                        severity=Severity.WARNING,
                        message=f"Missing FOLDER.md in {dir_path.relative_to(self.repo_path)}",
                        file_path=str(dir_path.relative_to(self.repo_path)),
                        line_number=None,
                        evidence=None,
                        suggestion="Create FOLDER.md to document the purpose and contents of this directory"
                    ))

        return findings
