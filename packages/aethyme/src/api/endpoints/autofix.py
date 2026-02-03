"""API endpoints for autofixers."""

from fastapi import APIRouter, HTTPException, Depends, BackgroundTasks
from pydantic import BaseModel, Field
from typing import Optional, List, Dict
from pathlib import Path
import uuid
import structlog

from ...auth.middleware import get_current_user, get_tenant_id
from ...autofixers.safety import SafetyEngine
from ...autofixers.patch import PatchGenerator
from ...autofixers.approval import ApprovalWorkflow, ApprovalStatus
from ...autofixers.github import GitHubIntegration
from ...autofixers.fixers import (
    DocsRegenerator,
    LinkFixer,
    SelectorInserter,
    I18nScaffolder,
    FormatFixer,
)

logger = structlog.get_logger()

router = APIRouter(prefix="/autofix", tags=["autofix"])


class DryRunRequest(BaseModel):
    """Request for dry run."""

    repository_path: str = Field(..., description="Path to repository")
    fix_types: List[str] = Field(
        default=["all"],
        description="Types of fixes to apply",
    )


class DryRunResponse(BaseModel):
    """Response for dry run."""

    fix_id: str
    summary: Dict
    patches: List[Dict]
    diff: str


class ApplyRequest(BaseModel):
    """Request to apply fixes."""

    repository_path: str
    fix_types: List[str] = Field(default=["all"])
    skip_approval: bool = Field(default=False)


class ApplyResponse(BaseModel):
    """Response for apply request."""

    fix_id: str
    status: str
    applied: List[str]
    failed: List[str]
    summary: Dict
    approval_id: Optional[str] = None


class PullRequestRequest(BaseModel):
    """Request to create PR."""

    repository_path: str
    fix_types: List[str] = Field(default=["all"])
    base_branch: str = Field(default="main")
    labels: Optional[List[str]] = None


class PullRequestResponse(BaseModel):
    """Response for PR request."""

    fix_id: str
    pr_url: Optional[str]
    pr_number: Optional[str]
    branch: str
    status: str


class ApprovalRequest(BaseModel):
    """Request to approve a fix."""

    comment: Optional[str] = None


class ApprovalResponse(BaseModel):
    """Response for approval."""

    approval_id: str
    status: str
    approved: bool


def collect_fixes(repo_path: Path, fix_types: List[str]) -> List[Dict]:
    """Collect all fixes based on types."""
    all_fixes = []

    if "all" in fix_types or "docs" in fix_types:
        docs_fixer = DocsRegenerator(repo_path)
        docs_fixes = docs_fixer.create_folder_docs()
        all_fixes.extend(docs_fixes)
        logger.info("Collected docs fixes", count=len(docs_fixes))

    if "all" in fix_types or "links" in fix_types:
        link_fixer = LinkFixer(repo_path)
        link_fixes = link_fixer.process_directory()
        all_fixes.extend(link_fixes)
        logger.info("Collected link fixes", count=len(link_fixes))

    if "all" in fix_types or "selectors" in fix_types:
        selector_fixer = SelectorInserter(repo_path)
        selector_fixes = selector_fixer.process_directory()
        all_fixes.extend(selector_fixes)
        logger.info("Collected selector fixes", count=len(selector_fixes))

    if "all" in fix_types or "i18n" in fix_types:
        i18n_fixer = I18nScaffolder(repo_path)
        i18n_fixes = i18n_fixer.process_directory()
        all_fixes.extend(i18n_fixes)
        logger.info("Collected i18n fixes", count=len(i18n_fixes))

    if "all" in fix_types or "format" in fix_types:
        format_fixer = FormatFixer(repo_path)
        format_fixes = format_fixer.process_directory()
        all_fixes.extend(format_fixes)
        logger.info("Collected format fixes", count=len(format_fixes))

    return all_fixes


@router.post("/dry-run", response_model=DryRunResponse)
async def dry_run(
    request: DryRunRequest,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """
    Preview fixes without applying them.

    This endpoint shows what changes would be made without actually modifying files.
    """
    try:
        repo_path = Path(request.repository_path)
        if not repo_path.exists():
            raise HTTPException(status_code=404, detail="Repository not found")

        # Collect fixes
        all_fixes = collect_fixes(repo_path, request.fix_types)

        # Generate patches
        safety_engine = SafetyEngine()
        patch_gen = PatchGenerator(repo_path, safety_engine)

        for fix in all_fixes:
            patch_gen.add_patch(
                fix["file_path"],
                fix["original_content"],
                fix["new_content"],
                fix["fix_type"],
            )

        # Generate dry run result
        result = patch_gen.dry_run()
        fix_id = str(uuid.uuid4())

        logger.info(
            "Dry run completed",
            fix_id=fix_id,
            tenant_id=tenant_id,
            files=result["summary"]["total_files"],
        )

        return DryRunResponse(
            fix_id=fix_id,
            summary=result["summary"],
            patches=result["patches"],
            diff=result["diff"],
        )

    except Exception as e:
        logger.error("Dry run failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/apply", response_model=ApplyResponse)
async def apply_fixes(
    request: ApplyRequest,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """
    Apply fixes to repository.

    Requires approval for medium/high risk changes unless skip_approval is True.
    """
    try:
        repo_path = Path(request.repository_path)
        if not repo_path.exists():
            raise HTTPException(status_code=404, detail="Repository not found")

        # Collect fixes
        all_fixes = collect_fixes(repo_path, request.fix_types)

        # Generate patches
        safety_engine = SafetyEngine()
        patch_gen = PatchGenerator(repo_path, safety_engine)

        for fix in all_fixes:
            patch_gen.add_patch(
                fix["file_path"],
                fix["original_content"],
                fix["new_content"],
                fix["fix_type"],
            )

        fix_id = str(uuid.uuid4())
        summary = patch_gen.get_summary()

        # Check if approval required
        if summary["requires_approval"] > 0 and not request.skip_approval:
            # Create approval request
            approval_workflow = ApprovalWorkflow(tenant_id)
            approval_id = approval_workflow.request_approval(
                fix_id=fix_id,
                fix_type=",".join(request.fix_types),
                risk_level="medium",  # Conservative
                file_count=summary["total_files"],
                summary=summary,
                requested_by=current_user.get("email", "unknown"),
            )

            logger.info(
                "Approval requested",
                fix_id=fix_id,
                approval_id=approval_id,
                files=summary["requires_approval"],
            )

            return ApplyResponse(
                fix_id=fix_id,
                status="requires_approval",
                applied=[],
                failed=[],
                summary=summary,
                approval_id=approval_id,
            )

        # Apply changes
        result = patch_gen.apply(skip_approval=request.skip_approval)

        logger.info(
            "Fixes applied",
            fix_id=fix_id,
            tenant_id=tenant_id,
            applied=len(result.get("applied", [])),
            failed=len(result.get("failed", [])),
        )

        return ApplyResponse(
            fix_id=fix_id,
            status=result["status"],
            applied=result.get("applied", []),
            failed=result.get("failed", []),
            summary=result.get("summary", {}),
        )

    except Exception as e:
        logger.error("Apply failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/pr", response_model=PullRequestResponse)
async def create_pull_request(
    request: PullRequestRequest,
    background_tasks: BackgroundTasks,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """
    Create a pull request with autofixes.

    Creates a new branch, applies fixes, commits, and opens a PR.
    """
    try:
        repo_path = Path(request.repository_path)
        if not repo_path.exists():
            raise HTTPException(status_code=404, detail="Repository not found")

        # Collect fixes
        all_fixes = collect_fixes(repo_path, request.fix_types)

        # Generate patches
        safety_engine = SafetyEngine()
        patch_gen = PatchGenerator(repo_path, safety_engine)

        for fix in all_fixes:
            patch_gen.add_patch(
                fix["file_path"],
                fix["original_content"],
                fix["new_content"],
                fix["fix_type"],
            )

        fix_id = str(uuid.uuid4())

        # Create PR
        gh_integration = GitHubIntegration(repo_path)

        # Check working tree
        if not gh_integration.is_clean_working_tree():
            raise HTTPException(
                status_code=400,
                detail="Working tree has uncommitted changes",
            )

        # Create PR in background
        pr_result = gh_integration.create_autofix_pr(
            patch_gen,
            base_branch=request.base_branch,
            labels=request.labels,
        )

        if not pr_result:
            raise HTTPException(status_code=500, detail="Failed to create PR")

        if pr_result.get("status") == "requires_approval":
            return PullRequestResponse(
                fix_id=fix_id,
                pr_url=None,
                pr_number=None,
                branch=pr_result["branch"],
                status="requires_approval",
            )

        logger.info(
            "PR created",
            fix_id=fix_id,
            tenant_id=tenant_id,
            pr_url=pr_result.get("url"),
        )

        return PullRequestResponse(
            fix_id=fix_id,
            pr_url=pr_result.get("url"),
            pr_number=pr_result.get("number"),
            branch=pr_result["branch"],
            status="created",
        )

    except Exception as e:
        logger.error("PR creation failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/status/{fix_id}")
async def get_status(
    fix_id: str,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """Get status of a fix request."""
    try:
        approval_workflow = ApprovalWorkflow(tenant_id)

        # Try to find approval by fix_id
        audit_trail = approval_workflow.get_audit_trail(fix_id)

        if not audit_trail:
            raise HTTPException(status_code=404, detail="Fix not found")

        return {
            "fix_id": fix_id,
            "audit_trail": audit_trail,
        }

    except HTTPException:
        raise
    except Exception as e:
        logger.error("Status check failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/approve/{approval_id}", response_model=ApprovalResponse)
async def approve_fix(
    approval_id: str,
    request: ApprovalRequest,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """Approve a risky fix."""
    try:
        approval_workflow = ApprovalWorkflow(tenant_id)

        # Check if approval exists
        status = approval_workflow.get_status(approval_id)
        if not status:
            raise HTTPException(status_code=404, detail="Approval not found")

        if status["status"] != "pending":
            raise HTTPException(
                status_code=400,
                detail=f"Approval already {status['status']}",
            )

        # Approve
        success = approval_workflow.approve(
            approval_id,
            reviewed_by=current_user.get("email", "unknown"),
            comment=request.comment,
        )

        if not success:
            raise HTTPException(status_code=500, detail="Failed to approve")

        logger.info(
            "Fix approved",
            approval_id=approval_id,
            tenant_id=tenant_id,
            reviewer=current_user.get("email"),
        )

        return ApprovalResponse(
            approval_id=approval_id,
            status="approved",
            approved=True,
        )

    except HTTPException:
        raise
    except Exception as e:
        logger.error("Approval failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.post("/reject/{approval_id}", response_model=ApprovalResponse)
async def reject_fix(
    approval_id: str,
    request: ApprovalRequest,
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
):
    """Reject a risky fix."""
    try:
        approval_workflow = ApprovalWorkflow(tenant_id)

        # Check if approval exists
        status = approval_workflow.get_status(approval_id)
        if not status:
            raise HTTPException(status_code=404, detail="Approval not found")

        if status["status"] != "pending":
            raise HTTPException(
                status_code=400,
                detail=f"Approval already {status['status']}",
            )

        # Reject
        success = approval_workflow.reject(
            approval_id,
            reviewed_by=current_user.get("email", "unknown"),
            comment=request.comment,
        )

        if not success:
            raise HTTPException(status_code=500, detail="Failed to reject")

        logger.info(
            "Fix rejected",
            approval_id=approval_id,
            tenant_id=tenant_id,
            reviewer=current_user.get("email"),
        )

        return ApprovalResponse(
            approval_id=approval_id,
            status="rejected",
            approved=False,
        )

    except HTTPException:
        raise
    except Exception as e:
        logger.error("Rejection failed", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/pending-approvals")
async def get_pending_approvals(
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
    limit: int = 50,
):
    """Get pending approval requests."""
    try:
        approval_workflow = ApprovalWorkflow(tenant_id)
        approvals = approval_workflow.get_pending_approvals(limit=limit)

        return {
            "approvals": approvals,
            "count": len(approvals),
        }

    except Exception as e:
        logger.error("Failed to get pending approvals", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))


@router.get("/history")
async def get_approval_history(
    current_user: dict = Depends(get_current_user),
    tenant_id: str = Depends(get_tenant_id),
    limit: int = 100,
):
    """Get approval history."""
    try:
        approval_workflow = ApprovalWorkflow(tenant_id)
        history = approval_workflow.get_approval_history(limit=limit)

        return {
            "history": history,
            "count": len(history),
        }

    except Exception as e:
        logger.error("Failed to get approval history", error=str(e))
        raise HTTPException(status_code=500, detail=str(e))
