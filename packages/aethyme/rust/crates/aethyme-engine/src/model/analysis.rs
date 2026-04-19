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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadCodeAnswer {
    pub analyzer: String,
    pub version: String,
    pub query: DeadCodeQuery,
    pub candidates: Vec<DeadCodeCandidate>,
    pub excluded: Vec<DeadCodeCandidate>,
    pub summary: DeadCodeSummary,
}
