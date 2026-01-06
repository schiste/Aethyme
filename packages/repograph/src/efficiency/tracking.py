"""Token and cost tracking for AI operations.

This module tracks token usage and costs across operations,
enforces budgets, and provides attribution by tenant/operation.
"""

from typing import Dict, List, Optional, Any
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from enum import Enum
import structlog

logger = structlog.get_logger(__name__)


class TimeWindow(str, Enum):
    """Time windows for tracking."""
    HOURLY = "hourly"
    DAILY = "daily"
    WEEKLY = "weekly"
    MONTHLY = "monthly"


@dataclass
class TokenUsage:
    """Token usage for an operation."""

    operation_id: str
    operation_type: str
    model_id: str
    input_tokens: int
    output_tokens: int
    total_tokens: int
    tenant_id: str
    user_id: str
    timestamp: datetime = field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'operation_id': self.operation_id,
            'operation_type': self.operation_type,
            'model_id': self.model_id,
            'input_tokens': self.input_tokens,
            'output_tokens': self.output_tokens,
            'total_tokens': self.total_tokens,
            'tenant_id': self.tenant_id,
            'user_id': self.user_id,
            'timestamp': self.timestamp.isoformat(),
            'metadata': self.metadata,
        }


@dataclass
class CostRecord:
    """Cost record for an operation."""

    operation_id: str
    operation_type: str
    model_id: str
    cost_usd: float
    input_tokens: int
    output_tokens: int
    tenant_id: str
    user_id: str
    timestamp: datetime = field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'operation_id': self.operation_id,
            'operation_type': self.operation_type,
            'model_id': self.model_id,
            'cost_usd': self.cost_usd,
            'input_tokens': self.input_tokens,
            'output_tokens': self.output_tokens,
            'tenant_id': self.tenant_id,
            'user_id': self.user_id,
            'timestamp': self.timestamp.isoformat(),
            'metadata': self.metadata,
        }


@dataclass
class Budget:
    """Budget configuration."""

    budget_id: str
    tenant_id: Optional[str] = None  # None = global
    limit_usd: float = 0.0
    window: TimeWindow = TimeWindow.DAILY
    alert_threshold: float = 0.8  # Alert at 80%
    hard_limit: bool = True  # Block operations when exceeded
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class UsageStats:
    """Usage statistics for a time window."""

    window: TimeWindow
    start_time: datetime
    end_time: datetime
    total_tokens: int = 0
    total_cost: float = 0.0
    operations_count: int = 0
    by_model: Dict[str, Dict[str, Any]] = field(default_factory=dict)
    by_operation_type: Dict[str, Dict[str, Any]] = field(default_factory=dict)
    by_tenant: Dict[str, Dict[str, Any]] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'window': self.window.value,
            'start_time': self.start_time.isoformat(),
            'end_time': self.end_time.isoformat(),
            'total_tokens': self.total_tokens,
            'total_cost': self.total_cost,
            'operations_count': self.operations_count,
            'avg_tokens_per_op': (
                self.total_tokens / self.operations_count
                if self.operations_count > 0 else 0
            ),
            'avg_cost_per_op': (
                self.total_cost / self.operations_count
                if self.operations_count > 0 else 0
            ),
            'by_model': self.by_model,
            'by_operation_type': self.by_operation_type,
            'by_tenant': self.by_tenant,
        }


class TokenTracker:
    """Track token usage across operations."""

    def __init__(self):
        """Initialize token tracker."""
        self.usage_records: List[TokenUsage] = []
        self._index_by_tenant: Dict[str, List[TokenUsage]] = {}
        self._index_by_operation: Dict[str, List[TokenUsage]] = {}

    def record_usage(
        self,
        operation_id: str,
        operation_type: str,
        model_id: str,
        input_tokens: int,
        output_tokens: int,
        tenant_id: str,
        user_id: str,
        **metadata,
    ) -> TokenUsage:
        """Record token usage.

        Args:
            operation_id: Unique operation ID
            operation_type: Type of operation
            model_id: Model identifier
            input_tokens: Input token count
            output_tokens: Output token count
            tenant_id: Tenant identifier
            user_id: User identifier
            **metadata: Additional metadata

        Returns:
            TokenUsage record
        """
        usage = TokenUsage(
            operation_id=operation_id,
            operation_type=operation_type,
            model_id=model_id,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            total_tokens=input_tokens + output_tokens,
            tenant_id=tenant_id,
            user_id=user_id,
            metadata=metadata,
        )

        self.usage_records.append(usage)

        # Update indexes
        if tenant_id not in self._index_by_tenant:
            self._index_by_tenant[tenant_id] = []
        self._index_by_tenant[tenant_id].append(usage)

        if operation_type not in self._index_by_operation:
            self._index_by_operation[operation_type] = []
        self._index_by_operation[operation_type].append(usage)

        logger.debug(
            "Token usage recorded",
            operation=operation_id,
            tokens=usage.total_tokens,
            tenant=tenant_id,
        )

        return usage

    def get_usage_by_tenant(
        self,
        tenant_id: str,
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None,
    ) -> List[TokenUsage]:
        """Get usage for a tenant.

        Args:
            tenant_id: Tenant identifier
            start_time: Start of time range
            end_time: End of time range

        Returns:
            List of usage records
        """
        records = self._index_by_tenant.get(tenant_id, [])

        if start_time or end_time:
            records = [
                r for r in records
                if (not start_time or r.timestamp >= start_time) and
                   (not end_time or r.timestamp <= end_time)
            ]

        return records

    def get_total_tokens(
        self,
        tenant_id: Optional[str] = None,
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None,
    ) -> int:
        """Get total token count.

        Args:
            tenant_id: Optional tenant filter
            start_time: Start of time range
            end_time: End of time range

        Returns:
            Total tokens
        """
        if tenant_id:
            records = self.get_usage_by_tenant(tenant_id, start_time, end_time)
        else:
            records = self.usage_records
            if start_time or end_time:
                records = [
                    r for r in records
                    if (not start_time or r.timestamp >= start_time) and
                       (not end_time or r.timestamp <= end_time)
                ]

        return sum(r.total_tokens for r in records)


class CostTracker:
    """Track costs across operations."""

    def __init__(self):
        """Initialize cost tracker."""
        self.cost_records: List[CostRecord] = []
        self._index_by_tenant: Dict[str, List[CostRecord]] = {}

    def record_cost(
        self,
        operation_id: str,
        operation_type: str,
        model_id: str,
        cost_usd: float,
        input_tokens: int,
        output_tokens: int,
        tenant_id: str,
        user_id: str,
        **metadata,
    ) -> CostRecord:
        """Record operation cost.

        Args:
            operation_id: Unique operation ID
            operation_type: Type of operation
            model_id: Model identifier
            cost_usd: Cost in USD
            input_tokens: Input token count
            output_tokens: Output token count
            tenant_id: Tenant identifier
            user_id: User identifier
            **metadata: Additional metadata

        Returns:
            CostRecord
        """
        record = CostRecord(
            operation_id=operation_id,
            operation_type=operation_type,
            model_id=model_id,
            cost_usd=cost_usd,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            tenant_id=tenant_id,
            user_id=user_id,
            metadata=metadata,
        )

        self.cost_records.append(record)

        # Update index
        if tenant_id not in self._index_by_tenant:
            self._index_by_tenant[tenant_id] = []
        self._index_by_tenant[tenant_id].append(record)

        logger.debug(
            "Cost recorded",
            operation=operation_id,
            cost=cost_usd,
            tenant=tenant_id,
        )

        return record

    def get_cost_by_tenant(
        self,
        tenant_id: str,
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None,
    ) -> List[CostRecord]:
        """Get costs for a tenant.

        Args:
            tenant_id: Tenant identifier
            start_time: Start of time range
            end_time: End of time range

        Returns:
            List of cost records
        """
        records = self._index_by_tenant.get(tenant_id, [])

        if start_time or end_time:
            records = [
                r for r in records
                if (not start_time or r.timestamp >= start_time) and
                   (not end_time or r.timestamp <= end_time)
            ]

        return records

    def get_total_cost(
        self,
        tenant_id: Optional[str] = None,
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None,
    ) -> float:
        """Get total cost.

        Args:
            tenant_id: Optional tenant filter
            start_time: Start of time range
            end_time: End of time range

        Returns:
            Total cost in USD
        """
        if tenant_id:
            records = self.get_cost_by_tenant(tenant_id, start_time, end_time)
        else:
            records = self.cost_records
            if start_time or end_time:
                records = [
                    r for r in records
                    if (not start_time or r.timestamp >= start_time) and
                       (not end_time or r.timestamp <= end_time)
                ]

        return sum(r.cost_usd for r in records)


class BudgetEnforcer:
    """Enforce budget limits."""

    def __init__(self):
        """Initialize budget enforcer."""
        self.budgets: Dict[str, Budget] = {}
        self.cost_tracker = CostTracker()
        self.alerts_sent: Set[str] = set()

    def set_budget(self, budget: Budget):
        """Set a budget.

        Args:
            budget: Budget configuration
        """
        self.budgets[budget.budget_id] = budget
        logger.info(
            "Budget set",
            budget_id=budget.budget_id,
            limit=budget.limit_usd,
            window=budget.window.value,
        )

    def check_budget(
        self,
        tenant_id: str,
        estimated_cost: float,
    ) -> Dict[str, Any]:
        """Check if operation is within budget.

        Args:
            tenant_id: Tenant identifier
            estimated_cost: Estimated cost of operation

        Returns:
            Dictionary with check results
        """
        # Find applicable budgets
        budgets_to_check = []

        for budget in self.budgets.values():
            if budget.tenant_id is None or budget.tenant_id == tenant_id:
                budgets_to_check.append(budget)

        if not budgets_to_check:
            return {
                'allowed': True,
                'reason': 'No budgets configured',
            }

        # Check each budget
        for budget in budgets_to_check:
            # Calculate time window
            now = datetime.utcnow()
            if budget.window == TimeWindow.HOURLY:
                start_time = now - timedelta(hours=1)
            elif budget.window == TimeWindow.DAILY:
                start_time = now - timedelta(days=1)
            elif budget.window == TimeWindow.WEEKLY:
                start_time = now - timedelta(weeks=1)
            else:  # MONTHLY
                start_time = now - timedelta(days=30)

            # Get current spend
            current_spend = self.cost_tracker.get_total_cost(
                tenant_id=budget.tenant_id,
                start_time=start_time,
                end_time=now,
            )

            projected_spend = current_spend + estimated_cost
            utilization = projected_spend / budget.limit_usd

            # Check alert threshold
            if utilization >= budget.alert_threshold:
                alert_key = f"{budget.budget_id}_{start_time.date()}"
                if alert_key not in self.alerts_sent:
                    logger.warning(
                        "Budget alert threshold reached",
                        budget=budget.budget_id,
                        utilization=f"{utilization * 100:.1f}%",
                        current=current_spend,
                        limit=budget.limit_usd,
                    )
                    self.alerts_sent.add(alert_key)

            # Check hard limit
            if budget.hard_limit and projected_spend > budget.limit_usd:
                logger.error(
                    "Budget limit exceeded",
                    budget=budget.budget_id,
                    current=current_spend,
                    estimated=estimated_cost,
                    limit=budget.limit_usd,
                )

                return {
                    'allowed': False,
                    'reason': f'Budget limit exceeded for {budget.window.value}',
                    'budget_id': budget.budget_id,
                    'current_spend': current_spend,
                    'limit': budget.limit_usd,
                    'utilization': utilization,
                }

        return {
            'allowed': True,
            'budgets_checked': len(budgets_to_check),
        }

    def record_cost(self, *args, **kwargs) -> CostRecord:
        """Record a cost (delegates to tracker).

        Returns:
            CostRecord
        """
        return self.cost_tracker.record_cost(*args, **kwargs)


def calculate_usage_stats(
    records: List[TokenUsage],
    cost_records: List[CostRecord],
    window: TimeWindow,
    start_time: datetime,
    end_time: datetime,
) -> UsageStats:
    """Calculate usage statistics.

    Args:
        records: Token usage records
        cost_records: Cost records
        window: Time window
        start_time: Start time
        end_time: End time

    Returns:
        UsageStats
    """
    stats = UsageStats(
        window=window,
        start_time=start_time,
        end_time=end_time,
    )

    # Process token records
    for record in records:
        stats.total_tokens += record.total_tokens
        stats.operations_count += 1

        # By model
        if record.model_id not in stats.by_model:
            stats.by_model[record.model_id] = {
                'tokens': 0,
                'operations': 0,
                'cost': 0.0,
            }
        stats.by_model[record.model_id]['tokens'] += record.total_tokens
        stats.by_model[record.model_id]['operations'] += 1

        # By operation type
        if record.operation_type not in stats.by_operation_type:
            stats.by_operation_type[record.operation_type] = {
                'tokens': 0,
                'operations': 0,
                'cost': 0.0,
            }
        stats.by_operation_type[record.operation_type]['tokens'] += record.total_tokens
        stats.by_operation_type[record.operation_type]['operations'] += 1

        # By tenant
        if record.tenant_id not in stats.by_tenant:
            stats.by_tenant[record.tenant_id] = {
                'tokens': 0,
                'operations': 0,
                'cost': 0.0,
            }
        stats.by_tenant[record.tenant_id]['tokens'] += record.total_tokens
        stats.by_tenant[record.tenant_id]['operations'] += 1

    # Process cost records
    for record in cost_records:
        stats.total_cost += record.cost_usd

        # Update model stats
        if record.model_id in stats.by_model:
            stats.by_model[record.model_id]['cost'] += record.cost_usd

        # Update operation stats
        if record.operation_type in stats.by_operation_type:
            stats.by_operation_type[record.operation_type]['cost'] += record.cost_usd

        # Update tenant stats
        if record.tenant_id in stats.by_tenant:
            stats.by_tenant[record.tenant_id]['cost'] += record.cost_usd

    return stats
