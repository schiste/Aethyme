"""
Gap Detector

Detects gaps in connected repositories and prioritizes them by impact.
Maps gaps to specific files and line numbers with fix recommendations.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Any
from enum import Enum

from .invariants import InvariantViolation, InvariantSeverity, InvariantRulesEngine
from .parity_scorer import ParityReport

logger = logging.getLogger(__name__)


class GapPriority(Enum):
    """Priority levels for gaps"""
    CRITICAL = "critical"  # Blocks agent work
    HIGH = "high"          # Significantly impacts agent
    MEDIUM = "medium"      # Moderate impact
    LOW = "low"            # Minor inconvenience


@dataclass
class GapFix:
    """Autofix recommendation for a gap"""
    fix_id: str
    description: str
    autofix_available: bool
    estimated_effort: str  # e.g., "2 minutes", "10 minutes"
    command: Optional[str] = None  # CLI command to run autofix
    dependencies: List[str] = field(default_factory=list)  # Other fixes needed first


@dataclass
class DetectedGap:
    """Represents a detected gap with prioritization"""
    gap_id: str
    invariant_id: str
    invariant_name: str
    file_path: str
    line_number: Optional[int]
    severity: InvariantSeverity
    priority: GapPriority
    impact_score: float  # 0-10 scale
    message: str
    fix: Optional[GapFix] = None
    related_gaps: List[str] = field(default_factory=list)
    context: Dict[str, Any] = field(default_factory=dict)


class GapDetector:
    """
    Detects and prioritizes gaps in repository agent-friendliness.

    Prioritization algorithm:
    - Severity: blocker > warning > info
    - Impact: Number of files affected
    - Fix availability: Autofixable gaps get higher priority
    - Dependencies: Gaps blocking other gaps get higher priority
    """

    def __init__(self, rules_engine: Optional[InvariantRulesEngine] = None):
        self.rules_engine = rules_engine or InvariantRulesEngine()

    def detect_gaps(
        self,
        repo_path: Path,
        parity_report: Optional[ParityReport] = None
    ) -> List[DetectedGap]:
        """
        Detect all gaps in repository.

        Args:
            repo_path: Repository path
            parity_report: Optional pre-computed parity report

        Returns:
            List of detected gaps, sorted by priority
        """
        logger.info(f"Detecting gaps in: {repo_path}")

        # Run detectors if no report provided
        if not parity_report:
            violations_by_invariant = self.rules_engine.run_all_detectors(repo_path)
        else:
            # Extract violations from report
            violations_by_invariant = {}
            for invariant_id, score in parity_report.invariants.items():
                # Re-run detector to get violation details
                rule = self.rules_engine.get_rule(invariant_id)
                if rule and score.violations > 0:
                    violations_by_invariant[invariant_id] = rule.detector(repo_path)

        # Convert violations to gaps with prioritization
        gaps = []
        gap_id_counter = 1

        for invariant_id, violations in violations_by_invariant.items():
            rule = self.rules_engine.get_rule(invariant_id)
            if not rule:
                continue

            for violation in violations:
                gap = self._violation_to_gap(
                    gap_id=f"GAP-{gap_id_counter:04d}",
                    violation=violation,
                    rule=rule,
                    repo_path=repo_path
                )
                gaps.append(gap)
                gap_id_counter += 1

        # Analyze impact and prioritize
        gaps = self._prioritize_gaps(gaps, repo_path)

        # Detect dependencies
        gaps = self._detect_dependencies(gaps)

        logger.info(f"Detected {len(gaps)} gaps")
        return gaps

    def _violation_to_gap(
        self,
        gap_id: str,
        violation: InvariantViolation,
        rule,
        repo_path: Path
    ) -> DetectedGap:
        """Convert invariant violation to detected gap"""

        # Calculate impact score
        impact_score = self._calculate_impact(violation, rule, repo_path)

        # Determine priority
        priority = self._determine_priority(violation.severity, impact_score)

        # Generate fix recommendation
        fix = None
        if rule.can_autofix:
            fix = self._generate_fix(violation, rule)

        gap = DetectedGap(
            gap_id=gap_id,
            invariant_id=violation.invariant_id,
            invariant_name=rule.name,
            file_path=violation.file_path,
            line_number=violation.line_number,
            severity=violation.severity,
            priority=priority,
            impact_score=impact_score,
            message=violation.message,
            fix=fix,
            context=violation.context
        )

        return gap

    def _calculate_impact(self, violation: InvariantViolation, rule, repo_path: Path) -> float:
        """
        Calculate impact score (0-10) for a violation.

        Factors:
        - Severity (blocker=10, warning=5, info=2)
        - File importance (more imports = higher impact)
        - Pattern frequency (how often this pattern appears)
        """
        impact = 0.0

        # Base impact from severity
        severity_impact = {
            InvariantSeverity.BLOCKER: 10.0,
            InvariantSeverity.WARNING: 5.0,
            InvariantSeverity.INFO: 2.0
        }
        impact += severity_impact.get(violation.severity, 2.0)

        # File importance (heuristic based on location)
        file_path = violation.file_path.lower()
        if "index" in file_path or "main" in file_path or "app" in file_path:
            impact += 2.0
        if "config" in file_path or "setup" in file_path:
            impact += 1.5
        if "test" in file_path:
            impact -= 1.0  # Tests are lower priority

        # Normalize to 0-10
        impact = min(10.0, max(0.0, impact))

        return round(impact, 2)

    def _determine_priority(self, severity: InvariantSeverity, impact_score: float) -> GapPriority:
        """Determine gap priority based on severity and impact"""

        if severity == InvariantSeverity.BLOCKER:
            if impact_score >= 8.0:
                return GapPriority.CRITICAL
            else:
                return GapPriority.HIGH

        elif severity == InvariantSeverity.WARNING:
            if impact_score >= 7.0:
                return GapPriority.HIGH
            elif impact_score >= 4.0:
                return GapPriority.MEDIUM
            else:
                return GapPriority.LOW

        else:  # INFO
            if impact_score >= 5.0:
                return GapPriority.MEDIUM
            else:
                return GapPriority.LOW

    def _generate_fix(self, violation: InvariantViolation, rule) -> GapFix:
        """Generate fix recommendation for a violation"""

        # Map invariants to fix commands
        fix_commands = {
            "data-ui-selectors": "repograph autofix data-ui --file {file}",
            "generated-file-protection": "repograph autofix generated-markers --file {file}",
            "relative-links": "repograph autofix links --file {file}",
            "i18n-coverage": "repograph autofix i18n --file {file}",
            "folder-docs": "repograph autofix folder-docs --dir {dir}",
            "index-files": "repograph autofix index-files --dir {dir}",
            "onboarding-prompts": "repograph agent manifest",
            "discovery-hooks": "repograph init",
        }

        command = fix_commands.get(violation.invariant_id, "")
        if command:
            if "{file}" in command:
                command = command.replace("{file}", violation.file_path)
            elif "{dir}" in command:
                dir_path = str(Path(violation.file_path).parent)
                command = command.replace("{dir}", dir_path)

        # Estimate effort
        effort_map = {
            "data-ui-selectors": "2 minutes per element",
            "generated-file-protection": "1 minute",
            "relative-links": "1 minute per link",
            "i18n-coverage": "5 minutes per string",
            "folder-docs": "10 minutes per directory",
            "index-files": "5 minutes per directory",
            "config-driven-routes": "1-2 hours (architectural)",
            "onboarding-prompts": "30 minutes",
            "discovery-hooks": "5 minutes",
        }

        effort = effort_map.get(violation.invariant_id, "varies")

        fix = GapFix(
            fix_id=f"FIX-{violation.invariant_id}",
            description=violation.suggested_fix or "Apply autofix",
            autofix_available=rule.can_autofix,
            estimated_effort=effort,
            command=command if command else None
        )

        return fix

    def _prioritize_gaps(self, gaps: List[DetectedGap], repo_path: Path) -> List[DetectedGap]:
        """Prioritize gaps by impact and severity"""

        # Sort by: priority level, then impact score, then severity
        priority_order = {
            GapPriority.CRITICAL: 0,
            GapPriority.HIGH: 1,
            GapPriority.MEDIUM: 2,
            GapPriority.LOW: 3
        }

        gaps.sort(
            key=lambda g: (
                priority_order[g.priority],
                -g.impact_score,  # Higher impact first
                g.severity != InvariantSeverity.BLOCKER  # Blockers first
            )
        )

        return gaps

    def _detect_dependencies(self, gaps: List[DetectedGap]) -> List[DetectedGap]:
        """Detect dependencies between gaps (which must be fixed first)"""

        # Build dependency graph
        # Example: config-driven-routes must be fixed before route additions
        dependency_rules = {
            "onboarding-prompts": [],  # No dependencies
            "discovery-hooks": [],
            "config-driven-routes": [],
            "folder-docs": [],
            "index-files": ["folder-docs"],  # Better to have docs first
            "data-ui-selectors": ["config-driven-routes"],  # Need routes defined
            "i18n-coverage": ["config-driven-routes"],
            "relative-links": ["folder-docs"],
            "generated-file-protection": [],
        }

        # Add dependencies to fixes
        for gap in gaps:
            if gap.fix and gap.invariant_id in dependency_rules:
                deps = dependency_rules[gap.invariant_id]
                gap.fix.dependencies = deps

        return gaps

    def get_gaps_by_priority(self, gaps: List[DetectedGap]) -> Dict[GapPriority, List[DetectedGap]]:
        """Group gaps by priority level"""
        grouped = {priority: [] for priority in GapPriority}

        for gap in gaps:
            grouped[gap.priority].append(gap)

        return grouped

    def get_autofixable_gaps(self, gaps: List[DetectedGap]) -> List[DetectedGap]:
        """Get only gaps that can be auto-fixed"""
        return [gap for gap in gaps if gap.fix and gap.fix.autofix_available]

    def get_fix_sequence(self, gaps: List[DetectedGap]) -> List[List[DetectedGap]]:
        """
        Get gaps in dependency-resolved fix sequence.

        Returns list of lists, where each inner list can be fixed in parallel.
        """
        # Build dependency graph
        gap_by_invariant = {}
        for gap in gaps:
            if gap.invariant_id not in gap_by_invariant:
                gap_by_invariant[gap.invariant_id] = []
            gap_by_invariant[gap.invariant_id].append(gap)

        # Topological sort by dependencies
        sequence = []
        fixed = set()

        while len(fixed) < len(gap_by_invariant):
            # Find invariants with no unfixed dependencies
            batch = []

            for invariant_id, inv_gaps in gap_by_invariant.items():
                if invariant_id in fixed:
                    continue

                # Check if dependencies are met
                dependencies = inv_gaps[0].fix.dependencies if inv_gaps[0].fix else []
                if all(dep in fixed for dep in dependencies):
                    batch.extend(inv_gaps)
                    fixed.add(invariant_id)

            if batch:
                sequence.append(batch)
            else:
                # Circular dependency or error - add remaining
                remaining = []
                for invariant_id, inv_gaps in gap_by_invariant.items():
                    if invariant_id not in fixed:
                        remaining.extend(inv_gaps)
                if remaining:
                    sequence.append(remaining)
                break

        return sequence

    def generate_gap_report(self, gaps: List[DetectedGap]) -> Dict[str, Any]:
        """Generate comprehensive gap report"""

        by_priority = self.get_gaps_by_priority(gaps)
        autofixable = self.get_autofixable_gaps(gaps)
        fix_sequence = self.get_fix_sequence(gaps)

        report = {
            "total_gaps": len(gaps),
            "by_priority": {
                "critical": len(by_priority[GapPriority.CRITICAL]),
                "high": len(by_priority[GapPriority.HIGH]),
                "medium": len(by_priority[GapPriority.MEDIUM]),
                "low": len(by_priority[GapPriority.LOW])
            },
            "autofixable": {
                "count": len(autofixable),
                "percentage": round(len(autofixable) / len(gaps) * 100, 1) if gaps else 0
            },
            "fix_batches": len(fix_sequence),
            "top_gaps": [
                {
                    "gap_id": gap.gap_id,
                    "invariant": gap.invariant_name,
                    "file": gap.file_path,
                    "line": gap.line_number,
                    "priority": gap.priority.value,
                    "impact": gap.impact_score,
                    "fix_command": gap.fix.command if gap.fix else None
                }
                for gap in gaps[:10]  # Top 10
            ],
            "recommendations": self._generate_gap_recommendations(gaps, by_priority, autofixable)
        }

        return report

    def _generate_gap_recommendations(
        self,
        gaps: List[DetectedGap],
        by_priority: Dict[GapPriority, List[DetectedGap]],
        autofixable: List[DetectedGap]
    ) -> List[str]:
        """Generate actionable recommendations from gaps"""
        recommendations = []

        critical_count = len(by_priority[GapPriority.CRITICAL])
        if critical_count > 0:
            recommendations.append(
                f"🚨 {critical_count} CRITICAL gaps detected - these block agent onboarding"
            )

        high_count = len(by_priority[GapPriority.HIGH])
        if high_count > 0:
            recommendations.append(
                f"⚠️  {high_count} HIGH priority gaps - should fix before agent work"
            )

        if autofixable:
            recommendations.append(
                f"💡 {len(autofixable)} gaps can be auto-fixed. "
                f"Run: repograph autofix --safe"
            )

        # Suggest fix order
        if critical_count > 0:
            critical_gaps = by_priority[GapPriority.CRITICAL]
            top_critical = critical_gaps[0]
            recommendations.append(
                f"📋 Start with: {top_critical.invariant_name} "
                f"({top_critical.file_path})"
            )

        return recommendations
