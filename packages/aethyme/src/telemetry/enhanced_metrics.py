"""
Enhanced Prometheus Metrics for Aethyme.

Extends existing metrics from S1-T2 and S1-T3 with:
- Token usage per operation
- Cost tracking (tokens to dollars)
- Violations prevented count
- Fixes applied count
- Cache hit rates
- All metrics correlated with trace IDs
"""

import time
import structlog
from typing import Dict, Optional, Any
from contextlib import contextmanager
from prometheus_client import (
    Counter,
    Gauge,
    Histogram,
    Info,
    CollectorRegistry,
    generate_latest,
)

logger = structlog.get_logger(__name__)

# Create enhanced metrics registry (separate from base metrics)
enhanced_registry = CollectorRegistry()

# Token usage metrics
tokens_consumed_total = Counter(
    "aethyme_tokens_consumed_total",
    "Total tokens consumed by operation type",
    labelnames=["operation_type", "model", "trace_id"],
    registry=enhanced_registry,
)

tokens_per_operation = Histogram(
    "aethyme_tokens_per_operation",
    "Tokens consumed per operation",
    labelnames=["operation_type", "model"],
    buckets=[100, 500, 1000, 2500, 5000, 10000, 25000, 50000, 100000],
    registry=enhanced_registry,
)

# Cost tracking metrics
cost_dollars_total = Counter(
    "aethyme_cost_dollars_total",
    "Total cost in dollars",
    labelnames=["operation_type", "model", "trace_id"],
    registry=enhanced_registry,
)

cost_per_operation = Histogram(
    "aethyme_cost_per_operation_dollars",
    "Cost per operation in dollars",
    labelnames=["operation_type", "model"],
    buckets=[0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0],
    registry=enhanced_registry,
)

# Violations and fixes metrics
violations_detected_total = Counter(
    "aethyme_violations_detected_total",
    "Total violations detected by scorecard",
    labelnames=["severity", "violation_type", "trace_id"],
    registry=enhanced_registry,
)

violations_prevented_total = Counter(
    "aethyme_violations_prevented_total",
    "Violations prevented by guardrails",
    labelnames=["guardrail_type", "trace_id"],
    registry=enhanced_registry,
)

fixes_applied_total = Counter(
    "aethyme_fixes_applied_total",
    "Total fixes successfully applied",
    labelnames=["fix_type", "safety_mode", "trace_id"],
    registry=enhanced_registry,
)

fixes_failed_total = Counter(
    "aethyme_fixes_failed_total",
    "Total fixes that failed",
    labelnames=["fix_type", "error_type", "trace_id"],
    registry=enhanced_registry,
)

# Enhanced cache metrics with trace IDs
cache_operations_total = Counter(
    "aethyme_cache_operations_total",
    "Total cache operations by result",
    labelnames=["operation_type", "cache_result", "trace_id"],
    registry=enhanced_registry,
)

cache_hit_rate_gauge = Gauge(
    "aethyme_cache_hit_rate",
    "Current cache hit rate percentage",
    labelnames=["cache_type"],
    registry=enhanced_registry,
)

# Latency metrics with trace correlation
operation_latency_seconds = Histogram(
    "aethyme_operation_latency_seconds",
    "Operation latency with trace correlation",
    labelnames=["operation_type", "trace_id"],
    buckets=[0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0],
    registry=enhanced_registry,
)

# Model routing metrics
model_routing_decisions_total = Counter(
    "aethyme_model_routing_decisions_total",
    "Model routing decisions",
    labelnames=["from_model", "to_model", "reason", "trace_id"],
    registry=enhanced_registry,
)

model_escalations_total = Counter(
    "aethyme_model_escalations_total",
    "Model escalations due to failures",
    labelnames=["original_model", "escalated_model", "trace_id"],
    registry=enhanced_registry,
)

# Context management metrics
context_compaction_savings_bytes = Counter(
    "aethyme_context_compaction_savings_bytes",
    "Bytes saved by context compaction",
    labelnames=["compaction_type", "trace_id"],
    registry=enhanced_registry,
)

context_slots_utilized = Histogram(
    "aethyme_context_slots_utilized",
    "Number of context slots utilized",
    labelnames=["operation_type"],
    buckets=[1, 2, 3, 5, 8, 10, 15, 20],
    registry=enhanced_registry,
)

# Safety metrics
safety_checks_total = Counter(
    "aethyme_safety_checks_total",
    "Total safety checks performed",
    labelnames=["check_type", "result", "trace_id"],
    registry=enhanced_registry,
)

dry_run_executions_total = Counter(
    "aethyme_dry_run_executions_total",
    "Total dry-run executions",
    labelnames=["operation_type", "trace_id"],
    registry=enhanced_registry,
)


class EnhancedMetricsCollector:
    """
    Enhanced metrics collector for Aethyme operations.

    Provides comprehensive tracking of:
    - Token usage and costs
    - Violations and fixes
    - Cache performance
    - Model routing decisions
    - Safety checks

    All metrics are correlated with trace IDs for distributed tracing.
    """

    def __init__(self):
        self.logger = logger.bind(component="EnhancedMetricsCollector")
        # Model pricing (per 1M tokens) - configurable
        self.model_pricing = {
            "gpt-4": {"input": 30.0, "output": 60.0},
            "gpt-4-turbo": {"input": 10.0, "output": 30.0},
            "gpt-3.5-turbo": {"input": 0.5, "output": 1.5},
            "claude-3-opus": {"input": 15.0, "output": 75.0},
            "claude-3-sonnet": {"input": 3.0, "output": 15.0},
            "claude-3-haiku": {"input": 0.25, "output": 1.25},
        }

    def record_token_usage(
        self,
        operation_type: str,
        model: str,
        input_tokens: int,
        output_tokens: int,
        trace_id: Optional[str] = None,
    ):
        """
        Record token usage and calculate cost.

        Args:
            operation_type: Type of operation (index, query, scorecard, autofix)
            model: Model name
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens
            trace_id: Optional trace ID for correlation
        """
        total_tokens = input_tokens + output_tokens
        trace_id = trace_id or "unknown"

        # Record total tokens
        tokens_consumed_total.labels(
            operation_type=operation_type,
            model=model,
            trace_id=trace_id,
        ).inc(total_tokens)

        # Record histogram
        tokens_per_operation.labels(
            operation_type=operation_type,
            model=model,
        ).observe(total_tokens)

        # Calculate and record cost
        pricing = self.model_pricing.get(model, {"input": 1.0, "output": 2.0})
        cost = (input_tokens * pricing["input"] + output_tokens * pricing["output"]) / 1_000_000

        cost_dollars_total.labels(
            operation_type=operation_type,
            model=model,
            trace_id=trace_id,
        ).inc(cost)

        cost_per_operation.labels(
            operation_type=operation_type,
            model=model,
        ).observe(cost)

        self.logger.info(
            "Token usage recorded",
            operation_type=operation_type,
            model=model,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            total_tokens=total_tokens,
            cost_dollars=cost,
            trace_id=trace_id,
        )

    def record_violation(
        self,
        severity: str,
        violation_type: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a detected violation.

        Args:
            severity: Severity level (blocker, warning, info)
            violation_type: Type of violation
            trace_id: Optional trace ID
        """
        violations_detected_total.labels(
            severity=severity,
            violation_type=violation_type,
            trace_id=trace_id or "unknown",
        ).inc()

    def record_violation_prevented(
        self,
        guardrail_type: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a violation prevented by guardrails.

        Args:
            guardrail_type: Type of guardrail (schema_check, sentinel, budget_limit)
            trace_id: Optional trace ID
        """
        violations_prevented_total.labels(
            guardrail_type=guardrail_type,
            trace_id=trace_id or "unknown",
        ).inc()

        self.logger.info(
            "Violation prevented",
            guardrail_type=guardrail_type,
            trace_id=trace_id,
        )

    def record_fix_applied(
        self,
        fix_type: str,
        safety_mode: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a successfully applied fix.

        Args:
            fix_type: Type of fix (docs, links, selectors, i18n)
            safety_mode: Safety mode (dry_run, pr, direct)
            trace_id: Optional trace ID
        """
        fixes_applied_total.labels(
            fix_type=fix_type,
            safety_mode=safety_mode,
            trace_id=trace_id or "unknown",
        ).inc()

    def record_fix_failed(
        self,
        fix_type: str,
        error_type: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a failed fix attempt.

        Args:
            fix_type: Type of fix
            error_type: Type of error
            trace_id: Optional trace ID
        """
        fixes_failed_total.labels(
            fix_type=fix_type,
            error_type=error_type,
            trace_id=trace_id or "unknown",
        ).inc()

    def record_cache_operation(
        self,
        operation_type: str,
        cache_result: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a cache operation.

        Args:
            operation_type: Type of operation (query, index, scorecard)
            cache_result: Result (hit, miss, write, invalidate)
            trace_id: Optional trace ID
        """
        cache_operations_total.labels(
            operation_type=operation_type,
            cache_result=cache_result,
            trace_id=trace_id or "unknown",
        ).inc()

    def update_cache_hit_rate(self, cache_type: str, hit_rate: float):
        """
        Update cache hit rate gauge.

        Args:
            cache_type: Type of cache (query, index)
            hit_rate: Hit rate as percentage (0-100)
        """
        cache_hit_rate_gauge.labels(cache_type=cache_type).set(hit_rate)

    @contextmanager
    def track_operation_latency(
        self,
        operation_type: str,
        trace_id: Optional[str] = None,
    ):
        """
        Context manager to track operation latency.

        Args:
            operation_type: Type of operation
            trace_id: Optional trace ID
        """
        start_time = time.time()
        trace_id = trace_id or "unknown"

        try:
            yield
        finally:
            duration = time.time() - start_time
            operation_latency_seconds.labels(
                operation_type=operation_type,
                trace_id=trace_id,
            ).observe(duration)

    def record_model_routing(
        self,
        from_model: str,
        to_model: str,
        reason: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a model routing decision.

        Args:
            from_model: Original model
            to_model: Selected model
            reason: Reason for routing (budget, performance, availability)
            trace_id: Optional trace ID
        """
        model_routing_decisions_total.labels(
            from_model=from_model,
            to_model=to_model,
            reason=reason,
            trace_id=trace_id or "unknown",
        ).inc()

    def record_model_escalation(
        self,
        original_model: str,
        escalated_model: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a model escalation due to failure.

        Args:
            original_model: Model that failed
            escalated_model: Model escalated to
            trace_id: Optional trace ID
        """
        model_escalations_total.labels(
            original_model=original_model,
            escalated_model=escalated_model,
            trace_id=trace_id or "unknown",
        ).inc()

        self.logger.warning(
            "Model escalation",
            original_model=original_model,
            escalated_model=escalated_model,
            trace_id=trace_id,
        )

    def record_context_compaction(
        self,
        compaction_type: str,
        bytes_saved: int,
        trace_id: Optional[str] = None,
    ):
        """
        Record context compaction savings.

        Args:
            compaction_type: Type of compaction (summary, dedup, prune)
            bytes_saved: Number of bytes saved
            trace_id: Optional trace ID
        """
        context_compaction_savings_bytes.labels(
            compaction_type=compaction_type,
            trace_id=trace_id or "unknown",
        ).inc(bytes_saved)

    def record_context_slots(self, operation_type: str, slots_used: int):
        """
        Record context slots utilization.

        Args:
            operation_type: Type of operation
            slots_used: Number of slots utilized
        """
        context_slots_utilized.labels(
            operation_type=operation_type,
        ).observe(slots_used)

    def record_safety_check(
        self,
        check_type: str,
        result: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a safety check.

        Args:
            check_type: Type of check (generated_file, approval_required)
            result: Result (passed, blocked)
            trace_id: Optional trace ID
        """
        safety_checks_total.labels(
            check_type=check_type,
            result=result,
            trace_id=trace_id or "unknown",
        ).inc()

    def record_dry_run(
        self,
        operation_type: str,
        trace_id: Optional[str] = None,
    ):
        """
        Record a dry-run execution.

        Args:
            operation_type: Type of operation
            trace_id: Optional trace ID
        """
        dry_run_executions_total.labels(
            operation_type=operation_type,
            trace_id=trace_id or "unknown",
        ).inc()


def get_enhanced_metrics_text() -> str:
    """
    Get enhanced Prometheus metrics in text format.

    Returns:
        Metrics in Prometheus exposition format
    """
    return generate_latest(enhanced_registry).decode("utf-8")


# Global enhanced metrics collector instance
enhanced_metrics_collector = EnhancedMetricsCollector()
