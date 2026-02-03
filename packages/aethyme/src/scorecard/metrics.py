"""Prometheus metrics for scorecard operations."""

from prometheus_client import Counter, Histogram, Gauge

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


def record_scan_metrics(report, tenant_id: str = None, repository_id: str = None):
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
