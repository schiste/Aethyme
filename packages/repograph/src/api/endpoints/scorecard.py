"""
Scorecard API Endpoints

Provides endpoints for running AI-readiness scorecards and retrieving results.
"""

from fastapi import APIRouter, Depends, HTTPException, Query, BackgroundTasks
from pydantic import BaseModel, Field
from typing import Dict, List, Optional
from datetime import datetime
from pathlib import Path
import uuid
import structlog

from src.api.auth import get_current_user
from src.graph.connection_pool import db_pool
from src.scorecard.engine import ScorecardEngine
from src.scorecard.models import ScorecardReport, ScanSummary, Severity
from src.scorecard.metrics import record_scan_metrics

logger = structlog.get_logger(__name__)
router = APIRouter(prefix="/api/scorecard", tags=["scorecard"])


class ScanRequest(BaseModel):
    """Request to trigger a scorecard scan."""
    repository_id: str = Field(..., description="Repository ID to scan")
    detectors: Optional[List[str]] = Field(None, description="Specific detectors to run")


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
    summary: Dict[str, int]
    findings: Dict[str, List[Dict]]
    detectors: List[Dict]
    performance: Dict[str, float]


@router.post("/scan", response_model=ScanResponse)
async def trigger_scan(
    request: ScanRequest,
    background_tasks: BackgroundTasks,
    user=Depends(get_current_user),
):
    """
    Trigger an AI-readiness scorecard scan for a repository.

    The scan runs asynchronously in the background. Use the scan_id
    to retrieve results via GET /api/scorecard/results/{scan_id}.
    """
    tenant_id = user.get("tenant_id")
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Verify repository exists and user has access (RLS)
    repo_result = db_pool.execute(
        """
        SELECT id, name, path
        FROM repograph.repositories
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
        INSERT INTO repograph.scorecard_scans
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
    detectors: Optional[List[str]] = None,
):
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
            UPDATE repograph.scorecard_scans
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

        result = engine.scan(detectors=detectors)

        # Store results
        db_pool.execute(
            """
            UPDATE repograph.scorecard_scans
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
            UPDATE repograph.scorecard_scans
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
    user=Depends(get_current_user),
):
    """
    Get results of a completed scorecard scan.

    Returns the full scorecard report including all findings.
    """
    tenant_id = user.get("tenant_id")
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
        FROM repograph.scorecard_scans s
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

    # Parse report JSON
    import json
    report_data = json.loads(scan["report_json"])

    return ScorecardResultResponse(**report_data)


@router.get("/summary/{repository_id}", response_model=ScanSummary)
async def get_latest_summary(
    repository_id: str,
    user=Depends(get_current_user),
):
    """
    Get summary of the latest scorecard scan for a repository.

    Returns high-level metrics without full findings details.
    """
    tenant_id = user.get("tenant_id")
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
        FROM repograph.scorecard_scans
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
    user=Depends(get_current_user),
    limit: int = Query(10, ge=1, le=100, description="Number of scans to return"),
):
    """
    Get scan history for a repository.

    Returns a list of past scans with summary information.
    """
    tenant_id = user.get("tenant_id")
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
        FROM repograph.scorecard_scans
        WHERE repository_id = %s AND tenant_id = %s
        ORDER BY created_at DESC
        LIMIT %s
        """,
        (repository_id, tenant_id, limit),
    )

    return {
        "repository_id": repository_id,
        "scans": [
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
            for scan in scans
        ],
    }
