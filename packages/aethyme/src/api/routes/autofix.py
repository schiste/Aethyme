"""Autofix API endpoints for automatic code fixes."""

from fastapi import APIRouter, Depends, HTTPException, status
from pydantic import BaseModel, Field
from typing import List, Dict, Any, Optional
from datetime import datetime
import structlog

from ..auth import get_current_user

logger = structlog.get_logger(__name__)

router = APIRouter()


# ============================================================================
# Models
# ============================================================================

class FixModel(BaseModel):
    """Model for a single fix."""
    type: str = Field(..., description="Fix type (links, selectors, i18n, etc.)")
    file: str = Field(..., description="File path")
    line: int = Field(..., description="Line number")
    old_content: str = Field(..., description="Original content")
    new_content: str = Field(..., description="Fixed content")
    confidence: float = Field(..., ge=0, le=1, description="Confidence score")
    safe: bool = Field(..., description="Whether fix is considered safe")


class AutofixRequest(BaseModel):
    """Request model for autofix."""
    repo_id: str = Field(..., description="Repository ID")
    fix_types: Optional[List[str]] = Field(None, description="Specific fix types to apply")
    dry_run: bool = Field(True, description="Run in dry-run mode")
    skip_generated: bool = Field(True, description="Skip generated files")
    require_approval: bool = Field(False, description="Require approval for risky fixes")


class AutofixResponse(BaseModel):
    """Response model for autofix."""
    repository_id: str
    timestamp: datetime
    dry_run: bool
    fixes: List[FixModel]
    summary: Dict[str, Any]
    patch: Optional[str] = Field(None, description="Git patch if not dry-run")


class AutofixApplyRequest(BaseModel):
    """Request model to apply specific fixes."""
    repo_id: str = Field(..., description="Repository ID")
    fix_ids: List[str] = Field(..., description="Fix IDs to apply")
    create_pr: bool = Field(False, description="Create pull request")
    branch_name: Optional[str] = Field(None, description="Branch name for PR")


# ============================================================================
# Endpoints
# ============================================================================

@router.post("/run", response_model=AutofixResponse)
async def run_autofix(
    request: AutofixRequest,
    current_user: Dict = Depends(get_current_user),
):
    """Run autofixes on a repository.

    Automatically fixes common issues:
    - Convert absolute to relative links
    - Add missing data-ui selectors
    - Add i18n scaffolding
    - Fix documentation structure
    - Regenerate indices

    **Example:**
    ```json
    {
      "repo_id": "abc123",
      "fix_types": ["links", "selectors"],
      "dry_run": true,
      "skip_generated": true
    }
    ```
    """
    logger.info(
        "Running autofix",
        repo_id=request.repo_id,
        dry_run=request.dry_run,
        user=current_user.get("sub"),
    )

    # Mock autofix implementation (will be fully implemented in Task S1-T5)
    fixes = [
        FixModel(
            type="relative_links",
            file="README.md",
            line=42,
            old_content='[API Docs](/docs/api)',
            new_content='[API Docs](./docs/api)',
            confidence=0.95,
            safe=True,
        ),
        FixModel(
            type="data_ui_selector",
            file="components/Button.tsx",
            line=15,
            old_content='<button onClick={handleClick}>',
            new_content='<button data-ui="submit-button" onClick={handleClick}>',
            confidence=0.85,
            safe=True,
        ),
        FixModel(
            type="i18n",
            file="pages/Login.tsx",
            line=28,
            old_content='<h1>Welcome</h1>',
            new_content='<h1>{t("login.welcome")}</h1>',
            confidence=0.90,
            safe=False,
        ),
    ]

    safe_fixes = [f for f in fixes if f.safe]
    risky_fixes = [f for f in fixes if not f.safe]

    # Generate patch if not dry-run
    patch = None
    if not request.dry_run:
        patch = """diff --git a/README.md b/README.md
index abc123..def456 100644
--- a/README.md
+++ b/README.md
@@ -39,7 +39,7 @@

 ## Documentation

-[API Docs](/docs/api)
+[API Docs](./docs/api)
"""

    return AutofixResponse(
        repository_id=request.repo_id,
        timestamp=datetime.now(),
        dry_run=request.dry_run,
        fixes=fixes,
        summary={
            "total_fixes": len(fixes),
            "safe_fixes": len(safe_fixes),
            "risky_fixes": len(risky_fixes),
            "by_type": {
                "relative_links": 1,
                "data_ui_selector": 1,
                "i18n": 1,
            },
        },
        patch=patch,
    )


@router.post("/apply")
async def apply_fixes(
    request: AutofixApplyRequest,
    current_user: Dict = Depends(get_current_user),
):
    """Apply selected fixes to repository.

    Applies the specified fixes and optionally creates a pull request.
    """
    logger.info(
        "Applying fixes",
        repo_id=request.repo_id,
        fix_count=len(request.fix_ids),
        create_pr=request.create_pr,
        user=current_user.get("sub"),
    )

    # Mock implementation
    result = {
        "repository_id": request.repo_id,
        "timestamp": datetime.now().isoformat(),
        "applied_fixes": len(request.fix_ids),
        "status": "success",
    }

    if request.create_pr:
        result["pull_request"] = {
            "url": "https://github.com/example/repo/pull/123",
            "branch": request.branch_name or "aethyme-autofixes",
            "number": 123,
        }

    return result


@router.get("/types")
async def list_fix_types():
    """List all available autofix types.

    Returns metadata about each available fix type.
    """
    fix_types = [
        {
            "name": "relative_links",
            "description": "Convert absolute links to relative paths",
            "category": "documentation",
            "safe": True,
            "confidence": 0.95,
        },
        {
            "name": "data_ui_selectors",
            "description": "Add data-ui attributes to interactive elements",
            "category": "testing",
            "safe": True,
            "confidence": 0.85,
        },
        {
            "name": "i18n_scaffolding",
            "description": "Add internationalization scaffolding",
            "category": "i18n",
            "safe": False,
            "confidence": 0.80,
        },
        {
            "name": "doc_regeneration",
            "description": "Regenerate FOLDER.md documentation",
            "category": "documentation",
            "safe": True,
            "confidence": 0.90,
        },
        {
            "name": "index_regeneration",
            "description": "Regenerate index files",
            "category": "structure",
            "safe": True,
            "confidence": 0.95,
        },
    ]

    return {
        "fix_types": fix_types,
        "count": len(fix_types),
    }


@router.get("/history/{repo_id}")
async def get_autofix_history(
    repo_id: str,
    limit: int = 10,
    current_user: Dict = Depends(get_current_user),
):
    """Get autofix history for a repository.

    Returns historical autofix runs to track changes over time.
    """
    logger.info("Fetching autofix history", repo_id=repo_id, user=current_user.get("sub"))

    # Mock history data
    history = [
        {
            "timestamp": datetime.now().isoformat(),
            "fixes_applied": 3,
            "dry_run": False,
            "pull_request": "https://github.com/example/repo/pull/123",
        },
    ]

    return {
        "repository_id": repo_id,
        "history": history,
        "count": len(history),
    }
