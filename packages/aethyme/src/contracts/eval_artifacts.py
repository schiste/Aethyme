"""Versioned wrappers for persisted eval artifacts."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from .versions import EVAL_ARTIFACT_SCHEMA_VERSION


def _default_generated_at() -> str:
    return datetime.now(UTC).isoformat()


@dataclass(frozen=True)
class EvalArtifactEnvelope:
    """Stable envelope for persisted eval artifact JSON files."""

    artifact_kind: str
    payload: Any
    run_id: str | None = None
    eval_type: str | None = None
    target: str | None = None
    generated_at: str = _default_generated_at()
    schema_version: str = EVAL_ARTIFACT_SCHEMA_VERSION

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "artifact_kind": self.artifact_kind,
            "generated_at": self.generated_at,
            "run_id": self.run_id,
            "eval_type": self.eval_type,
            "target": self.target,
            "payload": self.payload,
        }


def make_eval_artifact_envelope(
    artifact_kind: str,
    payload: Any,
    *,
    run_id: str | None = None,
    eval_type: str | None = None,
    target: str | None = None,
) -> dict[str, Any]:
    """Return a serializable eval artifact envelope."""
    return EvalArtifactEnvelope(
        artifact_kind=artifact_kind,
        payload=payload,
        run_id=run_id,
        eval_type=eval_type,
        target=target,
    ).to_dict()
