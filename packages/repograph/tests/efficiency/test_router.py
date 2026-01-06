"""Tests for model routing."""

import pytest
from src.efficiency.model_router import (
    ModelRouter,
    ModelConfig,
    ModelTier,
    ModelProvider,
    RoutingStrategy,
    EscalationConfig,
)


class TestModelConfig:
    """Tests for model configuration."""

    def test_estimate_cost(self):
        """Test cost estimation."""
        model = ModelConfig(
            model_id="gpt-3.5-turbo",
            provider=ModelProvider.OPENAI,
            tier=ModelTier.FAST,
            cost_per_1k_input=0.0005,
            cost_per_1k_output=0.0015,
            max_tokens=4096,
            context_window=16385,
        )

        cost = model.estimate_cost(input_tokens=1000, output_tokens=1000)
        expected = (1000/1000 * 0.0005) + (1000/1000 * 0.0015)
        assert cost == expected

    def test_estimate_cost_large_context(self):
        """Test cost estimation with large context."""
        model = ModelConfig(
            model_id="claude-3-opus",
            provider=ModelProvider.ANTHROPIC,
            tier=ModelTier.POWERFUL,
            cost_per_1k_input=0.015,
            cost_per_1k_output=0.075,
            max_tokens=4096,
            context_window=200000,
        )

        cost = model.estimate_cost(input_tokens=100000, output_tokens=2000)
        expected = (100 * 0.015) + (2 * 0.075)
        assert abs(cost - expected) < 0.01


class TestModelRouter:
    """Tests for model routing."""

    def test_route_cheapest_strategy(self):
        """Test routing with cheapest strategy."""
        router = ModelRouter(strategy=RoutingStrategy.CHEAPEST)

        decision = router.route(
            task_description="Simple task",
            estimated_input_tokens=100,
            estimated_output_tokens=100,
        )

        assert decision.tier == ModelTier.FAST
        assert "cheapest" in decision.reason.lower() or decision.tier == ModelTier.FAST

    def test_route_best_quality_strategy(self):
        """Test routing with best quality strategy."""
        router = ModelRouter(strategy=RoutingStrategy.BEST_QUALITY)

        decision = router.route(
            task_description="Complex task",
            estimated_input_tokens=1000,
            estimated_output_tokens=1000,
        )

        assert decision.tier == ModelTier.POWERFUL

    def test_route_adaptive_simple_task(self):
        """Test adaptive routing for simple task."""
        router = ModelRouter(strategy=RoutingStrategy.ADAPTIVE)

        decision = router.route(
            task_description="simple fix typo",
            estimated_input_tokens=100,
            estimated_output_tokens=50,
        )

        assert decision.tier in [ModelTier.FAST, ModelTier.BALANCED]

    def test_route_adaptive_complex_task(self):
        """Test adaptive routing for complex task."""
        router = ModelRouter(strategy=RoutingStrategy.ADAPTIVE)

        decision = router.route(
            task_description="complex algorithm optimization performance critical",
            estimated_input_tokens=5000,
            estimated_output_tokens=2000,
        )

        assert decision.tier in [ModelTier.BALANCED, ModelTier.POWERFUL]

    def test_route_with_required_tier(self):
        """Test routing with required tier override."""
        router = ModelRouter(strategy=RoutingStrategy.CHEAPEST)

        decision = router.route(
            task_description="Any task",
            estimated_input_tokens=100,
            estimated_output_tokens=100,
            required_tier=ModelTier.POWERFUL,
        )

        assert decision.tier == ModelTier.POWERFUL
        assert "required tier" in decision.reason.lower()

    def test_route_with_context_size_requirement(self):
        """Test routing with large context requirement."""
        router = ModelRouter(strategy=RoutingStrategy.ADAPTIVE)

        decision = router.route(
            task_description="Task with large context",
            estimated_input_tokens=100000,
            estimated_output_tokens=1000,
            context_size=150000,
        )

        # Should select model with sufficient context window
        assert decision.model_config.context_window >= 150000

    def test_budget_aware_routing(self):
        """Test budget-aware routing."""
        router = ModelRouter(
            strategy=RoutingStrategy.BUDGET_AWARE,
            budget_per_day=10.00,
        )

        # Use up most of budget
        router.total_cost = 9.00

        decision = router.route(
            task_description="Task",
            estimated_input_tokens=1000,
            estimated_output_tokens=1000,
        )

        # Should downgrade to save budget
        assert decision.tier == ModelTier.FAST

    def test_handle_failure_with_escalation(self):
        """Test handling failure with escalation."""
        router = ModelRouter(
            strategy=RoutingStrategy.ADAPTIVE,
            escalation_config=EscalationConfig(
                escalate_on_error=True,
                max_retries=3,
            ),
        )

        # Simulate failure with a balanced model
        decision = router.handle_failure(
            model_id="gpt-4-turbo",
            error="Rate limit exceeded",
            attempt=1,
        )

        assert decision is not None
        assert decision.tier == ModelTier.POWERFUL
        assert "escalated" in decision.reason.lower()

    def test_handle_failure_max_retries(self):
        """Test failure handling at max retries."""
        router = ModelRouter(
            escalation_config=EscalationConfig(max_retries=2),
        )

        decision = router.handle_failure(
            model_id="gpt-3.5-turbo",
            error="Error",
            attempt=3,  # Exceeds max
        )

        assert decision is None

    def test_record_cost(self):
        """Test recording costs."""
        router = ModelRouter()

        initial_cost = router.total_cost
        router.record_cost(0.25)

        assert router.total_cost == initial_cost + 0.25

    def test_get_stats(self):
        """Test getting routing statistics."""
        router = ModelRouter(budget_per_day=10.00)

        router.route("Task 1", 100, 100)
        router.route("Task 2", 200, 200)

        stats = router.get_stats()

        assert stats['total_requests'] >= 2
        assert 'budget_utilization' in stats
        assert stats['budget_per_day'] == 10.00

    def test_add_custom_model(self):
        """Test adding custom model configuration."""
        router = ModelRouter()

        custom_model = ModelConfig(
            model_id="custom-model",
            provider=ModelProvider.LOCAL,
            tier=ModelTier.FAST,
            cost_per_1k_input=0.0,
            cost_per_1k_output=0.0,
            max_tokens=8192,
            context_window=32000,
        )

        router.add_model(custom_model)

        # Route and verify custom model can be selected
        decision = router.route(
            "Task",
            100,
            100,
            required_tier=ModelTier.FAST,
        )

        # Custom model should be among candidates
        assert any(
            m.model_id == "custom-model"
            for m in router.models[ModelTier.FAST]
        )

    def test_escalation_tracking(self):
        """Test that escalations are tracked."""
        router = ModelRouter(
            escalation_config=EscalationConfig(escalate_on_error=True),
        )

        router.handle_failure("gpt-3.5-turbo", "Error", 1)

        assert len(router.escalations) > 0
        assert router.escalations[0]['from_tier'] == ModelTier.FAST.value
        assert router.escalations[0]['to_tier'] == ModelTier.BALANCED.value

    def test_failure_tracking(self):
        """Test that failures are tracked by model."""
        router = ModelRouter()

        model_id = "test-model"
        router.handle_failure(model_id, "Error", 1)

        assert model_id in router.failures_by_model
        assert router.failures_by_model[model_id] == 1

    def test_complexity_assessment(self):
        """Test task complexity assessment."""
        router = ModelRouter(strategy=RoutingStrategy.ADAPTIVE)

        # Test simple task
        simple_score = router._assess_complexity("simple fix typo")
        assert simple_score < 0.5

        # Test complex task
        complex_score = router._assess_complexity(
            "complex algorithm optimization with security requirements"
        )
        assert complex_score > 0.5

    def test_tier_escalation(self):
        """Test tier escalation logic."""
        router = ModelRouter()

        assert router._escalate_tier(ModelTier.FAST) == ModelTier.BALANCED
        assert router._escalate_tier(ModelTier.BALANCED) == ModelTier.POWERFUL
        assert router._escalate_tier(ModelTier.POWERFUL) == ModelTier.POWERFUL

    def test_tier_downgrade(self):
        """Test tier downgrade logic."""
        router = ModelRouter()

        assert router._downgrade_tier(ModelTier.POWERFUL) == ModelTier.BALANCED
        assert router._downgrade_tier(ModelTier.BALANCED) == ModelTier.FAST
        assert router._downgrade_tier(ModelTier.FAST) == ModelTier.FAST
