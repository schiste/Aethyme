import asyncio
import json
import uuid
from datetime import datetime
from pathlib import Path
from typing import Any, TypeAlias, cast

import structlog
from fastapi import APIRouter, BackgroundTasks, HTTPException, Query
from pydantic import BaseModel, Field

from ...auth import RepoReadUser, RepoWriteUser
from ...graph.connection_pool import db_pool
from ...scorecard.engine import ScorecardEngine
from ...scorecard.metrics import record_scan_metrics
from ...scorecard.models import ScanSummary

logger = structlog.get_logger(__name__)
router = APIRouter()
SummaryPayload: TypeAlias = dict[str, int]
FindingPayload: TypeAlias = dict[str, Any]
FindingsPayload: TypeAlias = dict[str, list[FindingPayload]]
DetectorPayload: TypeAlias = dict[str, Any]
PerformancePayload: TypeAlias = dict[str, float | int]
HistoryEntry: TypeAlias = dict[str, str | int | datetime | None]
DetectorInfo: TypeAlias = dict[str, str]


class ScanRequest(BaseModel):
    """Request to trigger a scorecard scan."""
    repository_id: str = Field(..., description="Repository ID to scan")
    detectors: list[str] | None = Field(None, description="Specific detectors to run")


class ScanResponse(BaseModel):
    """Response after triggering a scan."""
    scan_id: str = Field(..., description="Unique scan ID")
    status: str = Field(..., description="Scan status: pending, running, completed, failed")
    message: str = Field(..., description="Status message")


class ScorecardResultResponse(BaseModel):
    """Full scorecard result response."""
    scan_id: str
    repository_id: str
    tenant_id: str
    timestamp: datetime
    score: int
    summary: SummaryPayload
    findings: FindingsPayload
    detectors: list[DetectorPayload]
    performance: PerformancePayload


def _coerce_summary(data: object) -> SummaryPayload:
    if not isinstance(data, dict):
        return {}
    typed_data = cast(dict[object, object], data)
    summary: SummaryPayload = {}
    for key, value in typed_data.items():
        if isinstance(key, str) and isinstance(value, int):
            summary[key] = value
    return summary


def _coerce_findings(data: object) -> FindingsPayload:
    if not isinstance(data, dict):
        return {}
    typed_data = cast(dict[object, object], data)
    findings: FindingsPayload = {}
    for key, value in typed_data.items():
        if not isinstance(key, str) or not isinstance(value, list):
            continue
        finding_items: list[FindingPayload] = []
        for item in cast(list[object], value):
            if isinstance(item, dict):
                finding_items.append(dict(cast(dict[str, Any], item)))
        findings[key] = finding_items
    return findings


def _coerce_detectors(data: object) -> list[DetectorPayload]:
    if not isinstance(data, list):
        return []
    detectors: list[DetectorPayload] = []
    for item in cast(list[object], data):
        if isinstance(item, dict):
            detectors.append(dict(cast(dict[str, Any], item)))
    return detectors


def _coerce_performance(data: object) -> PerformancePayload:
    if not isinstance(data, dict):
        return {}
    typed_data = cast(dict[object, object], data)
    performance: PerformancePayload = {}
    for key, value in typed_data.items():
        if isinstance(key, str) and isinstance(value, (int, float)):
            performance[key] = value
    return performance


@router.post("/scan", response_model=ScanResponse)
async def trigger_scan(
    request: ScanRequest,
    background_tasks: BackgroundTasks,
    current_user: RepoWriteUser,
) -> ScanResponse:
    """
    Trigger an AI-readiness scorecard scan for a repository.

    The scan runs asynchronously in the background. Use the scan_id
    to retrieve results via GET /api/scorecard/results/{scan_id}.
    """
    tenant_id = current_user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Verify repository exists and user has access (RLS)
    repo_result = db_pool.execute(
        """
        SELECT id, name, path
        FROM aethyme.repositories
        WHERE id = %s AND tenant_id = %s
        """,
        (request.repository_id, tenant_id),
    )

    if not repo_result:
        raise HTTPException(status_code=404, detail="Repository not found")

    repo = repo_result[0]
    repo_path = repo["path"]

    # Generate scan ID
    scan_id = str(uuid.uuid4())

    # Create scan record
    db_pool.execute(
        """
        INSERT INTO aethyme.scorecard_scans
        (id, repository_id, tenant_id, status, created_at)
        VALUES (%s, %s, %s, 'pending', CURRENT_TIMESTAMP)
        """,
        (scan_id, request.repository_id, tenant_id),
        fetch=False,
    )

    # Run scan in background
    background_tasks.add_task(
        run_scan_background,
        scan_id=scan_id,
        repo_path=repo_path,
        repository_id=request.repository_id,
        tenant_id=tenant_id,
        detectors=request.detectors,
    )

    logger.info(
        "Scorecard scan triggered",
        scan_id=scan_id,
        repository_id=request.repository_id,
        tenant_id=tenant_id,
    )

    return ScanResponse(
        scan_id=scan_id,
        status="pending",
        message=f"Scan queued for repository {repo['name']}",
    )


async def run_scan_background(
    scan_id: str,
    repo_path: str,
    repository_id: str,
    tenant_id: str,
    detectors: list[str] | None = None,
) -> None:
    """
    Run scorecard scan in background.

    Args:
        scan_id: Scan ID
        repo_path: Path to repository
        repository_id: Repository ID
        tenant_id: Tenant ID
        detectors: Optional list of specific detectors to run
    """
    try:
        # Update status to running
        db_pool.execute(
            """
            UPDATE aethyme.scorecard_scans
            SET status = 'running', started_at = CURRENT_TIMESTAMP
            WHERE id = %s
            """,
            (scan_id,),
            fetch=False,
        )

        # Run scan
        engine = ScorecardEngine(
            repo_path=Path(repo_path),
            repository_id=repository_id,
            tenant_id=tenant_id,
        )

        result = await asyncio.to_thread(engine.scan, detectors=detectors)

        # Store results
        db_pool.execute(
            """
            UPDATE aethyme.scorecard_scans
            SET
                status = 'completed',
                completed_at = CURRENT_TIMESTAMP,
                score = %s,
                blocker_count = %s,
                warning_count = %s,
                info_count = %s,
                total_findings = %s,
                scan_time_ms = %s,
                files_scanned = %s,
                report_json = %s
            WHERE id = %s
            """,
            (
                result.report.score,
                result.report.blocker_count,
                result.report.warning_count,
                result.report.info_count,
                result.report.total_findings,
                result.report.total_scan_time_ms,
                result.report.files_scanned,
                result.to_json(),
                scan_id,
            ),
            fetch=False,
        )

        # Record metrics
        record_scan_metrics(result.report, tenant_id=tenant_id, repository_id=repository_id)

        logger.info(
            "Scorecard scan completed",
            scan_id=scan_id,
            score=result.score,
            findings=result.report.total_findings,
        )

    except Exception as e:
        # Mark scan as failed
        db_pool.execute(
            """
            UPDATE aethyme.scorecard_scans
            SET status = 'failed', completed_at = CURRENT_TIMESTAMP, error = %s
            WHERE id = %s
            """,
            (str(e), scan_id),
            fetch=False,
        )

        logger.error(
            "Scorecard scan failed",
            scan_id=scan_id,
            error=str(e),
            exc_info=True,
        )


@router.get("/results/{scan_id}", response_model=ScorecardResultResponse)
async def get_scan_results(
    scan_id: str,
    current_user: RepoReadUser,
) -> ScorecardResultResponse:
    """
    Get results of a completed scorecard scan.

    Returns the full scorecard report including all findings.
    """
    tenant_id = current_user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Get scan results (with RLS enforcement)
    scan_result = db_pool.execute(
        """
        SELECT
            s.id,
            s.repository_id,
            s.tenant_id,
            s.status,
            s.score,
            s.blocker_count,
            s.warning_count,
            s.info_count,
            s.total_findings,
            s.scan_time_ms,
            s.files_scanned,
            s.report_json,
            s.created_at,
            s.completed_at,
            s.error
        FROM aethyme.scorecard_scans s
        WHERE s.id = %s AND s.tenant_id = %s
        """,
        (scan_id, tenant_id),
    )

    if not scan_result:
        raise HTTPException(status_code=404, detail="Scan not found")

    scan = scan_result[0]

    if scan["status"] == "pending":
        raise HTTPException(status_code=202, detail="Scan is pending")
    elif scan["status"] == "running":
        raise HTTPException(status_code=202, detail="Scan is still running")
    elif scan["status"] == "failed":
        raise HTTPException(
            status_code=500,
            detail=f"Scan failed: {scan.get('error', 'Unknown error')}",
        )

    report_json = scan["report_json"]
    if isinstance(report_json, str):
        report_data = json.loads(report_json)
    elif isinstance(report_json, dict):
        report_data = cast(dict[str, object], report_json)
    else:
        report_data: dict[str, object] = {}

    summary = _coerce_summary(report_data.get("summary"))
    findings = _coerce_findings(report_data.get("findings"))
    detectors = _coerce_detectors(report_data.get("detectors"))
    performance = _coerce_performance(report_data.get("performance"))

    return ScorecardResultResponse(
        scan_id=scan["id"],
        repository_id=scan["repository_id"],
        tenant_id=scan["tenant_id"],
        timestamp=scan["completed_at"] or scan["created_at"],
        score=scan["score"],
        summary=summary,
        findings=findings,
        detectors=detectors,
        performance={
            "total_scan_time_ms": float(
                scan["scan_time_ms"] or performance.get("total_scan_time_ms", 0.0)
            ),
            "files_scanned": int(
                scan["files_scanned"] or performance.get("files_scanned", 0)
            ),
        },
    )


@router.get("/summary/{repository_id}", response_model=ScanSummary)
async def get_latest_summary(
    repository_id: str,
    current_user: RepoReadUser,
) -> ScanSummary:
    """
    Get summary of the latest scorecard scan for a repository.

    Returns high-level metrics without full findings details.
    """
    tenant_id = current_user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Get latest completed scan
    scan_result = db_pool.execute(
        """
        SELECT
            id,
            repository_id,
            score,
            blocker_count,
            warning_count,
            info_count,
            total_findings,
            files_scanned,
            scan_time_ms,
            completed_at
        FROM aethyme.scorecard_scans
        WHERE repository_id = %s
            AND tenant_id = %s
            AND status = 'completed'
        ORDER BY completed_at DESC
        LIMIT 1
        """,
        (repository_id, tenant_id),
    )

    if not scan_result:
        raise HTTPException(
            status_code=404,
            detail="No completed scans found for this repository",
        )

    scan = scan_result[0]

    return ScanSummary(
        scan_id=str(scan["id"]),
        repository_id=repository_id,
        timestamp=scan["completed_at"],
        score=scan["score"],
        blocker_count=scan["blocker_count"],
        warning_count=scan["warning_count"],
        info_count=scan["info_count"],
        total_findings=scan["total_findings"],
        files_scanned=scan["files_scanned"],
        scan_time_ms=scan["scan_time_ms"],
    )


@router.get("/history/{repository_id}")
async def get_scan_history(
    repository_id: str,
    current_user: RepoReadUser,
    limit: int = Query(10, ge=1, le=100, description="Number of scans to return"),
) -> dict[str, str | list[HistoryEntry]]:
    """
    Get scan history for a repository.

    Returns a list of past scans with summary information.
    """
    tenant_id = current_user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Get scan history
    scans = db_pool.execute(
        """
        SELECT
            id,
            status,
            score,
            blocker_count,
            warning_count,
            info_count,
            total_findings,
            created_at,
            completed_at
        FROM aethyme.scorecard_scans
        WHERE repository_id = %s AND tenant_id = %s
        ORDER BY created_at DESC
        LIMIT %s
        """,
        (repository_id, tenant_id, limit),
    )

    scan_history: list[HistoryEntry] = [
        {
            "scan_id": str(scan["id"]),
            "status": scan["status"],
            "score": scan["score"],
            "blocker_count": scan["blocker_count"],
            "warning_count": scan["warning_count"],
            "info_count": scan["info_count"],
            "total_findings": scan["total_findings"],
            "created_at": scan["created_at"],
            "completed_at": scan["completed_at"],
        }
        for scan in scans or []
    ]

    return {
        "repository_id": repository_id,
        "scans": scan_history,
    }


@router.get("/checks")
async def list_available_checks(
    current_user: RepoReadUser,
) -> dict[str, int | list[DetectorInfo]]:
    """List all available scorecard detectors.

    Returns metadata about each available detector type.
    """
    from ...scorecard.detectors import ALL_DETECTORS

    del current_user

    # Create temporary detector instances to get metadata
    temp_path = Path(".")
    detectors_info: list[DetectorInfo] = []

    for detector_class in ALL_DETECTORS:
        try:
            detector = detector_class(temp_path)
            detectors_info.append({
                "name": detector.name,
                "description": detector.description,
            })
        except Exception as exc:
            logger.warning(
                "Failed to load scorecard detector metadata",
                detector_class=getattr(detector_class, "__name__", str(detector_class)),
                error=str(exc),
            )

    return {
        "detectors": detectors_info,
        "count": len(detectors_info),
    }
