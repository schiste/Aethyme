//! Question-oriented observations, never a proof that an impact query is complete.
use crate::coverage::{GraphCoverage, GraphUseEvidence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionEvidenceState {
    Unknown,
    NotObserved,
    Partial,
    Observed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionEvidence {
    pub state: QuestionEvidenceState,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageQuestions {
    pub ci_sizing: QuestionEvidence,
    pub documentation_impact: QuestionEvidence,
    pub session_navigation: QuestionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphQuestionCapabilities {
    pub schema_version: u32,
    /// Majority is measured among observed code files, not unindexed files.
    pub majority_scope: String,
    pub majority_languages: Vec<String>,
    pub by_language: BTreeMap<String, LanguageQuestions>,
    pub majority_has_observed_question: bool,
    /// A complete bounded impact query and consumer policy remain required.
    pub grants_ci_selection_authority: bool,
}

fn assess(
    evidence: Option<&GraphUseEvidence>,
    complete: bool,
    required: &[(&str, bool)],
) -> QuestionEvidence {
    if evidence.is_none() {
        return QuestionEvidence {
            state: QuestionEvidenceState::Unknown,
            reasons: vec!["language_use_evidence_unavailable".into()],
        };
    }
    let mut reasons = required
        .iter()
        .filter(|(_, present)| !present)
        .map(|(name, _)| format!("{name}_not_observed"))
        .collect::<Vec<_>>();
    if !complete {
        reasons.push("coverage_incomplete_or_unbound".into());
    }
    let state = if required.iter().all(|(_, present)| !present) {
        QuestionEvidenceState::NotObserved
    } else if complete && reasons.is_empty() {
        QuestionEvidenceState::Observed
    } else {
        QuestionEvidenceState::Partial
    };
    QuestionEvidence { state, reasons }
}

impl GraphCoverage {
    /// Counts establish emitted prerequisites only. Even `observed` does not
    /// establish complete test selection, semantic resolution or a query result.
    pub fn question_capabilities(&self) -> GraphQuestionCapabilities {
        let mut weights = self
            .by_language
            .iter()
            .filter_map(|(language, bucket)| {
                let code_files = bucket.files.saturating_sub(bucket.non_code);
                (code_files > 0).then_some((language.clone(), code_files))
            })
            .collect::<Vec<_>>();
        weights.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total: u128 = weights.iter().map(|(_, n)| u128::from(*n)).sum();
        let mut majority_languages = Vec::new();
        let mut cumulative = 0_u128;
        let mut boundary = None;
        for (language, weight) in weights {
            if cumulative > total / 2 && boundary != Some(weight) {
                break;
            }
            cumulative += u128::from(weight);
            boundary = Some(weight);
            majority_languages.push(language);
        }
        let complete = self.available && self.safe_to_use && self.validate().is_ok();
        let mut by_language = BTreeMap::new();
        for (language, bucket) in &self.by_language {
            let evidence = if self.available {
                self.use_evidence_by_language
                    .as_ref()
                    .and_then(|map| map.get(language))
            } else {
                None
            };
            let language_complete = complete
                && bucket.partial == 0
                && bucket.unsupported == 0
                && bucket.excluded == 0
                && evidence.is_some_and(|e| e.files == bucket.files);
            let inputs = evidence.cloned().unwrap_or_default();
            by_language.insert(
                language.clone(),
                LanguageQuestions {
                    ci_sizing: assess(
                        evidence,
                        language_complete && inputs.unresolved_imports == 0,
                        &[
                            ("imports", inputs.imports > 0),
                            ("test_links", inputs.tests > 0),
                        ],
                    ),
                    documentation_impact: assess(
                        evidence,
                        language_complete,
                        &[("document_links", inputs.documents > 0)],
                    ),
                    session_navigation: assess(
                        evidence,
                        language_complete,
                        &[("files", inputs.files > 0), ("symbols", inputs.symbols > 0)],
                    ),
                },
            );
        }
        let majority_has_observed_question = !majority_languages.is_empty()
            && majority_languages.iter().all(|language| {
                by_language.get(language).is_some_and(|q| {
                    [&q.ci_sizing, &q.documentation_impact, &q.session_navigation]
                        .iter()
                        .any(|q| q.state == QuestionEvidenceState::Observed)
                })
            });
        GraphQuestionCapabilities {
            schema_version: 1,
            majority_scope: "observed_code_files_only".into(),
            majority_languages,
            by_language,
            majority_has_observed_question,
            grants_ci_selection_authority: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::CoverageBucket;

    fn coverage() -> GraphCoverage {
        let mut c = GraphCoverage::unavailable("fixture");
        c.available = true;
        c.safe_to_use = true;
        c.gaps.clear();
        c.source_revision = Some("revision".into());
        c.indexed_revision = c.source_revision.clone();
        c.source_tree_sha256 = Some("tree".into());
        c.indexed_tree_sha256 = c.source_tree_sha256.clone();
        c.coverage_mode = "complete".into();
        c.by_language.insert(
            "Rust".into(),
            CoverageBucket {
                files: 2,
                parsed: 2,
                ..Default::default()
            },
        );
        c.use_evidence_by_language = Some(BTreeMap::from([(
            "Rust".into(),
            GraphUseEvidence {
                files: 2,
                symbols: 3,
                imports: 1,
                tests: 1,
                ..Default::default()
            },
        )]));
        c
    }

    #[test]
    fn observed_prerequisites_are_question_specific_and_never_grant_gate_authority() {
        let report = coverage().question_capabilities();
        let rust = &report.by_language["Rust"];
        assert_eq!(rust.ci_sizing.state, QuestionEvidenceState::Observed);
        assert_eq!(
            rust.documentation_impact.state,
            QuestionEvidenceState::NotObserved
        );
        assert_eq!(
            rust.session_navigation.state,
            QuestionEvidenceState::Observed
        );
        assert!(report.majority_has_observed_question);
        assert!(!report.grants_ci_selection_authority);
    }

    #[test]
    fn legacy_missing_measurement_is_unknown_not_measured_zero() {
        let mut c = coverage();
        c.use_evidence_by_language = None;
        let report = c.question_capabilities();
        assert_eq!(
            report.by_language["Rust"].ci_sizing.state,
            QuestionEvidenceState::Unknown
        );
        assert!(!report.majority_has_observed_question);
        c.use_evidence_by_language = Some(BTreeMap::new());
        assert_eq!(
            c.question_capabilities().by_language["Rust"]
                .session_navigation
                .state,
            QuestionEvidenceState::Unknown
        );
        c.use_evidence_by_language
            .as_mut()
            .unwrap()
            .insert("Rust".into(), GraphUseEvidence::default());
        assert_eq!(
            c.question_capabilities().by_language["Rust"]
                .session_navigation
                .state,
            QuestionEvidenceState::NotObserved
        );
    }

    #[test]
    fn positive_counts_with_partial_or_unbound_coverage_never_become_observed() {
        let mut c = coverage();
        c.safe_to_use = false;
        c.gaps.push("excluded_files".into());
        assert_eq!(
            c.question_capabilities().by_language["Rust"]
                .ci_sizing
                .state,
            QuestionEvidenceState::Partial
        );
        assert!(!c.question_capabilities().majority_has_observed_question);
        c.gaps.clear();
        c.safe_to_use = true;
        c.indexed_tree_sha256 = Some("other".into());
        assert!(!c.question_capabilities().majority_has_observed_question);
    }

    #[test]
    fn unresolved_imports_and_inventory_mismatch_remain_partial() {
        let mut c = coverage();
        c.use_evidence_by_language
            .as_mut()
            .unwrap()
            .get_mut("Rust")
            .unwrap()
            .unresolved_imports = 1;
        let r = c.question_capabilities();
        assert_eq!(
            r.by_language["Rust"].ci_sizing.state,
            QuestionEvidenceState::Partial
        );
        assert_eq!(
            r.by_language["Rust"].session_navigation.state,
            QuestionEvidenceState::Observed
        );
        c.use_evidence_by_language
            .as_mut()
            .unwrap()
            .get_mut("Rust")
            .unwrap()
            .files = 1;
        assert!(!c.question_capabilities().majority_has_observed_question);
    }

    #[test]
    fn majority_includes_boundary_ties_and_unsupported_code_but_excludes_noncode() {
        let mut c = coverage();
        c.by_language.insert(
            "Python".into(),
            CoverageBucket {
                files: 2,
                unsupported: 2,
                ..Default::default()
            },
        );
        c.by_language.insert(
            "Markdown".into(),
            CoverageBucket {
                files: 20,
                non_code: 20,
                ..Default::default()
            },
        );
        let r = c.question_capabilities();
        assert_eq!(r.majority_languages, vec!["Python", "Rust"]);
        assert!(!r.majority_has_observed_question);
        assert_eq!(r.majority_scope, "observed_code_files_only");
        c.by_language.clear();
        assert!(!c.question_capabilities().majority_has_observed_question);
    }
}
