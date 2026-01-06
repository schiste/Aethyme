"""Detector for missing data-ui test selectors."""

import re
from typing import List
from pathlib import Path

from .base import BaseDetector
from ..models import Finding, Severity


class DataUICoverageDetector(BaseDetector):
    """Detect missing data-ui test selectors in UI components."""

    @property
    def name(self) -> str:
        return "data-ui-coverage"

    @property
    def description(self) -> str:
        return "Checks for missing data-ui attributes in UI components"

    def detect(self) -> List[Finding]:
        """Detect UI components without data-ui selectors."""
        findings = []

        # Patterns for UI components
        component_patterns = [
            (r'<button[^>]*>', 'button'),
            (r'<input[^>]*>', 'input'),
            (r'<select[^>]*>', 'select'),
            (r'<form[^>]*>', 'form'),
            (r'<a[^>]*href=', 'link'),
            (r'<textarea[^>]*>', 'textarea'),
        ]

        # Pattern to check for data-ui attribute
        data_ui_pattern = r'data-ui=["\'][\w-]+'

        # Scan TSX, JSX, HTML files
        extensions = {'.tsx', '.jsx', '.html', '.vue'}

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
                for pattern, element_type in component_patterns:
                    matches = re.finditer(pattern, line, re.IGNORECASE)
                    for match in matches:
                        component_html = match.group(0)

                        # Check if this component has data-ui
                        if not re.search(data_ui_pattern, component_html):
                            findings.append(Finding(
                                detector=self.name,
                                severity=Severity.WARNING,
                                message=f"Missing data-ui attribute on {element_type}",
                                file_path=str(file_path.relative_to(self.repo_path)),
                                line_number=line_num,
                                evidence=line.strip()[:100],
                                suggestion=f"Add data-ui attribute for test automation, e.g., data-ui=\"{element_type}-name\""
                            ))

        return findings
