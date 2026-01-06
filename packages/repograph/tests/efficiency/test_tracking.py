"""Tests for token and cost tracking."""

import pytest
from datetime import datetime, timedelta
from src.efficiency.tracking import (
    TokenTracker,
    CostTracker,
    BudgetEnforcer,
    Budget,
    TimeWindow,
    TokenUsage,
    CostRecord,
    calculate_usage_stats,
)


class TestTokenTracker:
    """Tests for token tracking."""

    def test_record_usage(self):
        """Test recording token usage."""
        tracker = TokenTracker()

        usage = tracker.record_usage(
            operation_id="op1",
            operation_type="code_gen",
            model_id="gpt-3.5-turbo",
            input_tokens=100,
            output_tokens=200,
            tenant_id="tenant1",
            user_id="user1",
        )

        assert usage.total_tokens == 300
        assert len(tracker.usage_records) == 1

    def test_get_usage_by_tenant(self):
        """Test getting usage for specific tenant."""
        tracker = TokenTracker()

        tracker.record_usage("op1", "type1", "model1", 100, 100, "tenant1", "user1")
        tracker.record_usage("op2", "type1", "model1", 200, 200, "tenant2", "user1")
        tracker.record_usage("op3", "type1", "model1", 150, 150, "tenant1", "user2")

        tenant1_usage = tracker.get_usage_by_tenant("tenant1")

        assert len(tenant1_usage) == 2
        assert all(u.tenant_id == "tenant1" for u in tenant1_usage)

    def test_get_total_tokens(self):
        """Test getting total token count."""
        tracker = TokenTracker()

        tracker.record_usage("op1", "type1", "model1", 100, 100, "tenant1", "user1")
        tracker.record_usage("op2", "type1", "model1", 200, 200, "tenant1", "user1")

        total = tracker.get_total_tokens()
        assert total == 600  # (100+100) + (200+200)

    def test_get_total_tokens_by_tenant(self):
        """Test getting tokens for specific tenant."""
        tracker = TokenTracker()

        tracker.record_usage("op1", "type1", "model1", 100, 100, "tenant1", "user1")
        tracker.record_usage("op2", "type1", "model1", 200, 200, "tenant2", "user1")

        tenant1_total = tracker.get_total_tokens(tenant_id="tenant1")
        assert tenant1_total == 200

    def test_get_tokens_with_time_range(self):
        """Test getting tokens within time range."""
        tracker = TokenTracker()

        now = datetime.utcnow()
        past = now - timedelta(hours=2)

        # Record with manual timestamp manipulation
        usage1 = tracker.record_usage("op1", "type1", "model1", 100, 100, "tenant1", "user1")
        usage1.timestamp = past

        usage2 = tracker.record_usage("op2", "type1", "model1", 200, 200, "tenant1", "user1")

        # Get recent tokens
        recent_total = tracker.get_total_tokens(
            start_time=now - timedelta(minutes=30)
        )

        assert recent_total == 400  # Only usage2


class TestCostTracker:
    """Tests for cost tracking."""

    def test_record_cost(self):
        """Test recording costs."""
        tracker = CostTracker()

        record = tracker.record_cost(
            operation_id="op1",
            operation_type="code_gen",
            model_id="gpt-4",
            cost_usd=0.05,
            input_tokens=1000,
            output_tokens=500,
            tenant_id="tenant1",
            user_id="user1",
        )

        assert record.cost_usd == 0.05
        assert len(tracker.cost_records) == 1

    def test_get_cost_by_tenant(self):
        """Test getting costs for specific tenant."""
        tracker = CostTracker()

        tracker.record_cost("op1", "type1", "model1", 0.05, 100, 100, "tenant1", "user1")
        tracker.record_cost("op2", "type1", "model1", 0.10, 200, 200, "tenant2", "user1")
        tracker.record_cost("op3", "type1", "model1", 0.03, 150, 150, "tenant1", "user2")

        tenant1_costs = tracker.get_cost_by_tenant("tenant1")

        assert len(tenant1_costs) == 2
        assert sum(c.cost_usd for c in tenant1_costs) == 0.08

    def test_get_total_cost(self):
        """Test getting total cost."""
        tracker = CostTracker()

        tracker.record_cost("op1", "type1", "model1", 0.05, 100, 100, "tenant1", "user1")
        tracker.record_cost("op2", "type1", "model1", 0.10, 200, 200, "tenant1", "user1")

        total = tracker.get_total_cost()
        assert total == 0.15

    def test_get_cost_with_time_range(self):
        """Test getting costs within time range."""
        tracker = CostTracker()

        now = datetime.utcnow()
        past = now - timedelta(hours=2)

        record1 = tracker.record_cost("op1", "type1", "model1", 0.05, 100, 100, "tenant1", "user1")
        record1.timestamp = past

        record2 = tracker.record_cost("op2", "type1", "model1", 0.10, 200, 200, "tenant1", "user1")

        recent_cost = tracker.get_total_cost(
            start_time=now - timedelta(minutes=30)
        )

        assert recent_cost == 0.10


class TestBudgetEnforcer:
    """Tests for budget enforcement."""

    def test_set_budget(self):
        """Test setting a budget."""
        enforcer = BudgetEnforcer()

        budget = Budget(
            budget_id="daily_budget",
            tenant_id="tenant1",
            limit_usd=10.00,
            window=TimeWindow.DAILY,
        )

        enforcer.set_budget(budget)

        assert "daily_budget" in enforcer.budgets

    def test_check_budget_allowed(self):
        """Test budget check when under limit."""
        enforcer = BudgetEnforcer()

        budget = Budget(
            budget_id="daily_budget",
            tenant_id="tenant1",
            limit_usd=10.00,
            window=TimeWindow.DAILY,
            hard_limit=True,
        )
        enforcer.set_budget(budget)

        # Record some cost
        enforcer.record_cost("op1", "type1", "model1", 5.00, 1000, 1000, "tenant1", "user1")

        # Check new operation
        result = enforcer.check_budget("tenant1", estimated_cost=3.00)

        assert result['allowed'] is True

    def test_check_budget_exceeded(self):
        """Test budget check when over limit."""
        enforcer = BudgetEnforcer()

        budget = Budget(
            budget_id="daily_budget",
            tenant_id="tenant1",
            limit_usd=10.00,
            window=TimeWindow.DAILY,
            hard_limit=True,
        )
        enforcer.set_budget(budget)

        # Record cost near limit
        enforcer.record_cost("op1", "type1", "model1", 9.50, 1000, 1000, "tenant1", "user1")

        # Check operation that would exceed
        result = enforcer.check_budget("tenant1", estimated_cost=1.00)

        assert result['allowed'] is False
        assert 'limit exceeded' in result['reason'].lower()

    def test_check_budget_soft_limit(self):
        """Test budget with soft limit (alert only)."""
        enforcer = BudgetEnforcer()

        budget = Budget(
            budget_id="daily_budget",
            tenant_id="tenant1",
            limit_usd=10.00,
            window=TimeWindow.DAILY,
            hard_limit=False,  # Soft limit
            alert_threshold=0.8,
        )
        enforcer.set_budget(budget)

        # Record cost above threshold
        enforcer.record_cost("op1", "type1", "model1", 9.00, 1000, 1000, "tenant1", "user1")

        # Should still allow but alert
        result = enforcer.check_budget("tenant1", estimated_cost=2.00)

        assert result['allowed'] is True  # Soft limit allows

    def test_check_budget_no_budget(self):
        """Test budget check with no budgets set."""
        enforcer = BudgetEnforcer()

        result = enforcer.check_budget("tenant1", estimated_cost=5.00)

        assert result['allowed'] is True
        assert 'no budgets' in result['reason'].lower()

    def test_budget_time_windows(self):
        """Test different budget time windows."""
        enforcer = BudgetEnforcer()

        # Hourly budget
        hourly_budget = Budget(
            budget_id="hourly",
            tenant_id="tenant1",
            limit_usd=1.00,
            window=TimeWindow.HOURLY,
        )
        enforcer.set_budget(hourly_budget)

        # Weekly budget
        weekly_budget = Budget(
            budget_id="weekly",
            tenant_id="tenant1",
            limit_usd=50.00,
            window=TimeWindow.WEEKLY,
        )
        enforcer.set_budget(weekly_budget)

        # Both should be checked
        result = enforcer.check_budget("tenant1", estimated_cost=0.50)
        assert result['budgets_checked'] == 2


class TestUsageStats:
    """Tests for usage statistics calculation."""

    def test_calculate_usage_stats(self):
        """Test calculating usage statistics."""
        now = datetime.utcnow()
        start = now - timedelta(days=1)

        token_records = [
            TokenUsage("op1", "code_gen", "gpt-3.5", 100, 100, 200, "tenant1", "user1", now),
            TokenUsage("op2", "docs", "gpt-3.5", 200, 150, 350, "tenant1", "user1", now),
        ]

        cost_records = [
            CostRecord("op1", "code_gen", "gpt-3.5", 0.05, 100, 100, "tenant1", "user1", now),
            CostRecord("op2", "docs", "gpt-3.5", 0.08, 200, 150, "tenant1", "user1", now),
        ]

        stats = calculate_usage_stats(
            token_records,
            cost_records,
            TimeWindow.DAILY,
            start,
            now,
        )

        assert stats.total_tokens == 550
        assert stats.total_cost == 0.13
        assert stats.operations_count == 2
        assert "gpt-3.5" in stats.by_model
        assert "code_gen" in stats.by_operation_type
        assert "tenant1" in stats.by_tenant

    def test_stats_by_model(self):
        """Test statistics grouped by model."""
        now = datetime.utcnow()

        token_records = [
            TokenUsage("op1", "type1", "gpt-3.5", 100, 100, 200, "tenant1", "user1", now),
            TokenUsage("op2", "type1", "gpt-4", 200, 200, 400, "tenant1", "user1", now),
        ]

        cost_records = [
            CostRecord("op1", "type1", "gpt-3.5", 0.01, 100, 100, "tenant1", "user1", now),
            CostRecord("op2", "type1", "gpt-4", 0.05, 200, 200, "tenant1", "user1", now),
        ]

        stats = calculate_usage_stats(
            token_records,
            cost_records,
            TimeWindow.DAILY,
            now - timedelta(days=1),
            now,
        )

        assert stats.by_model["gpt-3.5"]["tokens"] == 200
        assert stats.by_model["gpt-4"]["tokens"] == 400
        assert stats.by_model["gpt-3.5"]["cost"] == 0.01
        assert stats.by_model["gpt-4"]["cost"] == 0.05

    def test_stats_to_dict(self):
        """Test converting stats to dictionary."""
        now = datetime.utcnow()
        start = now - timedelta(days=1)

        stats = calculate_usage_stats([], [], TimeWindow.DAILY, start, now)
        data = stats.to_dict()

        assert data['window'] == "daily"
        assert 'avg_tokens_per_op' in data
        assert 'by_model' in data
