"""Detector for absolute links that should be relative."""

import re

from ..models import Finding, Severity
from .base import BaseDetector


class RelativeLinksDetector(BaseDetector):
    """Detect absolute file links that should be relative."""

    @property
    def name(self) -> str:
        return "relative-links"

    @property
    def description(self) -> str:
        return "Checks for absolute file paths that should use relative links"

    def detect(self) -> list[Finding]:
        """Detect absolute links in documentation and code."""
        findings: list[Finding] = []

        # Patterns for absolute file paths
        absolute_path_patterns = [
            (r'/home/[\w/.-]+', 'Linux home path'),
            (r'/Users/[\w/.-]+', 'macOS home path'),
            (r'C:\\[\w\\.-]+', 'Windows path'),
            (r'/var/[\w/.-]+', 'System path'),
            (r'/tmp/[\w/.-]+', 'Temp path'),
        ]

        # Scan markdown and code files
        extensions = {'.md', '.py', '.ts', '.tsx', '.jsx', '.js', '.json', '.yaml', '.yml'}

        for file_path in self.repo_path.rglob('*'):
            if not file_path.is_file() or file_path.suffix not in extensions:
                continue

            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            for line_num, line in enumerate(lines, start=1):
                for pattern, path_type in absolute_path_patterns:
                    matches = re.finditer(pattern, line)
                    for match in matches:
                        absolute_path = match.group(0)

                        # Skip if it's in a comment explaining paths
                        if re.search(r'#.*' + re.escape(absolute_path), line) or \
                           re.search(r'//.*' + re.escape(absolute_path), line):
                            continue

                        findings.append(Finding(
                            detector=self.name,
                            severity=Severity.WARNING,
                            message=f"Absolute {path_type} should use relative path",
                            file_path=str(file_path.relative_to(self.repo_path)),
                            line_number=line_num,
                            evidence=line.strip()[:150],
                            suggestion="Use relative paths for better portability across environments"
                        ))

        return findings
