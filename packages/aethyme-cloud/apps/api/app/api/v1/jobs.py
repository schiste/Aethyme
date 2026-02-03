"""Background job status and management endpoints."""

from typing import Optional
from fastapi import APIRouter, Depends, HTTPException, status
from pydantic import BaseModel
from celery.result import AsyncResult

from app.core.celery_app import celery_app
from app.core.deps import get_current_user
from app.models.user import User
from app.tasks.indexing import index_repository
from app.tasks.github import sync_github_repository


router = APIRouter()


class JobStatus(BaseModel):
    """Job status response schema."""

    job_id: str
    status: str  # PENDING, STARTED, SUCCESS, FAILURE, RETRY, REVOKED
    result: Optional[dict] = None
    error: Optional[str] = None
    progress: Optional[int] = None
    meta: Optional[dict] = None


class IndexRepositoryRequest(BaseModel):
    """Request to trigger repository indexing."""

    repository_id: str


class IndexRepositoryResponse(BaseModel):
    """Response from triggering repository indexing."""

    job_id: str
    repository_id: str
    status: str
    message: str


@router.get("/{job_id}", response_model=JobStatus)
async def get_job_status(
    job_id: str,
    current_user: User = Depends(get_current_user),
):
    """
    Get the status of a background job.

    Returns job state, result, and progress information.
    """
    result = AsyncResult(job_id, app=celery_app)

    # Build response based on job state
    response = {
        "job_id": job_id,
        "status": result.state,
    }

    if result.state == "PENDING":
        response["result"] = None
        response["meta"] = {"status": "Job is waiting to start"}
    elif result.state == "STARTED" or result.state in ["INDEXING", "SYNCING"]:
        # Get progress information
        if result.info:
            response["progress"] = result.info.get("progress")
            response["meta"] = result.info
    elif result.state == "SUCCESS":
        response["result"] = result.result
    elif result.state == "FAILURE":
        response["error"] = str(result.info)
        if isinstance(result.info, dict):
            response["meta"] = result.info
    else:
        response["meta"] = result.info if result.info else {}

    return JobStatus(**response)


@router.post("/index-repository", response_model=IndexRepositoryResponse)
async def trigger_repository_indexing(
    request: IndexRepositoryRequest,
    current_user: User = Depends(get_current_user),
):
    """
    Trigger background indexing for a repository.

    Returns job ID that can be used to check status.
    """
    # Enqueue indexing task
    task = index_repository.delay(request.repository_id)

    return IndexRepositoryResponse(
        job_id=task.id,
        repository_id=request.repository_id,
        status="queued",
        message=f"Repository indexing job queued successfully. Job ID: {task.id}",
    )


@router.post("/sync-repository/{repository_id}")
async def trigger_repository_sync(
    repository_id: str,
    current_user: User = Depends(get_current_user),
):
    """
    Trigger GitHub repository metadata sync.

    Returns job ID that can be used to check status.
    """
    # Enqueue sync task
    task = sync_github_repository.delay(repository_id)

    return {
        "job_id": task.id,
        "repository_id": repository_id,
        "status": "queued",
        "message": f"Repository sync job queued successfully. Job ID: {task.id}",
    }


@router.delete("/{job_id}")
async def cancel_job(
    job_id: str,
    current_user: User = Depends(get_current_user),
):
    """
    Cancel a running or pending job.

    Note: Jobs that are already executing may not stop immediately.
    """
    result = AsyncResult(job_id, app=celery_app)

    if result.state in ["PENDING", "STARTED", "RETRY"]:
        result.revoke(terminate=True)
        return {
            "job_id": job_id,
            "status": "cancelled",
            "message": "Job cancellation requested",
        }
    elif result.state == "SUCCESS":
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Cannot cancel completed job",
        )
    elif result.state == "FAILURE":
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST, detail="Cannot cancel failed job"
        )
    else:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND, detail="Job not found"
        )
