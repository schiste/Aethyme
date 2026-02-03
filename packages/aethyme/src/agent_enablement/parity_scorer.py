"""
Parity Scorer

Calculates agent-friendliness parity scores for repositories.
Scans for invariant compliance and provides actionable metrics.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import json
import logging
from dataclasses import dataclass, field, asdict
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any

from .invariants import InvariantRulesEngine, InvariantViolation, InvariantSeverity

logger = logging.getLogger(__name__)


@dataclass
class InvariantScore:
    """Score for a single invariant"""
    invariant_id: str
    invariant_name: str
    score: float  # 0-100
    total_checks: int
    violations: int
    gaps: int
    severity_breakdown: Dict[str, int] = field(default_factory=dict)
    can_autofix: bool = False


@dataclass
class ParityReport:
    """Complete parity report for a repository"""
    repo_path: str
    timestamp: str
    overall_score: float  # 0-100
    invariants: Dict[str, InvariantScore] = field(default_factory=dict)
    total_violations: int = 0
    actionable_fixes: int = 0
    estimated_fix_time: str = ""
    trend: Optional[Dict[str, float]] = None
    baseline_comparison: Optional[Dict[str, float]] = None
    recommendations: List[str] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization"""
        return asdict(self)

    def to_json(self, indent: int = 2) -> str:
        """Convert to JSON string"""
        return json.dumps(self.to_dict(), indent=indent)


class ParityScorer:
    """
    Calculates agent-friendliness parity scores.

    Scoring algorithm:
    - Each invariant contributes equally to overall score
    - Score = (total_checks - violations) / total_checks * 100
    - Weighted by severity: blocker = 1.0, warning = 0.5, info = 0.2
    - Overall score is weighted average of all invariant scores
    """

    def __init__(self, rules_engine: Optional[InvariantRulesEngine] = None):
        self.rules_engine = rules_engine or InvariantRulesEngine()
        self.severity_weights = {
            InvariantSeverity.BLOCKER: 1.0,
            InvariantSeverity.WARNING: 0.5,
            InvariantSeverity.INFO: 0.2,
        }

    def scan_repository(
        self,
        repo_path: Path,
        baseline: Optional[Dict[str, float]] = None,
        previous_report: Optional[ParityReport] = None
    ) -> ParityReport:
        """
        Scan repository and generate parity report.

        Args:
            repo_path: Path to repository root
            baseline: Optional baseline scores for comparison
            previous_report: Optional previous report for trend analysis

        Returns:
            Complete parity report
        """
        logger.info(f"Scanning repository: {repo_path}")

        # Run all detectors
        violation_results = self.rules_engine.run_all_detectors(repo_path)

        # Calculate scores for each invariant
        invariant_scores = {}
        total_violations = 0
        actionable_fixes = 0

        for rule_id, violations in violation_results.items():
            rule = self.rules_engine.get_rule(rule_id)
            if not rule:
                continue

            score = self._calculate_invariant_score(
                rule_id=rule_id,
                violations=violations,
                repo_path=repo_path
            )

            invariant_scores[rule_id] = score
            total_violations += score.violations
            if rule.can_autofix:
                actionable_fixes += score.violations

        # Calculate overall score
        overall_score = self._calculate_overall_score(invariant_scores)

        # Generate trend analysis
        trend = None
        if previous_report:
            trend = self._calculate_trend(invariant_scores, previous_report)

        # Generate baseline comparison
        baseline_comparison = None
        if baseline:
            baseline_comparison = self._compare_to_baseline(invariant_scores, baseline)

        # Estimate fix time
        estimated_time = self._estimate_fix_time(total_violations, actionable_fixes)

        # Generate recommendations
        recommendations = self._generate_recommendations(invariant_scores, overall_score)

        report = ParityReport(
            repo_path=str(repo_path),
            timestamp=datetime.utcnow().isoformat() + "Z",
            overall_score=overall_score,
            invariants=invariant_scores,
            total_violations=total_violations,
            actionable_fixes=actionable_fixes,
            estimated_fix_time=estimated_time,
            trend=trend,
            baseline_comparison=baseline_comparison,
            recommendations=recommendations
        )

        logger.info(f"Scan complete. Overall score: {overall_score:.1f}/100")
        return report

    def _calculate_invariant_score(
        self,
        rule_id: str,
        violations: List[InvariantViolation],
        repo_path: Path
    ) -> InvariantScore:
        """Calculate score for a single invariant"""
        rule = self.rules_engine.get_rule(rule_id)

        # Count total potential checks (heuristic based on file count)
        total_checks = self._estimate_total_checks(rule_id, repo_path)

        # Count violations by severity
        severity_breakdown = {
            "blocker": 0,
            "warn": 0,
            "info": 0
        }

        weighted_violations = 0.0
        for violation in violations:
            severity_breakdown[violation.severity.value] += 1
            weight = self.severity_weights.get(violation.severity, 1.0)
            weighted_violations += weight

        # Calculate score (weighted)
        if total_checks > 0:
            # Adjust total checks by average weight
            avg_weight = sum(self.severity_weights.values()) / len(self.severity_weights)
            weighted_total = total_checks * avg_weight
            score = max(0, (weighted_total - weighted_violations) / weighted_total * 100)
        else:
            score = 100.0  # No checks needed = perfect score

        return InvariantScore(
            invariant_id=rule_id,
            invariant_name=rule.name,
            score=round(score, 2),
            total_checks=total_checks,
            violations=len(violations),
            gaps=len(violations),  # Gaps = violations
            severity_breakdown=severity_breakdown,
            can_autofix=rule.can_autofix
        )

    def _estimate_total_checks(self, rule_id: str, repo_path: Path) -> int:
        """Estimate total number of checks for an invariant"""

        # Heuristics for different invariants
        if rule_id == "data-ui-selectors":
            # Count interactive elements in React files
            count = 0
            for pattern in ["**/*.tsx", "**/*.jsx"]:
                for file_path in repo_path.glob(pattern):
                    try:
                        content = file_path.read_text()
                        # Rough count of interactive elements
                        count += content.count("<button")
                        count += content.count("<input")
                        count += content.count("<a ")
                        count += content.count("<select")
                    except:
                        pass
            return max(count, 1)

        elif rule_id == "generated-file-protection":
            # Count generated files
            count = 0
            for pattern in ["**/generated/**/*", "**/*.generated.*"]:
                count += len(list(repo_path.glob(pattern)))
            return max(count, 1)

        elif rule_id == "relative-links":
            # Count markdown links
            count = 0
            for file_path in repo_path.glob("**/*.md"):
                try:
                    content = file_path.read_text()
                    count += content.count("](")
                except:
                    pass
            return max(count, 1)

        elif rule_id == "i18n-coverage":
            # Count UI strings
            count = 0
            for pattern in ["**/*.tsx", "**/*.jsx"]:
                for file_path in repo_path.glob(pattern):
                    try:
                        content = file_path.read_text()
                        # Rough heuristic for text content
                        count += content.count(">") // 2
                    except:
                        pass
            return max(count, 10)

        elif rule_id == "folder-docs":
            # Count directories with code
            dirs = set()
            for pattern in ["**/*.py", "**/*.ts", "**/*.tsx"]:
                for file_path in repo_path.glob(pattern):
                    dirs.add(file_path.parent)
            return len(dirs)

        elif rule_id == "index-files":
            # Count module directories
            dirs = set()
            for pattern in ["**/*.py", "**/*.ts"]:
                for file_path in repo_path.glob(pattern):
                    dirs.add(file_path.parent)
            return max(len(dirs), 1)

        elif rule_id in ["config-driven-routes", "onboarding-prompts", "discovery-hooks"]:
            # Binary checks (either exists or doesn't)
            return 1

        return 10  # Default

    def _calculate_overall_score(self, invariant_scores: Dict[str, InvariantScore]) -> float:
        """Calculate weighted overall score"""
        if not invariant_scores:
            return 0.0

        # Weight by severity of violations
        total_weight = 0.0
        weighted_sum = 0.0

        for score in invariant_scores.values():
            # More violations = higher weight in overall score
            weight = 1.0 + (score.violations / max(score.total_checks, 1))
            total_weight += weight
            weighted_sum += score.score * weight

        overall = weighted_sum / total_weight if total_weight > 0 else 0.0
        return round(overall, 2)

    def _calculate_trend(
        self,
        current_scores: Dict[str, InvariantScore],
        previous_report: ParityReport
    ) -> Dict[str, float]:
        """Calculate score trends compared to previous report"""
        trend = {}

        for rule_id, current_score in current_scores.items():
            if rule_id in previous_report.invariants:
                prev_score = previous_report.invariants[rule_id].score
                delta = current_score.score - prev_score
                trend[rule_id] = round(delta, 2)

        # Overall trend
        if previous_report.overall_score:
            overall_delta = sum(current_scores.values(), key=lambda s: s.score) / len(current_scores)
            overall_delta -= previous_report.overall_score
            trend["overall"] = round(overall_delta, 2)

        return trend

    def _compare_to_baseline(
        self,
        current_scores: Dict[str, InvariantScore],
        baseline: Dict[str, float]
    ) -> Dict[str, float]:
        """Compare scores to baseline (e.g., exemplar repo)"""
        comparison = {}

        for rule_id, current_score in current_scores.items():
            if rule_id in baseline:
                delta = current_score.score - baseline[rule_id]
                comparison[rule_id] = round(delta, 2)

        return comparison

    def _estimate_fix_time(self, total_violations: int, actionable_fixes: int) -> str:
        """Estimate time to fix violations"""
        # Heuristic: 2 minutes per autofix, 10 minutes per manual fix
        autofix_time = actionable_fixes * 2
        manual_time = (total_violations - actionable_fixes) * 10

        total_minutes = autofix_time + manual_time

        if total_minutes < 60:
            return f"{total_minutes} minutes"
        else:
            hours = total_minutes // 60
            minutes = total_minutes % 60
            if minutes > 0:
                return f"{hours}h {minutes}m"
            return f"{hours} hours"

    def _generate_recommendations(
        self,
        invariant_scores: Dict[str, InvariantScore],
        overall_score: float
    ) -> List[str]:
        """Generate actionable recommendations"""
        recommendations = []

        # Priority: Fix blockers first
        blockers = []
        warnings = []

        for rule_id, score in invariant_scores.items():
            if score.severity_breakdown.get("blocker", 0) > 0:
                blockers.append((rule_id, score))
            elif score.violations > 0:
                warnings.append((rule_id, score))

        # Sort by number of violations
        blockers.sort(key=lambda x: x[1].violations, reverse=True)
        warnings.sort(key=lambda x: x[1].violations, reverse=True)

        # Generate recommendations
        if overall_score >= 90:
            recommendations.append("🎉 Excellent! Repository is highly agent-friendly.")
        elif overall_score >= 80:
            recommendations.append("✅ Good agent readiness. Minor improvements recommended.")
        elif overall_score >= 60:
            recommendations.append("⚠️  Moderate agent readiness. Several improvements needed.")
        else:
            recommendations.append("❌ Low agent readiness. Significant work required.")

        # Blocker recommendations
        if blockers:
            recommendations.append(f"\n🚨 BLOCKERS ({len(blockers)} invariants):")
            for rule_id, score in blockers[:3]:  # Top 3
                fix_type = "autofixable" if score.can_autofix else "manual fix required"
                recommendations.append(
                    f"  • {score.invariant_name}: {score.violations} violations ({fix_type})"
                )

        # Warning recommendations
        if warnings:
            recommendations.append(f"\n⚠️  WARNINGS ({len(warnings)} invariants):")
            for rule_id, score in warnings[:3]:  # Top 3
                recommendations.append(
                    f"  • {score.invariant_name}: {score.violations} issues"
                )

        # Quick wins (autofixable)
        autofix_count = sum(1 for s in invariant_scores.values() if s.can_autofix and s.violations > 0)
        if autofix_count > 0:
            recommendations.append(
                f"\n💡 QUICK WINS: {autofix_count} invariants can be auto-fixed. "
                f"Run: aethyme agent autofix --safe"
            )

        return recommendations

    def calculate_parity_score_breakdown(self, report: ParityReport) -> Dict[str, Any]:
        """Calculate detailed breakdown of parity score"""
        breakdown = {
            "overall": {
                "score": report.overall_score,
                "grade": self._score_to_grade(report.overall_score),
                "status": self._score_to_status(report.overall_score)
            },
            "by_category": {},
            "by_severity": {
                "blocker": 0,
                "warning": 0,
                "info": 0
            },
            "fixability": {
                "autofixable": 0,
                "manual": 0,
                "percentage_autofixable": 0
            }
        }

        # Group by category
        for score in report.invariants.values():
            rule = self.rules_engine.get_rule(score.invariant_id)
            if rule:
                category = rule.category
                if category not in breakdown["by_category"]:
                    breakdown["by_category"][category] = {
                        "score": 0,
                        "violations": 0,
                        "count": 0
                    }
                breakdown["by_category"][category]["score"] += score.score
                breakdown["by_category"][category]["violations"] += score.violations
                breakdown["by_category"][category]["count"] += 1

            # Severity breakdown
            for severity, count in score.severity_breakdown.items():
                breakdown["by_severity"][severity] += count

            # Fixability
            if score.can_autofix:
                breakdown["fixability"]["autofixable"] += score.violations
            else:
                breakdown["fixability"]["manual"] += score.violations

        # Average category scores
        for category in breakdown["by_category"].values():
            if category["count"] > 0:
                category["score"] = round(category["score"] / category["count"], 2)

        # Calculate autofix percentage
        total_violations = report.total_violations
        if total_violations > 0:
            pct = (breakdown["fixability"]["autofixable"] / total_violations) * 100
            breakdown["fixability"]["percentage_autofixable"] = round(pct, 1)

        return breakdown

    def _score_to_grade(self, score: float) -> str:
        """Convert score to letter grade"""
        if score >= 90:
            return "A"
        elif score >= 80:
            return "B"
        elif score >= 70:
            return "C"
        elif score >= 60:
            return "D"
        else:
            return "F"

    def _score_to_status(self, score: float) -> str:
        """Convert score to status"""
        if score >= 80:
            return "ready"
        elif score >= 60:
            return "needs-improvement"
        else:
            return "not-ready"

    def save_report(self, report: ParityReport, output_path: Path) -> None:
        """Save report to file"""
        output_path.parent.mkdir(parents=True, exist_ok=True)

        if output_path.suffix == ".json":
            output_path.write_text(report.to_json())
        elif output_path.suffix == ".md":
            markdown = self._report_to_markdown(report)
            output_path.write_text(markdown)
        else:
            raise ValueError(f"Unsupported output format: {output_path.suffix}")

        logger.info(f"Report saved to: {output_path}")

    def _report_to_markdown(self, report: ParityReport) -> str:
        """Convert report to markdown format"""
        lines = [
            f"# Agent-Friendliness Parity Report",
            f"",
            f"**Repository:** `{report.repo_path}`  ",
            f"**Scan Date:** {report.timestamp}  ",
            f"**Overall Score:** {report.overall_score}/100  ",
            f"",
            f"## Summary",
            f"",
            f"- **Total Violations:** {report.total_violations}",
            f"- **Actionable Fixes:** {report.actionable_fixes}",
            f"- **Estimated Fix Time:** {report.estimated_fix_time}",
            f"",
            f"## Recommendations",
            f"",
        ]

        for rec in report.recommendations:
            lines.append(rec)

        lines.extend([
            f"",
            f"## Invariant Scores",
            f"",
            f"| Invariant | Score | Violations | Autofixable |",
            f"|-----------|-------|------------|-------------|",
        ])

        for invariant_id, score in report.invariants.items():
            autofix = "✅" if score.can_autofix else "❌"
            lines.append(
                f"| {score.invariant_name} | {score.score}/100 | {score.violations} | {autofix} |"
            )

        lines.append("")
        return "\n".join(lines)
