"""Detector for missing internationalization."""

import re

from ..models import Finding, Severity
from .base import BaseDetector


class I18nGapsDetector(BaseDetector):
    """Detect hardcoded strings that should be internationalized."""

    @property
    def name(self) -> str:
        return "i18n-gaps"

    @property
    def description(self) -> str:
        return "Checks for hardcoded user-facing strings that should be internationalized"

    def detect(self) -> list[Finding]:
        """Detect hardcoded strings in UI components."""
        findings: list[Finding] = []

        # Pattern for JSX/TSX strings
        jsx_string_patterns = [
            r'>\s*([A-Z][a-zA-Z\s]{10,})\s*<',  # Text between tags
            r'placeholder=["\']([^"\']{10,})["\']',  # Placeholder text
            r'title=["\']([^"\']{10,})["\']',  # Title attributes
            r'aria-label=["\']([^"\']{10,})["\']',  # ARIA labels
        ]

        # Skip certain patterns
        skip_patterns = [
            r't\(["\']',  # Already using translation function
            r'i18n\.',    # Already using i18n
            r'\$t\(',     # Vue i18n
            r'formatMessage',  # React Intl
        ]

        # Scan TSX, JSX files
        extensions = {'.tsx', '.jsx', '.vue'}

        for file_path in self.repo_path.rglob('*'):
            if not file_path.is_file() or file_path.suffix not in extensions:
                continue

            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            # Check if file uses i18n at all
            uses_i18n = any(re.search(pattern, content) for pattern in skip_patterns)

            lines = content.split('\n')

            for line_num, line in enumerate(lines, start=1):
                # Skip if line already uses i18n
                if any(re.search(pattern, line) for pattern in skip_patterns):
                    continue

                for pattern in jsx_string_patterns:
                    matches = re.finditer(pattern, line)
                    for match in matches:
                        text = match.group(1)

                        # Skip if too short or looks like code
                        if len(text.split()) < 3:
                            continue
                        if re.search(r'[{}()\[\]]', text):
                            continue

                        findings.append(Finding(
                            detector=self.name,
                            severity=Severity.INFO if uses_i18n else Severity.WARNING,
                            message=f"Hardcoded user-facing text: '{text[:50]}...'",
                            file_path=str(file_path.relative_to(self.repo_path)),
                            line_number=line_num,
                            evidence=line.strip()[:150],
                            suggestion="Use i18n translation function, e.g., t('key') or formatMessage()"
                        ))

        return findings
