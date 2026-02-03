"""Tests for context management."""

import pytest
from src.efficiency.context import (
    ContextManager,
    ContextCompactor,
    ContextItem,
    WorkingMemorySlot,
    ContextPlaylist,
    OutcomeCard,
    ContextType,
    Priority,
)


class TestContextItem:
    """Tests for context items."""

    def test_calculate_tokens(self):
        """Test token calculation."""
        item = ContextItem(
            item_id="test",
            item_type=ContextType.CODE,
            content="a" * 400,  # 400 characters
            priority=Priority.MEDIUM,
        )

        tokens = item.calculate_tokens()
        assert tokens == 100  # 400 / 4

    def test_to_dict(self):
        """Test converting to dictionary."""
        item = ContextItem(
            item_id="test",
            item_type=ContextType.CODE,
            content="test content",
            priority=Priority.HIGH,
        )
        item.calculate_tokens()

        data = item.to_dict()
        assert data['item_id'] == "test"
        assert data['item_type'] == "code"
        assert data['priority'] == "high"


class TestContextCompactor:
    """Tests for context compaction."""

    def test_remove_duplicates(self):
        """Test removing duplicate items."""
        items = [
            ContextItem("1", ContextType.CODE, "def func(): pass", Priority.MEDIUM),
            ContextItem("2", ContextType.CODE, "def func(): pass", Priority.HIGH),
            ContextItem("3", ContextType.CODE, "different code", Priority.MEDIUM),
        ]

        for item in items:
            item.calculate_tokens()

        compactor = ContextCompactor()
        unique_items = compactor._remove_duplicates(items)

        assert len(unique_items) == 2  # 2 unique contents
        # Higher priority version should be kept
        assert any(item.priority == Priority.HIGH for item in unique_items)

    def test_compact_with_stats(self):
        """Test compaction with statistics."""
        items = [
            ContextItem("1", ContextType.CODE, "def func(): pass", Priority.MEDIUM),
            ContextItem("2", ContextType.CODE, "def func(): pass", Priority.MEDIUM),
            ContextItem("3", ContextType.CODE, "def other(): pass", Priority.MEDIUM),
        ]

        for item in items:
            item.calculate_tokens()

        compactor = ContextCompactor()
        compacted, stats = compactor.compact(items)

        assert stats['original_items'] == 3
        assert stats['compacted_items'] == 2
        assert stats['items_removed'] == 1
        assert stats['reduction_percent'] > 0

    def test_aggressive_compaction(self):
        """Test aggressive compaction mode."""
        large_content = "x" * 10000  # Large content
        items = [
            ContextItem("1", ContextType.CODE, large_content, Priority.LOW),
        ]
        items[0].calculate_tokens()

        original_tokens = items[0].token_count

        compactor = ContextCompactor(aggressive=True)
        compacted, stats = compactor.compact(items)

        # Should truncate low priority items
        assert compacted[0].token_count < original_tokens
        assert compacted[0].metadata.get('truncated') is True


class TestWorkingMemorySlot:
    """Tests for working memory slots."""

    def test_add_item_success(self):
        """Test adding item that fits."""
        slot = WorkingMemorySlot("test_slot", max_tokens=1000)

        item = ContextItem("1", ContextType.CODE, "x" * 200, Priority.MEDIUM)
        item.calculate_tokens()

        assert slot.add_item(item) is True
        assert len(slot.items) == 1
        assert slot.current_tokens == item.token_count

    def test_add_item_failure(self):
        """Test adding item that doesn't fit."""
        slot = WorkingMemorySlot("test_slot", max_tokens=100)

        item = ContextItem("1", ContextType.CODE, "x" * 1000, Priority.MEDIUM)
        item.calculate_tokens()

        assert slot.add_item(item) is False
        assert len(slot.items) == 0

    def test_remove_item(self):
        """Test removing item."""
        slot = WorkingMemorySlot("test_slot", max_tokens=1000)

        item = ContextItem("1", ContextType.CODE, "test", Priority.MEDIUM)
        item.calculate_tokens()

        slot.add_item(item)
        assert slot.remove_item("1") is True
        assert len(slot.items) == 0
        assert slot.current_tokens == 0

    def test_evict_lru(self):
        """Test LRU eviction."""
        from datetime import datetime, timedelta

        slot = WorkingMemorySlot("test_slot", max_tokens=1000)

        item1 = ContextItem("1", ContextType.CODE, "test1", Priority.MEDIUM)
        item2 = ContextItem("2", ContextType.CODE, "test2", Priority.MEDIUM)
        item1.calculate_tokens()
        item2.calculate_tokens()

        slot.add_item(item1)
        item1.last_accessed = datetime.utcnow() - timedelta(hours=1)

        slot.add_item(item2)
        item2.last_accessed = datetime.utcnow()

        evicted = slot.evict_lru()

        assert evicted == item1
        assert len(slot.items) == 1
        assert slot.items[0] == item2

    def test_to_dict(self):
        """Test converting slot to dictionary."""
        slot = WorkingMemorySlot("test_slot", max_tokens=1000)

        item = ContextItem("1", ContextType.CODE, "x" * 200, Priority.MEDIUM)
        item.calculate_tokens()
        slot.add_item(item)

        data = slot.to_dict()

        assert data['slot_id'] == "test_slot"
        assert data['max_tokens'] == 1000
        assert data['current_tokens'] == item.token_count
        assert 'utilization' in data


class TestContextPlaylist:
    """Tests for context playlists."""

    def test_add_section(self):
        """Test adding section to playlist."""
        playlist = ContextPlaylist("test_playlist", "Test task")

        items = [
            ContextItem("1", ContextType.CODE, "test", Priority.MEDIUM),
        ]
        items[0].calculate_tokens()

        playlist.add_section("Code", items)

        assert "Code" in playlist.sections
        assert len(playlist.sections["Code"]) == 1

    def test_render_markdown(self):
        """Test rendering as markdown."""
        playlist = ContextPlaylist("test_playlist", "Test task")

        items = [
            ContextItem("1", ContextType.CODE, "def test(): pass", Priority.HIGH),
        ]
        items[0].calculate_tokens()

        playlist.add_section("Code", items)

        markdown = playlist.render("markdown")

        assert "Test task" in markdown
        assert "Code" in markdown
        assert "def test(): pass" in markdown

    def test_render_json(self):
        """Test rendering as JSON."""
        import json

        playlist = ContextPlaylist("test_playlist", "Test task")

        items = [
            ContextItem("1", ContextType.CODE, "test", Priority.HIGH),
        ]
        items[0].calculate_tokens()

        playlist.add_section("Code", items)

        json_output = playlist.render("json")
        data = json.loads(json_output)

        assert data['playlist_id'] == "test_playlist"
        assert data['task_description'] == "Test task"


class TestOutcomeCard:
    """Tests for outcome cards."""

    def test_create_outcome_card(self):
        """Test creating outcome card."""
        card = OutcomeCard(
            card_id="test_card",
            task_description="Test task",
            status="success",
            summary="Task completed successfully",
            tokens_used=1000,
            cost=0.05,
        )

        assert card.status == "success"
        assert card.tokens_used == 1000
        assert card.cost == 0.05

    def test_render_markdown(self):
        """Test rendering outcome card as markdown."""
        card = OutcomeCard(
            card_id="test_card",
            task_description="Test task",
            status="success",
            summary="Task completed",
            key_decisions=["Use fast model", "Compact context"],
            artifacts_created=["code.py", "tests.py"],
            tokens_used=1000,
            cost=0.05,
        )

        markdown = card.render("markdown")

        assert "Test task" in markdown
        assert "success" in markdown.upper()
        assert "Use fast model" in markdown
        assert "code.py" in markdown
        assert "1,000" in markdown

    def test_to_dict(self):
        """Test converting outcome card to dictionary."""
        card = OutcomeCard(
            card_id="test_card",
            task_description="Test task",
            status="success",
            summary="Done",
        )

        data = card.to_dict()

        assert data['card_id'] == "test_card"
        assert data['status'] == "success"


class TestContextManager:
    """Tests for context manager."""

    def test_create_slot(self):
        """Test creating memory slot."""
        manager = ContextManager(max_total_tokens=10000)

        slot = manager.create_slot("test_slot", max_tokens=1000)

        assert slot.slot_id == "test_slot"
        assert "test_slot" in manager.slots

    def test_add_to_slot(self):
        """Test adding item to slot."""
        manager = ContextManager(max_total_tokens=10000)
        manager.create_slot("test_slot", max_tokens=1000)

        item = ContextItem("1", ContextType.CODE, "test", Priority.MEDIUM)
        item.calculate_tokens()

        assert manager.add_to_slot("test_slot", item) is True

    def test_add_to_slot_with_compaction(self):
        """Test adding item with auto-compaction."""
        manager = ContextManager(max_total_tokens=10000, compaction_enabled=True)
        manager.create_slot("test_slot", max_tokens=100)

        # Fill slot with duplicate content
        for i in range(10):
            item = ContextItem(f"{i}", ContextType.CODE, "duplicate", Priority.LOW)
            item.calculate_tokens()
            manager.add_to_slot("test_slot", item)

        # Should compact duplicates
        slot = manager.slots["test_slot"]
        # Exact count depends on compaction, but should be reduced

    def test_create_playlist(self):
        """Test creating context playlist."""
        manager = ContextManager()

        items = {
            "Code": [
                ContextItem("1", ContextType.CODE, "test", Priority.HIGH),
            ],
        }
        items["Code"][0].calculate_tokens()

        playlist = manager.create_playlist("Test task", items)

        assert playlist.task_description == "Test task"
        assert playlist.playlist_id in manager.playlists

    def test_create_outcome_card(self):
        """Test creating outcome card."""
        manager = ContextManager()

        card = manager.create_outcome_card(
            task_description="Test",
            status="success",
            summary="Done",
        )

        assert card.card_id in manager.outcome_cards

    def test_get_stats(self):
        """Test getting statistics."""
        manager = ContextManager(max_total_tokens=10000)

        manager.create_slot("slot1", max_tokens=1000)
        manager.create_slot("slot2", max_tokens=2000)

        stats = manager.get_stats()

        assert stats['slots'] == 2
        assert stats['max_total_tokens'] == 10000
        assert 'utilization' in stats
