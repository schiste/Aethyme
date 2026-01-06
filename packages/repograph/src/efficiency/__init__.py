"""Efficiency features for cost-effective AI operations.

This module provides context management, model routing, and tracking
capabilities to optimize AI operations while maintaining quality.
"""

from .context import (
    ContextManager,
    ContextCompactor,
    WorkingMemorySlot,
    ContextPlaylist,
    OutcomeCard,
)
from .model_router import (
    ModelRouter,
    ModelTier,
    ModelConfig,
    RoutingStrategy,
)
from .tracking import (
    TokenTracker,
    CostTracker,
    BudgetEnforcer,
    UsageStats,
)
from .metrics import (
    GuardrailsMetrics,
    EfficiencyMetrics,
    emit_token_usage,
    emit_cost,
    emit_violation,
)

__all__ = [
    'ContextManager',
    'ContextCompactor',
    'WorkingMemorySlot',
    'ContextPlaylist',
    'OutcomeCard',
    'ModelRouter',
    'ModelTier',
    'ModelConfig',
    'RoutingStrategy',
    'TokenTracker',
    'CostTracker',
    'BudgetEnforcer',
    'UsageStats',
    'GuardrailsMetrics',
    'EfficiencyMetrics',
    'emit_token_usage',
    'emit_cost',
    'emit_violation',
]
