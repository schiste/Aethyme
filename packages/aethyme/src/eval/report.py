"""Evaluation report helpers for local-first Aethyme benchmarks."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from .runner import EvaluationRunResult


@dataclass(frozen=True)
class EvaluationReport:
    task: str
    baseline_prompt_chars: int
    aethyme_prompt_chars: int
    navigation_items: int
    risk_items: int
    baseline_run: dict[str, Any] | None = None
    aethyme_run: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "task": self.task,
            "baseline_prompt_chars": self.baseline_prompt_chars,
            "aethyme_prompt_chars": self.aethyme_prompt_chars,
            "navigation_items": self.navigation_items,
            "risk_items": self.risk_items,
            "baseline_run": self.baseline_run,
            "aethyme_run": self.aethyme_run,
        }


def estimate_report(
    task: str,
    baseline_prompt: str,
    aethyme_prompt: str,
    pack: dict[str, Any],
    baseline_run: EvaluationRunResult | None = None,
    aethyme_run: EvaluationRunResult | None = None,
) -> EvaluationReport:
    """Create a local evaluation report from prompts, pack, and optional live runs."""
    return EvaluationReport(
        task=task,
        baseline_prompt_chars=len(baseline_prompt),
        aethyme_prompt_chars=len(aethyme_prompt),
        navigation_items=len(pack.get("navigation_order", [])),
        risk_items=len(pack.get("risk_flags", [])),
        baseline_run=baseline_run.to_dict() if baseline_run else None,
        aethyme_run=aethyme_run.to_dict() if aethyme_run else None,
    )
