#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    ExplainRepo,
    ExplainComponent,
    ChangeSymbol,
    TraceImpact,
    NavigateConfigOwnership,
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

impl TaskKind {
    pub fn is_explain_repo(&self) -> bool {
        matches!(self, Self::ExplainRepo)
    }

    pub fn is_change_task(&self) -> bool {
        matches!(self, Self::ChangeSymbol | Self::TraceImpact)
    }
}

fn classify_task(normalized: &str) -> TaskKind {
    if is_config_ownership_task(normalized) {
        TaskKind::NavigateConfigOwnership
    } else if normalized.contains("explain this repo") || normalized == "explain repo" {
        TaskKind::ExplainRepo
    } else if normalized.contains("explain") && normalized.contains("component") {
        TaskKind::ExplainComponent
    } else if normalized.contains("impact") || normalized.contains("blast radius") {
        TaskKind::TraceImpact
    } else if normalized.contains("change")
        || normalized.contains("update")
        || normalized.contains("modify")
        || normalized.contains("fix")
    {
        TaskKind::ChangeSymbol
    } else {
        TaskKind::Unknown
    }
}

fn is_config_ownership_task(normalized: &str) -> bool {
    let asks_for_config = normalized.contains("manifest")
        || normalized.contains("config")
        || normalized.contains("configuration")
        || normalized.contains("project");
    let asks_for_entrypoint = normalized.contains("entrypoint") || normalized.contains("main code");
    let asks_for_ownership = normalized.contains("manage")
        || normalized.contains("managed")
        || normalized.contains("controls")
        || normalized.contains("owns")
        || normalized.contains("owner")
        || normalized.contains("top-level area");

    asks_for_config && (asks_for_entrypoint || asks_for_ownership)
}

#[cfg(test)]
mod tests {
    use super::{TaskInput, TaskKind};

    #[test]
    fn classify_manifest_navigation_task() {
        let task = TaskInput::from_task_text(
            "Find the manifest that manages the main code entrypoint in the GameEngine area",
        );

        assert_eq!(task.kind, TaskKind::NavigateConfigOwnership);
    }
}
