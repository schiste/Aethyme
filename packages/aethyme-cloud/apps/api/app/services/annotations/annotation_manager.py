"""Annotation manager for code highlights and annotations."""

import logging
from typing import List, Optional
from uuid import UUID
from datetime import datetime
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, delete, and_, or_

from app.models.collaboration.annotation import CodeAnnotation

logger = logging.getLogger(__name__)


class AnnotationManager:
    """
    Manages code annotations including highlights, markers, and labels.
    """

    async def create_annotation(
        self,
        db: AsyncSession,
        user_id: UUID,
        location: str,
        line_start: int,
        line_end: int,
        color: str = "yellow",
        label: Optional[str] = None,
        description: Optional[str] = None,
        column_start: Optional[int] = None,
        column_end: Optional[int] = None,
        is_temporary: bool = False,
        is_public: bool = True,
        expires_at: Optional[datetime] = None,
        metadata: Optional[dict] = None,
    ) -> CodeAnnotation:
        """
        Create a new code annotation.

        Args:
            db: Database session
            user_id: User creating the annotation
            location: File location
            line_start: Start line
            line_end: End line
            color: Highlight color (yellow, green, blue, red, purple, orange)
            label: Optional label/title
            description: Optional longer description
            column_start: Optional start column
            column_end: Optional end column
            is_temporary: Whether annotation is temporary
            is_public: Whether annotation is visible to others
            expires_at: Optional expiration datetime
            metadata: Additional metadata

        Returns:
            Created CodeAnnotation
        """
        annotation = CodeAnnotation(
            user_id=user_id,
            location=location,
            line_start=line_start,
            line_end=line_end,
            column_start=column_start,
            column_end=column_end,
            color=color,
            label=label,
            description=description,
            is_temporary=is_temporary,
            is_public=is_public,
            expires_at=expires_at,
            metadata=metadata or {},
        )

        db.add(annotation)
        await db.commit()
        await db.refresh(annotation)

        logger.info(f"Created annotation {annotation.id} at {location}:{line_start}-{line_end}")
        return annotation

    async def get_annotation(
        self,
        db: AsyncSession,
        annotation_id: UUID,
    ) -> Optional[CodeAnnotation]:
        """
        Get a single annotation by ID.

        Args:
            db: Database session
            annotation_id: Annotation UUID

        Returns:
            CodeAnnotation or None
        """
        result = await db.execute(
            select(CodeAnnotation).where(CodeAnnotation.id == annotation_id)
        )
        return result.scalar_one_or_none()

    async def get_annotations_for_location(
        self,
        db: AsyncSession,
        location: str,
        include_private: bool = False,
        include_expired: bool = False,
        user_id: Optional[UUID] = None,
        line_range: Optional[tuple[int, int]] = None,
        color: Optional[str] = None,
    ) -> List[CodeAnnotation]:
        """
        Get all annotations for a location with filters.

        Args:
            db: Database session
            location: File location
            include_private: Include private annotations
            include_expired: Include expired annotations
            user_id: Filter by user (required if include_private=True)
            line_range: Optional (start_line, end_line) tuple
            color: Optional color filter

        Returns:
            List of CodeAnnotation objects
        """
        query = select(CodeAnnotation).where(CodeAnnotation.location == location)

        # Public/private filter
        if not include_private:
            query = query.where(CodeAnnotation.is_public.is_(True))
        elif user_id:
            # Include public + user's private annotations
            query = query.where(
                or_(
                    CodeAnnotation.is_public.is_(True),
                    CodeAnnotation.user_id == user_id,
                )
            )

        # Expiration filter
        if not include_expired:
            query = query.where(
                or_(
                    CodeAnnotation.expires_at.is_(None),
                    CodeAnnotation.expires_at > datetime.utcnow(),
                )
            )

        # Line range filter
        if line_range:
            start, end = line_range
            query = query.where(
                and_(
                    CodeAnnotation.line_start <= end,
                    CodeAnnotation.line_end >= start,
                )
            )

        # Color filter
        if color:
            query = query.where(CodeAnnotation.color == color)

        query = query.order_by(CodeAnnotation.line_start, CodeAnnotation.created_at)

        result = await db.execute(query)
        return list(result.scalars().all())

    async def get_annotations_by_user(
        self,
        db: AsyncSession,
        user_id: UUID,
        location: Optional[str] = None,
        limit: int = 50,
    ) -> List[CodeAnnotation]:
        """
        Get annotations created by a user.

        Args:
            db: Database session
            user_id: User UUID
            location: Optional location filter
            limit: Maximum number of results

        Returns:
            List of CodeAnnotation objects
        """
        query = select(CodeAnnotation).where(CodeAnnotation.user_id == user_id)

        if location:
            query = query.where(CodeAnnotation.location == location)

        query = query.order_by(CodeAnnotation.created_at.desc()).limit(limit)

        result = await db.execute(query)
        return list(result.scalars().all())

    async def update_annotation(
        self,
        db: AsyncSession,
        annotation_id: UUID,
        color: Optional[str] = None,
        label: Optional[str] = None,
        description: Optional[str] = None,
        is_public: Optional[bool] = None,
        expires_at: Optional[datetime] = None,
        metadata: Optional[dict] = None,
    ) -> Optional[CodeAnnotation]:
        """
        Update an existing annotation.

        Args:
            db: Database session
            annotation_id: Annotation UUID
            color: New color
            label: New label
            description: New description
            is_public: New public/private status
            expires_at: New expiration datetime
            metadata: New metadata

        Returns:
            Updated CodeAnnotation or None

        Raises:
            ValueError: If annotation not found
        """
        annotation = await self.get_annotation(db, annotation_id)
        if not annotation:
            raise ValueError(f"Annotation {annotation_id} not found")

        if color is not None:
            annotation.color = color
        if label is not None:
            annotation.label = label
        if description is not None:
            annotation.description = description
        if is_public is not None:
            annotation.is_public = is_public
        if expires_at is not None:
            annotation.expires_at = expires_at
        if metadata is not None:
            annotation.metadata = metadata

        annotation.updated_at = datetime.utcnow()

        await db.commit()
        await db.refresh(annotation)

        logger.info(f"Updated annotation {annotation_id}")
        return annotation

    async def delete_annotation(
        self,
        db: AsyncSession,
        annotation_id: UUID,
    ) -> bool:
        """
        Delete an annotation.

        Args:
            db: Database session
            annotation_id: Annotation UUID

        Returns:
            True if deleted, False if not found
        """
        result = await db.execute(
            delete(CodeAnnotation).where(CodeAnnotation.id == annotation_id)
        )
        await db.commit()

        deleted = result.rowcount > 0
        if deleted:
            logger.info(f"Deleted annotation {annotation_id}")
        return deleted

    async def delete_annotations_for_location(
        self,
        db: AsyncSession,
        location: str,
        user_id: Optional[UUID] = None,
    ) -> int:
        """
        Delete all annotations for a location.

        Args:
            db: Database session
            location: File location
            user_id: Optional user filter (only delete user's annotations)

        Returns:
            Number of annotations deleted
        """
        query = delete(CodeAnnotation).where(CodeAnnotation.location == location)

        if user_id:
            query = query.where(CodeAnnotation.user_id == user_id)

        result = await db.execute(query)
        await db.commit()

        count = result.rowcount
        logger.info(f"Deleted {count} annotations at {location}")
        return count

    async def cleanup_expired(
        self,
        db: AsyncSession,
    ) -> int:
        """
        Delete all expired annotations.

        Args:
            db: Database session

        Returns:
            Number of annotations deleted
        """
        result = await db.execute(
            delete(CodeAnnotation).where(
                and_(
                    CodeAnnotation.expires_at.is_not(None),
                    CodeAnnotation.expires_at <= datetime.utcnow(),
                )
            )
        )
        await db.commit()

        count = result.rowcount
        if count > 0:
            logger.info(f"Cleaned up {count} expired annotations")
        return count

    async def get_annotation_count(
        self,
        db: AsyncSession,
        location: Optional[str] = None,
        user_id: Optional[UUID] = None,
        color: Optional[str] = None,
    ) -> int:
        """
        Get count of annotations matching filters.

        Args:
            db: Database session
            location: Optional location filter
            user_id: Optional user filter
            color: Optional color filter

        Returns:
            Annotation count
        """
        query = select(CodeAnnotation)

        if location:
            query = query.where(CodeAnnotation.location == location)
        if user_id:
            query = query.where(CodeAnnotation.user_id == user_id)
        if color:
            query = query.where(CodeAnnotation.color == color)

        # Exclude expired
        query = query.where(
            or_(
                CodeAnnotation.expires_at.is_(None),
                CodeAnnotation.expires_at > datetime.utcnow(),
            )
        )

        result = await db.execute(query)
        return len(list(result.scalars().all()))
