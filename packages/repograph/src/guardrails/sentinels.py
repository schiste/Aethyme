"""Drift sentinels for detecting schema and code changes.

This module provides preflight checks to detect breaking changes,
schema drift, and other risky modifications before they cause issues.
"""

import re
import hashlib
from typing import Dict, List, Optional, Set, Any, Callable
from dataclasses import dataclass, field
from pathlib import Path
from enum import Enum
from datetime import datetime
import structlog

from .schema_first import SchemaExtractor, ExtractedSchema

logger = structlog.get_logger(__name__)


class RiskLevel(str, Enum):
    """Risk levels for detected changes."""
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"
    CRITICAL = "critical"


@dataclass
class DriftDetection:
    """Detected drift in codebase."""

    rule_id: str
    risk_level: RiskLevel
    description: str
    file_path: str
    line_number: Optional[int] = None
    old_value: Optional[str] = None
    new_value: Optional[str] = None
    suggested_fix: Optional[str] = None
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'rule_id': self.rule_id,
            'risk_level': self.risk_level.value,
            'description': self.description,
            'file_path': self.file_path,
            'line_number': self.line_number,
            'old_value': self.old_value,
            'new_value': self.new_value,
            'suggested_fix': self.suggested_fix,
            'metadata': self.metadata,
        }


@dataclass
class SentinelRule:
    """Rule for detecting drift."""

    rule_id: str
    name: str
    description: str
    risk_level: RiskLevel
    check_function: Callable
    enabled: bool = True
    metadata: Dict[str, Any] = field(default_factory=dict)


class DriftDetector:
    """Detect drift in various aspects of codebase."""

    def __init__(self, repo_root: str):
        """Initialize drift detector.

        Args:
            repo_root: Root directory of repository
        """
        self.repo_root = Path(repo_root)

    def detect_schema_drift(
        self,
        old_schemas: List[ExtractedSchema],
        new_schemas: List[ExtractedSchema],
    ) -> List[DriftDetection]:
        """Detect drift between old and new schemas.

        Args:
            old_schemas: Previous schemas
            new_schemas: Current schemas

        Returns:
            List of detected drifts
        """
        detections = []

        # Build indexes
        old_entities = {}
        new_entities = {}

        for schema in old_schemas:
            for entity in schema.entities:
                old_entities[entity] = schema

        for schema in new_schemas:
            for entity in schema.entities:
                new_entities[entity] = schema

        # Detect removed entities
        removed = set(old_entities.keys()) - set(new_entities.keys())
        for entity in removed:
            schema = old_entities[entity]
            detections.append(DriftDetection(
                rule_id='schema_entity_removed',
                risk_level=RiskLevel.HIGH,
                description=f"Entity '{entity}' removed from schema",
                file_path=schema.file_path,
                old_value=entity,
                suggested_fix=f"Restore '{entity}' or provide migration path",
            ))

        # Detect added entities (informational)
        added = set(new_entities.keys()) - set(old_entities.keys())
        for entity in added:
            schema = new_entities[entity]
            detections.append(DriftDetection(
                rule_id='schema_entity_added',
                risk_level=RiskLevel.LOW,
                description=f"New entity '{entity}' added to schema",
                file_path=schema.file_path,
                new_value=entity,
            ))

        # Detect endpoints changes
        old_endpoints = {}
        new_endpoints = {}

        for schema in old_schemas:
            for endpoint in schema.endpoints:
                old_endpoints[endpoint] = schema

        for schema in new_schemas:
            for endpoint in schema.endpoints:
                new_endpoints[endpoint] = schema

        # Detect removed endpoints
        removed_endpoints = set(old_endpoints.keys()) - set(new_endpoints.keys())
        for endpoint in removed_endpoints:
            schema = old_endpoints[endpoint]
            detections.append(DriftDetection(
                rule_id='schema_endpoint_removed',
                risk_level=RiskLevel.CRITICAL,
                description=f"API endpoint '{endpoint}' removed",
                file_path=schema.file_path,
                old_value=endpoint,
                suggested_fix=f"Restore '{endpoint}' or document breaking change",
            ))

        return detections

    def detect_generated_file_edits(self, file_path: str, content: str) -> List[DriftDetection]:
        """Detect edits to generated files.

        Args:
            file_path: Path to file
            content: File content

        Returns:
            List of detections
        """
        detections = []

        # Markers for generated files
        generated_markers = [
            '@generated',
            'AUTO-GENERATED',
            'DO NOT EDIT',
            'Code generated by',
            'This file is automatically generated',
        ]

        # Check first 20 lines for markers
        lines = content.split('\n')[:20]
        for i, line in enumerate(lines):
            for marker in generated_markers:
                if marker in line:
                    detections.append(DriftDetection(
                        rule_id='generated_file_edit',
                        risk_level=RiskLevel.HIGH,
                        description=f"Generated file '{file_path}' may have been edited",
                        file_path=file_path,
                        line_number=i + 1,
                        suggested_fix="Regenerate file or move changes to non-generated code",
                    ))
                    return detections  # Only report once per file

        return detections

    def detect_type_mismatches(
        self,
        file_path: str,
        content: str,
        expected_types: Dict[str, str],
    ) -> List[DriftDetection]:
        """Detect type mismatches in code.

        Args:
            file_path: Path to file
            content: File content
            expected_types: Expected types from schema

        Returns:
            List of detections
        """
        detections = []

        # Simple pattern matching for Python type hints
        type_pattern = r'(\w+):\s*([A-Za-z_][\w\[\],\s]*)'

        for i, line in enumerate(content.split('\n')):
            matches = re.findall(type_pattern, line)
            for var_name, actual_type in matches:
                if var_name in expected_types:
                    expected = expected_types[var_name]
                    actual = actual_type.strip()

                    if expected != actual:
                        detections.append(DriftDetection(
                            rule_id='type_mismatch',
                            risk_level=RiskLevel.MEDIUM,
                            description=f"Type mismatch for '{var_name}'",
                            file_path=file_path,
                            line_number=i + 1,
                            old_value=expected,
                            new_value=actual,
                            suggested_fix=f"Update type to '{expected}' or update schema",
                        ))

        return detections

    def detect_route_changes(
        self,
        file_path: str,
        content: str,
        known_routes: Set[str],
    ) -> List[DriftDetection]:
        """Detect route/endpoint changes.

        Args:
            file_path: Path to file
            content: File content
            known_routes: Set of known routes from schema

        Returns:
            List of detections
        """
        detections = []

        # Pattern for FastAPI routes
        route_patterns = [
            r'@router\.(get|post|put|delete|patch)\(["\']([^"\']+)["\']',
            r'@app\.(get|post|put|delete|patch)\(["\']([^"\']+)["\']',
        ]

        for i, line in enumerate(content.split('\n')):
            for pattern in route_patterns:
                matches = re.findall(pattern, line)
                for method, route in matches:
                    if route not in known_routes:
                        detections.append(DriftDetection(
                            rule_id='route_not_in_schema',
                            risk_level=RiskLevel.HIGH,
                            description=f"Route '{route}' not found in API schema",
                            file_path=file_path,
                            line_number=i + 1,
                            new_value=f"{method.upper()} {route}",
                            suggested_fix=f"Add '{route}' to API specification",
                        ))

        return detections


class PreflightCheck:
    """Preflight checks before operations."""

    def __init__(self, repo_root: str):
        """Initialize preflight checker.

        Args:
            repo_root: Root directory of repository
        """
        self.repo_root = repo_root
        self.detector = DriftDetector(repo_root)
        self.rules: List[SentinelRule] = []
        self._register_default_rules()

    def _register_default_rules(self):
        """Register default sentinel rules."""

        # Rule: Check for generated file edits
        self.rules.append(SentinelRule(
            rule_id='no_generated_edits',
            name='No Generated File Edits',
            description='Prevent edits to auto-generated files',
            risk_level=RiskLevel.HIGH,
            check_function=self._check_generated_files,
        ))

        # Rule: Check for breaking schema changes
        self.rules.append(SentinelRule(
            rule_id='no_breaking_schema',
            name='No Breaking Schema Changes',
            description='Prevent breaking changes to API schemas',
            risk_level=RiskLevel.CRITICAL,
            check_function=self._check_schema_changes,
        ))

        # Rule: Check for route drift
        self.rules.append(SentinelRule(
            rule_id='no_route_drift',
            name='No Route Drift',
            description='Ensure routes match API specification',
            risk_level=RiskLevel.HIGH,
            check_function=self._check_route_drift,
        ))

    def _check_generated_files(self, context: Dict[str, Any]) -> List[DriftDetection]:
        """Check for generated file edits."""
        detections = []

        files_to_check = context.get('files', [])
        for file_info in files_to_check:
            file_path = file_info.get('path', '')
            content = file_info.get('content', '')

            detections.extend(
                self.detector.detect_generated_file_edits(file_path, content)
            )

        return detections

    def _check_schema_changes(self, context: Dict[str, Any]) -> List[DriftDetection]:
        """Check for breaking schema changes."""
        old_schemas = context.get('old_schemas', [])
        new_schemas = context.get('new_schemas', [])

        if not old_schemas or not new_schemas:
            return []

        return self.detector.detect_schema_drift(old_schemas, new_schemas)

    def _check_route_drift(self, context: Dict[str, Any]) -> List[DriftDetection]:
        """Check for route drift."""
        detections = []

        known_routes = context.get('known_routes', set())
        files_to_check = context.get('files', [])

        for file_info in files_to_check:
            file_path = file_info.get('path', '')
            content = file_info.get('content', '')

            # Only check route files
            if 'route' in file_path.lower() or 'api' in file_path.lower():
                detections.extend(
                    self.detector.detect_route_changes(file_path, content, known_routes)
                )

        return detections

    def run_checks(self, context: Dict[str, Any]) -> Dict[str, Any]:
        """Run all preflight checks.

        Args:
            context: Context dictionary with files, schemas, etc.

        Returns:
            Dictionary with check results
        """
        all_detections = []
        rule_results = {}

        for rule in self.rules:
            if not rule.enabled:
                continue

            try:
                detections = rule.check_function(context)
                all_detections.extend(detections)

                rule_results[rule.rule_id] = {
                    'passed': len(detections) == 0,
                    'detections': len(detections),
                    'risk_level': rule.risk_level.value,
                }

                logger.debug(
                    "Sentinel rule checked",
                    rule=rule.rule_id,
                    detections=len(detections),
                )

            except Exception as e:
                logger.error("Sentinel rule failed", rule=rule.rule_id, error=str(e))
                rule_results[rule.rule_id] = {
                    'passed': False,
                    'error': str(e),
                }

        # Calculate overall risk
        max_risk = RiskLevel.LOW
        for detection in all_detections:
            if detection.risk_level == RiskLevel.CRITICAL:
                max_risk = RiskLevel.CRITICAL
                break
            elif detection.risk_level == RiskLevel.HIGH:
                max_risk = RiskLevel.HIGH
            elif detection.risk_level == RiskLevel.MEDIUM and max_risk == RiskLevel.LOW:
                max_risk = RiskLevel.MEDIUM

        passed = len(all_detections) == 0
        blocked = max_risk in [RiskLevel.CRITICAL, RiskLevel.HIGH]

        logger.info(
            "Preflight checks complete",
            total_detections=len(all_detections),
            max_risk=max_risk.value,
            passed=passed,
            blocked=blocked,
        )

        return {
            'passed': passed,
            'blocked': blocked,
            'max_risk': max_risk.value,
            'total_detections': len(all_detections),
            'detections': [d.to_dict() for d in all_detections],
            'rules': rule_results,
            'timestamp': datetime.utcnow().isoformat(),
        }

    def add_rule(self, rule: SentinelRule):
        """Add a custom sentinel rule.

        Args:
            rule: Sentinel rule to add
        """
        self.rules.append(rule)
        logger.info("Added sentinel rule", rule_id=rule.rule_id)

    def disable_rule(self, rule_id: str):
        """Disable a sentinel rule.

        Args:
            rule_id: ID of rule to disable
        """
        for rule in self.rules:
            if rule.rule_id == rule_id:
                rule.enabled = False
                logger.info("Disabled sentinel rule", rule_id=rule_id)
                return

        logger.warning("Rule not found", rule_id=rule_id)


class DriftSentinel:
    """High-level sentinel for monitoring drift."""

    def __init__(self, repo_root: str):
        """Initialize drift sentinel.

        Args:
            repo_root: Root directory of repository
        """
        self.repo_root = repo_root
        self.preflight = PreflightCheck(repo_root)
        self.schema_extractor = SchemaExtractor(repo_root)
        self._baseline_schemas: Optional[List[ExtractedSchema]] = None

    def set_baseline(self):
        """Set current state as baseline for drift detection."""
        logger.info("Setting baseline schemas")
        self._baseline_schemas = self.schema_extractor.extract_all()
        logger.info("Baseline set", schemas=len(self._baseline_schemas))

    def check_drift(self, files: List[Dict[str, str]]) -> Dict[str, Any]:
        """Check for drift in modified files.

        Args:
            files: List of file dicts with 'path' and 'content'

        Returns:
            Check results
        """
        # Extract current schemas
        current_schemas = self.schema_extractor.extract_all()

        # Get known routes
        known_routes = set()
        for schema in current_schemas:
            known_routes.update(schema.endpoints)

        # Build context
        context = {
            'files': files,
            'old_schemas': self._baseline_schemas or [],
            'new_schemas': current_schemas,
            'known_routes': known_routes,
        }

        # Run preflight checks
        results = self.preflight.run_checks(context)

        return results

    def targeted_fetch(self, detections: List[DriftDetection]) -> Dict[str, Any]:
        """Perform targeted fetch for high-risk detections.

        Instead of full re-index, fetch only affected files/schemas.

        Args:
            detections: List of drift detections

        Returns:
            Fetch results
        """
        files_to_fetch = set()
        schemas_to_update = set()

        for detection in detections:
            if detection.risk_level in [RiskLevel.HIGH, RiskLevel.CRITICAL]:
                files_to_fetch.add(detection.file_path)

                # Determine which schemas need updating
                if 'schema' in detection.rule_id:
                    schemas_to_update.add(detection.file_path)

        logger.info(
            "Targeted fetch initiated",
            files=len(files_to_fetch),
            schemas=len(schemas_to_update),
        )

        return {
            'files_to_fetch': list(files_to_fetch),
            'schemas_to_update': list(schemas_to_update),
            'full_reindex_needed': len(schemas_to_update) > 10,  # Threshold
        }
