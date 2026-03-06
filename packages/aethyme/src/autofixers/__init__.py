"""Autofixers for Aethyme - Safe automated code improvements."""

from .github import GitHubIntegration
from .patch import PatchGenerator, PatchMode
from .safety import GeneratedFileDetector, RiskLevel, SafetyEngine

__all__ = [
    "SafetyEngine",
    "RiskLevel",
    "GeneratedFileDetector",
    "PatchGenerator",
    "PatchMode",
    "GitHubIntegration",
]
