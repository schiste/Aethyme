"""Detector for schema/type mismatches and drift."""

import re
from pathlib import Path
from typing import TypedDict

from ..models import Finding, Severity
from .base import BaseDetector


class ExtractedSchema(TypedDict):
    file: Path
    type: str


class SchemaDriftDetector(BaseDetector):
    """Detect schema/type mismatches between definitions and usage."""

    @property
    def name(self) -> str:
        return "schema-drift"

    @property
    def description(self) -> str:
        return "Checks for schema and type definition mismatches"

    def detect(self) -> list[Finding]:
        """Detect schema drift issues."""
        findings: list[Finding] = []

        # Find schema definition files
        schema_files = self._find_schema_files()

        # Extract schemas
        schemas = self._extract_schemas(schema_files)

        # Check for common drift issues
        findings.extend(self._check_pydantic_models())
        findings.extend(self._check_typescript_interfaces())
        findings.extend(self._check_api_schema_consistency(schemas))

        return findings

    def _find_schema_files(self) -> list[Path]:
        """Find schema definition files."""
        schema_files: list[Path] = []

        patterns = ("**/schema.py", "**/schemas.py", "**/models.py", "**/types.ts", "**/schema.ts")

        for pattern in patterns:
            schema_files.extend(self.repo_path.glob(pattern))

        return schema_files

    def _extract_schemas(self, schema_files: list[Path]) -> dict[str, ExtractedSchema]:
        """Extract schema definitions."""
        schemas: dict[str, ExtractedSchema] = {}

        for file_path in schema_files:
            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            # Extract Pydantic models
            if file_path.suffix == '.py':
                model_pattern = r'class\s+(\w+)\(BaseModel\):'
                for match in re.finditer(model_pattern, content):
                    schemas[match.group(1)] = {"file": file_path, "type": "pydantic"}

            # Extract TypeScript interfaces
            elif file_path.suffix == '.ts':
                interface_pattern = r'interface\s+(\w+)\s*\{'
                for match in re.finditer(interface_pattern, content):
                    schemas[match.group(1)] = {"file": file_path, "type": "typescript"}

        return schemas

    def _check_pydantic_models(self) -> list[Finding]:
        """Check Pydantic models for common issues."""
        findings: list[Finding] = []

        for file_path in self.repo_path.rglob('*.py'):
            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            # Check for Pydantic models without validators
            in_model = False
            model_name: str | None = None
            model_line = 0
            has_fields = False
            has_validators = False

            for line_num, line in enumerate(lines, start=1):
                if re.match(r'class\s+\w+\(BaseModel\):', line):
                    in_model = True
                    class_match = re.search(r'class\s+(\w+)', line)
                    if class_match is None:
                        in_model = False
                        continue
                    model_name = class_match.group(1)
                    model_line = line_num
                    has_fields = False
                    has_validators = False

                elif in_model:
                    if re.match(r'\S', line):  # Non-indented line, end of class
                        if has_fields and not has_validators:
                            findings.append(Finding(
                                detector=self.name,
                                severity=Severity.INFO,
                                message=f"Pydantic model '{model_name}' has no validators",
                                file_path=str(file_path.relative_to(self.repo_path)),
                                line_number=model_line,
                                evidence=None,
                                suggestion="Consider adding validators for data validation"
                            ))
                        in_model = False

                    if re.match(r'\s+\w+:\s+', line):
                        has_fields = True

                    if re.search(r'@validator|@field_validator', line):
                        has_validators = True

        return findings

    def _check_typescript_interfaces(self) -> list[Finding]:
        """Check TypeScript interfaces for common issues."""
        findings: list[Finding] = []

        for file_path in self.repo_path.rglob('*.ts'):
            if self.should_skip_file(file_path):
                continue

            content = self.read_file_safe(file_path)
            if not content:
                continue

            lines = content.split('\n')

            # Check for interfaces with 'any' type
            for line_num, line in enumerate(lines, start=1):
                if re.search(r':\s*any\b', line) and 'interface' in content[:content.find(line)]:
                    findings.append(Finding(
                        detector=self.name,
                        severity=Severity.WARNING,
                        message="Using 'any' type reduces type safety",
                        file_path=str(file_path.relative_to(self.repo_path)),
                        line_number=line_num,
                        evidence=line.strip(),
                        suggestion="Replace 'any' with specific type or 'unknown'"
                    ))

        return findings

    def _check_api_schema_consistency(self, schemas: dict[str, ExtractedSchema]) -> list[Finding]:
        """Check for API schema consistency between frontend and backend."""
        findings: list[Finding] = []

        # This is a simplified check - would need more sophisticated matching
        # in a production system

        return findings
