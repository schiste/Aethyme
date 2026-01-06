"""Prometheus metrics for guardrails and efficiency.

This module provides metrics collection for monitoring guardrails
effectiveness, token usage, costs, and model routing decisions.
"""

from typing import Dict, Optional, Any
from prometheus_client import Counter, Histogram, Gauge, Summary
import structlog

logger = structlog.get_logger(__name__)


# Token usage metrics
tokens_total = Counter(
    'repograph_tokens_total',
    'Total tokens used',
    ['operation_type', 'model_id', 'tenant_id'],
)

tokens_input = Counter(
    'repograph_tokens_input',
    'Input tokens',
    ['operation_type', 'model_id', 'tenant_id'],
)

tokens_output = Counter(
    'repograph_tokens_output',
    'Output tokens',
    ['operation_type', 'model_id', 'tenant_id'],
)

# Cost metrics
cost_total = Counter(
    'repograph_cost_total',
    'Total cost in USD',
    ['tenant_id', 'model_id', 'operation_type'],
)

cost_per_operation = Histogram(
    'repograph_cost_per_operation',
    'Cost per operation in USD',
    ['operation_type', 'model_id'],
    buckets=[0.0001, 0.001, 0.01, 0.1, 1.0, 10.0],
)

# Guardrail metrics
violations_prevented = Counter(
    'repograph_violations_prevented',
    'Number of violations prevented by guardrails',
    ['rule_id', 'risk_level', 'tenant_id'],
)

preflight_checks = Counter(
    'repograph_preflight_checks',
    'Number of preflight checks performed',
    ['result', 'tenant_id'],
)

schema_validations = Counter(
    'repograph_schema_validations',
    'Schema validation attempts',
    ['result', 'schema_type'],
)

# Model routing metrics
model_escalations = Counter(
    'repograph_model_escalations',
    'Number of model escalations',
    ['from_tier', 'to_tier', 'reason', 'tenant_id'],
)

model_requests = Counter(
    'repograph_model_requests',
    'Model requests by tier',
    ['tier', 'model_id', 'tenant_id'],
)

model_failures = Counter(
    'repograph_model_failures',
    'Model failures',
    ['model_id', 'error_type', 'tenant_id'],
)

routing_latency = Histogram(
    'repograph_routing_latency_seconds',
    'Time spent routing requests',
    ['strategy'],
    buckets=[0.001, 0.005, 0.01, 0.05, 0.1, 0.5],
)

# Context management metrics
context_size_bytes = Histogram(
    'repograph_context_size_bytes',
    'Context size distribution',
    ['operation_type'],
    buckets=[1000, 10000, 50000, 100000, 500000, 1000000, 5000000],
)

context_compaction_ratio = Histogram(
    'repograph_context_compaction_ratio',
    'Context compaction ratio (tokens_saved / original_tokens)',
    [],
    buckets=[0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0],
)

working_memory_utilization = Gauge(
    'repograph_working_memory_utilization',
    'Working memory slot utilization',
    ['slot_id', 'tenant_id'],
)

# Budget metrics
budget_utilization = Gauge(
    'repograph_budget_utilization',
    'Budget utilization percentage',
    ['budget_id', 'tenant_id', 'window'],
)

budget_exceeded = Counter(
    'repograph_budget_exceeded',
    'Number of times budget was exceeded',
    ['budget_id', 'tenant_id'],
)

# Outcome metrics
outcomes_created = Counter(
    'repograph_outcomes_created',
    'Number of outcome cards created',
    ['status', 'tenant_id'],
)

playlists_created = Counter(
    'repograph_playlists_created',
    'Number of context playlists created',
    ['tenant_id'],
)

# Performance metrics
operation_duration = Histogram(
    'repograph_operation_duration_seconds',
    'Operation duration',
    ['operation_type'],
    buckets=[0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0],
)

guardrail_check_duration = Histogram(
    'repograph_guardrail_check_duration_seconds',
    'Guardrail check duration',
    ['check_type'],
    buckets=[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0],
)


class GuardrailsMetrics:
    """Metrics collector for guardrails."""

    @staticmethod
    def record_violation(
        rule_id: str,
        risk_level: str,
        tenant_id: str,
    ):
        """Record a prevented violation.

        Args:
            rule_id: Rule identifier
            risk_level: Risk level (low/medium/high/critical)
            tenant_id: Tenant identifier
        """
        violations_prevented.labels(
            rule_id=rule_id,
            risk_level=risk_level,
            tenant_id=tenant_id,
        ).inc()

        logger.debug(
            "Violation recorded",
            rule=rule_id,
            risk=risk_level,
            tenant=tenant_id,
        )

    @staticmethod
    def record_preflight_check(result: str, tenant_id: str):
        """Record a preflight check.

        Args:
            result: Result (passed/failed/blocked)
            tenant_id: Tenant identifier
        """
        preflight_checks.labels(
            result=result,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_schema_validation(result: str, schema_type: str):
        """Record a schema validation.

        Args:
            result: Result (valid/invalid)
            schema_type: Type of schema
        """
        schema_validations.labels(
            result=result,
            schema_type=schema_type,
        ).inc()

    @staticmethod
    def record_check_duration(check_type: str, duration: float):
        """Record guardrail check duration.

        Args:
            check_type: Type of check
            duration: Duration in seconds
        """
        guardrail_check_duration.labels(check_type=check_type).observe(duration)


class EfficiencyMetrics:
    """Metrics collector for efficiency features."""

    @staticmethod
    def record_token_usage(
        operation_type: str,
        model_id: str,
        tenant_id: str,
        input_tokens: int,
        output_tokens: int,
    ):
        """Record token usage.

        Args:
            operation_type: Type of operation
            model_id: Model identifier
            tenant_id: Tenant identifier
            input_tokens: Input token count
            output_tokens: Output token count
        """
        labels = {
            'operation_type': operation_type,
            'model_id': model_id,
            'tenant_id': tenant_id,
        }

        tokens_total.labels(**labels).inc(input_tokens + output_tokens)
        tokens_input.labels(**labels).inc(input_tokens)
        tokens_output.labels(**labels).inc(output_tokens)

    @staticmethod
    def record_cost(
        tenant_id: str,
        model_id: str,
        operation_type: str,
        cost_usd: float,
    ):
        """Record operation cost.

        Args:
            tenant_id: Tenant identifier
            model_id: Model identifier
            operation_type: Type of operation
            cost_usd: Cost in USD
        """
        cost_total.labels(
            tenant_id=tenant_id,
            model_id=model_id,
            operation_type=operation_type,
        ).inc(cost_usd)

        cost_per_operation.labels(
            operation_type=operation_type,
            model_id=model_id,
        ).observe(cost_usd)

    @staticmethod
    def record_escalation(
        from_tier: str,
        to_tier: str,
        reason: str,
        tenant_id: str,
    ):
        """Record a model escalation.

        Args:
            from_tier: Original tier
            to_tier: Escalated tier
            reason: Reason for escalation
            tenant_id: Tenant identifier
        """
        model_escalations.labels(
            from_tier=from_tier,
            to_tier=to_tier,
            reason=reason,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_model_request(tier: str, model_id: str, tenant_id: str):
        """Record a model request.

        Args:
            tier: Model tier
            model_id: Model identifier
            tenant_id: Tenant identifier
        """
        model_requests.labels(
            tier=tier,
            model_id=model_id,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_model_failure(
        model_id: str,
        error_type: str,
        tenant_id: str,
    ):
        """Record a model failure.

        Args:
            model_id: Model identifier
            error_type: Type of error
            tenant_id: Tenant identifier
        """
        model_failures.labels(
            model_id=model_id,
            error_type=error_type,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_context_size(operation_type: str, size_bytes: int):
        """Record context size.

        Args:
            operation_type: Type of operation
            size_bytes: Size in bytes
        """
        context_size_bytes.labels(operation_type=operation_type).observe(size_bytes)

    @staticmethod
    def record_compaction_ratio(ratio: float):
        """Record context compaction ratio.

        Args:
            ratio: Compaction ratio (0-1)
        """
        context_compaction_ratio.observe(ratio)

    @staticmethod
    def set_memory_utilization(slot_id: str, tenant_id: str, utilization: float):
        """Set working memory utilization.

        Args:
            slot_id: Slot identifier
            tenant_id: Tenant identifier
            utilization: Utilization percentage (0-1)
        """
        working_memory_utilization.labels(
            slot_id=slot_id,
            tenant_id=tenant_id,
        ).set(utilization)

    @staticmethod
    def set_budget_utilization(
        budget_id: str,
        tenant_id: str,
        window: str,
        utilization: float,
    ):
        """Set budget utilization.

        Args:
            budget_id: Budget identifier
            tenant_id: Tenant identifier
            window: Time window
            utilization: Utilization percentage (0-1)
        """
        budget_utilization.labels(
            budget_id=budget_id,
            tenant_id=tenant_id,
            window=window,
        ).set(utilization)

    @staticmethod
    def record_budget_exceeded(budget_id: str, tenant_id: str):
        """Record budget exceeded event.

        Args:
            budget_id: Budget identifier
            tenant_id: Tenant identifier
        """
        budget_exceeded.labels(
            budget_id=budget_id,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_outcome_created(status: str, tenant_id: str):
        """Record outcome card creation.

        Args:
            status: Outcome status
            tenant_id: Tenant identifier
        """
        outcomes_created.labels(
            status=status,
            tenant_id=tenant_id,
        ).inc()

    @staticmethod
    def record_playlist_created(tenant_id: str):
        """Record playlist creation.

        Args:
            tenant_id: Tenant identifier
        """
        playlists_created.labels(tenant_id=tenant_id).inc()

    @staticmethod
    def record_operation_duration(operation_type: str, duration: float):
        """Record operation duration.

        Args:
            operation_type: Type of operation
            duration: Duration in seconds
        """
        operation_duration.labels(operation_type=operation_type).observe(duration)


# Convenience functions
def emit_token_usage(
    operation_type: str,
    model_id: str,
    tenant_id: str,
    input_tokens: int,
    output_tokens: int,
):
    """Emit token usage metrics.

    Args:
        operation_type: Type of operation
        model_id: Model identifier
        tenant_id: Tenant identifier
        input_tokens: Input token count
        output_tokens: Output token count
    """
    EfficiencyMetrics.record_token_usage(
        operation_type,
        model_id,
        tenant_id,
        input_tokens,
        output_tokens,
    )


def emit_cost(
    tenant_id: str,
    model_id: str,
    operation_type: str,
    cost_usd: float,
):
    """Emit cost metrics.

    Args:
        tenant_id: Tenant identifier
        model_id: Model identifier
        operation_type: Type of operation
        cost_usd: Cost in USD
    """
    EfficiencyMetrics.record_cost(tenant_id, model_id, operation_type, cost_usd)


def emit_violation(rule_id: str, risk_level: str, tenant_id: str):
    """Emit violation metric.

    Args:
        rule_id: Rule identifier
        risk_level: Risk level
        tenant_id: Tenant identifier
    """
    GuardrailsMetrics.record_violation(rule_id, risk_level, tenant_id)
