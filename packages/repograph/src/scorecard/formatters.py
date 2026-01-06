"""Formatters for scorecard reports."""

import json
from typing import Dict, Any
from datetime import datetime

from .models import ScorecardReport, Severity


class JSONFormatter:
    """Format scorecard report as JSON."""

    def format(self, report: ScorecardReport) -> str:
        """
        Format report as JSON.

        Args:
            report: ScorecardReport to format

        Returns:
            JSON string
        """
        data = self._build_json_structure(report)
        return json.dumps(data, indent=2, default=str)

    def _build_json_structure(self, report: ScorecardReport) -> Dict[str, Any]:
        """Build JSON structure from report."""
        return {
            "scan_id": report.scan_id,
            "repository": {
                "path": report.repository_path,
                "id": report.repository_id,
                "tenant_id": report.tenant_id,
            },
            "timestamp": report.timestamp.isoformat(),
            "score": report.score,
            "summary": {
                "total_findings": report.total_findings,
                "blockers": report.blocker_count,
                "warnings": report.warning_count,
                "info": report.info_count,
            },
            "findings": {
                "blockers": [self._finding_to_dict(f) for f in report.blockers],
                "warnings": [self._finding_to_dict(f) for f in report.warnings],
                "info": [self._finding_to_dict(f) for f in report.info],
            },
            "detectors": [
                {
                    "name": dr.detector_name,
                    "findings_count": len(dr.findings),
                    "execution_time_ms": dr.execution_time_ms,
                    "error": dr.error,
                }
                for dr in report.detector_results
            ],
            "performance": {
                "total_scan_time_ms": report.total_scan_time_ms,
                "files_scanned": report.files_scanned,
            },
        }

    def _finding_to_dict(self, finding) -> Dict[str, Any]:
        """Convert finding to dictionary."""
        return {
            "detector": finding.detector,
            "severity": finding.severity,
            "message": finding.message,
            "file": finding.file_path,
            "line": finding.line_number,
            "evidence": finding.evidence,
            "suggestion": finding.suggestion,
        }


class MarkdownFormatter:
    """Format scorecard report as Markdown."""

    def format(self, report: ScorecardReport) -> str:
        """
        Format report as Markdown.

        Args:
            report: ScorecardReport to format

        Returns:
            Markdown string
        """
        lines = []

        # Header
        lines.append("# AI-Readiness Scorecard Report")
        lines.append("")
        lines.append(f"**Scan ID:** `{report.scan_id}`")
        lines.append(f"**Repository:** `{report.repository_path}`")
        lines.append(f"**Timestamp:** {report.timestamp.strftime('%Y-%m-%d %H:%M:%S UTC')}")
        lines.append("")

        # Score
        score_emoji = self._get_score_emoji(report.score)
        lines.append(f"## Overall Score: {report.score}/100 {score_emoji}")
        lines.append("")

        # Summary
        lines.append("## Summary")
        lines.append("")
        lines.append(f"- **Total Findings:** {report.total_findings}")
        lines.append(f"- **Blockers:** {report.blocker_count} 🔴")
        lines.append(f"- **Warnings:** {report.warning_count} 🟡")
        lines.append(f"- **Info:** {report.info_count} 🔵")
        lines.append(f"- **Files Scanned:** {report.files_scanned}")
        lines.append(f"- **Scan Time:** {report.total_scan_time_ms:.0f}ms")
        lines.append("")

        # Blockers
        if report.blockers:
            lines.append("## 🔴 Blockers")
            lines.append("")
            lines.append("These issues **must** be fixed before agent deployment:")
            lines.append("")
            for finding in report.blockers:
                lines.extend(self._format_finding(finding))
            lines.append("")

        # Warnings
        if report.warnings:
            lines.append("## 🟡 Warnings")
            lines.append("")
            lines.append("These issues **should** be addressed for better agent performance:")
            lines.append("")
            for finding in report.warnings:
                lines.extend(self._format_finding(finding))
            lines.append("")

        # Info
        if report.info:
            lines.append("## 🔵 Info")
            lines.append("")
            lines.append("These suggestions may improve agent effectiveness:")
            lines.append("")
            for finding in report.info:
                lines.extend(self._format_finding(finding))
            lines.append("")

        # Detector Performance
        lines.append("## Detector Performance")
        lines.append("")
        lines.append("| Detector | Findings | Time (ms) | Status |")
        lines.append("|----------|----------|-----------|--------|")

        for dr in report.detector_results:
            status = "✅" if dr.error is None else "❌"
            lines.append(f"| {dr.detector_name} | {len(dr.findings)} | {dr.execution_time_ms:.0f} | {status} |")

        lines.append("")

        # Recommendations
        lines.append("## Recommendations")
        lines.append("")

        if report.score >= 90:
            lines.append("✨ **Excellent!** Your repository is well-prepared for AI agents.")
        elif report.score >= 70:
            lines.append("👍 **Good** - Address the warnings to improve agent effectiveness.")
        elif report.score >= 50:
            lines.append("⚠️ **Needs Improvement** - Address blockers and warnings before deploying agents.")
        else:
            lines.append("🚨 **Critical** - Significant issues detected. Fix blockers immediately.")

        lines.append("")

        return "\n".join(lines)

    def _format_finding(self, finding) -> list:
        """Format a single finding as Markdown lines."""
        lines = []

        # Finding header
        location = f"{finding.file_path}"
        if finding.line_number:
            location += f":{finding.line_number}"

        lines.append(f"### {finding.message}")
        lines.append("")
        lines.append(f"- **Location:** `{location}`")
        lines.append(f"- **Detector:** `{finding.detector}`")

        if finding.evidence:
            lines.append(f"- **Evidence:**")
            lines.append(f"  ```")
            lines.append(f"  {finding.evidence}")
            lines.append(f"  ```")

        if finding.suggestion:
            lines.append(f"- **Suggestion:** {finding.suggestion}")

        lines.append("")

        return lines

    def _get_score_emoji(self, score: int) -> str:
        """Get emoji for score."""
        if score >= 90:
            return "🌟"
        elif score >= 70:
            return "✅"
        elif score >= 50:
            return "⚠️"
        else:
            return "🚨"
