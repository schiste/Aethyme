#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    ExplainRepo,
    ExplainComponent,
    ChangeSymbol,
    TraceImpact,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInput {
    pub raw: String,
    pub normalized: String,
    pub kind: TaskKind,
}

impl TaskInput {
    pub fn new(raw: impl Into<String>, normalized: impl Into<String>, kind: TaskKind) -> Self {
        Self {
            raw: raw.into(),
            normalized: normalized.into(),
            kind,
        }
    }

    pub fn from_task_text(raw: &str) -> Self {
        let normalized = raw.to_ascii_lowercase().trim().to_string();
        let kind = classify_task(&normalized);
        Self::new(raw, normalized, kind)
    }
}

fn classify_task(normalized: &str) -> TaskKind {
    if normalized.contains("explain this repo") || normalized == "explain repo" {
        TaskKind::ExplainRepo
    } else if normalized.contains("explain") && normalized.contains("component") {
        TaskKind::ExplainComponent
    } else if normalized.contains("impact") || normalized.contains("blast radius") {
        TaskKind::TraceImpact
    } else if normalized.contains("change") || normalized.contains("update") || normalized.contains("modify") {
        TaskKind::ChangeSymbol
    } else {
        TaskKind::Unknown
    }
}
