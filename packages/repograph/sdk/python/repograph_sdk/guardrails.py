"""Guardrails API for RepoGraph SDK."""

from typing import List, Dict, Any, Optional, TYPE_CHECKING
from .models import GuardrailConfig, GuardrailViolation

if TYPE_CHECKING:
    from .client import RepoGraphClient


class GuardrailsAPI:
    """
    Guardrails API interface.

    Provides methods for managing safety and efficiency guardrails.
    """

    def __init__(self, client: "RepoGraphClient"):
        self.client = client

    def list_guardrails(self) -> List[dict]:
        """
        List all available guardrails.

        Returns:
            List of guardrails

        Example:
            >>> guardrails = client.guardrails.list_guardrails()
            >>> for g in guardrails:
            ...     print(f"{g['name']}: {g['description']}")
        """
        response = self.client.get("/api/v1/guardrails/list")
        return response.get("guardrails", [])

    def get_config(self) -> List[GuardrailConfig]:
        """
        Get current guardrail configuration.

        Returns:
            List of guardrail configurations

        Example:
            >>> configs = client.guardrails.get_config()
            >>> for config in configs:
            ...     print(f"{config.name}: {'enabled' if config.enabled else 'disabled'}")
        """
        response = self.client.get("/api/v1/guardrails/config")
        return [GuardrailConfig(**c) for c in response.get("config", [])]

    def update_config(self, configs: List[GuardrailConfig]) -> dict:
        """
        Update guardrail configuration.

        Args:
            configs: List of guardrail configurations to update

        Returns:
            Update confirmation

        Example:
            >>> config = GuardrailConfig(
            ...     name="token_budget",
            ...     enabled=True,
            ...     severity="block",
            ...     config={"max_tokens_per_operation": 30000}
            ... )
            >>> result = client.guardrails.update_config([config])
        """
        data = {"configs": [c.dict() for c in configs]}
        return self.client.post("/api/v1/guardrails/config", json=data)

    def validate_schema_first(
        self,
        operation: str,
        schema: Dict[str, Any],
        validate_only: bool = False,
    ) -> dict:
        """
        Validate schema-first compliance.

        Args:
            operation: Operation type
            schema: Schema definition
            validate_only: Only validate, don't enforce

        Returns:
            Validation result

        Example:
            >>> result = client.guardrails.validate_schema_first(
            ...     operation="create_route",
            ...     schema={"type": "object", "properties": {...}}
            ... )
            >>> print(f"Valid: {result['valid']}")
        """
        data = {
            "operation": operation,
            "schema": schema,
            "validate_only": validate_only,
        }
        return self.client.post("/api/v1/guardrails/schema-first/validate", json=data)

    def check_drift(
        self,
        repo_id: str,
        check_types: Optional[List[str]] = None,
    ) -> dict:
        """
        Check for schema/route drift.

        Args:
            repo_id: Repository ID
            check_types: Specific check types

        Returns:
            Drift check result

        Example:
            >>> result = client.guardrails.check_drift(repo_id="abc123")
            >>> if result['drift_detected']:
            ...     print("Drift detected! Use targeted fetch.")
        """
        data = {"repo_id": repo_id}
        if check_types:
            data["check_types"] = check_types

        return self.client.post("/api/v1/guardrails/drift-sentinel/check", json=data)

    def route_model(
        self,
        operation: str,
        budget: Optional[float] = None,
        latency_sla_ms: Optional[int] = None,
        prefer_model: Optional[str] = None,
    ) -> dict:
        """
        Get model routing recommendation.

        Args:
            operation: Operation type
            budget: Cost budget in USD
            latency_sla_ms: Latency SLA in milliseconds
            prefer_model: Preferred model

        Returns:
            Routing recommendation

        Example:
            >>> result = client.guardrails.route_model(
            ...     operation="code_generation",
            ...     budget=0.10,
            ...     latency_sla_ms=1000
            ... )
            >>> print(f"Use model: {result['recommended_model']}")
        """
        data = {"operation": operation}
        if budget is not None:
            data["budget"] = budget
        if latency_sla_ms is not None:
            data["latency_sla_ms"] = latency_sla_ms
        if prefer_model:
            data["prefer_model"] = prefer_model

        return self.client.post("/api/v1/guardrails/model-routing/route", json=data)

    def get_violations(
        self,
        hours: int = 24,
        severity: Optional[str] = None,
    ) -> List[GuardrailViolation]:
        """
        Get recent guardrail violations.

        Args:
            hours: Time period in hours
            severity: Filter by severity

        Returns:
            List of violations

        Example:
            >>> violations = client.guardrails.get_violations(hours=24)
            >>> for v in violations:
            ...     print(f"{v.guardrail}: {v.message}")
        """
        params = {"hours": hours}
        if severity:
            params["severity"] = severity

        response = self.client.get("/api/v1/guardrails/violations", params=params)
        return [GuardrailViolation(**v) for v in response.get("violations", [])]

    def get_stats(self, period: str = "7d") -> dict:
        """
        Get guardrail statistics.

        Args:
            period: Time period

        Returns:
            Statistics

        Example:
            >>> stats = client.guardrails.get_stats(period="7d")
            >>> print(f"Total checks: {stats['total_checks']}")
            >>> print(f"Violations prevented: {stats['effectiveness']['violations_prevented']}")
        """
        response = self.client.get(
            "/api/v1/guardrails/stats",
            params={"period": period},
        )
        return response
