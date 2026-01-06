"""Detector for API route documentation coverage."""

import re
from typing import List, Dict, Set
from pathlib import Path

from .base import BaseDetector
from ..models import Finding, Severity


class RouteCoverageDetector(BaseDetector):
    """Detect missing API route documentation."""

    @property
    def name(self) -> str:
        return "route-coverage"

    @property
    def description(self) -> str:
        return "Checks for undocumented API routes and endpoints"

    def detect(self) -> List[Finding]:
        """Detect undocumented API routes."""
        findings = []

        # Find route definitions
        routes = self._find_routes()

        # Check documentation
        findings.extend(self._check_route_documentation(routes))

        return findings

    def _find_routes(self) -> List[Dict]:
        """Find API route definitions."""
        routes = []

        # FastAPI/Flask patterns
        fastapi_pattern = r'@(router|app)\.(get|post|put|delete|patch)\(["\']([^"\']+)["\']'
        flask_pattern = r'@app\.route\(["\']([^"\']+)["\'].*methods=\[([^\]]+)\]'

        # Express.js patterns
        express_pattern = r'(router|app)\.(get|post|put|delete|patch)\(["\']([^"\']+)["\']'

        for file_path in self.repo_path.rglob('*'):
            if not file_path.is_file():
                continue

            if self.should_skip_file(file_path):
                continue

            # Check Python files for FastAPI/Flask
            if file_path.suffix == '.py':
                content = self.read_file_safe(file_path)
                if not content:
                    continue

                lines = content.split('\n')

                for line_num, line in enumerate(lines, start=1):
                    # FastAPI routes
                    match = re.search(fastapi_pattern, line)
                    if match:
                        routes.append({
                            'method': match.group(2).upper(),
                            'path': match.group(3),
                            'file': file_path,
                            'line': line_num,
                            'framework': 'fastapi'
                        })

                    # Flask routes
                    match = re.search(flask_pattern, line)
                    if match:
                        methods = [m.strip(' "\'') for m in match.group(2).split(',')]
                        for method in methods:
                            routes.append({
                                'method': method.upper(),
                                'path': match.group(1),
                                'file': file_path,
                                'line': line_num,
                                'framework': 'flask'
                            })

            # Check TypeScript/JavaScript files for Express
            elif file_path.suffix in {'.ts', '.js'}:
                content = self.read_file_safe(file_path)
                if not content:
                    continue

                lines = content.split('\n')

                for line_num, line in enumerate(lines, start=1):
                    match = re.search(express_pattern, line)
                    if match:
                        routes.append({
                            'method': match.group(2).upper(),
                            'path': match.group(3),
                            'file': file_path,
                            'line': line_num,
                            'framework': 'express'
                        })

        return routes

    def _check_route_documentation(self, routes: List[Dict]) -> List[Finding]:
        """Check if routes are documented."""
        findings = []

        for route in routes:
            file_path = route['file']
            line_num = route['line']

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            # Check if function has docstring/JSDoc
            has_docs = False

            # Look at lines before the route decorator
            for i in range(max(0, line_num - 10), line_num):
                line = lines[i] if i < len(lines) else ""

                # Check for docstring or JSDoc
                if '"""' in line or "'''" in line or '/**' in line:
                    has_docs = True
                    break

            # Look at lines after decorator (function definition)
            if not has_docs and line_num < len(lines) - 1:
                next_lines = lines[line_num:min(line_num + 5, len(lines))]
                next_content = '\n'.join(next_lines)

                if '"""' in next_content or "'''" in next_content or '/**' in next_content:
                    has_docs = True

            if not has_docs:
                findings.append(Finding(
                    detector=self.name,
                    severity=Severity.WARNING,
                    message=f"Undocumented API route: {route['method']} {route['path']}",
                    file_path=str(file_path.relative_to(self.repo_path)),
                    line_number=line_num,
                    evidence=None,
                    suggestion="Add docstring/JSDoc describing the endpoint, parameters, and responses"
                ))

        return findings
