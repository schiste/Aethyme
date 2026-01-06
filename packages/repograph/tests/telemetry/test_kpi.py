"""
Tests for KPI calculation and reporting.
"""

import pytest
import json
import tempfile
from datetime import datetime, timedelta
from src.telemetry.kpi import (
    KPICalculator,
    KPISnapshot,
    kpi_calculator,
    export_kpi_report,
)


class TestKPICalculator:
    """Test KPI calculator."""

    def setup_method(self):
        """Set up test fixtures."""
        self.calculator = KPICalculator()

    def test_record_operation_metrics(self):
        """Test recording operation metrics."""
        self.calculator.record_operation_metrics(
            operation_type="query",
            duration=0.150,
            tokens=1000,
            cost=0.05,
            success=True,
            cache_hit=False,
        )

        assert len(self.calculator._in_memory_metrics) == 1

    def test_calculate_percentile_empty(self):
        """Test percentile calculation with empty list."""
        result = self.calculator.calculate_percentile([], 95)
        assert result is None

    def test_calculate_percentile(self):
        """Test percentile calculation."""
        values = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]

        p50 = self.calculator.calculate_percentile(values, 50)
        assert p50 == pytest.approx(5.5, rel=0.1)

        p95 = self.calculator.calculate_percentile(values, 95)
        assert p95 == pytest.approx(9.5, rel=0.1)

        p99 = self.calculator.calculate_percentile(values, 99)
        assert p99 == pytest.approx(9.9, rel=0.1)

    def test_calculate_kpis_empty(self):
        """Test KPI calculation with no data."""
        snapshot = self.calculator.calculate_kpis()

        assert snapshot.index_latency_p50 is None
        assert snapshot.query_latency_p50 is None

    def test_calculate_kpis_with_index_data(self):
        """Test KPI calculation with index data."""
        # Record some index operations
        for i in range(10):
            self.calculator.record_operation_metrics(
                operation_type="index",
                duration=1.0 + i * 0.1,
                success=True,
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.index_latency_p50 is not None
        assert snapshot.index_latency_p95 is not None
        assert snapshot.index_latency_p99 is not None

    def test_calculate_kpis_with_query_data(self):
        """Test KPI calculation with query data."""
        # Record some query operations
        for i in range(10):
            self.calculator.record_operation_metrics(
                operation_type="search",
                duration=0.1 + i * 0.01,
                success=True,
                cache_hit=(i % 2 == 0),
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.query_latency_p50 is not None
        assert snapshot.query_cache_hit_rate is not None

    def test_calculate_kpis_with_token_data(self):
        """Test KPI calculation with token data."""
        for i in range(5):
            self.calculator.record_operation_metrics(
                operation_type="query",
                duration=0.1,
                tokens=1000 * (i + 1),
                cost=0.01 * (i + 1),
                success=True,
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.avg_tokens_per_task is not None
        assert snapshot.total_tokens == 15000  # 1000+2000+3000+4000+5000
        assert snapshot.avg_cost_per_task is not None
        assert snapshot.total_cost == pytest.approx(0.15)

    def test_calculate_kpis_with_fix_data(self):
        """Test KPI calculation with autofix data."""
        # Record 8 successful, 2 failed fixes
        for i in range(10):
            self.calculator.record_operation_metrics(
                operation_type="autofix",
                duration=0.5,
                success=(i < 8),
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.fix_success_rate == pytest.approx(80.0)
        assert snapshot.fix_attempts == 10
        assert snapshot.fix_successes == 8

    def test_calculate_kpis_with_cache_data(self):
        """Test KPI calculation with cache data."""
        # 7 hits, 3 misses = 70% hit rate
        for i in range(10):
            self.calculator.record_operation_metrics(
                operation_type="search",
                duration=0.1,
                success=True,
                cache_hit=(i < 7),
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.query_cache_hit_rate == pytest.approx(70.0)

    def test_calculate_kpis_with_errors(self):
        """Test KPI calculation with errors."""
        # 6 successful, 4 failed = 40% error rate
        for i in range(10):
            self.calculator.record_operation_metrics(
                operation_type="query",
                duration=0.1,
                success=(i < 6),
            )

        snapshot = self.calculator.calculate_kpis()

        assert snapshot.error_rate == pytest.approx(40.0)
        assert snapshot.total_operations == 10
        assert snapshot.failed_operations == 4

    def test_calculate_kpis_with_time_window(self):
        """Test KPI calculation with time window."""
        # Add old metrics
        old_time = datetime.utcnow() - timedelta(hours=2)
        self.calculator._in_memory_metrics.append({
            "timestamp": old_time.isoformat(),
            "operation_type": "query",
            "duration": 0.1,
            "success": True,
        })

        # Add recent metrics
        self.calculator.record_operation_metrics(
            operation_type="query",
            duration=0.2,
            success=True,
        )

        # Calculate with 1 hour window (should only include recent)
        snapshot = self.calculator.calculate_kpis(
            time_window=timedelta(hours=1)
        )

        # Should only count 1 operation
        assert snapshot.total_operations == 1

    def test_export_to_csv(self):
        """Test exporting to CSV."""
        # Record some data
        self.calculator.record_operation_metrics(
            operation_type="query",
            duration=0.1,
            tokens=1000,
            cost=0.01,
            success=True,
        )

        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.csv') as f:
            output_path = f.name

        self.calculator.export_to_csv(output_path)

        # Verify file was created
        import os
        assert os.path.exists(output_path)

        # Clean up
        os.unlink(output_path)

    def test_export_to_json(self):
        """Test exporting to JSON."""
        self.calculator.record_operation_metrics(
            operation_type="query",
            duration=0.1,
            success=True,
        )

        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.json') as f:
            output_path = f.name

        self.calculator.export_to_json(output_path)

        # Verify file was created and is valid JSON
        with open(output_path, 'r') as f:
            data = json.load(f)
            assert "snapshots" in data
            assert len(data["snapshots"]) > 0

        # Clean up
        import os
        os.unlink(output_path)

    def test_print_summary(self, capsys):
        """Test printing summary."""
        self.calculator.record_operation_metrics(
            operation_type="index",
            duration=1.5,
            tokens=5000,
            cost=0.10,
            success=True,
        )

        snapshot = self.calculator.calculate_kpis()
        self.calculator.print_summary(snapshot)

        captured = capsys.readouterr()
        assert "RepoGraph KPI Summary" in captured.out
        assert "LATENCY METRICS" in captured.out


class TestKPISnapshot:
    """Test KPI snapshot dataclass."""

    def test_kpi_snapshot_creation(self):
        """Test creating a KPI snapshot."""
        snapshot = KPISnapshot(
            timestamp="2025-01-01T00:00:00",
            index_latency_p50=1.0,
            index_latency_p95=2.0,
            query_latency_p50=0.1,
            total_tokens=10000,
            avg_cost_per_task=0.05,
        )

        assert snapshot.timestamp == "2025-01-01T00:00:00"
        assert snapshot.index_latency_p50 == 1.0
        assert snapshot.total_tokens == 10000


class TestGlobalFunctions:
    """Test global functions."""

    def test_global_kpi_calculator(self):
        """Test global kpi_calculator instance."""
        assert kpi_calculator is not None
        assert isinstance(kpi_calculator, KPICalculator)

    def test_export_kpi_report_csv(self):
        """Test global export_kpi_report for CSV."""
        # Add some data to global calculator
        kpi_calculator.record_operation_metrics(
            operation_type="query",
            duration=0.1,
            success=True,
        )

        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.csv') as f:
            output_path = f.name

        export_kpi_report(output_path, format="csv")

        import os
        assert os.path.exists(output_path)
        os.unlink(output_path)

    def test_export_kpi_report_json(self):
        """Test global export_kpi_report for JSON."""
        kpi_calculator.record_operation_metrics(
            operation_type="query",
            duration=0.1,
            success=True,
        )

        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.json') as f:
            output_path = f.name

        export_kpi_report(output_path, format="json")

        import os
        assert os.path.exists(output_path)
        os.unlink(output_path)

    def test_export_kpi_report_invalid_format(self):
        """Test export with invalid format."""
        with pytest.raises(ValueError):
            export_kpi_report("output.txt", format="txt")


class TestKPIIntegration:
    """Integration tests for KPI calculation."""

    def test_full_kpi_workflow(self):
        """Test complete KPI workflow."""
        calculator = KPICalculator()

        # Simulate various operations
        # Index operations
        for i in range(5):
            calculator.record_operation_metrics(
                operation_type="index",
                duration=1.0 + i * 0.2,
                success=True,
            )

        # Query operations
        for i in range(20):
            calculator.record_operation_metrics(
                operation_type="search",
                duration=0.1 + i * 0.01,
                tokens=500 + i * 50,
                cost=0.005 + i * 0.001,
                success=(i < 18),  # 2 failures
                cache_hit=(i % 3 == 0),  # ~33% hit rate
            )

        # Autofix operations
        for i in range(10):
            calculator.record_operation_metrics(
                operation_type="autofix",
                duration=0.5,
                success=(i < 9),  # 1 failure
            )

        # Calculate KPIs
        snapshot = calculator.calculate_kpis()

        # Verify comprehensive metrics
        assert snapshot.index_latency_p50 is not None
        assert snapshot.query_latency_p50 is not None
        assert snapshot.avg_tokens_per_task is not None
        assert snapshot.fix_success_rate == pytest.approx(90.0)
        assert snapshot.error_rate > 0  # Should have some errors
        assert snapshot.query_cache_hit_rate > 0

        # Export to both formats
        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.csv') as f:
            csv_path = f.name
        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.json') as f:
            json_path = f.name

        calculator.export_to_csv(csv_path, [snapshot])
        calculator.export_to_json(json_path, [snapshot])

        # Verify files exist
        import os
        assert os.path.exists(csv_path)
        assert os.path.exists(json_path)

        # Clean up
        os.unlink(csv_path)
        os.unlink(json_path)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
