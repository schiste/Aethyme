"""Feature flags for guardrails and efficiency features.

This module provides dynamic feature flag management for controlling
guardrails, efficiency features, and gradual rollouts.
"""

from typing import Dict, Optional, Any, List
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
import structlog

logger = structlog.get_logger(__name__)


class FlagStatus(str, Enum):
    """Feature flag status."""
    ENABLED = "enabled"
    DISABLED = "disabled"
    ROLLOUT = "rollout"  # Gradual rollout
    AB_TEST = "ab_test"  # A/B testing


@dataclass
class FeatureFlag:
    """Feature flag configuration."""

    flag_id: str
    name: str
    description: str
    status: FlagStatus = FlagStatus.DISABLED
    rollout_percentage: int = 0  # 0-100
    tenant_whitelist: List[str] = field(default_factory=list)
    tenant_blacklist: List[str] = field(default_factory=list)
    created_at: datetime = field(default_factory=datetime.utcnow)
    updated_at: datetime = field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def is_enabled_for_tenant(self, tenant_id: str) -> bool:
        """Check if flag is enabled for tenant.

        Args:
            tenant_id: Tenant identifier

        Returns:
            True if enabled for this tenant
        """
        # Check blacklist first
        if tenant_id in self.tenant_blacklist:
            return False

        # Check whitelist
        if self.tenant_whitelist and tenant_id in self.tenant_whitelist:
            return True

        # Check status
        if self.status == FlagStatus.ENABLED:
            return True

        if self.status == FlagStatus.DISABLED:
            return False

        if self.status == FlagStatus.ROLLOUT:
            # Use hash of tenant_id for consistent rollout
            hash_val = hash(f"{self.flag_id}:{tenant_id}") % 100
            return hash_val < self.rollout_percentage

        # AB_TEST - default to 50/50
        if self.status == FlagStatus.AB_TEST:
            hash_val = hash(f"{self.flag_id}:{tenant_id}") % 100
            return hash_val < 50

        return False

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'flag_id': self.flag_id,
            'name': self.name,
            'description': self.description,
            'status': self.status.value,
            'rollout_percentage': self.rollout_percentage,
            'tenant_whitelist': self.tenant_whitelist,
            'tenant_blacklist': self.tenant_blacklist,
            'created_at': self.created_at.isoformat(),
            'updated_at': self.updated_at.isoformat(),
            'metadata': self.metadata,
        }


class FeatureFlags:
    """Feature flags registry."""

    def __init__(self):
        """Initialize feature flags."""
        self.flags: Dict[str, FeatureFlag] = {}
        self._register_default_flags()

    def _register_default_flags(self):
        """Register default feature flags."""

        # Guardrails flags
        self.register(FeatureFlag(
            flag_id='GUARDRAILS_V1',
            name='Guardrails System',
            description='Enable guardrails for safe operations',
            status=FlagStatus.ENABLED,  # Default on
        ))

        self.register(FeatureFlag(
            flag_id='SCHEMA_FIRST',
            name='Schema-First Planning',
            description='Require schema before code generation',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='DRIFT_SENTINELS',
            name='Drift Sentinels',
            description='Detect schema and code drift',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='PREFLIGHT_CHECKS',
            name='Preflight Checks',
            description='Run preflight checks before operations',
            status=FlagStatus.ENABLED,
        ))

        # Efficiency flags
        self.register(FeatureFlag(
            flag_id='CONTEXT_COMPACTION',
            name='Context Compaction',
            description='Automatically compact context to save tokens',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='MODEL_ROUTING',
            name='Model Routing',
            description='Intelligent model routing',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='BUDGET_ENFORCEMENT',
            name='Budget Enforcement',
            description='Enforce cost budgets',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='TOKEN_TRACKING',
            name='Token Tracking',
            description='Track token usage',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='OUTCOME_CARDS',
            name='Outcome Cards',
            description='Generate outcome cards',
            status=FlagStatus.ENABLED,
        ))

        self.register(FeatureFlag(
            flag_id='CONTEXT_PLAYLISTS',
            name='Context Playlists',
            description='Generate context playlists',
            status=FlagStatus.ENABLED,
        ))

        # Advanced features (rollout mode)
        self.register(FeatureFlag(
            flag_id='AUTO_ESCALATION',
            name='Auto Escalation',
            description='Automatically escalate to better models on failure',
            status=FlagStatus.ROLLOUT,
            rollout_percentage=50,
        ))

        self.register(FeatureFlag(
            flag_id='AGGRESSIVE_COMPACTION',
            name='Aggressive Compaction',
            description='Use aggressive context compaction',
            status=FlagStatus.ROLLOUT,
            rollout_percentage=25,
        ))

    def register(self, flag: FeatureFlag):
        """Register a feature flag.

        Args:
            flag: Feature flag to register
        """
        self.flags[flag.flag_id] = flag
        logger.debug("Registered feature flag", flag_id=flag.flag_id, status=flag.status.value)

    def get(self, flag_id: str) -> Optional[FeatureFlag]:
        """Get a feature flag.

        Args:
            flag_id: Flag identifier

        Returns:
            FeatureFlag or None
        """
        return self.flags.get(flag_id)

    def is_enabled(
        self,
        flag_id: str,
        tenant_id: Optional[str] = None,
        default: bool = False,
    ) -> bool:
        """Check if a flag is enabled.

        Args:
            flag_id: Flag identifier
            tenant_id: Optional tenant identifier
            default: Default value if flag not found

        Returns:
            True if enabled
        """
        flag = self.get(flag_id)

        if not flag:
            logger.warning("Feature flag not found", flag_id=flag_id)
            return default

        if tenant_id:
            return flag.is_enabled_for_tenant(tenant_id)

        # Global check (no tenant)
        return flag.status == FlagStatus.ENABLED

    def enable(self, flag_id: str):
        """Enable a flag globally.

        Args:
            flag_id: Flag identifier
        """
        flag = self.get(flag_id)
        if flag:
            flag.status = FlagStatus.ENABLED
            flag.updated_at = datetime.utcnow()
            logger.info("Feature flag enabled", flag_id=flag_id)
        else:
            logger.warning("Cannot enable unknown flag", flag_id=flag_id)

    def disable(self, flag_id: str):
        """Disable a flag globally.

        Args:
            flag_id: Flag identifier
        """
        flag = self.get(flag_id)
        if flag:
            flag.status = FlagStatus.DISABLED
            flag.updated_at = datetime.utcnow()
            logger.info("Feature flag disabled", flag_id=flag_id)
        else:
            logger.warning("Cannot disable unknown flag", flag_id=flag_id)

    def set_rollout(self, flag_id: str, percentage: int):
        """Set rollout percentage for a flag.

        Args:
            flag_id: Flag identifier
            percentage: Rollout percentage (0-100)
        """
        if not 0 <= percentage <= 100:
            raise ValueError("Percentage must be between 0 and 100")

        flag = self.get(flag_id)
        if flag:
            flag.status = FlagStatus.ROLLOUT
            flag.rollout_percentage = percentage
            flag.updated_at = datetime.utcnow()
            logger.info(
                "Feature flag rollout updated",
                flag_id=flag_id,
                percentage=percentage,
            )
        else:
            logger.warning("Cannot set rollout for unknown flag", flag_id=flag_id)

    def whitelist_tenant(self, flag_id: str, tenant_id: str):
        """Add tenant to whitelist.

        Args:
            flag_id: Flag identifier
            tenant_id: Tenant identifier
        """
        flag = self.get(flag_id)
        if flag and tenant_id not in flag.tenant_whitelist:
            flag.tenant_whitelist.append(tenant_id)
            flag.updated_at = datetime.utcnow()
            logger.info("Tenant whitelisted", flag_id=flag_id, tenant_id=tenant_id)

    def blacklist_tenant(self, flag_id: str, tenant_id: str):
        """Add tenant to blacklist.

        Args:
            flag_id: Flag identifier
            tenant_id: Tenant identifier
        """
        flag = self.get(flag_id)
        if flag and tenant_id not in flag.tenant_blacklist:
            flag.tenant_blacklist.append(tenant_id)
            flag.updated_at = datetime.utcnow()
            logger.info("Tenant blacklisted", flag_id=flag_id, tenant_id=tenant_id)

    def get_all_flags(self) -> Dict[str, FeatureFlag]:
        """Get all feature flags.

        Returns:
            Dictionary of flags
        """
        return self.flags.copy()

    def get_stats(self) -> Dict[str, Any]:
        """Get feature flag statistics.

        Returns:
            Statistics dictionary
        """
        status_counts = {
            FlagStatus.ENABLED: 0,
            FlagStatus.DISABLED: 0,
            FlagStatus.ROLLOUT: 0,
            FlagStatus.AB_TEST: 0,
        }

        for flag in self.flags.values():
            status_counts[flag.status] += 1

        return {
            'total_flags': len(self.flags),
            'by_status': {
                status.value: count
                for status, count in status_counts.items()
            },
            'flags': {
                flag_id: flag.to_dict()
                for flag_id, flag in self.flags.items()
            },
        }


# Global instance
_feature_flags = FeatureFlags()


def get_feature_flag(flag_id: str) -> Optional[FeatureFlag]:
    """Get a feature flag from global instance.

    Args:
        flag_id: Flag identifier

    Returns:
        FeatureFlag or None
    """
    return _feature_flags.get(flag_id)


def is_enabled(
    flag_id: str,
    tenant_id: Optional[str] = None,
    default: bool = False,
) -> bool:
    """Check if feature is enabled from global instance.

    Args:
        flag_id: Flag identifier
        tenant_id: Optional tenant identifier
        default: Default value if flag not found

    Returns:
        True if enabled
    """
    return _feature_flags.is_enabled(flag_id, tenant_id, default)


def get_flags_instance() -> FeatureFlags:
    """Get global feature flags instance.

    Returns:
        FeatureFlags instance
    """
    return _feature_flags
