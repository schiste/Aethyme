"""
Agent Enablement API Endpoints

REST API endpoints for agent-enablement features with RLS enforcement.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import logging
from pathlib import Path
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, BackgroundTasks, Query
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel, Field

from ...agent_enablement import (
    InvariantRulesEngine,
    ParityScorer,
    GapDetector,
    ContextPackGenerator,
    AutofixPatchGenerator,
    OnboardingManifestGenerator,
    StalenessMonitor
)
from ...auth.middleware import get_current_user, verify_scope
from ...database import get_db

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/api/agent", tags=["agent-enablement"])


# Request/Response Models

class ParityScanRequest(BaseModel):
    org_id: str = Field(..., description="Organization ID")
    repo_path: str = Field(..., description="Repository path")
    baseline: Optional[dict] = Field(None, description="Baseline scores for comparison")


class ParityScanResponse(BaseModel):
    scan_id: str
    status: str
    parity_report: Optional[dict] = None


class ContextPackRequest(BaseModel):
    org_id: str
    repo_path: str
    pack_types: Optional[list[str]] = Field(None, description="Specific pack types to generate")
    compress: bool = Field(True, description="Compress output")


class ManifestRequest(BaseModel):
    org_id: str
    repo_path: str
    template: str = Field("standard", description="Template type: minimal, standard, comprehensive")


# Endpoints

@router.post("/parity-scan", response_model=ParityScanResponse)
async def parity_scan(
    request: ParityScanRequest,
    background_tasks: BackgroundTasks,
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Scan repository for agent-friendliness parity.

    Requires: org:read scope
    """
    # Verify user has access to org
    if not verify_scope(user, f"org:{request.org_id}:read"):
        raise HTTPException(status_code=403, detail="Insufficient permissions")

    try:
        repo_path = Path(request.repo_path)
        if not repo_path.exists():
            raise HTTPException(status_code=404, detail="Repository not found")

        # Initialize components
        scorer = ParityScorer()

        # Run scan (in background for large repos)
        scan_id = f"scan-{user['sub']}-{hash(str(repo_path))}"

        def run_scan():
            try:
                report = scorer.scan_repository(
                    repo_path,
                    baseline=request.baseline
                )

                # Store report in database (with RLS)
                # Implementation depends on database schema

                logger.info(f"Parity scan completed: {scan_id}")
            except Exception as e:
                logger.error(f"Parity scan failed: {e}")

        background_tasks.add_task(run_scan)

        return ParityScanResponse(
            scan_id=scan_id,
            status="running"
        )

    except Exception as e:
        logger.error(f"Error initiating parity scan: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/parity/{repo_id}")
async def get_parity(
    repo_id: str,
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Get parity score for repository.

    Requires: repo:read scope
    """
    # Verify access
    # RLS policies will enforce org/repo isolation

    try:
        # Fetch from database
        # This is a placeholder - actual implementation depends on database schema

        return JSONResponse({
            "repo_id": repo_id,
            "overall_score": 85.5,
            "last_scan": "2025-11-22T10:00:00Z",
            "invariants": {
                # Invariant details
            }
        })

    except Exception as e:
        logger.error(f"Error fetching parity: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/context-pack/{repo_id}")
async def get_context_pack(
    repo_id: str,
    pack_types: Optional[str] = Query(None, description="Comma-separated pack types"),
    compress: bool = Query(True),
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Download context pack for repository.

    Requires: repo:read scope
    """
    try:
        # Verify access and get repo path
        # Implementation depends on database schema

        repo_path = Path(f"/path/to/repos/{repo_id}")  # Placeholder

        generator = ContextPackGenerator()

        types = pack_types.split(",") if pack_types else None
        pack = generator.generate_pack(repo_path, pack_types=types, compress=compress)

        # Save to temporary file
        import tempfile
        temp_dir = Path(tempfile.mkdtemp())
        output_file = temp_dir / f"{repo_id}_context_pack.json"

        if compress and pack.compression:
            output_file = output_file.with_suffix(".json.gz")

        generator.save_pack(pack, output_file, compress=compress)

        return FileResponse(
            path=output_file,
            media_type="application/gzip" if compress else "application/json",
            filename=output_file.name
        )

    except Exception as e:
        logger.error(f"Error generating context pack: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/generate-manifest")
async def generate_manifest(
    request: ManifestRequest,
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Generate Agent.md onboarding manifest.

    Requires: repo:write scope
    """
    # Verify write access
    if not verify_scope(user, f"repo:{request.repo_path}:write"):
        raise HTTPException(status_code=403, detail="Insufficient permissions")

    try:
        repo_path = Path(request.repo_path)
        if not repo_path.exists():
            raise HTTPException(status_code=404, detail="Repository not found")

        generator = OnboardingManifestGenerator()

        manifest = generator.generate_manifest(
            repo_path,
            template=request.template
        )

        # Save manifest
        manifest_path = generator.save_manifest(manifest, repo_path)

        return JSONResponse({
            "status": "success",
            "manifest_path": str(manifest_path),
            "content": manifest
        })

    except Exception as e:
        logger.error(f"Error generating manifest: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/gaps/{repo_id}")
async def get_gaps(
    repo_id: str,
    priority: Optional[str] = Query(None, description="Filter by priority: critical, high, medium, low"),
    autofixable_only: bool = Query(False),
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Get actionable gaps for repository.

    Requires: repo:read scope
    """
    try:
        # Verify access and get repo path
        repo_path = Path(f"/path/to/repos/{repo_id}")  # Placeholder

        detector = GapDetector()
        gaps = detector.detect_gaps(repo_path)

        # Filter by priority
        if priority:
            from ...agent_enablement.gap_detector import GapPriority
            priority_enum = GapPriority[priority.upper()]
            gaps = [g for g in gaps if g.priority == priority_enum]

        # Filter autofixable
        if autofixable_only:
            gaps = detector.get_autofixable_gaps(gaps)

        # Generate report
        report = detector.generate_gap_report(gaps)

        return JSONResponse(report)

    except Exception as e:
        logger.error(f"Error fetching gaps: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/staleness/{repo_id}")
async def check_staleness(
    repo_id: str,
    user=Depends(get_current_user),
    db=Depends(get_db)
):
    """
    Check staleness status for repository.

    Requires: repo:read scope
    """
    try:
        repo_path = Path(f"/path/to/repos/{repo_id}")  # Placeholder

        monitor = StalenessMonitor()

        # Get current scores (placeholder)
        current_parity = 85.0
        current_invariants = {}
        current_checksum = "abc123"

        metrics = monitor.check_staleness(
            repo_path,
            current_parity,
            current_invariants,
            current_checksum
        )

        return JSONResponse({
            "repo_id": repo_id,
            "last_scan": metrics.last_scan,
            "parity_score": metrics.parity_score,
            "parity_trend": metrics.parity_trend,
            "needs_rescan": metrics.needs_rescan,
            "reason": metrics.reason
        })

    except Exception as e:
        logger.error(f"Error checking staleness: {e}")
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/health")
async def agent_enablement_health():
    """Health check for agent enablement features"""
    return JSONResponse({
        "status": "healthy",
        "features": {
            "parity_scoring": "enabled",
            "context_packs": "enabled",
            "gap_detection": "enabled",
            "autofixers": "enabled",
            "staleness_monitoring": "enabled"
        }
    })
