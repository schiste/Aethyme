"""
Comment Manager

Business logic for code comments, threading, mentions, and resolutions.
"""

import logging
import re
from datetime import datetime
from typing import List, Optional, Set
from uuid import UUID

from sqlalchemy import select, and_, or_
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from app.models.collaboration.comment import Comment, Mention

logger = logging.getLogger(__name__)


class CommentManager:
    """
    Manages code comments with threading, mentions, and resolution.
    """

    def __init__(self):
        self.mention_pattern = re.compile(r'@(\w+)')

    async def create_comment(
        self,
        db: AsyncSession,
        user_id: UUID,
        location: str,
        line_start: int,
        content: str,
        line_end: Optional[int] = None,
        column_start: Optional[int] = None,
        column_end: Optional[int] = None,
        parent_id: Optional[UUID] = None,
        metadata: Optional[dict] = None,
    ) -> Comment:
        """
        Create a new comment.

        Args:
            db: Database session
            user_id: User creating the comment
            location: File/code location
            line_start: Starting line number
            content: Comment text
            line_end: Ending line number (for multi-line comments)
            column_start: Starting column
            column_end: Ending column
            parent_id: Parent comment ID (for replies)
            metadata: Additional metadata

        Returns:
            Created Comment object
        """
        # Validate parent exists if provided
        if parent_id:
            parent = await self.get_comment(db, parent_id)
            if not parent:
                raise ValueError(f"Parent comment {parent_id} not found")

        # Create comment
        comment = Comment(
            user_id=user_id,
            location=location,
            line_start=line_start,
            line_end=line_end,
            column_start=column_start,
            column_end=column_end,
            content=content,
            parent_id=parent_id,
            metadata=metadata or {},
        )
        db.add(comment)
        await db.flush()  # Get comment ID

        # Extract and create mentions
        mentioned_users = self._extract_mentions(content)
        for mentioned_user_id in mentioned_users:
            mention = Mention(
                comment_id=comment.id,
                mentioned_user_id=mentioned_user_id,
            )
            db.add(mention)

        await db.commit()
        await db.refresh(comment)

        logger.info(
            f"Comment created: {comment.id} by {user_id} at {location}:{line_start}"
        )

        return comment

    async def get_comment(
        self,
        db: AsyncSession,
        comment_id: UUID,
        include_replies: bool = False,
    ) -> Optional[Comment]:
        """
        Get a comment by ID.

        Args:
            db: Database session
            comment_id: Comment ID
            include_replies: Load replies recursively

        Returns:
            Comment object or None
        """
        query = select(Comment).where(Comment.id == comment_id)

        if include_replies:
            query = query.options(selectinload(Comment.replies))

        result = await db.execute(query)
        return result.scalar_one_or_none()

    async def get_comments_for_location(
        self,
        db: AsyncSession,
        location: str,
        include_resolved: bool = False,
        line_range: Optional[tuple[int, int]] = None,
    ) -> List[Comment]:
        """
        Get all comments for a location.

        Args:
            db: Database session
            location: File/code location
            include_resolved: Include resolved comments
            line_range: Optional (start_line, end_line) tuple to filter

        Returns:
            List of Comment objects
        """
        query = select(Comment).where(
            Comment.location == location,
            Comment.parent_id.is_(None),  # Only top-level comments
        )

        if not include_resolved:
            query = query.where(Comment.resolved == False)

        if line_range:
            start_line, end_line = line_range
            query = query.where(
                or_(
                    and_(
                        Comment.line_start >= start_line,
                        Comment.line_start <= end_line,
                    ),
                    and_(
                        Comment.line_end >= start_line,
                        Comment.line_end <= end_line,
                    ),
                )
            )

        query = query.options(selectinload(Comment.replies)).order_by(
            Comment.line_start, Comment.created_at
        )

        result = await db.execute(query)
        return list(result.scalars().all())

    async def get_comments_by_user(
        self,
        db: AsyncSession,
        user_id: UUID,
        include_resolved: bool = False,
        limit: int = 50,
    ) -> List[Comment]:
        """
        Get comments created by a user.

        Args:
            db: Database session
            user_id: User ID
            include_resolved: Include resolved comments
            limit: Maximum number of comments

        Returns:
            List of Comment objects
        """
        query = select(Comment).where(Comment.user_id == user_id)

        if not include_resolved:
            query = query.where(Comment.resolved == False)

        query = query.order_by(Comment.created_at.desc()).limit(limit)

        result = await db.execute(query)
        return list(result.scalars().all())

    async def get_mentions_for_user(
        self,
        db: AsyncSession,
        user_id: UUID,
        unread_only: bool = False,
    ) -> List[Comment]:
        """
        Get comments that mention a user.

        Args:
            db: Database session
            user_id: User ID
            unread_only: Only return unread mentions

        Returns:
            List of Comment objects
        """
        query = (
            select(Comment)
            .join(Mention, Mention.comment_id == Comment.id)
            .where(Mention.mentioned_user_id == user_id)
        )

        if unread_only:
            query = query.where(Mention.notified == False)

        query = query.order_by(Comment.created_at.desc())

        result = await db.execute(query)
        return list(result.scalars().all())

    async def update_comment(
        self,
        db: AsyncSession,
        comment_id: UUID,
        content: Optional[str] = None,
        metadata: Optional[dict] = None,
    ) -> Comment:
        """
        Update a comment's content or metadata.

        Args:
            db: Database session
            comment_id: Comment ID
            content: New content (optional)
            metadata: New metadata (optional)

        Returns:
            Updated Comment object
        """
        comment = await self.get_comment(db, comment_id)
        if not comment:
            raise ValueError(f"Comment {comment_id} not found")

        if content is not None:
            comment.content = content
            comment.updated_at = datetime.utcnow()

        if metadata is not None:
            comment.metadata = metadata

        await db.commit()
        await db.refresh(comment)

        logger.info(f"Comment updated: {comment_id}")

        return comment

    async def delete_comment(
        self,
        db: AsyncSession,
        comment_id: UUID,
    ) -> bool:
        """
        Delete a comment and all its replies.

        Args:
            db: Database session
            comment_id: Comment ID

        Returns:
            True if deleted, False if not found
        """
        comment = await self.get_comment(db, comment_id)
        if not comment:
            return False

        await db.delete(comment)
        await db.commit()

        logger.info(f"Comment deleted: {comment_id}")

        return True

    async def resolve_comment(
        self,
        db: AsyncSession,
        comment_id: UUID,
        user_id: UUID,
    ) -> Comment:
        """
        Mark a comment as resolved.

        Args:
            db: Database session
            comment_id: Comment ID
            user_id: User resolving the comment

        Returns:
            Updated Comment object
        """
        comment = await self.get_comment(db, comment_id)
        if not comment:
            raise ValueError(f"Comment {comment_id} not found")

        comment.resolved = True
        comment.resolved_by = user_id
        comment.resolved_at = datetime.utcnow()

        await db.commit()
        await db.refresh(comment)

        logger.info(f"Comment resolved: {comment_id} by {user_id}")

        return comment

    async def unresolve_comment(
        self,
        db: AsyncSession,
        comment_id: UUID,
    ) -> Comment:
        """
        Mark a comment as unresolved.

        Args:
            db: Database session
            comment_id: Comment ID

        Returns:
            Updated Comment object
        """
        comment = await self.get_comment(db, comment_id)
        if not comment:
            raise ValueError(f"Comment {comment_id} not found")

        comment.resolved = False
        comment.resolved_by = None
        comment.resolved_at = None

        await db.commit()
        await db.refresh(comment)

        logger.info(f"Comment unresolved: {comment_id}")

        return comment

    async def mark_mentions_notified(
        self,
        db: AsyncSession,
        comment_id: UUID,
    ):
        """
        Mark all mentions in a comment as notified.

        Args:
            db: Database session
            comment_id: Comment ID
        """
        query = select(Mention).where(Mention.comment_id == comment_id)
        result = await db.execute(query)
        mentions = result.scalars().all()

        for mention in mentions:
            mention.notified = True

        await db.commit()

        logger.info(f"Marked {len(mentions)} mentions as notified for comment {comment_id}")

    async def get_comment_count(
        self,
        db: AsyncSession,
        location: Optional[str] = None,
        user_id: Optional[UUID] = None,
        resolved: Optional[bool] = None,
    ) -> int:
        """
        Get count of comments matching filters.

        Args:
            db: Database session
            location: Filter by location
            user_id: Filter by user
            resolved: Filter by resolution status

        Returns:
            Comment count
        """
        query = select(Comment)

        if location:
            query = query.where(Comment.location == location)

        if user_id:
            query = query.where(Comment.user_id == user_id)

        if resolved is not None:
            query = query.where(Comment.resolved == resolved)

        result = await db.execute(query)
        return len(result.scalars().all())

    def _extract_mentions(self, content: str) -> Set[UUID]:
        """
        Extract user IDs from @mentions in comment content.

        Args:
            content: Comment text

        Returns:
            Set of mentioned user IDs
        """
        # This is a simplified version
        # In production, you'd look up usernames in the database
        # For now, we'll just extract the pattern
        mentions = set()

        # TODO: Implement actual user lookup by username
        # matches = self.mention_pattern.findall(content)
        # for username in matches:
        #     user = await get_user_by_username(username)
        #     if user:
        #         mentions.add(user.id)

        return mentions

    async def get_thread(
        self,
        db: AsyncSession,
        comment_id: UUID,
    ) -> List[Comment]:
        """
        Get a comment and all its replies in a flat list.

        Args:
            db: Database session
            comment_id: Root comment ID

        Returns:
            List of comments (root + all replies)
        """
        root = await self.get_comment(db, comment_id, include_replies=True)
        if not root:
            return []

        # Flatten the tree
        thread = [root]

        def collect_replies(comment: Comment):
            for reply in comment.replies:
                thread.append(reply)
                collect_replies(reply)

        collect_replies(root)

        return thread
