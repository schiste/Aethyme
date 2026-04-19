use crate::graph::facts::{function_usage_fact, public_function_facts};
use crate::map::RepositoryMap;
use crate::model::analysis::{
    AnswerStatus, DeadCodeAnswer, DeadCodeCandidate, DeadCodeQuery, DeadCodeSummary, EvidencePacket,
};

pub fn analyze_dead_code(
    map: &RepositoryMap,
    scope: &str,
    searched_roots: &[String],
    include_methods: bool,
) -> DeadCodeAnswer {
    let roots = normalize_roots(searched_roots);
    let facts = public_function_facts(map, scope, include_methods);
    let mut candidates = Vec::new();
    let mut excluded = Vec::new();

    for fact in facts {
        let usage = function_usage_fact(map, &fact, scope, &roots);
        let ambiguity = ambiguity_for(&usage);
        let status = if !usage.external_callers.is_empty() {
            AnswerStatus::Used
        } else if ambiguity.is_empty() {
            AnswerStatus::Unused
        } else {
            AnswerStatus::Ambiguous
        };
        let confidence = confidence_for(&status, &usage);
        let rationale = rationale_for(&status, &usage, &ambiguity);
        let candidate = DeadCodeCandidate {
            function: usage.function.clone(),
            status: status.clone(),
            confidence,
            evidence: EvidencePacket {
                searched_roots: usage.searched_roots.clone(),
                internal_callers: usage.internal_callers.clone(),
                external_callers: usage.external_callers.clone(),
            },
            ambiguity,
            rationale,
        };
        match status {
            AnswerStatus::Used => excluded.push(candidate),
            _ => candidates.push(candidate),
        }
    }

    candidates.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.function.defined_in.cmp(&right.function.defined_in))
            .then_with(|| left.function.name.cmp(&right.function.name))
    });
    excluded.sort_by(|left, right| {
        left.function
            .defined_in
            .cmp(&right.function.defined_in)
            .then_with(|| left.function.name.cmp(&right.function.name))
    });

    let summary = DeadCodeSummary {
        total_candidates: candidates.len() + excluded.len(),
        unused: candidates
            .iter()
            .filter(|candidate| matches!(candidate.status, AnswerStatus::Unused))
            .count(),
        ambiguous: candidates
            .iter()
            .filter(|candidate| matches!(candidate.status, AnswerStatus::Ambiguous))
            .count(),
        used: excluded.len(),
    };

    DeadCodeAnswer {
        analyzer: "dead-code".to_string(),
        version: "1".to_string(),
        query: DeadCodeQuery {
            scope: scope.trim_end_matches('/').to_string(),
            searched_roots: roots,
            include_methods,
        },
        candidates,
        excluded,
        summary,
    }
}

fn normalize_roots(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .collect()
}

fn ambiguity_for(usage: &crate::model::analysis::FunctionUsageFact) -> Vec<String> {
    let mut ambiguity = Vec::new();
    if usage.external_callers.is_empty() && !usage.internal_callers.is_empty() {
        ambiguity.push("exported_but_internal_only".to_string());
    }
    if usage.function.name == "main" {
        ambiguity.push("entrypoint_like_name".to_string());
    }
    ambiguity
}

fn confidence_for(status: &AnswerStatus, usage: &crate::model::analysis::FunctionUsageFact) -> f32 {
    match status {
        AnswerStatus::Unused => 0.95,
        AnswerStatus::Ambiguous => {
            if usage.external_callers.is_empty() {
                0.65
            } else {
                0.4
            }
        }
        AnswerStatus::Used => 0.98,
    }
}

fn rationale_for(
    status: &AnswerStatus,
    usage: &crate::model::analysis::FunctionUsageFact,
    ambiguity: &[String],
) -> String {
    let searched = if usage.searched_roots.is_empty() {
        "the indexed repository".to_string()
    } else {
        usage.searched_roots.join(", ")
    };
    match status {
        AnswerStatus::Unused => format!("No external callers found under [{}].", searched),
        AnswerStatus::Ambiguous => format!(
            "No external callers found under [{}], but ambiguity remains: {}.",
            searched,
            ambiguity.join(", ")
        ),
        AnswerStatus::Used => format!(
            "External callers found: {}.",
            usage.external_callers.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::analyze_dead_code;
    use crate::map::RepositoryMap;

    fn temp_repo(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aethyme_engine_{name}_{nonce}"))
    }

    #[test]
    fn analyze_dead_code_reports_unused_and_excluded_functions() {
        let root = temp_repo("analyze_dead_code");
        fs::create_dir_all(root.join("src/indexing")).expect("create indexing");
        fs::create_dir_all(root.join("src/api")).expect("create api");
        fs::write(
            root.join("src/indexing/service.py"),
            "def exported_helper():\n    return 1\n\n\
             def unused_helper():\n    return 2\n",
        )
        .expect("write service");
        fs::write(
            root.join("src/api/routes.py"),
            "from src.indexing.service import exported_helper\n\n\
             def handler():\n    return exported_helper()\n",
        )
        .expect("write external");

        let map = RepositoryMap::build(&root).expect("build map");
        let answer = analyze_dead_code(&map, "src/indexing", &["src".to_string()], false);

        assert!(
            answer
                .candidates
                .iter()
                .any(|candidate| candidate.function.name == "unused_helper")
        );
        assert!(
            answer
                .excluded
                .iter()
                .any(|candidate| candidate.function.name == "exported_helper")
        );

        let _ = fs::remove_dir_all(&root);
    }
}
