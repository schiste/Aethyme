"""
KPI Calculation and Reporting for RepoGraph.

Provides:
- KPI calculation engine
- CSV/JSON export
- CLI integration
- Prometheus query integration for historical data

Key KPIs:
- Index latency (p50, p95, p99)
- Query p95
- Tokens per task
- Cost per task
- Fix success rate
- Violation reduction percentage
- Cache hit rates
- Error rates
"""

import csv
import json
import statistics
from datetime import datetime, timedelta
from typing import Dict, List, Optional, Any
from dataclasses import dataclass, asdict
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class KPISnapshot:
    """Snapshot of KPI metrics at a point in time."""
    timestamp: str

    # Latency metrics (seconds)
    index_latency_p50: Optional[float] = None
    index_latency_p95: Optional[float] = None
    index_latency_p99: Optional[float] = None
    query_latency_p50: Optional[float] = None
    query_latency_p95: Optional[float] = None
    query_latency_p99: Optional[float] = None

    # Token and cost metrics
    avg_tokens_per_task: Optional[float] = None
    total_tokens: Optional[int] = None
    avg_cost_per_task: Optional[float] = None
    total_cost: Optional[float] = None

    # Quality metrics
    fix_success_rate: Optional[float] = None
    fix_attempts: Optional[int] = None
    fix_successes: Optional[int] = None
    violations_detected: Optional[int] = None
    violations_prevented: Optional[int] = None
    violation_reduction_pct: Optional[float] = None

    # Cache metrics
    query_cache_hit_rate: Optional[float] = None
    index_cache_hit_rate: Optional[float] = None

    # Reliability metrics
    error_rate: Optional[float] = None
    total_operations: Optional[int] = None
    failed_operations: Optional[int] = None

    # Model routing metrics
    model_escalations: Optional[int] = None
    avg_context_slots: Optional[float] = None


class KPICalculator:
    """
    Calculates and exports KPIs for RepoGraph.

    Aggregates metrics from Prometheus and in-memory collectors
    to produce comprehensive KPI reports.
    """

    def __init__(self):
        self.logger = logger.bind(component="KPICalculator")
        self._in_memory_metrics: List[Dict[str, Any]] = []

    def record_operation_metrics(
        self,
        operation_type: str,
        duration: float,
        tokens: Optional[int] = None,
        cost: Optional[float] = None,
        success: bool = True,
        cache_hit: bool = False,
    ):
        """
        Record metrics for a single operation.

        Args:
            operation_type: Type of operation (index, query, scorecard, autofix)
            duration: Operation duration in seconds
            tokens: Optional token count
            cost: Optional cost in dollars
            success: Whether operation succeeded
            cache_hit: Whether it was a cache hit
        """
        self._in_memory_metrics.append({
            "timestamp": datetime.utcnow().isoformat(),
            "operation_type": operation_type,
            "duration": duration,
            "tokens": tokens,
            "cost": cost,
            "success": success,
            "cache_hit": cache_hit,
        })

    def calculate_percentile(self, values: List[float], percentile: float) -> Optional[float]:
        """
        Calculate percentile from a list of values.

        Args:
            values: List of numeric values
            percentile: Percentile to calculate (0-100)

        Returns:
            Percentile value or None if no data
        """
        if not values:
            return None

        sorted_values = sorted(values)
        k = (len(sorted_values) - 1) * (percentile / 100)
        f = int(k)
        c = f + 1

        if c >= len(sorted_values):
            return sorted_values[-1]

        d0 = sorted_values[f] * (c - k)
        d1 = sorted_values[c] * (k - f)
        return d0 + d1

    def calculate_kpis(
        self,
        time_window: Optional[timedelta] = None,
    ) -> KPISnapshot:
        """
        Calculate current KPIs.

        Args:
            time_window: Optional time window to filter metrics (default: all time)

        Returns:
            KPISnapshot with calculated metrics
        """
        now = datetime.utcnow()
        cutoff = now - time_window if time_window else datetime.min

        # Filter metrics by time window
        filtered_metrics = [
            m for m in self._in_memory_metrics
            if datetime.fromisoformat(m["timestamp"]) >= cutoff
        ]

        if not filtered_metrics:
            self.logger.warning("No metrics available for KPI calculation")
            return KPISnapshot(timestamp=now.isoformat())

        # Calculate index latency percentiles
        index_durations = [
            m["duration"] for m in filtered_metrics
            if m["operation_type"] == "index" and m["success"]
        ]

        # Calculate query latency percentiles
        query_durations = [
            m["duration"] for m in filtered_metrics
            if m["operation_type"] in ("search", "ego", "impact") and m["success"]
        ]

        # Calculate token metrics
        token_counts = [
            m["tokens"] for m in filtered_metrics
            if m.get("tokens") is not None
        ]

        # Calculate cost metrics
        costs = [
            m["cost"] for m in filtered_metrics
            if m.get("cost") is not None
        ]

        # Calculate fix success rate
        fix_attempts = sum(
            1 for m in filtered_metrics
            if m["operation_type"] == "autofix"
        )
        fix_successes = sum(
            1 for m in filtered_metrics
            if m["operation_type"] == "autofix" and m["success"]
        )
        fix_success_rate = (fix_successes / fix_attempts * 100) if fix_attempts > 0 else None

        # Calculate cache hit rates
        query_cache_ops = [
            m for m in filtered_metrics
            if m["operation_type"] in ("search", "ego", "impact")
        ]
        query_cache_hits = sum(1 for m in query_cache_ops if m.get("cache_hit"))
        query_cache_hit_rate = (
            query_cache_hits / len(query_cache_ops) * 100
            if query_cache_ops else None
        )

        # Calculate error rate
        total_ops = len(filtered_metrics)
        failed_ops = sum(1 for m in filtered_metrics if not m["success"])
        error_rate = (failed_ops / total_ops * 100) if total_ops > 0 else None

        snapshot = KPISnapshot(
            timestamp=now.isoformat(),
            # Index latency
            index_latency_p50=self.calculate_percentile(index_durations, 50),
            index_latency_p95=self.calculate_percentile(index_durations, 95),
            index_latency_p99=self.calculate_percentile(index_durations, 99),
            # Query latency
            query_latency_p50=self.calculate_percentile(query_durations, 50),
            query_latency_p95=self.calculate_percentile(query_durations, 95),
            query_latency_p99=self.calculate_percentile(query_durations, 99),
            # Tokens and cost
            avg_tokens_per_task=statistics.mean(token_counts) if token_counts else None,
            total_tokens=sum(token_counts) if token_counts else None,
            avg_cost_per_task=statistics.mean(costs) if costs else None,
            total_cost=sum(costs) if costs else None,
            # Quality
            fix_success_rate=fix_success_rate,
            fix_attempts=fix_attempts,
            fix_successes=fix_successes,
            # Cache
            query_cache_hit_rate=query_cache_hit_rate,
            # Reliability
            error_rate=error_rate,
            total_operations=total_ops,
            failed_operations=failed_ops,
        )

        self.logger.info(
            "KPIs calculated",
            time_window=str(time_window) if time_window else "all",
            total_metrics=len(filtered_metrics),
        )

        return snapshot

    def export_to_csv(
        self,
        output_path: str,
        snapshots: Optional[List[KPISnapshot]] = None,
    ):
        """
        Export KPIs to CSV.

        Args:
            output_path: Path to output CSV file
            snapshots: Optional list of snapshots (default: current snapshot)
        """
        if snapshots is None:
            snapshots = [self.calculate_kpis()]

        if not snapshots:
            self.logger.warning("No snapshots to export")
            return

        with open(output_path, 'w', newline='') as csvfile:
            # Get all fields from the dataclass
            fieldnames = list(asdict(snapshots[0]).keys())
            writer = csv.DictWriter(csvfile, fieldnames=fieldnames)

            writer.writeheader()
            for snapshot in snapshots:
                writer.writerow(asdict(snapshot))

        self.logger.info(
            "KPIs exported to CSV",
            output_path=output_path,
            snapshot_count=len(snapshots),
        )

    def export_to_json(
        self,
        output_path: str,
        snapshots: Optional[List[KPISnapshot]] = None,
    ):
        """
        Export KPIs to JSON.

        Args:
            output_path: Path to output JSON file
            snapshots: Optional list of snapshots (default: current snapshot)
        """
        if snapshots is None:
            snapshots = [self.calculate_kpis()]

        if not snapshots:
            self.logger.warning("No snapshots to export")
            return

        data = {
            "generated_at": datetime.utcnow().isoformat(),
            "snapshots": [asdict(s) for s in snapshots],
        }

        with open(output_path, 'w') as jsonfile:
            json.dump(data, jsonfile, indent=2)

        self.logger.info(
            "KPIs exported to JSON",
            output_path=output_path,
            snapshot_count=len(snapshots),
        )

    def print_summary(self, snapshot: Optional[KPISnapshot] = None):
        """
        Print a human-readable KPI summary.

        Args:
            snapshot: Optional snapshot (default: current snapshot)
        """
        if snapshot is None:
            snapshot = self.calculate_kpis()

        print("\n" + "="*60)
        print("RepoGraph KPI Summary")
        print("="*60)
        print(f"Generated: {snapshot.timestamp}")
        print()

        print("LATENCY METRICS (seconds)")
        print("-" * 60)
        if snapshot.index_latency_p50 is not None:
            print(f"  Index Latency:")
            print(f"    p50: {snapshot.index_latency_p50:.3f}s")
            print(f"    p95: {snapshot.index_latency_p95:.3f}s")
            print(f"    p99: {snapshot.index_latency_p99:.3f}s")
        if snapshot.query_latency_p50 is not None:
            print(f"  Query Latency:")
            print(f"    p50: {snapshot.query_latency_p50:.3f}s")
            print(f"    p95: {snapshot.query_latency_p95:.3f}s")
            print(f"    p99: {snapshot.query_latency_p99:.3f}s")
        print()

        print("TOKEN & COST METRICS")
        print("-" * 60)
        if snapshot.avg_tokens_per_task is not None:
            print(f"  Avg Tokens/Task: {snapshot.avg_tokens_per_task:,.0f}")
            print(f"  Total Tokens: {snapshot.total_tokens:,}")
        if snapshot.avg_cost_per_task is not None:
            print(f"  Avg Cost/Task: ${snapshot.avg_cost_per_task:.4f}")
            print(f"  Total Cost: ${snapshot.total_cost:.2f}")
        print()

        print("QUALITY METRICS")
        print("-" * 60)
        if snapshot.fix_success_rate is not None:
            print(f"  Fix Success Rate: {snapshot.fix_success_rate:.1f}%")
            print(f"  Fix Attempts: {snapshot.fix_attempts}")
            print(f"  Fix Successes: {snapshot.fix_successes}")
        if snapshot.violations_detected is not None:
            print(f"  Violations Detected: {snapshot.violations_detected}")
        if snapshot.violations_prevented is not None:
            print(f"  Violations Prevented: {snapshot.violations_prevented}")
        print()

        print("CACHE METRICS")
        print("-" * 60)
        if snapshot.query_cache_hit_rate is not None:
            print(f"  Query Cache Hit Rate: {snapshot.query_cache_hit_rate:.1f}%")
        if snapshot.index_cache_hit_rate is not None:
            print(f"  Index Cache Hit Rate: {snapshot.index_cache_hit_rate:.1f}%")
        print()

        print("RELIABILITY METRICS")
        print("-" * 60)
        if snapshot.error_rate is not None:
            print(f"  Error Rate: {snapshot.error_rate:.2f}%")
            print(f"  Total Operations: {snapshot.total_operations}")
            print(f"  Failed Operations: {snapshot.failed_operations}")
        print()
        print("="*60 + "\n")


# Global KPI calculator instance
kpi_calculator = KPICalculator()


def export_kpi_report(
    output_path: str,
    format: str = "csv",
    time_window: Optional[timedelta] = None,
):
    """
    Export a KPI report.

    Args:
        output_path: Path to output file
        format: Output format ('csv' or 'json')
        time_window: Optional time window for metrics
    """
    snapshot = kpi_calculator.calculate_kpis(time_window=time_window)

    if format.lower() == "csv":
        kpi_calculator.export_to_csv(output_path, [snapshot])
    elif format.lower() == "json":
        kpi_calculator.export_to_json(output_path, [snapshot])
    else:
        raise ValueError(f"Unsupported format: {format}")
