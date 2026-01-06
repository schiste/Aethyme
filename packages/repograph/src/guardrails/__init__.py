"""Guardrails for safe AI operations in RepoGraph.

This module provides safety mechanisms to prevent unsafe operations,
detect schema drift, and enforce schema-first development practices.
"""

from .schema_first import (
    SchemaExtractor,
    SchemaValidator,
    SchemaGate,
    ExtractedSchema,
)
from .sentinels import (
    DriftSentinel,
    SentinelRule,
    DriftDetector,
    PreflightCheck,
)
from .flags import (
    FeatureFlags,
    get_feature_flag,
    is_enabled,
)

__all__ = [
    'SchemaExtractor',
    'SchemaValidator',
    'SchemaGate',
    'ExtractedSchema',
    'DriftSentinel',
    'SentinelRule',
    'DriftDetector',
    'PreflightCheck',
    'FeatureFlags',
    'get_feature_flag',
    'is_enabled',
]
