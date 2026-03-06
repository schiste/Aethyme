"""Data models for the live Aethyme core API."""

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field


class SearchResult(BaseModel):
    """Search result model."""

    symbol: str
    kind: str
    file_path: str
    line_number: int
    score: float = 0.0
    language: str | None = None
    documentation: str | None = None


class EgoGraphResult(BaseModel):
    """Ego graph result model."""

    symbol: str
    definition: dict[str, Any] | None = None
    nodes_by_depth: dict[int, list[dict[str, Any]]] = Field(default_factory=lambda: {})
    total_nodes: int = 0


class ImpactAnalysisResult(BaseModel):
    """Impact analysis result model."""

    symbol: str
    total_impacted: int
    max_depth_reached: int
    by_depth: dict[int, list[dict[str, Any]]] = Field(default_factory=lambda: {})


class ViolationModel(BaseModel):
    """Scorecard violation model."""

    severity: str
    type: str
    file: str
    line: int
    message: str
    suggestion: str | None = None


class CheckResultModel(BaseModel):
    """Scorecard check result model."""

    name: str
    status: str
    score: float
    coverage: float | None = None
    details: dict[str, Any] = Field(default_factory=dict)


class ScorecardResult(BaseModel):
    """Scorecard scan result model."""

    repository_id: str
    repository_name: str
    timestamp: datetime
    overall_score: float
    checks: list[CheckResultModel]
    violations: list[ViolationModel]
    summary: dict[str, Any]
