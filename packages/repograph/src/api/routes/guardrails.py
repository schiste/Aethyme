"""Guardrails API endpoints for safety and efficiency controls."""

from fastapi import APIRouter, Depends
from pydantic import BaseModel, Field
from typing import List, Dict, Any, Optional
from datetime import datetime
import structlog

from ..auth import get_current_user

logger = structlog.get_logger(__name__)

router = APIRouter()


# ============================================================================
# Models
# ============================================================================

class GuardrailConfig(BaseModel):
    """Model for guardrail configuration."""
    name: str = Field(..., description="Guardrail name")
    enabled: bool = Field(..., description="Whether guardrail is enabled")
    severity: str = Field(..., description="Severity level (block, warn, log)")
    config: Dict[str, Any] = Field(default_factory=dict, description="Guardrail-specific configuration")


class GuardrailViolation(BaseModel):
    """Model for guardrail violation."""
    guardrail: str = Field(..., description="Guardrail name")
    timestamp: datetime
    severity: str = Field(..., description="Severity level")
    message: str = Field(..., description="Violation message")
    context: Dict[str, Any] = Field(default_factory=dict, description="Additional context")
    prevented: bool = Field(..., description="Whether operation was prevented")


class SchemaFirstRequest(BaseModel):
    """Request model for schema-first validation."""
    operation: str = Field(..., description="Operation type (create_route, update_model, etc.)")
    schema: Dict[str, Any] = Field(..., description="Schema definition")
    validate_only: bool = Field(False, description="Only validate, don't enforce")


class DriftSentinelRequest(BaseModel):
    """Request model for drift sentinel check."""
    repo_id: str = Field(..., description="Repository ID")
    check_types: Optional[List[str]] = Field(None, description="Specific check types")


class ModelRoutingRequest(BaseModel):
    """Request model for model routing configuration."""
    operation: str = Field(..., description="Operation type")
    budget: Optional[float] = Field(None, description="Cost budget in USD")
    latency_sla_ms: Optional[int] = Field(None, description="Latency SLA in milliseconds")
    prefer_model: Optional[str] = Field(None, description="Preferred model")


# ============================================================================
# Endpoints
# ============================================================================

@router.get("/list")
async def list_guardrails(current_user: Dict = Depends(get_current_user)):
    """List all available guardrails.

    Returns metadata about each guardrail including:
    - Schema-first enforcement
    - Drift sentinels
    - Token/cost budgets
    - Context compaction
    - Model routing policies
    """
    guardrails = [
        {
            "name": "schema_first",
            "description": "Require schema definition before implementation",
            "category": "safety",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "drift_sentinel",
            "description": "Detect schema/route drift and require targeted fetch",
            "category": "safety",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "token_budget",
            "description": "Enforce token usage budgets per operation",
            "category": "efficiency",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "cost_budget",
            "description": "Enforce cost budgets per operation",
            "category": "efficiency",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "context_compaction",
            "description": "Auto-compact context to reduce token usage",
            "category": "efficiency",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "model_routing",
            "description": "Route to appropriate model based on complexity",
            "category": "efficiency",
            "default_enabled": True,
            "configurable": True,
        },
        {
            "name": "generated_file_protection",
            "description": "Prevent edits to generated files",
            "category": "safety",
            "default_enabled": True,
            "configurable": False,
        },
    ]

    return {
        "guardrails": guardrails,
        "count": len(guardrails),
    }


@router.get("/config")
async def get_guardrail_config(current_user: Dict = Depends(get_current_user)):
    """Get current guardrail configuration.

    Returns the active configuration for all guardrails.
    """
    config = [
        GuardrailConfig(
            name="schema_first",
            enabled=True,
            severity="block",
            config={
                "require_openapi": True,
                "require_types": True,
                "allow_override": False,
            },
        ),
        GuardrailConfig(
            name="drift_sentinel",
            enabled=True,
            severity="warn",
            config={
                "check_schemas": True,
                "check_routes": True,
                "check_abilities": True,
            },
        ),
        GuardrailConfig(
            name="token_budget",
            enabled=True,
            severity="warn",
            config={
                "max_tokens_per_operation": 50000,
                "warn_threshold": 0.8,
            },
        ),
        GuardrailConfig(
            name="cost_budget",
            enabled=True,
            severity="warn",
            config={
                "max_cost_per_operation_usd": 1.0,
                "warn_threshold": 0.8,
            },
        ),
        GuardrailConfig(
            name="context_compaction",
            enabled=True,
            severity="log",
            config={
                "target_compression_ratio": 0.5,
                "preserve_critical_context": True,
            },
        ),
        GuardrailConfig(
            name="model_routing",
            enabled=True,
            severity="log",
            config={
                "fast_model": "claude-haiku",
                "balanced_model": "claude-sonnet",
                "powerful_model": "claude-opus",
                "auto_escalate": True,
            },
        ),
    ]

    return {
        "config": config,
        "count": len(config),
    }


@router.post("/config")
async def update_guardrail_config(
    configs: List[GuardrailConfig],
    current_user: Dict = Depends(get_current_user),
):
    """Update guardrail configuration.

    **Example:**
    ```json
    {
      "configs": [
        {
          "name": "token_budget",
          "enabled": true,
          "severity": "block",
          "config": {
            "max_tokens_per_operation": 30000
          }
        }
      ]
    }
    ```
    """
    logger.info(
        "Updating guardrail config",
        config_count=len(configs),
        user=current_user.get("sub"),
    )

    # Store configuration (in production, persist to database)
    return {
        "status": "updated",
        "updated_count": len(configs),
        "timestamp": datetime.now().isoformat(),
    }


@router.post("/schema-first/validate")
async def validate_schema_first(
    request: SchemaFirstRequest,
    current_user: Dict = Depends(get_current_user),
):
    """Validate schema-first compliance.

    Checks if operation has proper schema definition before allowing implementation.
    """
    logger.info(
        "Validating schema-first",
        operation=request.operation,
        user=current_user.get("sub"),
    )

    # Mock validation
    validation_result = {
        "valid": True,
        "operation": request.operation,
        "checks": {
            "has_schema": True,
            "schema_complete": True,
            "types_defined": True,
            "examples_provided": False,
        },
        "warnings": [
            "Schema missing request/response examples",
        ],
    }

    return validation_result


@router.post("/drift-sentinel/check")
async def check_drift_sentinel(
    request: DriftSentinelRequest,
    current_user: Dict = Depends(get_current_user),
):
    """Check for schema/route drift.

    Detects if there are changes to schemas, routes, or abilities that require
    targeted fetch instead of full context.
    """
    logger.info(
        "Checking drift sentinel",
        repo_id=request.repo_id,
        user=current_user.get("sub"),
    )

    # Mock drift check
    drift_result = {
        "repo_id": request.repo_id,
        "timestamp": datetime.now().isoformat(),
        "drift_detected": False,
        "checks": {
            "schema_drift": {"detected": False, "changes": 0},
            "route_drift": {"detected": False, "changes": 0},
            "ability_drift": {"detected": False, "changes": 0},
        },
        "recommendation": "full_context_safe",
    }

    return drift_result


@router.post("/model-routing/route")
async def route_model(
    request: ModelRoutingRequest,
    current_user: Dict = Depends(get_current_user),
):
    """Get model routing recommendation.

    Returns recommended model based on operation complexity, budget, and SLA.
    """
    logger.info(
        "Routing model",
        operation=request.operation,
        user=current_user.get("sub"),
    )

    # Mock routing logic
    routing_result = {
        "operation": request.operation,
        "recommended_model": request.prefer_model or "claude-sonnet",
        "reasoning": "Balanced complexity and cost",
        "estimated_cost_usd": 0.05,
        "estimated_latency_ms": 800,
        "fallback_model": "claude-haiku",
        "escalation_model": "claude-opus",
        "within_budget": True,
        "within_sla": True if not request.latency_sla_ms else 800 < request.latency_sla_ms,
    }

    return routing_result


@router.get("/violations")
async def get_violations(
    hours: int = 24,
    severity: Optional[str] = None,
    current_user: Dict = Depends(get_current_user),
):
    """Get recent guardrail violations.

    Returns violations that occurred in the specified time period.
    """
    logger.info(
        "Fetching violations",
        hours=hours,
        severity=severity,
        user=current_user.get("sub"),
    )

    # Mock violations
    violations = [
        GuardrailViolation(
            guardrail="token_budget",
            timestamp=datetime.now(),
            severity="warning",
            message="Operation exceeded 80% of token budget",
            context={
                "tokens_used": 42000,
                "budget": 50000,
                "operation": "code_generation",
            },
            prevented=False,
        ),
        GuardrailViolation(
            guardrail="schema_first",
            timestamp=datetime.now(),
            severity="block",
            message="Attempted to create route without schema definition",
            context={
                "route": "/api/new-endpoint",
                "operation": "create_route",
            },
            prevented=True,
        ),
    ]

    if severity:
        violations = [v for v in violations if v.severity == severity]

    return {
        "violations": violations,
        "count": len(violations),
        "period_hours": hours,
    }


@router.get("/stats")
async def get_guardrail_stats(
    period: str = "7d",
    current_user: Dict = Depends(get_current_user),
):
    """Get guardrail statistics.

    Returns statistics about guardrail effectiveness over time.
    """
    logger.info("Fetching guardrail stats", period=period, user=current_user.get("sub"))

    stats = {
        "period": period,
        "timestamp": datetime.now().isoformat(),
        "total_checks": 5432,
        "violations": {
            "total": 234,
            "blocked": 45,
            "warned": 156,
            "logged": 33,
        },
        "by_guardrail": {
            "schema_first": {"checks": 1234, "violations": 12, "prevented": 8},
            "drift_sentinel": {"checks": 890, "violations": 23, "prevented": 15},
            "token_budget": {"checks": 2341, "violations": 89, "prevented": 0},
            "cost_budget": {"checks": 2341, "violations": 45, "prevented": 5},
            "context_compaction": {"checks": 2341, "violations": 0, "prevented": 0},
            "model_routing": {"checks": 2341, "violations": 65, "prevented": 17},
        },
        "effectiveness": {
            "violations_prevented": 45,
            "cost_saved_usd": 123.45,
            "tokens_saved": 234567,
        },
    }

    return stats
