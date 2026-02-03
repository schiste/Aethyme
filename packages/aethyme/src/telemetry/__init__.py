"""
Telemetry and Observability Infrastructure for Aethyme.

Provides comprehensive telemetry including:
- Enhanced Prometheus metrics (tokens, cost, violations, fixes)
- OpenTelemetry distributed tracing
- Request correlation and context propagation
- KPI calculation and reporting
"""

from .enhanced_metrics import (
    EnhancedMetricsCollector,
    enhanced_metrics_collector,
    get_enhanced_metrics_text,
)
from .tracing import (
    TracingManager,
    tracing_manager,
    trace_operation,
    get_current_trace_id,
)
from .correlation import (
    CorrelationManager,
    correlation_manager,
    get_request_id,
    set_request_id,
)
from .kpi import (
    KPICalculator,
    kpi_calculator,
    export_kpi_report,
)

__all__ = [
    # Enhanced Metrics
    "EnhancedMetricsCollector",
    "enhanced_metrics_collector",
    "get_enhanced_metrics_text",
    # Tracing
    "TracingManager",
    "tracing_manager",
    "trace_operation",
    "get_current_trace_id",
    # Correlation
    "CorrelationManager",
    "correlation_manager",
    "get_request_id",
    "set_request_id",
    # KPI
    "KPICalculator",
    "kpi_calculator",
    "export_kpi_report",
]
