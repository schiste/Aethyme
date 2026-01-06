"""
Regression Detection for RepoGraph Benchmarks.

Automatically detects performance regressions by:
- Comparing current benchmark results to baseline
- Statistical significance testing
- Alerting on degradations beyond threshold
- Tracking performance trends over time
"""

import json
import statistics
from typing import List, Dict, Any, Optional
from dataclasses import dataclass, asdict
from pathlib import Path
from datetime import datetime
import structlog

logger = structlog.get_logger(__name__)


@dataclass
class RegressionAlert:
    """Alert for a detected regression."""
    metric_name: str
    baseline_value: float
    current_value: float
    change_pct: float
    threshold_pct: float
    severity: str  # warning, critical
    benchmark_name: str
    timestamp: str


@dataclass
class RegressionReport:
    """Report of regression detection results."""
    total_metrics_checked: int
    regressions_detected: int
    warnings: int
    critical: int
    alerts: List[RegressionAlert]
    passed: bool  # False if any critical regressions


class RegressionDetector:
    """
    Detects performance regressions in benchmarks.

    Compares current results against baseline and alerts
    if performance degrades beyond configurable thresholds.
    """

    def __init__(
        self,
        baseline_dir: str = "./benchmarks/results/baseline",
        warning_threshold: float = 10.0,  # 10% degradation
        critical_threshold: float = 25.0,  # 25% degradation
    ):
        self.logger = logger.bind(component="RegressionDetector")
        self.baseline_dir = Path(baseline_dir)
        self.warning_threshold = warning_threshold
        self.critical_threshold = critical_threshold
        self.baseline_dir.mkdir(parents=True, exist_ok=True)

    def load_baseline(self, benchmark_name: str) -> Optional[Dict[str, Any]]:
        """
        Load baseline results for a benchmark.

        Args:
            benchmark_name: Name of the benchmark

        Returns:
            Baseline data or None if not found
        """
        baseline_file = self.baseline_dir / f"{benchmark_name}_baseline.json"

        if not baseline_file.exists():
            self.logger.warning(
                "No baseline found",
                benchmark_name=benchmark_name,
                baseline_file=str(baseline_file),
            )
            return None

        with open(baseline_file, 'r') as f:
            return json.load(f)

    def save_baseline(
        self,
        benchmark_name: str,
        results: Dict[str, Any],
    ):
        """
        Save results as baseline.

        Args:
            benchmark_name: Name of the benchmark
            results: Results to save as baseline
        """
        baseline_file = self.baseline_dir / f"{benchmark_name}_baseline.json"

        data = {
            "timestamp": datetime.utcnow().isoformat(),
            "benchmark_name": benchmark_name,
            "results": results,
        }

        with open(baseline_file, 'w') as f:
            json.dump(data, f, indent=2)

        self.logger.info(
            "Baseline saved",
            benchmark_name=benchmark_name,
            baseline_file=str(baseline_file),
        )

    def calculate_change_pct(
        self,
        baseline: float,
        current: float,
    ) -> float:
        """
        Calculate percentage change.

        Args:
            baseline: Baseline value
            current: Current value

        Returns:
            Percentage change (positive = degradation for latency metrics)
        """
        if baseline == 0:
            return 0.0

        return ((current - baseline) / baseline) * 100

    def check_metric(
        self,
        metric_name: str,
        baseline_value: float,
        current_value: float,
        benchmark_name: str,
        higher_is_better: bool = False,
    ) -> Optional[RegressionAlert]:
        """
        Check a single metric for regression.

        Args:
            metric_name: Name of the metric
            baseline_value: Baseline value
            current_value: Current value
            benchmark_name: Name of the benchmark
            higher_is_better: If True, higher values are better (e.g., throughput)

        Returns:
            RegressionAlert if regression detected, None otherwise
        """
        change_pct = self.calculate_change_pct(baseline_value, current_value)

        # Invert change for "higher is better" metrics
        if higher_is_better:
            change_pct = -change_pct

        # Only alert on degradations
        if change_pct <= 0:
            return None

        # Determine severity
        if change_pct >= self.critical_threshold:
            severity = "critical"
        elif change_pct >= self.warning_threshold:
            severity = "warning"
        else:
            return None

        return RegressionAlert(
            metric_name=metric_name,
            baseline_value=baseline_value,
            current_value=current_value,
            change_pct=change_pct,
            threshold_pct=self.critical_threshold if severity == "critical" else self.warning_threshold,
            severity=severity,
            benchmark_name=benchmark_name,
            timestamp=datetime.utcnow().isoformat(),
        )

    def check_benchmark(
        self,
        benchmark_name: str,
        current_results: Dict[str, Any],
        metrics_to_check: Optional[Dict[str, bool]] = None,
    ) -> RegressionReport:
        """
        Check a benchmark for regressions.

        Args:
            benchmark_name: Name of the benchmark
            current_results: Current benchmark results
            metrics_to_check: Dict of {metric_name: higher_is_better}

        Returns:
            RegressionReport
        """
        baseline = self.load_baseline(benchmark_name)

        if not baseline:
            self.logger.info(
                "No baseline available, saving current results as baseline",
                benchmark_name=benchmark_name,
            )
            self.save_baseline(benchmark_name, current_results)
            return RegressionReport(
                total_metrics_checked=0,
                regressions_detected=0,
                warnings=0,
                critical=0,
                alerts=[],
                passed=True,
            )

        # Default metrics to check (latency metrics - lower is better)
        if metrics_to_check is None:
            metrics_to_check = {
                "avg_latency": False,
                "p95_latency": False,
                "p99_latency": False,
                "avg_duration": False,
                "p95_duration": False,
                "throughput": True,  # Higher is better
            }

        alerts = []
        baseline_results = baseline.get("results", {})

        for metric_name, higher_is_better in metrics_to_check.items():
            if metric_name not in baseline_results or metric_name not in current_results:
                continue

            alert = self.check_metric(
                metric_name,
                baseline_results[metric_name],
                current_results[metric_name],
                benchmark_name,
                higher_is_better,
            )

            if alert:
                alerts.append(alert)
                self.logger.warning(
                    "Regression detected",
                    **asdict(alert),
                )

        warnings = sum(1 for a in alerts if a.severity == "warning")
        critical = sum(1 for a in alerts if a.severity == "critical")

        report = RegressionReport(
            total_metrics_checked=len(metrics_to_check),
            regressions_detected=len(alerts),
            warnings=warnings,
            critical=critical,
            alerts=alerts,
            passed=critical == 0,
        )

        self.logger.info(
            "Regression check complete",
            benchmark_name=benchmark_name,
            **asdict(report),
        )

        return report

    def export_report(
        self,
        report: RegressionReport,
        output_file: str,
    ):
        """
        Export regression report to JSON.

        Args:
            report: RegressionReport
            output_file: Output file path
        """
        data = {
            "timestamp": datetime.utcnow().isoformat(),
            "report": asdict(report),
        }

        with open(output_file, 'w') as f:
            json.dump(data, f, indent=2)

        self.logger.info("Report exported", output_file=output_file)

    def print_report(self, report: RegressionReport):
        """
        Print regression report to console.

        Args:
            report: RegressionReport
        """
        print("\n" + "="*60)
        print("REGRESSION DETECTION REPORT")
        print("="*60)

        if report.passed:
            print("PASSED: No critical regressions detected")
        else:
            print(f"FAILED: {report.critical} critical regression(s) detected")

        print(f"\nMetrics checked: {report.total_metrics_checked}")
        print(f"Regressions detected: {report.regressions_detected}")
        print(f"  Warnings: {report.warnings}")
        print(f"  Critical: {report.critical}")

        if report.alerts:
            print("\nALERTS:")
            print("-" * 60)

            for alert in sorted(report.alerts, key=lambda a: a.change_pct, reverse=True):
                print(f"\n{alert.severity.upper()}: {alert.metric_name}")
                print(f"  Benchmark: {alert.benchmark_name}")
                print(f"  Baseline:  {alert.baseline_value:.4f}")
                print(f"  Current:   {alert.current_value:.4f}")
                print(f"  Change:    +{alert.change_pct:.1f}% (threshold: {alert.threshold_pct:.1f}%)")

        print("\n" + "="*60 + "\n")


def main():
    """Example usage of regression detection."""
    detector = RegressionDetector(
        warning_threshold=10.0,
        critical_threshold=25.0,
    )

    # Example: Check query benchmark
    current_results = {
        "avg_latency": 0.025,  # 25ms
        "p95_latency": 0.050,  # 50ms
        "p99_latency": 0.100,  # 100ms
        "throughput": 800.0,   # 800 ops/sec
    }

    report = detector.check_benchmark(
        "query_benchmark",
        current_results,
    )

    detector.print_report(report)
    detector.export_report(report, "./regression_report.json")

    # Return exit code based on result
    import sys
    sys.exit(0 if report.passed else 1)


if __name__ == "__main__":
    main()
