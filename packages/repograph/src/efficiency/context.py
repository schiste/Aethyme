"""Context management for efficient AI operations.

This module provides context compaction, working memory slots,
playlists, and outcome cards to optimize token usage.
"""

import re
import json
from typing import Dict, List, Optional, Set, Any, Tuple
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
import hashlib
import structlog

logger = structlog.get_logger(__name__)


class ContextType(str, Enum):
    """Types of context items."""
    CODE = "code"
    DOCUMENTATION = "documentation"
    SCHEMA = "schema"
    TEST = "test"
    CONFIG = "config"
    DEPENDENCY = "dependency"


class Priority(str, Enum):
    """Priority levels for context items."""
    CRITICAL = "critical"
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"


@dataclass
class ContextItem:
    """Individual context item."""

    item_id: str
    item_type: ContextType
    content: str
    priority: Priority = Priority.MEDIUM
    token_count: int = 0
    metadata: Dict[str, Any] = field(default_factory=dict)
    dependencies: Set[str] = field(default_factory=set)
    last_accessed: Optional[datetime] = None

    def calculate_tokens(self) -> int:
        """Estimate token count (rough approximation).

        Returns:
            Estimated token count
        """
        # Rough estimate: 1 token ≈ 4 characters
        self.token_count = len(self.content) // 4
        return self.token_count

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'item_id': self.item_id,
            'item_type': self.item_type.value,
            'content': self.content,
            'priority': self.priority.value,
            'token_count': self.token_count,
            'metadata': self.metadata,
            'dependencies': list(self.dependencies),
        }


class ContextCompactor:
    """Compact context by removing redundancy."""

    def __init__(self, aggressive: bool = False):
        """Initialize compactor.

        Args:
            aggressive: If True, use aggressive compaction
        """
        self.aggressive = aggressive

    def compact(self, items: List[ContextItem]) -> Tuple[List[ContextItem], Dict[str, Any]]:
        """Compact context items.

        Args:
            items: List of context items

        Returns:
            Tuple of (compacted items, statistics)
        """
        original_count = len(items)
        original_tokens = sum(item.token_count for item in items)

        # Remove duplicates
        items = self._remove_duplicates(items)

        # Remove redundant imports/headers
        items = self._remove_redundant_headers(items)

        # Truncate low-priority items if aggressive
        if self.aggressive:
            items = self._truncate_low_priority(items)

        # Remove comments if aggressive
        if self.aggressive:
            items = self._remove_comments(items)

        compacted_count = len(items)
        compacted_tokens = sum(item.token_count for item in items)

        stats = {
            'original_items': original_count,
            'original_tokens': original_tokens,
            'compacted_items': compacted_count,
            'compacted_tokens': compacted_tokens,
            'items_removed': original_count - compacted_count,
            'tokens_saved': original_tokens - compacted_tokens,
            'reduction_percent': (
                ((original_tokens - compacted_tokens) / original_tokens * 100)
                if original_tokens > 0 else 0
            ),
        }

        logger.info(
            "Context compacted",
            original=original_count,
            compacted=compacted_count,
            saved_tokens=stats['tokens_saved'],
            reduction=f"{stats['reduction_percent']:.1f}%",
        )

        return items, stats

    def _remove_duplicates(self, items: List[ContextItem]) -> List[ContextItem]:
        """Remove duplicate items."""
        seen = {}
        unique_items = []

        for item in items:
            # Use content hash as key
            content_hash = hashlib.md5(item.content.encode()).hexdigest()

            if content_hash not in seen:
                seen[content_hash] = item
                unique_items.append(item)
            else:
                # Keep higher priority version
                existing = seen[content_hash]
                if item.priority.value > existing.priority.value:
                    unique_items.remove(existing)
                    unique_items.append(item)
                    seen[content_hash] = item

        return unique_items

    def _remove_redundant_headers(self, items: List[ContextItem]) -> List[ContextItem]:
        """Remove redundant imports and headers."""
        for item in items:
            if item.item_type == ContextType.CODE:
                # Remove duplicate imports
                lines = item.content.split('\n')
                import_lines = set()
                new_lines = []

                for line in lines:
                    if line.strip().startswith(('import ', 'from ')):
                        if line not in import_lines:
                            import_lines.add(line)
                            new_lines.append(line)
                    else:
                        new_lines.append(line)

                item.content = '\n'.join(new_lines)
                item.calculate_tokens()

        return items

    def _truncate_low_priority(self, items: List[ContextItem]) -> List[ContextItem]:
        """Truncate low priority items."""
        for item in items:
            if item.priority == Priority.LOW and item.token_count > 500:
                # Keep first and last 200 tokens worth
                chars_to_keep = 200 * 4
                if len(item.content) > chars_to_keep * 2:
                    start = item.content[:chars_to_keep]
                    end = item.content[-chars_to_keep:]
                    item.content = f"{start}\n\n... [truncated] ...\n\n{end}"
                    item.calculate_tokens()
                    item.metadata['truncated'] = True

        return items

    def _remove_comments(self, items: List[ContextItem]) -> List[ContextItem]:
        """Remove comments from code."""
        for item in items:
            if item.item_type == ContextType.CODE:
                # Remove single-line comments
                item.content = re.sub(r'#.*$', '', item.content, flags=re.MULTILINE)
                # Remove multi-line comments/docstrings (basic)
                item.content = re.sub(r'""".*?"""', '', item.content, flags=re.DOTALL)
                item.content = re.sub(r"'''.*?'''", '', item.content, flags=re.DOTALL)
                # Clean up extra whitespace
                item.content = re.sub(r'\n\s*\n', '\n\n', item.content)
                item.calculate_tokens()

        return items


@dataclass
class WorkingMemorySlot:
    """Slot in working memory with size limit."""

    slot_id: str
    max_tokens: int
    items: List[ContextItem] = field(default_factory=list)
    current_tokens: int = 0
    metadata: Dict[str, Any] = field(default_factory=dict)

    def add_item(self, item: ContextItem) -> bool:
        """Add item to slot if it fits.

        Args:
            item: Context item to add

        Returns:
            True if item was added
        """
        if self.current_tokens + item.token_count <= self.max_tokens:
            self.items.append(item)
            self.current_tokens += item.token_count
            item.last_accessed = datetime.utcnow()
            return True

        logger.debug(
            "Item doesn't fit in slot",
            slot=self.slot_id,
            current=self.current_tokens,
            needed=item.token_count,
            max=self.max_tokens,
        )
        return False

    def remove_item(self, item_id: str) -> bool:
        """Remove item from slot.

        Args:
            item_id: ID of item to remove

        Returns:
            True if item was removed
        """
        for item in self.items:
            if item.item_id == item_id:
                self.items.remove(item)
                self.current_tokens -= item.token_count
                return True
        return False

    def evict_lru(self) -> Optional[ContextItem]:
        """Evict least recently used item.

        Returns:
            Evicted item or None
        """
        if not self.items:
            return None

        # Find LRU item
        lru_item = min(
            self.items,
            key=lambda x: x.last_accessed or datetime.min,
        )

        self.remove_item(lru_item.item_id)
        return lru_item

    def available_tokens(self) -> int:
        """Get available token capacity.

        Returns:
            Available tokens
        """
        return self.max_tokens - self.current_tokens

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'slot_id': self.slot_id,
            'max_tokens': self.max_tokens,
            'current_tokens': self.current_tokens,
            'items': [item.to_dict() for item in self.items],
            'utilization': self.current_tokens / self.max_tokens,
        }


@dataclass
class ContextPlaylist:
    """Organized playlist of context for a task."""

    playlist_id: str
    task_description: str
    sections: Dict[str, List[ContextItem]] = field(default_factory=dict)
    total_tokens: int = 0
    created_at: datetime = field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def add_section(self, section_name: str, items: List[ContextItem]):
        """Add a section to playlist.

        Args:
            section_name: Name of section
            items: Items in section
        """
        self.sections[section_name] = items
        self._recalculate_tokens()

    def _recalculate_tokens(self):
        """Recalculate total tokens."""
        self.total_tokens = sum(
            sum(item.token_count for item in items)
            for items in self.sections.values()
        )

    def render(self, format: str = 'markdown') -> str:
        """Render playlist to string.

        Args:
            format: Output format (markdown, json, plain)

        Returns:
            Rendered playlist
        """
        if format == 'markdown':
            return self._render_markdown()
        elif format == 'json':
            return self._render_json()
        else:
            return self._render_plain()

    def _render_markdown(self) -> str:
        """Render as markdown."""
        lines = [
            f"# Context Playlist: {self.task_description}",
            f"",
            f"**Total Tokens:** {self.total_tokens}",
            f"**Created:** {self.created_at.isoformat()}",
            f"",
        ]

        for section_name, items in self.sections.items():
            section_tokens = sum(item.token_count for item in items)
            lines.append(f"## {section_name} ({len(items)} items, {section_tokens} tokens)")
            lines.append("")

            for item in items:
                lines.append(f"### {item.item_id} ({item.item_type.value})")
                lines.append(f"Priority: {item.priority.value} | Tokens: {item.token_count}")
                lines.append("```")
                lines.append(item.content[:500])  # Preview
                if len(item.content) > 500:
                    lines.append("... [truncated]")
                lines.append("```")
                lines.append("")

        return '\n'.join(lines)

    def _render_json(self) -> str:
        """Render as JSON."""
        data = {
            'playlist_id': self.playlist_id,
            'task_description': self.task_description,
            'total_tokens': self.total_tokens,
            'created_at': self.created_at.isoformat(),
            'sections': {
                name: [item.to_dict() for item in items]
                for name, items in self.sections.items()
            },
        }
        return json.dumps(data, indent=2)

    def _render_plain(self) -> str:
        """Render as plain text."""
        lines = [f"Playlist: {self.task_description}", ""]

        for section_name, items in self.sections.items():
            lines.append(f"=== {section_name} ===")
            for item in items:
                lines.append(f"- {item.item_id}: {item.token_count} tokens")
            lines.append("")

        return '\n'.join(lines)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'playlist_id': self.playlist_id,
            'task_description': self.task_description,
            'total_tokens': self.total_tokens,
            'created_at': self.created_at.isoformat(),
            'sections': {
                name: [item.to_dict() for item in items]
                for name, items in self.sections.items()
            },
            'metadata': self.metadata,
        }


@dataclass
class OutcomeCard:
    """Summary card of task outcome."""

    card_id: str
    task_description: str
    status: str  # success, failure, partial
    summary: str
    key_decisions: List[str] = field(default_factory=list)
    artifacts_created: List[str] = field(default_factory=list)
    tokens_used: int = 0
    cost: float = 0.0
    duration_seconds: float = 0.0
    errors: List[str] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    created_at: datetime = field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def render(self, format: str = 'markdown') -> str:
        """Render outcome card.

        Args:
            format: Output format (markdown, json)

        Returns:
            Rendered card
        """
        if format == 'json':
            return json.dumps(self.to_dict(), indent=2)

        # Markdown format
        lines = [
            f"# Outcome Card: {self.task_description}",
            f"",
            f"**Status:** {self.status.upper()}",
            f"**Created:** {self.created_at.isoformat()}",
            f"",
            f"## Summary",
            f"{self.summary}",
            f"",
        ]

        if self.key_decisions:
            lines.append("## Key Decisions")
            for decision in self.key_decisions:
                lines.append(f"- {decision}")
            lines.append("")

        if self.artifacts_created:
            lines.append("## Artifacts Created")
            for artifact in self.artifacts_created:
                lines.append(f"- {artifact}")
            lines.append("")

        lines.extend([
            "## Metrics",
            f"- Tokens Used: {self.tokens_used:,}",
            f"- Cost: ${self.cost:.4f}",
            f"- Duration: {self.duration_seconds:.1f}s",
            "",
        ])

        if self.errors:
            lines.append("## Errors")
            for error in self.errors:
                lines.append(f"- {error}")
            lines.append("")

        if self.warnings:
            lines.append("## Warnings")
            for warning in self.warnings:
                lines.append(f"- {warning}")
            lines.append("")

        return '\n'.join(lines)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'card_id': self.card_id,
            'task_description': self.task_description,
            'status': self.status,
            'summary': self.summary,
            'key_decisions': self.key_decisions,
            'artifacts_created': self.artifacts_created,
            'tokens_used': self.tokens_used,
            'cost': self.cost,
            'duration_seconds': self.duration_seconds,
            'errors': self.errors,
            'warnings': self.warnings,
            'created_at': self.created_at.isoformat(),
            'metadata': self.metadata,
        }


class ContextManager:
    """Manage context for AI operations."""

    def __init__(
        self,
        max_total_tokens: int = 100000,
        compaction_enabled: bool = True,
    ):
        """Initialize context manager.

        Args:
            max_total_tokens: Maximum total tokens across all slots
            compaction_enabled: Enable automatic compaction
        """
        self.max_total_tokens = max_total_tokens
        self.compaction_enabled = compaction_enabled
        self.compactor = ContextCompactor(aggressive=False)
        self.slots: Dict[str, WorkingMemorySlot] = {}
        self.playlists: Dict[str, ContextPlaylist] = {}
        self.outcome_cards: Dict[str, OutcomeCard] = {}

    def create_slot(self, slot_id: str, max_tokens: int) -> WorkingMemorySlot:
        """Create a working memory slot.

        Args:
            slot_id: Slot identifier
            max_tokens: Maximum tokens for slot

        Returns:
            Created slot
        """
        slot = WorkingMemorySlot(slot_id=slot_id, max_tokens=max_tokens)
        self.slots[slot_id] = slot
        logger.info("Created memory slot", slot_id=slot_id, max_tokens=max_tokens)
        return slot

    def add_to_slot(self, slot_id: str, item: ContextItem) -> bool:
        """Add item to slot with auto-compaction.

        Args:
            slot_id: Slot identifier
            item: Context item to add

        Returns:
            True if item was added
        """
        if slot_id not in self.slots:
            raise ValueError(f"Slot '{slot_id}' not found")

        slot = self.slots[slot_id]

        # Try to add directly
        if slot.add_item(item):
            return True

        # Try compaction if enabled
        if self.compaction_enabled and len(slot.items) > 0:
            compacted, stats = self.compactor.compact(slot.items)
            slot.items = compacted
            slot.current_tokens = sum(i.token_count for i in compacted)

            logger.info("Slot compacted", slot_id=slot_id, **stats)

            # Try again
            if slot.add_item(item):
                return True

        # Try LRU eviction
        if slot.items:
            evicted = slot.evict_lru()
            logger.debug("Evicted item from slot", slot_id=slot_id, evicted=evicted.item_id if evicted else None)

            return slot.add_item(item)

        return False

    def create_playlist(
        self,
        task_description: str,
        context_items: Dict[str, List[ContextItem]],
    ) -> ContextPlaylist:
        """Create a context playlist for a task.

        Args:
            task_description: Description of task
            context_items: Dictionary of section name to items

        Returns:
            Created playlist
        """
        playlist_id = hashlib.md5(
            f"{task_description}_{datetime.utcnow().isoformat()}".encode()
        ).hexdigest()[:16]

        playlist = ContextPlaylist(
            playlist_id=playlist_id,
            task_description=task_description,
            sections=context_items,
        )
        playlist._recalculate_tokens()

        self.playlists[playlist_id] = playlist

        logger.info(
            "Created context playlist",
            playlist_id=playlist_id,
            sections=len(context_items),
            total_tokens=playlist.total_tokens,
        )

        return playlist

    def create_outcome_card(self, **kwargs) -> OutcomeCard:
        """Create an outcome card.

        Args:
            **kwargs: Outcome card fields

        Returns:
            Created outcome card
        """
        card_id = hashlib.md5(
            f"{kwargs.get('task_description', '')}_{datetime.utcnow().isoformat()}".encode()
        ).hexdigest()[:16]

        card = OutcomeCard(card_id=card_id, **kwargs)
        self.outcome_cards[card_id] = card

        logger.info(
            "Created outcome card",
            card_id=card_id,
            status=card.status,
            tokens=card.tokens_used,
        )

        return card

    def get_stats(self) -> Dict[str, Any]:
        """Get context management statistics.

        Returns:
            Statistics dictionary
        """
        total_slot_tokens = sum(slot.current_tokens for slot in self.slots.values())
        total_playlist_tokens = sum(p.total_tokens for p in self.playlists.values())

        return {
            'slots': len(self.slots),
            'playlists': len(self.playlists),
            'outcome_cards': len(self.outcome_cards),
            'total_slot_tokens': total_slot_tokens,
            'total_playlist_tokens': total_playlist_tokens,
            'max_total_tokens': self.max_total_tokens,
            'utilization': total_slot_tokens / self.max_total_tokens if self.max_total_tokens > 0 else 0,
        }
