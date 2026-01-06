"""Tests for feature flags."""

import pytest
from src.guardrails.flags import (
    FeatureFlags,
    FeatureFlag,
    FlagStatus,
    get_feature_flag,
    is_enabled,
)


class TestFeatureFlag:
    """Tests for feature flag."""

    def test_is_enabled_for_tenant_whitelist(self):
        """Test tenant whitelist."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test",
            description="Test flag",
            status=FlagStatus.DISABLED,
            tenant_whitelist=["tenant1"],
        )

        assert flag.is_enabled_for_tenant("tenant1") is True
        assert flag.is_enabled_for_tenant("tenant2") is False

    def test_is_enabled_for_tenant_blacklist(self):
        """Test tenant blacklist."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test",
            description="Test flag",
            status=FlagStatus.ENABLED,
            tenant_blacklist=["tenant1"],
        )

        assert flag.is_enabled_for_tenant("tenant1") is False
        assert flag.is_enabled_for_tenant("tenant2") is True

    def test_is_enabled_globally(self):
        """Test globally enabled flag."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test",
            description="Test flag",
            status=FlagStatus.ENABLED,
        )

        assert flag.is_enabled_for_tenant("any_tenant") is True

    def test_is_enabled_rollout(self):
        """Test rollout percentage."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test",
            description="Test flag",
            status=FlagStatus.ROLLOUT,
            rollout_percentage=50,
        )

        # Rollout is deterministic based on hash
        results = [flag.is_enabled_for_tenant(f"tenant{i}") for i in range(100)]
        enabled_count = sum(results)

        # Should be roughly 50% (allow some variance)
        assert 40 <= enabled_count <= 60

    def test_rollout_consistency(self):
        """Test that rollout is consistent for same tenant."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test",
            description="Test flag",
            status=FlagStatus.ROLLOUT,
            rollout_percentage=30,
        )

        # Same tenant should get same result
        result1 = flag.is_enabled_for_tenant("tenant1")
        result2 = flag.is_enabled_for_tenant("tenant1")

        assert result1 == result2

    def test_to_dict(self):
        """Test converting flag to dictionary."""
        flag = FeatureFlag(
            flag_id="test_flag",
            name="Test Flag",
            description="Test description",
            status=FlagStatus.ENABLED,
        )

        data = flag.to_dict()

        assert data['flag_id'] == "test_flag"
        assert data['name'] == "Test Flag"
        assert data['status'] == "enabled"


class TestFeatureFlags:
    """Tests for feature flags registry."""

    def test_initialization_with_defaults(self):
        """Test that default flags are registered."""
        flags = FeatureFlags()

        assert len(flags.flags) > 0
        assert 'GUARDRAILS_V1' in flags.flags
        assert 'SCHEMA_FIRST' in flags.flags
        assert 'DRIFT_SENTINELS' in flags.flags

    def test_register_custom_flag(self):
        """Test registering custom flag."""
        flags = FeatureFlags()

        custom_flag = FeatureFlag(
            flag_id="custom",
            name="Custom",
            description="Custom flag",
        )

        initial_count = len(flags.flags)
        flags.register(custom_flag)

        assert len(flags.flags) == initial_count + 1
        assert flags.get("custom") == custom_flag

    def test_is_enabled_global(self):
        """Test checking if flag is enabled globally."""
        flags = FeatureFlags()

        # Default flags are enabled
        assert flags.is_enabled('GUARDRAILS_V1') is True
        assert flags.is_enabled('SCHEMA_FIRST') is True

    def test_is_enabled_for_tenant(self):
        """Test checking if flag is enabled for tenant."""
        flags = FeatureFlags()

        # Enable for specific tenant
        flags.whitelist_tenant('SCHEMA_FIRST', 'tenant1')

        assert flags.is_enabled('SCHEMA_FIRST', 'tenant1') is True

    def test_is_enabled_nonexistent(self):
        """Test checking nonexistent flag."""
        flags = FeatureFlags()

        # Should return default value
        assert flags.is_enabled('NONEXISTENT', default=False) is False
        assert flags.is_enabled('NONEXISTENT', default=True) is True

    def test_enable_flag(self):
        """Test enabling a flag."""
        flags = FeatureFlags()

        # Create disabled flag
        flag = FeatureFlag(
            flag_id="test",
            name="Test",
            description="Test",
            status=FlagStatus.DISABLED,
        )
        flags.register(flag)

        flags.enable("test")

        assert flags.get("test").status == FlagStatus.ENABLED

    def test_disable_flag(self):
        """Test disabling a flag."""
        flags = FeatureFlags()

        flags.disable('SCHEMA_FIRST')

        assert flags.get('SCHEMA_FIRST').status == FlagStatus.DISABLED

    def test_set_rollout(self):
        """Test setting rollout percentage."""
        flags = FeatureFlags()

        flags.set_rollout('SCHEMA_FIRST', 75)

        flag = flags.get('SCHEMA_FIRST')
        assert flag.status == FlagStatus.ROLLOUT
        assert flag.rollout_percentage == 75

    def test_set_rollout_invalid_percentage(self):
        """Test setting invalid rollout percentage."""
        flags = FeatureFlags()

        with pytest.raises(ValueError):
            flags.set_rollout('SCHEMA_FIRST', 150)

        with pytest.raises(ValueError):
            flags.set_rollout('SCHEMA_FIRST', -10)

    def test_whitelist_tenant(self):
        """Test whitelisting tenant."""
        flags = FeatureFlags()

        flags.whitelist_tenant('SCHEMA_FIRST', 'tenant1')

        flag = flags.get('SCHEMA_FIRST')
        assert 'tenant1' in flag.tenant_whitelist

    def test_blacklist_tenant(self):
        """Test blacklisting tenant."""
        flags = FeatureFlags()

        flags.blacklist_tenant('SCHEMA_FIRST', 'tenant1')

        flag = flags.get('SCHEMA_FIRST')
        assert 'tenant1' in flag.tenant_blacklist

    def test_get_all_flags(self):
        """Test getting all flags."""
        flags = FeatureFlags()

        all_flags = flags.get_all_flags()

        assert isinstance(all_flags, dict)
        assert len(all_flags) > 0

    def test_get_stats(self):
        """Test getting statistics."""
        flags = FeatureFlags()

        stats = flags.get_stats()

        assert 'total_flags' in stats
        assert 'by_status' in stats
        assert 'flags' in stats
        assert stats['total_flags'] > 0


class TestGlobalFunctions:
    """Tests for global helper functions."""

    def test_get_feature_flag(self):
        """Test get_feature_flag function."""
        flag = get_feature_flag('GUARDRAILS_V1')

        assert flag is not None
        assert flag.flag_id == 'GUARDRAILS_V1'

    def test_is_enabled_function(self):
        """Test is_enabled helper function."""
        # Test with existing flag
        assert is_enabled('GUARDRAILS_V1') is True

        # Test with nonexistent flag
        assert is_enabled('NONEXISTENT', default=False) is False
