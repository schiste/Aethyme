use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExposureKind {
    ExportedTopLevel,
    PublicMethod,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FunctionFact {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub defined_in: String,
    pub line: usize,
    pub language: String,
    pub parent_class: Option<String>,
    pub exposure_kind: ExposureKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionUsageFact {
    pub function: FunctionFact,
    pub boundary: String,
    pub searched_roots: Vec<String>,
    pub internal_callers: Vec<String>,
    pub external_callers: Vec<String>,
    pub docs_config_references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AnswerStatus {
    Unused,
    Ambiguous,
    Used,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePacket {
    pub searched_roots: Vec<String>,
    pub internal_callers: Vec<String>,
    pub external_callers: Vec<String>,
    pub docs_config_references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeCandidate {
    pub function: FunctionFact,
    pub status: AnswerStatus,
    pub confidence: f32,
    pub evidence: EvidencePacket,
    pub ambiguity: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadCodeQuery {
    pub scope: String,
    pub searched_roots: Vec<String>,
    pub include_methods: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadCodeSummary {
    pub total_candidates: usize,
    pub unused: usize,
    pub ambiguous: usize,
    pub used: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadCodeGraphCounts {
    pub functions: usize,
    pub docs: usize,
    pub configs: usize,
    pub edges: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadCodeFactCounts {
    pub public_functions: usize,
    pub usage_facts: usize,
    pub internal_callers: usize,
    pub external_callers: usize,
    pub docs_config_references: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeConfidenceSummary {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub min: Option<f32>,
    pub max: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeObservability {
    pub graph_counts: DeadCodeGraphCounts,
    pub fact_counts: DeadCodeFactCounts,
    pub confidence_summary: DeadCodeConfidenceSummary,
    pub degraded_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeAnswer {
    pub analyzer: String,
    pub version: String,
    pub query: DeadCodeQuery,
    pub candidates: Vec<DeadCodeCandidate>,
    pub excluded: Vec<DeadCodeCandidate>,
    pub summary: DeadCodeSummary,
    pub observability: DeadCodeObservability,
}
