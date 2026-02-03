"""Telemetry API for Aethyme SDK."""

from typing import List, Dict, Any, Optional, TYPE_CHECKING
from datetime import datetime
from .models import TelemetryMetric, MetricDataPoint

if TYPE_CHECKING:
    from .client import AethymeClient


class TelemetryAPI:
    """
    Telemetry API interface.

    Provides methods for querying metrics and telemetry data.
    """

    def __init__(self, client: "AethymeClient"):
        self.client = client

    def list_metrics(self) -> List[TelemetryMetric]:
        """
        List all available metrics.

        Returns:
            List of available metrics

        Example:
            >>> metrics = client.telemetry.list_metrics()
            >>> for metric in metrics:
            ...     print(f"{metric.name}: {metric.description}")
        """
        response = self.client.get("/api/v1/telemetry/metrics")
        return [TelemetryMetric(**m) for m in response.get("metrics", [])]

    def query(
        self,
        metrics: List[str],
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None,
        aggregation: str = "avg",
        interval: str = "1h",
    ) -> Dict[str, List[MetricDataPoint]]:
        """
        Query metric data.

        Args:
            metrics: Metric names to query
            start_time: Start time for query
            end_time: End time for query
            aggregation: Aggregation function (avg, sum, min, max)
            interval: Aggregation interval

        Returns:
            Dictionary of metric data points

        Example:
            >>> data = client.telemetry.query(
            ...     metrics=["query_latency_ms", "cache_hit_rate"],
            ...     aggregation="avg",
            ...     interval="1h"
            ... )
            >>> for metric_name, points in data.items():
            ...     print(f"{metric_name}: {len(points)} data points")
        """
        data = {
            "metrics": metrics,
            "aggregation": aggregation,
            "interval": interval,
        }
        if start_time:
            data["start_time"] = start_time.isoformat()
        if end_time:
            data["end_time"] = end_time.isoformat()

        response = self.client.post("/api/v1/telemetry/query", json=data)

        result = {}
        for metric_name, points in response.get("metrics", {}).items():
            result[metric_name] = [MetricDataPoint(**p) for p in points]

        return result

    def get_summary(self, metric_name: str, period: str = "24h") -> dict:
        """
        Get summary statistics for a metric.

        Args:
            metric_name: Metric name
            period: Time period (1h, 24h, 7d, 30d)

        Returns:
            Summary statistics

        Example:
            >>> summary = client.telemetry.get_summary("query_latency_ms")
            >>> print(f"Average: {summary['avg']}ms")
            >>> print(f"P95: {summary['p95']}ms")
        """
        response = self.client.get(
            f"/api/v1/telemetry/summary/{metric_name}",
            params={"period": period},
        )
        return response

    def get_kpis(self, period: str = "7d") -> dict:
        """
        Get key performance indicators.

        Args:
            period: Time period (7d, 30d, 90d)

        Returns:
            KPI data

        Example:
            >>> kpis = client.telemetry.get_kpis(period="7d")
            >>> print(f"Total queries: {kpis['query_performance']['total_queries']}")
            >>> print(f"Avg score: {kpis['ai_readiness']['avg_score']}")
        """
        response = self.client.get(
            "/api/v1/telemetry/kpi",
            params={"period": period},
        )
        return response

    def log_event(self, event_type: str, event_data: Dict[str, Any]) -> dict:
        """
        Log a custom telemetry event.

        Args:
            event_type: Event type
            event_data: Event data

        Returns:
            Log confirmation

        Example:
            >>> result = client.telemetry.log_event(
            ...     event_type="custom_action",
            ...     event_data={"action": "deploy", "success": True}
            ... )
        """
        response = self.client.post(
            "/api/v1/telemetry/event",
            params={"event_type": event_type},
            json=event_data,
        )
        return response
