"""
SCIP + Fallback Validation Module

Tests SCIP indexer on real repositories, measures success rate, timing, and symbol count,
and implements fallback to tree-sitter when SCIP fails.
"""

import time
import structlog
from pathlib import Path
from typing import Dict, List, Optional, Tuple
from dataclasses import dataclass
from enum import Enum

logger = structlog.get_logger(__name__)


class IndexerType(Enum):
    """Type of indexer used."""
    SCIP = "scip"
    FALLBACK = "fallback"


class IndexerResult(Enum):
    """Result of indexing operation."""
    SUCCESS = "success"
    FAILURE = "failure"
    FALLBACK_USED = "fallback_used"


@dataclass
class ValidationMetrics:
    """Metrics from a validation run."""
    repo_name: str
    repo_path: str
    language: str
    indexer_type: IndexerType
    result: IndexerResult
    duration_seconds: float
    symbol_count: int
    node_count: int
    edge_count: int
    file_count: int
    error_message: Optional[str] = None
    memory_usage_mb: Optional[float] = None


class IndexerValidator:
    """
    Validates SCIP and fallback indexers on real repositories.

    Tests indexing reliability across different languages and repo sizes,
    measures performance metrics, and determines which languages work best
    with each indexer.
    """

    def __init__(self):
        self.metrics: List[ValidationMetrics] = []
        self.logger = logger.bind(component="IndexerValidator")

    def validate_repository(
        self,
        repo_path: Path,
        repo_name: str,
        language: str,
        try_scip: bool = True,
        force_fallback: bool = False,
    ) -> ValidationMetrics:
        """
        Validate indexing on a single repository.

        Args:
            repo_path: Path to repository
            repo_name: Name of repository
            language: Primary language to index
            try_scip: Whether to try SCIP first
            force_fallback: Force use of fallback indexer

        Returns:
            ValidationMetrics with results
        """
        start_time = time.time()

        self.logger.info(
            "Starting validation",
            repo_name=repo_name,
            language=language,
            try_scip=try_scip,
            force_fallback=force_fallback,
        )

        # Try SCIP first if requested and not forcing fallback
        if try_scip and not force_fallback:
            scip_result = self._try_scip_indexer(repo_path, repo_name, language)
            if scip_result:
                duration = time.time() - start_time
                metrics = ValidationMetrics(
                    repo_name=repo_name,
                    repo_path=str(repo_path),
                    language=language,
                    indexer_type=IndexerType.SCIP,
                    result=IndexerResult.SUCCESS,
                    duration_seconds=duration,
                    symbol_count=scip_result["symbol_count"],
                    node_count=scip_result["node_count"],
                    edge_count=scip_result["edge_count"],
                    file_count=scip_result["file_count"],
                )
                self.metrics.append(metrics)
                self.logger.info(
                    "SCIP validation successful",
                    repo_name=repo_name,
                    duration=duration,
                    symbols=metrics.symbol_count,
                )
                return metrics
            else:
                self.logger.warning(
                    "SCIP validation failed, falling back",
                    repo_name=repo_name,
                )

        # Fall back to regex-based indexer
        fallback_result = self._try_fallback_indexer(repo_path, repo_name, language)
        duration = time.time() - start_time

        if fallback_result:
            result_type = IndexerResult.FALLBACK_USED if try_scip else IndexerResult.SUCCESS
            metrics = ValidationMetrics(
                repo_name=repo_name,
                repo_path=str(repo_path),
                language=language,
                indexer_type=IndexerType.FALLBACK,
                result=result_type,
                duration_seconds=duration,
                symbol_count=fallback_result["symbol_count"],
                node_count=fallback_result["node_count"],
                edge_count=fallback_result["edge_count"],
                file_count=fallback_result["file_count"],
            )
            self.metrics.append(metrics)
            self.logger.info(
                "Fallback validation successful",
                repo_name=repo_name,
                duration=duration,
                symbols=metrics.symbol_count,
            )
            return metrics
        else:
            # Both failed
            metrics = ValidationMetrics(
                repo_name=repo_name,
                repo_path=str(repo_path),
                language=language,
                indexer_type=IndexerType.FALLBACK,
                result=IndexerResult.FAILURE,
                duration_seconds=duration,
                symbol_count=0,
                node_count=0,
                edge_count=0,
                file_count=0,
                error_message="Both SCIP and fallback indexers failed",
            )
            self.metrics.append(metrics)
            self.logger.error(
                "All indexers failed",
                repo_name=repo_name,
                duration=duration,
            )
            return metrics

    def _try_scip_indexer(
        self, repo_path: Path, repo_name: str, language: str
    ) -> Optional[Dict]:
        """
        Try to index with SCIP.

        Returns:
            Dict with counts if successful, None if failed
        """
        try:
            from src.indexer.scip_wrapper import SCIPIndexer

            indexer = SCIPIndexer(language)
            if not indexer.is_available():
                self.logger.debug("SCIP indexer not available", language=language)
                return None

            scip_data = indexer.index(repo_path, repo_name)

            # Count symbols and relationships
            symbol_count = len(scip_data.get("symbols", []))
            relationship_count = len(scip_data.get("relationships", []))
            file_count = len(set(s.get("file_path") for s in scip_data.get("symbols", [])))

            return {
                "symbol_count": symbol_count,
                "node_count": symbol_count,
                "edge_count": relationship_count,
                "file_count": file_count,
            }
        except Exception as e:
            self.logger.warning(
                "SCIP indexer failed",
                language=language,
                error=str(e),
            )
            return None

    def _try_fallback_indexer(
        self, repo_path: Path, repo_name: str, language: str
    ) -> Optional[Dict]:
        """
        Try to index with fallback indexer.

        Returns:
            Dict with counts if successful, None if failed
        """
        try:
            from src.indexer.fallback_indexer import FallbackIndexer

            indexer = FallbackIndexer(language)
            exclude_dirs = [
                'node_modules', '__pycache__', '.git', 'venv', '.venv',
                'dist', 'build', '.pytest_cache', '.mypy_cache',
                'site-packages', 'proc', 'sys', 'dev', 'run', 'tmp'
            ]

            nodes, edges = indexer.index(repo_path, exclude_dirs=exclude_dirs)

            # Count symbols from nodes
            symbol_count = len([n for n in nodes if n.get("kind") in [
                "function", "class", "method", "variable", "constant"
            ]])
            file_count = len(set(n.get("file_path") for n in nodes))

            return {
                "symbol_count": symbol_count,
                "node_count": len(nodes),
                "edge_count": len(edges),
                "file_count": file_count,
            }
        except Exception as e:
            self.logger.error(
                "Fallback indexer failed",
                language=language,
                error=str(e),
            )
            return None

    def get_success_rate(self) -> Dict[str, float]:
        """
        Calculate success rates for different indexers.

        Returns:
            Dict with success rates
        """
        total = len(self.metrics)
        if total == 0:
            return {"scip": 0.0, "fallback": 0.0, "overall": 0.0}

        scip_success = len([
            m for m in self.metrics
            if m.indexer_type == IndexerType.SCIP and m.result == IndexerResult.SUCCESS
        ])

        fallback_success = len([
            m for m in self.metrics
            if m.result in [IndexerResult.SUCCESS, IndexerResult.FALLBACK_USED]
        ])

        return {
            "scip": scip_success / total,
            "fallback": fallback_success / total,
            "overall": fallback_success / total,
        }

    def get_language_breakdown(self) -> Dict[str, Dict[str, int]]:
        """
        Get breakdown of results by language.

        Returns:
            Dict mapping language to indexer success counts
        """
        breakdown = {}
        for metric in self.metrics:
            lang = metric.language
            if lang not in breakdown:
                breakdown[lang] = {"scip_success": 0, "fallback_used": 0, "failed": 0}

            if metric.indexer_type == IndexerType.SCIP and metric.result == IndexerResult.SUCCESS:
                breakdown[lang]["scip_success"] += 1
            elif metric.result == IndexerResult.FALLBACK_USED:
                breakdown[lang]["fallback_used"] += 1
            elif metric.result == IndexerResult.FAILURE:
                breakdown[lang]["failed"] += 1

        return breakdown

    def get_performance_summary(self) -> Dict[str, Dict[str, float]]:
        """
        Get performance summary statistics.

        Returns:
            Dict with median, p95, p99 durations by indexer type
        """
        import statistics

        scip_durations = [
            m.duration_seconds for m in self.metrics
            if m.indexer_type == IndexerType.SCIP
        ]

        fallback_durations = [
            m.duration_seconds for m in self.metrics
            if m.indexer_type == IndexerType.FALLBACK
        ]

        def calc_percentiles(durations: List[float]) -> Dict[str, float]:
            if not durations:
                return {"median": 0.0, "p95": 0.0, "p99": 0.0}
            sorted_durations = sorted(durations)
            return {
                "median": statistics.median(sorted_durations),
                "p95": sorted_durations[int(len(sorted_durations) * 0.95)] if len(sorted_durations) > 1 else sorted_durations[0],
                "p99": sorted_durations[int(len(sorted_durations) * 0.99)] if len(sorted_durations) > 1 else sorted_durations[0],
            }

        return {
            "scip": calc_percentiles(scip_durations),
            "fallback": calc_percentiles(fallback_durations),
        }

    def generate_report(self) -> str:
        """
        Generate a detailed validation report.

        Returns:
            Markdown report string
        """
        success_rates = self.get_success_rate()
        language_breakdown = self.get_language_breakdown()
        perf_summary = self.get_performance_summary()

        report = ["# Indexer Validation Report\n"]
        report.append(f"Total repositories tested: {len(self.metrics)}\n")
        report.append("\n## Success Rates\n")
        report.append(f"- SCIP: {success_rates['scip']:.1%}")
        report.append(f"- Fallback: {success_rates['fallback']:.1%}")
        report.append(f"- Overall: {success_rates['overall']:.1%}\n")

        report.append("\n## Language Breakdown\n")
        report.append("| Language | SCIP Success | Fallback Used | Failed |")
        report.append("| --- | --- | --- | --- |")
        for lang, counts in sorted(language_breakdown.items()):
            report.append(
                f"| {lang} | {counts['scip_success']} | "
                f"{counts['fallback_used']} | {counts['failed']} |"
            )

        report.append("\n\n## Performance Summary\n")
        report.append("### SCIP Indexer")
        scip_perf = perf_summary["scip"]
        report.append(f"- Median: {scip_perf['median']:.2f}s")
        report.append(f"- P95: {scip_perf['p95']:.2f}s")
        report.append(f"- P99: {scip_perf['p99']:.2f}s\n")

        report.append("### Fallback Indexer")
        fallback_perf = perf_summary["fallback"]
        report.append(f"- Median: {fallback_perf['median']:.2f}s")
        report.append(f"- P95: {fallback_perf['p95']:.2f}s")
        report.append(f"- P99: {fallback_perf['p99']:.2f}s\n")

        report.append("\n## Detailed Results\n")
        report.append("| Repo | Language | Indexer | Result | Duration (s) | Symbols |")
        report.append("| --- | --- | --- | --- | --- | --- |")
        for metric in self.metrics:
            report.append(
                f"| {metric.repo_name} | {metric.language} | "
                f"{metric.indexer_type.value} | {metric.result.value} | "
                f"{metric.duration_seconds:.2f} | {metric.symbol_count} |"
            )

        return "\n".join(report)
