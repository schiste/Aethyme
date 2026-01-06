"""Model routing with cost optimization and escalation.

This module provides intelligent routing between different model tiers
based on task complexity, budget constraints, and quality requirements.
"""

from typing import Dict, List, Optional, Any, Callable
from dataclasses import dataclass, field
from enum import Enum
from datetime import datetime
import structlog

logger = structlog.get_logger(__name__)


class ModelTier(str, Enum):
    """Model tier levels."""
    FAST = "fast"  # Quick, cheap models for simple tasks
    BALANCED = "balanced"  # Medium cost/quality for typical tasks
    POWERFUL = "powerful"  # Expensive, high-quality for complex tasks


class ModelProvider(str, Enum):
    """Model providers."""
    OPENAI = "openai"
    ANTHROPIC = "anthropic"
    LOCAL = "local"
    AZURE = "azure"


@dataclass
class ModelConfig:
    """Configuration for a specific model."""

    model_id: str
    provider: ModelProvider
    tier: ModelTier
    cost_per_1k_input: float  # USD per 1K input tokens
    cost_per_1k_output: float  # USD per 1K output tokens
    max_tokens: int
    context_window: int
    supports_streaming: bool = True
    metadata: Dict[str, Any] = field(default_factory=dict)

    def estimate_cost(self, input_tokens: int, output_tokens: int) -> float:
        """Estimate cost for token counts.

        Args:
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens

        Returns:
            Estimated cost in USD
        """
        input_cost = (input_tokens / 1000) * self.cost_per_1k_input
        output_cost = (output_tokens / 1000) * self.cost_per_1k_output
        return input_cost + output_cost


# Default model configurations
DEFAULT_MODELS = {
    ModelTier.FAST: [
        ModelConfig(
            model_id="gpt-3.5-turbo",
            provider=ModelProvider.OPENAI,
            tier=ModelTier.FAST,
            cost_per_1k_input=0.0005,
            cost_per_1k_output=0.0015,
            max_tokens=4096,
            context_window=16385,
        ),
        ModelConfig(
            model_id="claude-3-haiku-20240307",
            provider=ModelProvider.ANTHROPIC,
            tier=ModelTier.FAST,
            cost_per_1k_input=0.00025,
            cost_per_1k_output=0.00125,
            max_tokens=4096,
            context_window=200000,
        ),
    ],
    ModelTier.BALANCED: [
        ModelConfig(
            model_id="gpt-4-turbo",
            provider=ModelProvider.OPENAI,
            tier=ModelTier.BALANCED,
            cost_per_1k_input=0.01,
            cost_per_1k_output=0.03,
            max_tokens=4096,
            context_window=128000,
        ),
        ModelConfig(
            model_id="claude-3-sonnet-20240229",
            provider=ModelProvider.ANTHROPIC,
            tier=ModelTier.BALANCED,
            cost_per_1k_input=0.003,
            cost_per_1k_output=0.015,
            max_tokens=4096,
            context_window=200000,
        ),
    ],
    ModelTier.POWERFUL: [
        ModelConfig(
            model_id="gpt-4",
            provider=ModelProvider.OPENAI,
            tier=ModelTier.POWERFUL,
            cost_per_1k_input=0.03,
            cost_per_1k_output=0.06,
            max_tokens=8192,
            context_window=8192,
        ),
        ModelConfig(
            model_id="claude-3-opus-20240229",
            provider=ModelProvider.ANTHROPIC,
            tier=ModelTier.POWERFUL,
            cost_per_1k_input=0.015,
            cost_per_1k_output=0.075,
            max_tokens=4096,
            context_window=200000,
        ),
    ],
}


class RoutingStrategy(str, Enum):
    """Strategies for model selection."""
    CHEAPEST = "cheapest"  # Always use cheapest model
    FASTEST = "fastest"  # Always use fastest model
    BEST_QUALITY = "best_quality"  # Always use best model
    ADAPTIVE = "adaptive"  # Adapt based on task complexity
    BUDGET_AWARE = "budget_aware"  # Consider remaining budget


@dataclass
class RoutingDecision:
    """Decision about which model to use."""

    model_config: ModelConfig
    tier: ModelTier
    reason: str
    estimated_cost: float
    alternatives_considered: List[str] = field(default_factory=list)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'model_id': self.model_config.model_id,
            'provider': self.model_config.provider.value,
            'tier': self.tier.value,
            'reason': self.reason,
            'estimated_cost': self.estimated_cost,
            'alternatives_considered': self.alternatives_considered,
            'metadata': self.metadata,
        }


@dataclass
class EscalationConfig:
    """Configuration for escalation behavior."""

    max_retries: int = 3
    escalate_on_error: bool = True
    escalate_on_quality: bool = True
    quality_threshold: float = 0.7  # Escalate if quality below this
    cooldown_seconds: int = 60
    max_tier: ModelTier = ModelTier.POWERFUL


class ModelRouter:
    """Route requests to appropriate models with escalation."""

    def __init__(
        self,
        strategy: RoutingStrategy = RoutingStrategy.ADAPTIVE,
        budget_per_day: Optional[float] = None,
        escalation_config: Optional[EscalationConfig] = None,
    ):
        """Initialize model router.

        Args:
            strategy: Routing strategy to use
            budget_per_day: Daily budget in USD
            escalation_config: Escalation configuration
        """
        self.strategy = strategy
        self.budget_per_day = budget_per_day
        self.escalation_config = escalation_config or EscalationConfig()
        self.models = DEFAULT_MODELS.copy()

        # Tracking
        self.total_cost = 0.0
        self.requests_by_tier: Dict[ModelTier, int] = {
            tier: 0 for tier in ModelTier
        }
        self.escalations: List[Dict[str, Any]] = []
        self.failures_by_model: Dict[str, int] = {}

    def add_model(self, model: ModelConfig):
        """Add a custom model configuration.

        Args:
            model: Model configuration
        """
        if model.tier not in self.models:
            self.models[model.tier] = []

        self.models[model.tier].append(model)
        logger.info("Added model", model_id=model.model_id, tier=model.tier.value)

    def route(
        self,
        task_description: str,
        estimated_input_tokens: int,
        estimated_output_tokens: int = 1000,
        required_tier: Optional[ModelTier] = None,
        context_size: Optional[int] = None,
    ) -> RoutingDecision:
        """Route request to appropriate model.

        Args:
            task_description: Description of task
            estimated_input_tokens: Estimated input tokens
            estimated_output_tokens: Estimated output tokens
            required_tier: Override tier selection
            context_size: Required context window size

        Returns:
            Routing decision
        """
        # Determine tier
        if required_tier:
            tier = required_tier
            reason = f"Required tier: {required_tier.value}"
        else:
            tier = self._determine_tier(
                task_description,
                estimated_input_tokens,
                estimated_output_tokens,
            )
            reason = f"Strategy: {self.strategy.value}"

        # Get candidates for tier
        candidates = self.models.get(tier, [])

        if not candidates:
            logger.warning("No models for tier, using balanced", tier=tier.value)
            tier = ModelTier.BALANCED
            candidates = self.models[tier]
            reason += " (fallback to balanced)"

        # Filter by context window if needed
        if context_size:
            candidates = [
                m for m in candidates
                if m.context_window >= context_size
            ]

            if not candidates:
                logger.warning("No models with sufficient context, escalating")
                tier = self._escalate_tier(tier)
                candidates = self.models[tier]
                reason += " (escalated for context)"

        # Select model from candidates
        model = self._select_from_candidates(
            candidates,
            estimated_input_tokens,
            estimated_output_tokens,
        )

        # Calculate cost
        estimated_cost = model.estimate_cost(
            estimated_input_tokens,
            estimated_output_tokens,
        )

        # Check budget
        if self.budget_per_day:
            remaining_budget = self.budget_per_day - self.total_cost
            if estimated_cost > remaining_budget:
                logger.warning(
                    "Over budget, downgrading",
                    estimated=estimated_cost,
                    remaining=remaining_budget,
                )
                # Try cheaper tier
                if tier != ModelTier.FAST:
                    cheaper_tier = self._downgrade_tier(tier)
                    model = self._select_from_candidates(
                        self.models[cheaper_tier],
                        estimated_input_tokens,
                        estimated_output_tokens,
                    )
                    tier = cheaper_tier
                    reason += " (budget constrained)"
                    estimated_cost = model.estimate_cost(
                        estimated_input_tokens,
                        estimated_output_tokens,
                    )

        # Track request
        self.requests_by_tier[tier] += 1

        decision = RoutingDecision(
            model_config=model,
            tier=tier,
            reason=reason,
            estimated_cost=estimated_cost,
            alternatives_considered=[c.model_id for c in candidates if c != model],
        )

        logger.info(
            "Routed request",
            model=model.model_id,
            tier=tier.value,
            cost=estimated_cost,
        )

        return decision

    def _determine_tier(
        self,
        task_description: str,
        input_tokens: int,
        output_tokens: int,
    ) -> ModelTier:
        """Determine appropriate tier for task.

        Args:
            task_description: Task description
            input_tokens: Input token count
            output_tokens: Output token count

        Returns:
            Recommended tier
        """
        if self.strategy == RoutingStrategy.CHEAPEST:
            return ModelTier.FAST

        if self.strategy == RoutingStrategy.BEST_QUALITY:
            return ModelTier.POWERFUL

        if self.strategy == RoutingStrategy.ADAPTIVE:
            # Analyze task complexity
            complexity_score = self._assess_complexity(task_description)

            # Large context requires better models
            if input_tokens > 50000:
                complexity_score += 0.3

            if complexity_score < 0.3:
                return ModelTier.FAST
            elif complexity_score < 0.7:
                return ModelTier.BALANCED
            else:
                return ModelTier.POWERFUL

        if self.strategy == RoutingStrategy.BUDGET_AWARE:
            if not self.budget_per_day:
                return ModelTier.BALANCED

            # Check budget utilization
            utilization = self.total_cost / self.budget_per_day

            if utilization < 0.5:
                return ModelTier.BALANCED
            elif utilization < 0.8:
                return ModelTier.FAST
            else:
                # Almost out of budget
                return ModelTier.FAST

        # Default
        return ModelTier.BALANCED

    def _assess_complexity(self, task_description: str) -> float:
        """Assess task complexity.

        Args:
            task_description: Task description

        Returns:
            Complexity score 0-1
        """
        score = 0.3  # Base score

        # Keywords indicating complexity
        complex_keywords = [
            'complex', 'difficult', 'optimize', 'refactor',
            'architecture', 'design', 'algorithm', 'performance',
            'security', 'critical', 'production',
        ]

        simple_keywords = [
            'simple', 'basic', 'trivial', 'fix typo',
            'add comment', 'format', 'rename',
        ]

        desc_lower = task_description.lower()

        for keyword in complex_keywords:
            if keyword in desc_lower:
                score += 0.15

        for keyword in simple_keywords:
            if keyword in desc_lower:
                score -= 0.15

        # Long descriptions suggest complexity
        if len(task_description) > 200:
            score += 0.1

        return max(0.0, min(1.0, score))

    def _select_from_candidates(
        self,
        candidates: List[ModelConfig],
        input_tokens: int,
        output_tokens: int,
    ) -> ModelConfig:
        """Select best model from candidates.

        Args:
            candidates: Candidate models
            input_tokens: Input token count
            output_tokens: Output token count

        Returns:
            Selected model
        """
        if not candidates:
            raise ValueError("No candidate models available")

        # Score candidates
        scored = []
        for model in candidates:
            score = 0.0

            # Prefer models with fewer failures
            failures = self.failures_by_model.get(model.model_id, 0)
            score -= failures * 0.2

            # Prefer cheaper models (within tier)
            cost = model.estimate_cost(input_tokens, output_tokens)
            max_cost = max(
                m.estimate_cost(input_tokens, output_tokens)
                for m in candidates
            )
            if max_cost > 0:
                score += (1 - (cost / max_cost)) * 0.3

            # Prefer larger context windows
            max_context = max(m.context_window for m in candidates)
            score += (model.context_window / max_context) * 0.2

            scored.append((score, model))

        # Return highest scoring
        scored.sort(key=lambda x: x[0], reverse=True)
        return scored[0][1]

    def _escalate_tier(self, current_tier: ModelTier) -> ModelTier:
        """Escalate to next tier.

        Args:
            current_tier: Current tier

        Returns:
            Next tier
        """
        if current_tier == ModelTier.FAST:
            return ModelTier.BALANCED
        elif current_tier == ModelTier.BALANCED:
            return ModelTier.POWERFUL
        else:
            return ModelTier.POWERFUL  # Already at max

    def _downgrade_tier(self, current_tier: ModelTier) -> ModelTier:
        """Downgrade to cheaper tier.

        Args:
            current_tier: Current tier

        Returns:
            Cheaper tier
        """
        if current_tier == ModelTier.POWERFUL:
            return ModelTier.BALANCED
        elif current_tier == ModelTier.BALANCED:
            return ModelTier.FAST
        else:
            return ModelTier.FAST  # Already at min

    def handle_failure(
        self,
        model_id: str,
        error: str,
        attempt: int = 1,
    ) -> Optional[RoutingDecision]:
        """Handle model failure with retry/escalation.

        Args:
            model_id: ID of failed model
            error: Error message
            attempt: Attempt number
            Returns:
            New routing decision or None if max retries exceeded
        """
        # Track failure
        if model_id not in self.failures_by_model:
            self.failures_by_model[model_id] = 0
        self.failures_by_model[model_id] += 1

        logger.warning(
            "Model failure",
            model=model_id,
            error=error,
            attempt=attempt,
            total_failures=self.failures_by_model[model_id],
        )

        # Check retry limit
        if attempt >= self.escalation_config.max_retries:
            logger.error("Max retries exceeded", model=model_id)
            return None

        # Find current model tier
        current_tier = None
        for tier, models in self.models.items():
            if any(m.model_id == model_id for m in models):
                current_tier = tier
                break

        if not current_tier:
            logger.error("Model not found", model=model_id)
            return None

        # Escalate if enabled
        if self.escalation_config.escalate_on_error:
            new_tier = self._escalate_tier(current_tier)

            if new_tier == current_tier:
                logger.warning("Already at max tier", tier=current_tier.value)
                return None

            logger.info("Escalating tier", from_tier=current_tier.value, to_tier=new_tier.value)

            # Select model from new tier
            candidates = self.models[new_tier]
            # Filter out failed model
            candidates = [m for m in candidates if m.model_id != model_id]

            if not candidates:
                return None

            model = candidates[0]  # Simple selection for retry

            self.escalations.append({
                'timestamp': datetime.utcnow().isoformat(),
                'from_model': model_id,
                'to_model': model.model_id,
                'from_tier': current_tier.value,
                'to_tier': new_tier.value,
                'reason': 'failure',
                'attempt': attempt,
            })

            return RoutingDecision(
                model_config=model,
                tier=new_tier,
                reason=f"Escalated after failure (attempt {attempt})",
                estimated_cost=0.0,  # Will be calculated by caller
            )

        return None

    def record_cost(self, actual_cost: float):
        """Record actual cost incurred.

        Args:
            actual_cost: Cost in USD
        """
        self.total_cost += actual_cost
        logger.debug("Cost recorded", cost=actual_cost, total=self.total_cost)

    def get_stats(self) -> Dict[str, Any]:
        """Get routing statistics.

        Returns:
            Statistics dictionary
        """
        return {
            'total_cost': self.total_cost,
            'budget_per_day': self.budget_per_day,
            'budget_remaining': (
                self.budget_per_day - self.total_cost
                if self.budget_per_day else None
            ),
            'budget_utilization': (
                self.total_cost / self.budget_per_day
                if self.budget_per_day else None
            ),
            'requests_by_tier': {
                tier.value: count
                for tier, count in self.requests_by_tier.items()
            },
            'total_requests': sum(self.requests_by_tier.values()),
            'escalations': len(self.escalations),
            'failures_by_model': self.failures_by_model,
            'strategy': self.strategy.value,
        }
