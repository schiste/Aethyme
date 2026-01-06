"""Data models for RepoGraph SDK."""

from typing import List, Dict, Any, Optional
from datetime import datetime
from pydantic import BaseModel, Field


class SearchResult(BaseModel):
    """Search result model."""
    symbol: str
    kind: str
    file_path: str
    line_number: int
    score: float = 0.0
    language: Optional[str] = None
    documentation: Optional[str] = None


class EgoGraphResult(BaseModel):
    """Ego graph result model."""
    symbol: str
    definition: Optional[Dict[str, Any]] = None
    nodes_by_depth: Dict[int, List[Dict[str, Any]]] = Field(default_factory=dict)
    total_nodes: int = 0


class ImpactAnalysisResult(BaseModel):
    """Impact analysis result model."""
    symbol: str
    total_impacted: int
    max_depth_reached: int
    by_depth: Dict[int, List[Dict[str, Any]]] = Field(default_factory=dict)


class ViolationModel(BaseModel):
    """Scorecard violation model."""
    severity: str
    type: str
    file: str
    line: int
    message: str
    suggestion: Optional[str] = None


class CheckResultModel(BaseModel):
    """Scorecard check result model."""
    name: str
    status: str
    score: float
    coverage: Optional[float] = None
    details: Dict[str, Any] = Field(default_factory=dict)


class ScorecardResult(BaseModel):
    """Scorecard scan result model."""
    repository_id: str
    repository_name: str
    timestamp: datetime
    overall_score: float
    checks: List[CheckResultModel]
    violations: List[ViolationModel]
    summary: Dict[str, Any]


class FixModel(BaseModel):
    """Autofix model."""
    type: str
    file: str
    line: int
    old_content: str
    new_content: str
    confidence: float
    safe: bool


class AutofixResult(BaseModel):
    """Autofix result model."""
    repository_id: str
    timestamp: datetime
    dry_run: bool
    fixes: List[FixModel]
    summary: Dict[str, Any]
    patch: Optional[str] = None


class TelemetryMetric(BaseModel):
    """Telemetry metric model."""
    name: str
    description: str
    type: str
    unit: str
    labels: List[str] = Field(default_factory=list)


class MetricDataPoint(BaseModel):
    """Metric data point model."""
    timestamp: datetime
    value: float
    labels: Dict[str, str] = Field(default_factory=dict)


class GuardrailConfig(BaseModel):
    """Guardrail configuration model."""
    name: str
    enabled: bool
    severity: str
    config: Dict[str, Any] = Field(default_factory=dict)


class GuardrailViolation(BaseModel):
    """Guardrail violation model."""
    guardrail: str
    timestamp: datetime
    severity: str
    message: str
    context: Dict[str, Any] = Field(default_factory=dict)
    prevented: bool
