"""Structured evaluation result models."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class EvaluationSide:
    prompt: str
    run: dict[str, Any] | None
    assessment: dict[str, Any] | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "prompt": self.prompt,
            "run": self.run,
            "assessment": self.assessment,
        }

