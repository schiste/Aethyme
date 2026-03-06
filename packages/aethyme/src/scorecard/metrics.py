"""Prometheus metrics for scorecard operations."""

import importlib
from collections.abc import Callable
from typing import Protocol, cast

from .models import ScorecardReport


class _LabeledMetric(Protocol):
    def inc(self, amount: float = 1) -> None: ...

    def set(self, value: float) -> None: ...

    def observe(self, value: float) -> None: ...


class _Metric(Protocol):
    def labels(self, **kwargs: str) -> _LabeledMetric: ...


class _NoOpLabeledMetric:
    def inc(self, amount: float = 1) -> None:
        del amount

    def set(self, value: float) -> None:
        del value

    def observe(self, value: float) -> None:
        del value


class _NoOpMetric:
    def labels(self, **kwargs: str) -> _LabeledMetric:
        del kwargs
        return _NoOpLabeledMetric()


MetricFactory = Callable[..., _Metric]


def _load_prometheus_factories() -> tuple[MetricFactory, MetricFactory, MetricFactory]:
    try:
        prometheus_client = importlib.import_module("prometheus_client")
    except ModuleNotFoundError:
        def noop_factory(*args: object, **kwargs: object) -> _Metric:
            del args, kwargs
            return _NoOpMetric()

        return noop_factory, noop_factory, noop_factory

    counter = cast(MetricFactory, prometheus_client.Counter)
    gauge = cast(MetricFactory, prometheus_client.Gauge)
    histogram = cast(MetricFactory, prometheus_client.Histogram)
    return counter, gauge, histogram


Counter, Gauge, Histogram = _load_prometheus_factories()

# Scan metrics
scorecard_scans_total = Counter(
    'aethyme_scorecard_scans_total',
    'Total number of scorecard scans',
    ['tenant_id', 'repository_id']
)

scorecard_scan_duration_seconds = Histogram(
    'aethyme_scorecard_scan_duration_seconds',
    'Scorecard scan duration in seconds',
    ['tenant_id'],
    buckets=[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
)

scorecard_detector_duration_seconds = Histogram(
    'aethyme_scorecard_detector_duration_seconds',
    'Individual detector execution time in seconds',
    ['detector_name'],
    buckets=[0.1, 0.25, 0.5, 1.0, 2.0, 5.0]
)

# Findings metrics
scorecard_findings_total = Counter(
    'aethyme_scorecard_findings_total',
    'Total number of findings',
    ['severity', 'detector']
)

scorecard_current_score = Gauge(
    'aethyme_scorecard_current_score',
    'Current AI-readiness score for repository',
    ['tenant_id', 'repository_id']
)

scorecard_blocker_count = Gauge(
    'aethyme_scorecard_blocker_count',
    'Number of blocker findings',
    ['tenant_id', 'repository_id']
)

scorecard_warning_count = Gauge(
    'aethyme_scorecard_warning_count',
    'Number of warning findings',
    ['tenant_id', 'repository_id']
)

# Detector metrics
scorecard_detector_errors_total = Counter(
    'aethyme_scorecard_detector_errors_total',
    'Total number of detector errors',
    ['detector_name']
)

scorecard_files_scanned = Gauge(
    'aethyme_scorecard_files_scanned',
    'Number of files scanned in last scan',
    ['tenant_id', 'repository_id']
)


def record_scan_metrics(
    report: ScorecardReport,
    tenant_id: str | None = None,
    repository_id: str | None = None,
) -> None:
    """
    Record metrics from a completed scan.

    Args:
        report: ScorecardReport to record metrics from
        tenant_id: Optional tenant ID
        repository_id: Optional repository ID
    """
    tenant = tenant_id or report.tenant_id or 'unknown'
    repo = repository_id or report.repository_id or 'unknown'

    # Record scan
    scorecard_scans_total.labels(tenant_id=tenant, repository_id=repo).inc()

    # Record duration (convert ms to seconds)
    scorecard_scan_duration_seconds.labels(tenant_id=tenant).observe(
        report.total_scan_time_ms / 1000.0
    )

    # Record score and findings
    scorecard_current_score.labels(tenant_id=tenant, repository_id=repo).set(report.score)
    scorecard_blocker_count.labels(tenant_id=tenant, repository_id=repo).set(report.blocker_count)
    scorecard_warning_count.labels(tenant_id=tenant, repository_id=repo).set(report.warning_count)
    scorecard_files_scanned.labels(tenant_id=tenant, repository_id=repo).set(report.files_scanned)

    # Record findings by severity and detector
    for finding in report.blockers + report.warnings + report.info:
        scorecard_findings_total.labels(
            severity=finding.severity,
            detector=finding.detector
        ).inc()

    # Record detector performance and errors
    for detector_result in report.detector_results:
        scorecard_detector_duration_seconds.labels(
            detector_name=detector_result.detector_name
        ).observe(detector_result.execution_time_ms / 1000.0)

        if detector_result.error:
            scorecard_detector_errors_total.labels(
                detector_name=detector_result.detector_name
            ).inc()
