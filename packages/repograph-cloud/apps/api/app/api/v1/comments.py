"""
Comments API endpoints for code collaboration.

Provides REST API for creating, reading, updating, and deleting code comments.
"""

import logging
from typing import List, Optional
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, status, Query
from pydantic import BaseModel, Field
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import get_db
from app.core.security import get_current_user
from app.models.user import User
from app.services.comments import CommentManager
from app.websocket.distributed_connection_manager import distributed_manager

logger = logging.getLogger(__name__)

router = APIRouter()
comment_manager = CommentManager()


# Request/Response Models
class CreateCommentRequest(BaseModel):
    """Request model for creating a comment."""
    location: str = Field(..., description="File/code location")
    line_start: int = Field(..., ge=1, description="Starting line number")
    line_end: Optional[int] = Field(None, ge=1, description="Ending line number")
    column_start: Optional[int] = Field(None, ge=0, description="Starting column")
    column_end: Optional[int] = Field(None, ge=0, description="Ending column")
    content: str = Field(..., min_length=1, max_length=10000, description="Comment text")
    parent_id: Optional[UUID] = Field(None, description="Parent comment ID for replies")
    metadata: Optional[dict] = Field(default_factory=dict, description="Additional metadata")


class UpdateCommentRequest(BaseModel):
    """Request model for updating a comment."""
    content: Optional[str] = Field(None, min_length=1, max_length=10000, description="New comment text")
    metadata: Optional[dict] = Field(None, description="New metadata")


class CommentResponse(BaseModel):
    """Response model for a comment."""
    id: UUID
    user_id: UUID
    location: str
    line_start: int
    line_end: Optional[int]
    column_start: Optional[int]
    column_end: Optional[int]
    content: str
    parent_id: Optional[UUID]
    resolved: bool
    resolved_by: Optional[UUID]
    resolved_at: Optional[str]
    created_at: str
    updated_at: str
    metadata: dict

    class Config:
        from_attributes = True


class CommentWithRepliesResponse(CommentResponse):
    """Response model for a comment with its replies."""
    replies: List[CommentResponse] = []


class CommentCountResponse(BaseModel):
    """Response model for comment count."""
    count: int
    location: Optional[str]
    resolved: Optional[bool]


@router.post("", response_model=CommentResponse, status_code=status.HTTP_201_CREATED)
async def create_comment(
    request: CreateCommentRequest,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Create a new code comment.

    Args:
        request: Comment creation data
        db: Database session
        current_user: Authenticated user

    Returns:
        Created comment

    Raises:
        HTTPException: If parent comment not found (404)
    """
    try:
        comment = await comment_manager.create_comment(
            db=db,
            user_id=current_user.id,
            location=request.location,
            line_start=request.line_start,
            line_end=request.line_end,
            column_start=request.column_start,
            column_end=request.column_end,
            content=request.content,
            parent_id=request.parent_id,
            metadata=request.metadata,
        )

        # Broadcast to other users at this location
        await distributed_manager.broadcast_to_location(
            location=request.location,
            message={
                "type": "comment.created",
                "payload": {
                    "comment_id": str(comment.id),
                    "user_id": str(current_user.id),
                    "location": request.location,
                    "line_start": request.line_start,
                    "content": request.content[:100],  # Preview
                    "parent_id": str(request.parent_id) if request.parent_id else None,
                },
            },
        )

        return comment

    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail=str(e))
    except Exception as e:
        logger.error(f"Failed to create comment: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to create comment",
        )


@router.get("/location/{location:path}", response_model=List[CommentWithRepliesResponse])
async def get_comments_for_location(
    location: str,
    include_resolved: bool = Query(False, description="Include resolved comments"),
    line_start: Optional[int] = Query(None, ge=1, description="Filter by line range start"),
    line_end: Optional[int] = Query(None, ge=1, description="Filter by line range end"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get all comments for a location.

    Args:
        location: File/code location
        include_resolved: Include resolved comments
        line_start: Optional line range start
        line_end: Optional line range end
        db: Database session
        current_user: Authenticated user

    Returns:
        List of comments with replies
    """
    try:
        line_range = None
        if line_start is not None and line_end is not None:
            line_range = (line_start, line_end)

        comments = await comment_manager.get_comments_for_location(
            db=db,
            location=location,
            include_resolved=include_resolved,
            line_range=line_range,
        )

        return comments

    except Exception as e:
        logger.error(f"Failed to get comments for location {location}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve comments",
        )


@router.get("/{comment_id}", response_model=CommentWithRepliesResponse)
async def get_comment(
    comment_id: UUID,
    include_replies: bool = Query(True, description="Include comment replies"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get a single comment by ID.

    Args:
        comment_id: Comment ID
        include_replies: Load replies recursively
        db: Database session
        current_user: Authenticated user

    Returns:
        Comment with optional replies

    Raises:
        HTTPException: If comment not found (404)
    """
    try:
        comment = await comment_manager.get_comment(
            db=db,
            comment_id=comment_id,
            include_replies=include_replies,
        )

        if not comment:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Comment {comment_id} not found",
            )

        return comment

    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve comment",
        )


@router.get("/{comment_id}/thread", response_model=List[CommentResponse])
async def get_comment_thread(
    comment_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get a comment thread (flattened).

    Args:
        comment_id: Root comment ID
        db: Database session
        current_user: Authenticated user

    Returns:
        Flat list of comments in thread

    Raises:
        HTTPException: If comment not found (404)
    """
    try:
        thread = await comment_manager.get_thread(db=db, comment_id=comment_id)

        if not thread:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Comment {comment_id} not found",
            )

        return thread

    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get thread for comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve comment thread",
        )


@router.put("/{comment_id}", response_model=CommentResponse)
async def update_comment(
    comment_id: UUID,
    request: UpdateCommentRequest,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Update a comment.

    Args:
        comment_id: Comment ID
        request: Update data
        db: Database session
        current_user: Authenticated user

    Returns:
        Updated comment

    Raises:
        HTTPException: If comment not found (404) or not authorized (403)
    """
    try:
        # Check ownership
        existing = await comment_manager.get_comment(db=db, comment_id=comment_id)
        if not existing:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Comment {comment_id} not found",
            )

        if existing.user_id != current_user.id:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Not authorized to update this comment",
            )

        comment = await comment_manager.update_comment(
            db=db,
            comment_id=comment_id,
            content=request.content,
            metadata=request.metadata,
        )

        # Broadcast update
        await distributed_manager.broadcast_to_location(
            location=existing.location,
            message={
                "type": "comment.updated",
                "payload": {
                    "comment_id": str(comment_id),
                    "user_id": str(current_user.id),
                },
            },
        )

        return comment

    except HTTPException:
        raise
    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail=str(e))
    except Exception as e:
        logger.error(f"Failed to update comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to update comment",
        )


@router.delete("/{comment_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_comment(
    comment_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Delete a comment and all its replies.

    Args:
        comment_id: Comment ID
        db: Database session
        current_user: Authenticated user

    Raises:
        HTTPException: If comment not found (404) or not authorized (403)
    """
    try:
        # Check ownership
        existing = await comment_manager.get_comment(db=db, comment_id=comment_id)
        if not existing:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Comment {comment_id} not found",
            )

        if existing.user_id != current_user.id:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Not authorized to delete this comment",
            )

        success = await comment_manager.delete_comment(db=db, comment_id=comment_id)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Comment {comment_id} not found",
            )

        # Broadcast deletion
        await distributed_manager.broadcast_to_location(
            location=existing.location,
            message={
                "type": "comment.deleted",
                "payload": {
                    "comment_id": str(comment_id),
                    "user_id": str(current_user.id),
                },
            },
        )

    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to delete comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to delete comment",
        )


@router.post("/{comment_id}/resolve", response_model=CommentResponse)
async def resolve_comment(
    comment_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Mark a comment as resolved.

    Args:
        comment_id: Comment ID
        db: Database session
        current_user: Authenticated user

    Returns:
        Resolved comment

    Raises:
        HTTPException: If comment not found (404)
    """
    try:
        comment = await comment_manager.resolve_comment(
            db=db,
            comment_id=comment_id,
            user_id=current_user.id,
        )

        # Broadcast resolution
        await distributed_manager.broadcast_to_location(
            location=comment.location,
            message={
                "type": "comment.resolved",
                "payload": {
                    "comment_id": str(comment_id),
                    "resolved_by": str(current_user.id),
                },
            },
        )

        return comment

    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail=str(e))
    except Exception as e:
        logger.error(f"Failed to resolve comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to resolve comment",
        )


@router.post("/{comment_id}/unresolve", response_model=CommentResponse)
async def unresolve_comment(
    comment_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Mark a comment as unresolved.

    Args:
        comment_id: Comment ID
        db: Database session
        current_user: Authenticated user

    Returns:
        Unresolved comment

    Raises:
        HTTPException: If comment not found (404)
    """
    try:
        comment = await comment_manager.unresolve_comment(
            db=db,
            comment_id=comment_id,
        )

        # Broadcast unresolve
        await distributed_manager.broadcast_to_location(
            location=comment.location,
            message={
                "type": "comment.unresolved",
                "payload": {
                    "comment_id": str(comment_id),
                    "user_id": str(current_user.id),
                },
            },
        )

        return comment

    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail=str(e))
    except Exception as e:
        logger.error(f"Failed to unresolve comment {comment_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to unresolve comment",
        )


@router.get("/mentions/me", response_model=List[CommentResponse])
async def get_my_mentions(
    unread_only: bool = Query(False, description="Only unread mentions"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get comments that mention the current user.

    Args:
        unread_only: Only return unread mentions
        db: Database session
        current_user: Authenticated user

    Returns:
        List of comments with mentions
    """
    try:
        comments = await comment_manager.get_mentions_for_user(
            db=db,
            user_id=current_user.id,
            unread_only=unread_only,
        )

        return comments

    except Exception as e:
        logger.error(f"Failed to get mentions for user {current_user.id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve mentions",
        )


@router.get("/users/me", response_model=List[CommentResponse])
async def get_my_comments(
    include_resolved: bool = Query(False, description="Include resolved comments"),
    limit: int = Query(50, ge=1, le=100, description="Maximum number of comments"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get comments created by the current user.

    Args:
        include_resolved: Include resolved comments
        limit: Maximum number of comments
        db: Database session
        current_user: Authenticated user

    Returns:
        List of user's comments
    """
    try:
        comments = await comment_manager.get_comments_by_user(
            db=db,
            user_id=current_user.id,
            include_resolved=include_resolved,
            limit=limit,
        )

        return comments

    except Exception as e:
        logger.error(f"Failed to get comments for user {current_user.id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve comments",
        )


@router.get("/count", response_model=CommentCountResponse)
async def get_comment_count(
    location: Optional[str] = Query(None, description="Filter by location"),
    resolved: Optional[bool] = Query(None, description="Filter by resolution status"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get count of comments matching filters.

    Args:
        location: Filter by location
        resolved: Filter by resolution status
        db: Database session
        current_user: Authenticated user

    Returns:
        Comment count
    """
    try:
        count = await comment_manager.get_comment_count(
            db=db,
            location=location,
            resolved=resolved,
        )

        return CommentCountResponse(
            count=count,
            location=location,
            resolved=resolved,
        )

    except Exception as e:
        logger.error(f"Failed to get comment count: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to get comment count",
        )
