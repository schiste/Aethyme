"""
Agent Enablement Module for RepoGraph

This module provides agent-friendly invariant enforcement, parity scoring,
gap detection, context pack generation, and onboarding manifest creation.

Part of RepoGraph Sprint 1 Task S1-T11: Agent-Enablement Parity & Ingestion
"""

from .invariants import InvariantRulesEngine, InvariantSeverity
from .parity_scorer import ParityScorer
from .gap_detector import GapDetector
from .context_packs import ContextPackGenerator
from .patch_generator import AutofixPatchGenerator
from .onboarding import OnboardingManifestGenerator
from .staleness import StalenessMonitor

__all__ = [
    "InvariantRulesEngine",
    "InvariantSeverity",
    "ParityScorer",
    "GapDetector",
    "ContextPackGenerator",
    "AutofixPatchGenerator",
    "OnboardingManifestGenerator",
    "StalenessMonitor",
]

__version__ = "1.0.0"
