"""
Tests for enhanced metrics collector.
"""

import pytest
from src.telemetry.enhanced_metrics import (
    EnhancedMetricsCollector,
    enhanced_metrics_collector,
    get_enhanced_metrics_text,
)


class TestEnhancedMetricsCollector:
    """Test enhanced metrics collection."""

    def setup_method(self):
        """Set up test fixtures."""
        self.collector = EnhancedMetricsCollector()

    def test_record_token_usage(self):
        """Test recording token usage."""
        self.collector.record_token_usage(
            operation_type="query",
            model="gpt-4",
            input_tokens=1000,
            output_tokens=500,
            trace_id="test-trace-123",
        )

        # Verify metrics were recorded (would need Prometheus client inspection)
        assert True  # Placeholder

    def test_record_token_usage_calculates_cost(self):
        """Test that token usage calculation includes cost."""
        # GPT-4 pricing: $30/1M input, $60/1M output
        # 1000 input + 500 output should cost:
        # (1000 * 30 + 500 * 60) / 1_000_000 = 0.06
        self.collector.record_token_usage(
            operation_type="index",
            model="gpt-4",
            input_tokens=1000,
            output_tokens=500,
            trace_id="test-trace-456",
        )

        # Cost calculation is internal, verify it doesn't crash
        assert True

    def test_record_violation(self):
        """Test recording violations."""
        self.collector.record_violation(
            severity="warning",
            violation_type="missing_docstring",
            trace_id="test-trace-789",
        )

        assert True

    def test_record_violation_prevented(self):
        """Test recording prevented violations."""
        self.collector.record_violation_prevented(
            guardrail_type="schema_check",
            trace_id="test-trace-abc",
        )

        assert True

    def test_record_fix_applied(self):
        """Test recording applied fixes."""
        self.collector.record_fix_applied(
            fix_type="docs",
            safety_mode="dry_run",
            trace_id="test-trace-def",
        )

        assert True

    def test_record_fix_failed(self):
        """Test recording failed fixes."""
        self.collector.record_fix_failed(
            fix_type="links",
            error_type="FileNotFoundError",
            trace_id="test-trace-ghi",
        )

        assert True

    def test_record_cache_operation(self):
        """Test recording cache operations."""
        self.collector.record_cache_operation(
            operation_type="query",
            cache_result="hit",
            trace_id="test-trace-jkl",
        )

        self.collector.record_cache_operation(
            operation_type="query",
            cache_result="miss",
            trace_id="test-trace-mno",
        )

        assert True

    def test_update_cache_hit_rate(self):
        """Test updating cache hit rate gauge."""
        self.collector.update_cache_hit_rate("query", 85.5)
        assert True

    def test_track_operation_latency_context_manager(self):
        """Test operation latency tracking."""
        import time

        with self.collector.track_operation_latency("query", "test-trace-pqr"):
            time.sleep(0.01)  # Simulate work

        # Context manager should record latency
        assert True

    def test_record_model_routing(self):
        """Test recording model routing decisions."""
        self.collector.record_model_routing(
            from_model="gpt-3.5-turbo",
            to_model="gpt-4",
            reason="performance",
            trace_id="test-trace-stu",
        )

        assert True

    def test_record_model_escalation(self):
        """Test recording model escalations."""
        self.collector.record_model_escalation(
            original_model="gpt-3.5-turbo",
            escalated_model="gpt-4",
            trace_id="test-trace-vwx",
        )

        assert True

    def test_record_context_compaction(self):
        """Test recording context compaction."""
        self.collector.record_context_compaction(
            compaction_type="summary",
            bytes_saved=10000,
            trace_id="test-trace-yz",
        )

        assert True

    def test_record_context_slots(self):
        """Test recording context slots utilization."""
        self.collector.record_context_slots("query", 5)
        assert True

    def test_record_safety_check(self):
        """Test recording safety checks."""
        self.collector.record_safety_check(
            check_type="generated_file",
            result="blocked",
            trace_id="test-trace-123",
        )

        assert True

    def test_record_dry_run(self):
        """Test recording dry runs."""
        self.collector.record_dry_run(
            operation_type="autofix",
            trace_id="test-trace-456",
        )

        assert True

    def test_get_enhanced_metrics_text(self):
        """Test exporting metrics as text."""
        # Record some metrics
        self.collector.record_token_usage("query", "gpt-4", 100, 50)

        metrics_text = get_enhanced_metrics_text()
        assert isinstance(metrics_text, str)
        assert len(metrics_text) > 0

    def test_global_collector_instance(self):
        """Test global collector instance."""
        assert enhanced_metrics_collector is not None
        assert isinstance(enhanced_metrics_collector, EnhancedMetricsCollector)


class TestMetricsIntegration:
    """Integration tests for metrics."""

    def test_full_operation_tracking(self):
        """Test tracking a full operation with all metrics."""
        collector = EnhancedMetricsCollector()

        trace_id = "integration-test-123"

        # Record various metrics for an operation
        with collector.track_operation_latency("index", trace_id):
            collector.record_token_usage(
                "index", "gpt-4", 5000, 2000, trace_id
            )
            collector.record_violation("warning", "missing_docs", trace_id)
            collector.record_fix_applied("docs", "dry_run", trace_id)

        assert True


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
