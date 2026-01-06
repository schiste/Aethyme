"""Webhook endpoints for repository updates from GitHub, GitLab, and Bitbucket."""

import hmac
import hashlib
import logging
from typing import Optional
from fastapi import APIRouter, Request, HTTPException, Header, status, Depends
from sqlalchemy.orm import Session

from app.core.database import get_db
from app.models.repository import Repository
from app.workers.tasks.indexing import reindex_files_task
from app.core.config import settings

logger = logging.getLogger(__name__)
router = APIRouter()


def verify_github_signature(payload: bytes, signature: str, secret: str) -> bool:
    """Verify GitHub webhook signature.

    GitHub sends signature as: sha256=<hash>
    """
    if not signature:
        return False

    if not signature.startswith("sha256="):
        return False

    expected_signature = signature.split("=")[1]

    # Compute HMAC
    mac = hmac.new(
        secret.encode("utf-8"),
        msg=payload,
        digestmod=hashlib.sha256
    )
    computed_signature = mac.hexdigest()

    # Constant-time comparison
    return hmac.compare_digest(computed_signature, expected_signature)


def verify_gitlab_token(token: str, secret: str) -> bool:
    """Verify GitLab webhook token.

    GitLab sends token in X-Gitlab-Token header.
    """
    if not token:
        return False

    # Constant-time comparison
    return hmac.compare_digest(token, secret)


def verify_bitbucket_signature(payload: bytes, signature: str, secret: str) -> bool:
    """Verify Bitbucket webhook signature.

    Bitbucket sends signature as: sha256=<hash>
    """
    if not signature:
        return False

    if not signature.startswith("sha256="):
        return False

    expected_signature = signature.split("=")[1]

    # Compute HMAC
    mac = hmac.new(
        secret.encode("utf-8"),
        msg=payload,
        digestmod=hashlib.sha256
    )
    computed_signature = mac.hexdigest()

    # Constant-time comparison
    return hmac.compare_digest(computed_signature, expected_signature)


@router.post("/github")
async def github_webhook(
    request: Request,
    x_hub_signature_256: Optional[str] = Header(None),
    x_github_event: Optional[str] = Header(None),
    db: Session = Depends(get_db),
):
    """Handle GitHub webhook events.

    Supported events:
    - push: Triggered when commits are pushed
    - delete: Triggered when a branch/tag is deleted

    GitHub Webhook Documentation:
    https://docs.github.com/en/webhooks/webhook-events-and-payloads
    """
    # Read raw body for signature verification
    body = await request.body()

    # Verify signature (if secret is configured)
    if settings.GITHUB_WEBHOOK_SECRET:
        if not x_hub_signature_256:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Missing X-Hub-Signature-256 header"
            )

        if not verify_github_signature(
            body,
            x_hub_signature_256,
            settings.GITHUB_WEBHOOK_SECRET
        ):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Invalid signature"
            )

    # Parse JSON payload
    payload = await request.json()

    logger.info(f"Received GitHub webhook event: {x_github_event}")

    # Handle push event
    if x_github_event == "push":
        repository_data = payload.get("repository", {})
        full_name = repository_data.get("full_name")  # e.g., "owner/repo"
        ref = payload.get("ref")  # e.g., "refs/heads/main"

        if not full_name:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Missing repository full_name"
            )

        # Find repository in database
        repository = db.query(Repository).filter(
            Repository.full_name == full_name
        ).first()

        if not repository:
            logger.warning(f"Repository {full_name} not found in database")
            return {"status": "ignored", "reason": "repository_not_found"}

        # Get modified files from commits
        modified_files = set()
        added_files = set()
        removed_files = set()

        for commit in payload.get("commits", []):
            modified_files.update(commit.get("modified", []))
            added_files.update(commit.get("added", []))
            removed_files.update(commit.get("removed", []))

        all_changed_files = list(modified_files | added_files)

        logger.info(
            f"GitHub push to {full_name} ({ref}): "
            f"{len(all_changed_files)} files changed, "
            f"{len(removed_files)} files removed"
        )

        # Trigger incremental re-indexing
        if all_changed_files or removed_files:
            task = reindex_files_task.delay(
                repository_id=str(repository.id),
                file_paths=all_changed_files,
                deleted_file_paths=list(removed_files)
            )

            logger.info(f"Triggered re-indexing task {task.id} for {full_name}")

            # Also trigger incremental embedding generation if repository has embeddings
            from app.workers.tasks.embeddings import (
                generate_embeddings_for_files_task,
                delete_embeddings_for_files_task
            )

            embedding_task_id = None
            if all_changed_files:
                # Generate embeddings for changed files
                # Note: This will use default provider (OpenAI) and model
                # TODO: Store user's preferred provider/model in repository settings
                try:
                    embedding_task = generate_embeddings_for_files_task.delay(
                        user_id=repository.owner_id,
                        repository_id=str(repository.id),
                        file_paths=all_changed_files,
                    )
                    embedding_task_id = embedding_task.id
                    logger.info(f"Triggered embedding generation task {embedding_task_id}")
                except Exception as e:
                    logger.warning(f"Failed to trigger embedding generation: {e}")

            if removed_files:
                # Delete embeddings for removed files
                try:
                    delete_embeddings_for_files_task.delay(
                        repository_id=str(repository.id),
                        file_paths=list(removed_files),
                    )
                    logger.info(f"Triggered embedding deletion for {len(removed_files)} files")
                except Exception as e:
                    logger.warning(f"Failed to trigger embedding deletion: {e}")

            return {
                "status": "accepted",
                "task_id": task.id,
                "embedding_task_id": embedding_task_id,
                "files_to_reindex": len(all_changed_files),
                "files_to_delete": len(removed_files)
            }
        else:
            return {"status": "ignored", "reason": "no_file_changes"}

    # Handle delete event (branch/tag deletion)
    elif x_github_event == "delete":
        # For now, we don't need to do anything
        # Full re-index would be triggered manually if needed
        return {"status": "ignored", "reason": "delete_event_not_handled"}

    # Unsupported event
    else:
        return {"status": "ignored", "reason": "unsupported_event"}


@router.post("/gitlab")
async def gitlab_webhook(
    request: Request,
    x_gitlab_token: Optional[str] = Header(None),
    x_gitlab_event: Optional[str] = Header(None),
    db: Session = Depends(get_db),
):
    """Handle GitLab webhook events.

    Supported events:
    - Push Hook: Triggered when commits are pushed
    - Tag Push Hook: Triggered when tags are pushed

    GitLab Webhook Documentation:
    https://docs.gitlab.com/ee/user/project/integrations/webhooks.html
    """
    # Verify token (if secret is configured)
    if settings.GITLAB_WEBHOOK_SECRET:
        if not x_gitlab_token:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Missing X-Gitlab-Token header"
            )

        if not verify_gitlab_token(x_gitlab_token, settings.GITLAB_WEBHOOK_SECRET):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Invalid token"
            )

    # Parse JSON payload
    payload = await request.json()

    logger.info(f"Received GitLab webhook event: {x_gitlab_event}")

    # Handle push event
    if x_gitlab_event == "Push Hook":
        project = payload.get("project", {})
        path_with_namespace = project.get("path_with_namespace")  # e.g., "owner/repo"
        ref = payload.get("ref")  # e.g., "refs/heads/main"

        if not path_with_namespace:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Missing project path_with_namespace"
            )

        # Find repository in database
        repository = db.query(Repository).filter(
            Repository.full_name == path_with_namespace
        ).first()

        if not repository:
            logger.warning(f"Repository {path_with_namespace} not found in database")
            return {"status": "ignored", "reason": "repository_not_found"}

        # Get modified files from commits
        modified_files = set()
        added_files = set()
        removed_files = set()

        for commit in payload.get("commits", []):
            modified_files.update(commit.get("modified", []))
            added_files.update(commit.get("added", []))
            removed_files.update(commit.get("removed", []))

        all_changed_files = list(modified_files | added_files)

        logger.info(
            f"GitLab push to {path_with_namespace} ({ref}): "
            f"{len(all_changed_files)} files changed, "
            f"{len(removed_files)} files removed"
        )

        # Trigger incremental re-indexing
        if all_changed_files or removed_files:
            task = reindex_files_task.delay(
                repository_id=str(repository.id),
                file_paths=all_changed_files,
                deleted_file_paths=list(removed_files)
            )

            logger.info(f"Triggered re-indexing task {task.id} for {path_with_namespace}")

            return {
                "status": "accepted",
                "task_id": task.id,
                "files_to_reindex": len(all_changed_files),
                "files_to_delete": len(removed_files)
            }
        else:
            return {"status": "ignored", "reason": "no_file_changes"}

    # Unsupported event
    else:
        return {"status": "ignored", "reason": "unsupported_event"}


@router.post("/bitbucket")
async def bitbucket_webhook(
    request: Request,
    x_hub_signature: Optional[str] = Header(None),
    x_event_key: Optional[str] = Header(None),
    db: Session = Depends(get_db),
):
    """Handle Bitbucket webhook events.

    Supported events:
    - repo:push: Triggered when commits are pushed

    Bitbucket Webhook Documentation:
    https://support.atlassian.com/bitbucket-cloud/docs/manage-webhooks/
    """
    # Read raw body for signature verification
    body = await request.body()

    # Verify signature (if secret is configured)
    if settings.BITBUCKET_WEBHOOK_SECRET:
        if not x_hub_signature:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Missing X-Hub-Signature header"
            )

        if not verify_bitbucket_signature(
            body,
            x_hub_signature,
            settings.BITBUCKET_WEBHOOK_SECRET
        ):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Invalid signature"
            )

    # Parse JSON payload
    payload = await request.json()

    logger.info(f"Received Bitbucket webhook event: {x_event_key}")

    # Handle push event
    if x_event_key == "repo:push":
        repository_data = payload.get("repository", {})
        full_name = repository_data.get("full_name")  # e.g., "owner/repo"

        if not full_name:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Missing repository full_name"
            )

        # Find repository in database
        repository = db.query(Repository).filter(
            Repository.full_name == full_name
        ).first()

        if not repository:
            logger.warning(f"Repository {full_name} not found in database")
            return {"status": "ignored", "reason": "repository_not_found"}

        # Bitbucket webhook doesn't include file lists in push events
        # We need to trigger a full re-index or use Bitbucket API to get changed files
        # For now, trigger full re-index

        logger.info(f"Bitbucket push to {full_name}: triggering full re-index")

        from app.workers.tasks.indexing import index_repository_task

        task = index_repository_task.delay(
            repository_id=str(repository.id),
            full_reindex=False  # Incremental update
        )

        logger.info(f"Triggered re-indexing task {task.id} for {full_name}")

        return {
            "status": "accepted",
            "task_id": task.id,
            "mode": "incremental_reindex"
        }

    # Unsupported event
    else:
        return {"status": "ignored", "reason": "unsupported_event"}


@router.get("/health")
async def webhook_health():
    """Health check for webhook endpoints."""
    return {
        "status": "ok",
        "endpoints": {
            "github": "/webhooks/github",
            "gitlab": "/webhooks/gitlab",
            "bitbucket": "/webhooks/bitbucket"
        },
        "signature_verification": {
            "github": bool(settings.GITHUB_WEBHOOK_SECRET),
            "gitlab": bool(settings.GITLAB_WEBHOOK_SECRET),
            "bitbucket": bool(settings.BITBUCKET_WEBHOOK_SECRET)
        }
    }
