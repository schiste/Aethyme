"""API endpoints for guardrails and efficiency features.

This module provides REST API endpoints for:
- Guardrail configuration and checks
- Feature flag management
- Cost and token tracking
- Budget management
"""

from typing import Dict, List, Optional, Any
from fastapi import APIRouter, Depends, HTTPException, status, Query
from pydantic import BaseModel, Field
from datetime import datetime, timedelta

import structlog

from ...auth.middleware import UserContext, require_scope, get_current_user
from ...guardrails.schema_first import SchemaExtractor, SchemaGate, SchemaValidator
from ...guardrails.sentinels import DriftSentinel, PreflightCheck
from ...guardrails.flags import get_flags_instance, FeatureFlag, FlagStatus
from ...efficiency.model_router import ModelRouter, RoutingStrategy, ModelTier
from ...efficiency.tracking import (
    BudgetEnforcer,
    TokenTracker,
    CostTracker,
    Budget,
    TimeWindow,
    calculate_usage_stats,
)
from ...efficiency.metrics import GuardrailsMetrics, EfficiencyMetrics
from ...efficiency.context import ContextManager

logger = structlog.get_logger(__name__)

router = APIRouter(prefix="/api/v1/guardrails")


# Request/Response models
class CheckOperationRequest(BaseModel):
    """Request to check an operation."""

    operation_type: str = Field(..., description="Type of operation (endpoint, entity, etc.)")
    targets: List[str] = Field(..., description="Target names to validate")
    override: bool = Field(default=False, description="Override strict mode")


class CheckOperationResponse(BaseModel):
    """Response from operation check."""

    is_valid: bool
    errors: List[str] = []
    warnings: List[str] = []
    suggestions: List[str] = []


class PreflightCheckRequest(BaseModel):
    """Request for preflight check."""

    files: List[Dict[str, str]] = Field(..., description="Files to check")
    context: Dict[str, Any] = Field(default_factory=dict, description="Additional context")


class PreflightCheckResponse(BaseModel):
    """Response from preflight check."""

    passed: bool
    blocked: bool
    max_risk: str
    total_detections: int
    detections: List[Dict[str, Any]]
    rules: Dict[str, Any]


class GuardrailConfigResponse(BaseModel):
    """Guardrail configuration."""

    schema_first_enabled: bool
    drift_sentinels_enabled: bool
    preflight_checks_enabled: bool
    strict_mode: bool


class UpdateGuardrailConfigRequest(BaseModel):
    """Request to update guardrail configuration."""

    schema_first_enabled: Optional[bool] = None
    drift_sentinels_enabled: Optional[bool] = None
    preflight_checks_enabled: Optional[bool] = None
    strict_mode: Optional[bool] = None


class BudgetRequest(BaseModel):
    """Request to create/update budget."""

    limit_usd: float = Field(..., gt=0, description="Budget limit in USD")
    window: TimeWindow = Field(..., description="Time window")
    alert_threshold: float = Field(default=0.8, ge=0, le=1, description="Alert threshold")
    hard_limit: bool = Field(default=True, description="Hard limit enforcement")


class BudgetResponse(BaseModel):
    """Budget information."""

    budget_id: str
    tenant_id: Optional[str]
    limit_usd: float
    window: str
    alert_threshold: float
    hard_limit: bool


class UsageStatsResponse(BaseModel):
    """Usage statistics."""

    window: str
    start_time: str
    end_time: str
    total_tokens: int
    total_cost: float
    operations_count: int
    avg_tokens_per_op: float
    avg_cost_per_op: float
    by_model: Dict[str, Any]
    by_operation_type: Dict[str, Any]


class FeatureFlagResponse(BaseModel):
    """Feature flag information."""

    flag_id: str
    name: str
    description: str
    status: str
    rollout_percentage: int
    enabled_for_tenant: bool


# Endpoints

@router.post("/check", response_model=CheckOperationResponse)
async def check_operation(
    request: CheckOperationRequest,
    user: UserContext = Depends(require_scope("repo:read")),
):
    """Check if an operation is allowed by guardrails.

    This validates operations against schemas and returns any violations.
    """
    try:
        # Initialize schema gate (would be cached in production)
        # For now, use a mock implementation
        gate = SchemaGate("/tmp", strict_mode=not request.override)

        result = gate.check_operation(
            request.operation_type,
            request.targets,
            override=request.override,
        )

        return CheckOperationResponse(
            is_valid=result.is_valid,
            errors=result.errors,
            warnings=result.warnings,
            suggestions=result.suggestions,
        )

    except Exception as e:
        logger.error("Operation check failed", error=str(e), user=user.user_id)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Check failed: {e}",
        )


@router.post("/preflight", response_model=PreflightCheckResponse)
async def preflight_check(
    request: PreflightCheckRequest,
    user: UserContext = Depends(require_scope("repo:write")),
):
    """Run preflight checks before making changes.

    This detects drift, breaking changes, and other risky modifications.
    """
    try:
        sentinel = DriftSentinel("/tmp")

        # Run drift checks
        results = sentinel.check_drift(request.files)

        # Record metrics
        GuardrailsMetrics.record_preflight_check(
            "passed" if results['passed'] else "failed",
            user.org_id,
        )

        # Record violations
        for detection in results.get('detections', []):
            GuardrailsMetrics.record_violation(
                detection['rule_id'],
                detection['risk_level'],
                user.org_id,
            )

        return PreflightCheckResponse(**results)

    except Exception as e:
        logger.error("Preflight check failed", error=str(e), user=user.user_id)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Preflight check failed: {e}",
        )


@router.get("/config", response_model=GuardrailConfigResponse)
async def get_guardrail_config(
    user: UserContext = Depends(require_scope("repo:read")),
):
    """Get guardrail configuration for current tenant."""
    flags = get_flags_instance()

    return GuardrailConfigResponse(
        schema_first_enabled=flags.is_enabled('SCHEMA_FIRST', user.org_id),
        drift_sentinels_enabled=flags.is_enabled('DRIFT_SENTINELS', user.org_id),
        preflight_checks_enabled=flags.is_enabled('PREFLIGHT_CHECKS', user.org_id),
        strict_mode=True,  # Could be configurable
    )


@router.put("/config")
async def update_guardrail_config(
    request: UpdateGuardrailConfigRequest,
    user: UserContext = Depends(require_scope("org:admin")),
):
    """Update guardrail configuration.

    Requires org:admin scope.
    """
    flags = get_flags_instance()

    updated = []

    if request.schema_first_enabled is not None:
        if request.schema_first_enabled:
            flags.whitelist_tenant('SCHEMA_FIRST', user.org_id)
        else:
            flags.blacklist_tenant('SCHEMA_FIRST', user.org_id)
        updated.append('schema_first')

    if request.drift_sentinels_enabled is not None:
        if request.drift_sentinels_enabled:
            flags.whitelist_tenant('DRIFT_SENTINELS', user.org_id)
        else:
            flags.blacklist_tenant('DRIFT_SENTINELS', user.org_id)
        updated.append('drift_sentinels')

    if request.preflight_checks_enabled is not None:
        if request.preflight_checks_enabled:
            flags.whitelist_tenant('PREFLIGHT_CHECKS', user.org_id)
        else:
            flags.blacklist_tenant('PREFLIGHT_CHECKS', user.org_id)
        updated.append('preflight_checks')

    logger.info(
        "Guardrail config updated",
        tenant=user.org_id,
        updated=updated,
        user=user.user_id,
    )

    return {"updated": updated}


@router.get("/flags", response_model=List[FeatureFlagResponse])
async def list_feature_flags(
    user: UserContext = Depends(require_scope("repo:read")),
):
    """List all feature flags and their status for current tenant."""
    flags = get_flags_instance()

    result = []
    for flag_id, flag in flags.get_all_flags().items():
        result.append(FeatureFlagResponse(
            flag_id=flag.flag_id,
            name=flag.name,
            description=flag.description,
            status=flag.status.value,
            rollout_percentage=flag.rollout_percentage,
            enabled_for_tenant=flag.is_enabled_for_tenant(user.org_id),
        ))

    return result


@router.get("/efficiency/stats", response_model=UsageStatsResponse)
async def get_efficiency_stats(
    window: TimeWindow = Query(TimeWindow.DAILY, description="Time window"),
    user: UserContext = Depends(require_scope("repo:read")),
):
    """Get efficiency statistics for current tenant.

    Returns token usage and cost statistics.
    """
    # Calculate time range
    now = datetime.utcnow()
    if window == TimeWindow.HOURLY:
        start_time = now - timedelta(hours=1)
    elif window == TimeWindow.DAILY:
        start_time = now - timedelta(days=1)
    elif window == TimeWindow.WEEKLY:
        start_time = now - timedelta(weeks=1)
    else:  # MONTHLY
        start_time = now - timedelta(days=30)

    # In production, fetch from database
    # For now, return mock data
    return UsageStatsResponse(
        window=window.value,
        start_time=start_time.isoformat(),
        end_time=now.isoformat(),
        total_tokens=150000,
        total_cost=2.45,
        operations_count=42,
        avg_tokens_per_op=3571.4,
        avg_cost_per_op=0.058,
        by_model={
            "gpt-3.5-turbo": {"tokens": 50000, "operations": 20, "cost": 0.50},
            "claude-3-sonnet": {"tokens": 100000, "operations": 22, "cost": 1.95},
        },
        by_operation_type={
            "code_generation": {"tokens": 80000, "operations": 15, "cost": 1.20},
            "documentation": {"tokens": 40000, "operations": 12, "cost": 0.60},
            "analysis": {"tokens": 30000, "operations": 15, "cost": 0.65},
        },
    )


@router.get("/efficiency/budget")
async def get_budget_status(
    user: UserContext = Depends(require_scope("repo:read")),
):
    """Get budget status for current tenant."""
    # In production, fetch from database
    return {
        "budgets": [
            {
                "budget_id": "daily_budget",
                "limit_usd": 10.00,
                "current_spend": 2.45,
                "remaining": 7.55,
                "utilization": 0.245,
                "window": "daily",
            }
        ],
        "alerts": [],
    }


@router.post("/efficiency/budget", response_model=BudgetResponse)
async def create_budget(
    request: BudgetRequest,
    user: UserContext = Depends(require_scope("org:admin")),
):
    """Create or update a budget.

    Requires org:admin scope.
    """
    budget_id = f"{user.org_id}_{request.window.value}"

    budget = Budget(
        budget_id=budget_id,
        tenant_id=user.org_id,
        limit_usd=request.limit_usd,
        window=request.window,
        alert_threshold=request.alert_threshold,
        hard_limit=request.hard_limit,
    )

    # In production, save to database
    logger.info(
        "Budget created",
        budget_id=budget_id,
        limit=request.limit_usd,
        window=request.window.value,
        tenant=user.org_id,
    )

    return BudgetResponse(
        budget_id=budget.budget_id,
        tenant_id=budget.tenant_id,
        limit_usd=budget.limit_usd,
        window=budget.window.value,
        alert_threshold=budget.alert_threshold,
        hard_limit=budget.hard_limit,
    )


@router.get("/health")
async def guardrails_health():
    """Health check for guardrails system."""
    flags = get_flags_instance()

    return {
        "status": "healthy",
        "feature_flags": len(flags.get_all_flags()),
        "timestamp": datetime.utcnow().isoformat(),
    }
