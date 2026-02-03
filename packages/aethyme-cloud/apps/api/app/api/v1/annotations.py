"""
Annotations API endpoints for code highlights.

Provides REST API for creating, reading, updating, and deleting code annotations.
"""

import logging
from typing import List, Optional
from uuid import UUID
from datetime import datetime

from fastapi import APIRouter, Depends, HTTPException, status, Query
from pydantic import BaseModel, Field
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import get_db
from app.core.security import get_current_user
from app.models.user import User
from app.services.annotations import AnnotationManager
from app.websocket.distributed_connection_manager import distributed_manager

logger = logging.getLogger(__name__)

router = APIRouter()
annotation_manager = AnnotationManager()


# Request/Response Models
class CreateAnnotationRequest(BaseModel):
    """Request model for creating an annotation."""
    location: str = Field(..., description="File/code location")
    line_start: int = Field(..., ge=1, description="Start line")
    line_end: int = Field(..., ge=1, description="End line")
    column_start: Optional[int] = Field(None, ge=0, description="Start column")
    column_end: Optional[int] = Field(None, ge=0, description="End column")
    color: str = Field("yellow", description="Highlight color")
    label: Optional[str] = Field(None, max_length=200, description="Label/title")
    description: Optional[str] = Field(None, description="Description")
    is_temporary: bool = Field(False, description="Temporary annotation")
    is_public: bool = Field(True, description="Visible to others")
    expires_at: Optional[datetime] = Field(None, description="Expiration datetime")
    metadata: Optional[dict] = Field(default_factory=dict, description="Additional metadata")


class UpdateAnnotationRequest(BaseModel):
    """Request model for updating an annotation."""
    color: Optional[str] = Field(None, description="New color")
    label: Optional[str] = Field(None, max_length=200, description="New label")
    description: Optional[str] = Field(None, description="New description")
    is_public: Optional[bool] = Field(None, description="New visibility")
    expires_at: Optional[datetime] = Field(None, description="New expiration")
    metadata: Optional[dict] = Field(None, description="New metadata")


class AnnotationResponse(BaseModel):
    """Response model for an annotation."""
    id: UUID
    user_id: UUID
    location: str
    line_start: int
    line_end: int
    column_start: Optional[int]
    column_end: Optional[int]
    color: str
    label: Optional[str]
    description: Optional[str]
    is_temporary: bool
    is_public: bool
    created_at: str
    updated_at: str
    expires_at: Optional[str]
    metadata: dict

    class Config:
        from_attributes = True


@router.post("", response_model=AnnotationResponse, status_code=status.HTTP_201_CREATED)
async def create_annotation(
    request: CreateAnnotationRequest,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Create a new code annotation.

    Args:
        request: Annotation creation data
        db: Database session
        current_user: Authenticated user

    Returns:
        Created annotation
    """
    try:
        annotation = await annotation_manager.create_annotation(
            db=db,
            user_id=current_user.id,
            location=request.location,
            line_start=request.line_start,
            line_end=request.line_end,
            column_start=request.column_start,
            column_end=request.column_end,
            color=request.color,
            label=request.label,
            description=request.description,
            is_temporary=request.is_temporary,
            is_public=request.is_public,
            expires_at=request.expires_at,
            metadata=request.metadata,
        )

        # Broadcast to other users at this location
        if annotation.is_public:
            await distributed_manager.broadcast_to_location(
                location=request.location,
                message={
                    "type": "annotation.created",
                    "payload": annotation.to_dict(),
                },
                exclude_connection_id=None,
            )

        return annotation

    except Exception as e:
        logger.error(f"Failed to create annotation: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to create annotation",
        )


@router.get("/location/{location:path}", response_model=List[AnnotationResponse])
async def get_annotations_for_location(
    location: str,
    include_private: bool = Query(False, description="Include private annotations"),
    line_start: Optional[int] = Query(None, ge=1, description="Filter by line range start"),
    line_end: Optional[int] = Query(None, ge=1, description="Filter by line range end"),
    color: Optional[str] = Query(None, description="Filter by color"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get all annotations for a location.

    Args:
        location: File/code location
        include_private: Include user's private annotations
        line_start: Optional line range start
        line_end: Optional line range end
        color: Optional color filter
        db: Database session
        current_user: Authenticated user

    Returns:
        List of annotations
    """
    try:
        line_range = None
        if line_start is not None and line_end is not None:
            line_range = (line_start, line_end)

        annotations = await annotation_manager.get_annotations_for_location(
            db=db,
            location=location,
            include_private=include_private,
            user_id=current_user.id if include_private else None,
            line_range=line_range,
            color=color,
        )

        return annotations

    except Exception as e:
        logger.error(f"Failed to get annotations for {location}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve annotations",
        )


@router.get("/{annotation_id}", response_model=AnnotationResponse)
async def get_annotation(
    annotation_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get a single annotation by ID.

    Args:
        annotation_id: Annotation UUID
        db: Database session
        current_user: Authenticated user

    Returns:
        Annotation

    Raises:
        HTTPException: If annotation not found (404)
    """
    try:
        annotation = await annotation_manager.get_annotation(db=db, annotation_id=annotation_id)

        if not annotation:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Annotation {annotation_id} not found",
            )

        # Check access rights
        if not annotation.is_public and annotation.user_id != current_user.id:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Not authorized to view this annotation",
            )

        return annotation

    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get annotation {annotation_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve annotation",
        )


@router.put("/{annotation_id}", response_model=AnnotationResponse)
async def update_annotation(
    annotation_id: UUID,
    request: UpdateAnnotationRequest,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Update an annotation.

    Args:
        annotation_id: Annotation UUID
        request: Update data
        db: Database session
        current_user: Authenticated user

    Returns:
        Updated annotation

    Raises:
        HTTPException: If annotation not found (404) or not authorized (403)
    """
    try:
        # Check ownership
        existing = await annotation_manager.get_annotation(db=db, annotation_id=annotation_id)
        if not existing:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Annotation {annotation_id} not found",
            )

        if existing.user_id != current_user.id:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Not authorized to update this annotation",
            )

        annotation = await annotation_manager.update_annotation(
            db=db,
            annotation_id=annotation_id,
            color=request.color,
            label=request.label,
            description=request.description,
            is_public=request.is_public,
            expires_at=request.expires_at,
            metadata=request.metadata,
        )

        # Broadcast update
        await distributed_manager.broadcast_to_location(
            location=annotation.location,
            message={
                "type": "annotation.updated",
                "payload": annotation.to_dict(),
            },
        )

        return annotation

    except HTTPException:
        raise
    except ValueError as e:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail=str(e))
    except Exception as e:
        logger.error(f"Failed to update annotation {annotation_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to update annotation",
        )


@router.delete("/{annotation_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_annotation(
    annotation_id: UUID,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Delete an annotation.

    Args:
        annotation_id: Annotation UUID
        db: Database session
        current_user: Authenticated user

    Raises:
        HTTPException: If annotation not found (404) or not authorized (403)
    """
    try:
        # Check ownership
        existing = await annotation_manager.get_annotation(db=db, annotation_id=annotation_id)
        if not existing:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Annotation {annotation_id} not found",
            )

        if existing.user_id != current_user.id:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Not authorized to delete this annotation",
            )

        success = await annotation_manager.delete_annotation(db=db, annotation_id=annotation_id)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Annotation {annotation_id} not found",
            )

        # Broadcast deletion
        await distributed_manager.broadcast_to_location(
            location=existing.location,
            message={
                "type": "annotation.deleted",
                "payload": {
                    "annotation_id": str(annotation_id),
                    "user_id": str(current_user.id),
                },
            },
        )

    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to delete annotation {annotation_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to delete annotation",
        )


@router.get("/users/me", response_model=List[AnnotationResponse])
async def get_my_annotations(
    location: Optional[str] = Query(None, description="Filter by location"),
    limit: int = Query(50, ge=1, le=100, description="Maximum number of annotations"),
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user),
):
    """
    Get annotations created by the current user.

    Args:
        location: Optional location filter
        limit: Maximum number of annotations
        db: Database session
        current_user: Authenticated user

    Returns:
        List of user's annotations
    """
    try:
        annotations = await annotation_manager.get_annotations_by_user(
            db=db,
            user_id=current_user.id,
            location=location,
            limit=limit,
        )

        return annotations

    except Exception as e:
        logger.error(f"Failed to get annotations for user {current_user.id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve annotations",
        )
