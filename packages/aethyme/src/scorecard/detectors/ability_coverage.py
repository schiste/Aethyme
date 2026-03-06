"""Detector for permission/ability coverage."""

import re
from pathlib import Path
from typing import TypedDict

from ..models import Finding, Severity
from .base import BaseDetector


class ProtectedRoute(TypedDict):
    file: Path
    line: int
    content: str


class AbilityCoverageDetector(BaseDetector):
    """Detect missing permission checks and ability definitions."""

    @property
    def name(self) -> str:
        return "ability-coverage"

    @property
    def description(self) -> str:
        return "Checks for missing authorization and permission definitions"

    def detect(self) -> list[Finding]:
        """Detect missing ability/permission checks."""
        findings: list[Finding] = []

        # Find protected routes
        routes = self._find_protected_routes()

        # Check for permission definitions
        findings.extend(self._check_permission_definitions(routes))

        # Check for authorization checks
        findings.extend(self._check_authorization_checks())

        return findings

    def _find_protected_routes(self) -> list[ProtectedRoute]:
        """Find routes that should have permission checks."""
        routes: list[ProtectedRoute] = []

        # Patterns for routes with auth
        auth_patterns = [
            r'@router\.(post|put|delete|patch)',  # Mutating operations
            r'Depends\(get_current_user\)',  # Auth dependency
            r'@require_auth',
            r'@login_required',
        ]

        for file_path in self.repo_path.rglob('*.py'):
            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            for line_num, line in enumerate(lines, start=1):
                for pattern in auth_patterns:
                    if re.search(pattern, line):
                        routes.append({
                            'file': file_path,
                            'line': line_num,
                            'content': line.strip()
                        })
                        break

        return routes

    def _check_permission_definitions(self, routes: list[ProtectedRoute]) -> list[Finding]:
        """Check if routes have permission checks."""
        findings: list[Finding] = []

        permission_patterns = [
            r'can\(',
            r'check_permission',
            r'require_permission',
            r'@permission_required',
            r'has_permission',
            r'authorize\(',
        ]

        for route in routes:
            file_path = route["file"]
            line_num = route["line"]

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            # Check surrounding lines for permission checks
            has_permission = False

            start = max(0, line_num - 5)
            end = min(len(lines), line_num + 15)  # Check more lines after

            for i in range(start, end):
                line = lines[i] if i < len(lines) else ""

                for pattern in permission_patterns:
                    if re.search(pattern, line):
                        has_permission = True
                        break

                if has_permission:
                    break

            if not has_permission:
                findings.append(Finding(
                    detector=self.name,
                    severity=Severity.WARNING,
                    message="Protected route missing explicit permission check",
                    file_path=str(file_path.relative_to(self.repo_path)),
                    line_number=line_num,
                    evidence=route["content"][:100],
                    suggestion="Add explicit permission check, e.g., check_permission('resource:action')"
                ))

        return findings

    def _check_authorization_checks(self) -> list[Finding]:
        """Check for authorization model definitions."""
        findings: list[Finding] = []

        # Look for ability/permission definition files
        ability_files = list(self.repo_path.glob('**/abilities.py')) + \
                       list(self.repo_path.glob('**/permissions.py')) + \
                       list(self.repo_path.glob('**/authorization.py'))

        if not ability_files:
            # No centralized ability definitions found
            findings.append(Finding(
                detector=self.name,
                severity=Severity.INFO,
                message="No centralized permission/ability definitions found",
                file_path=".",
                line_number=None,
                evidence=None,
                suggestion="Create abilities.py or permissions.py to centralize authorization logic"
            ))

        return findings
