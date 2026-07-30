//! Detector for missing data-ui test selectors (port of
//! `src/scorecard/detectors/data_ui_coverage.py`).

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::{py_slice, py_strip};
use crate::walk::{py_suffix, read_file_safe, rglob_all, should_skip_file};

pub struct DataUiCoverageDetector;

const EXTENSIONS: [&str; 4] = [".tsx", ".jsx", ".html", ".vue"];

struct Patterns {
    components: Vec<(Regex, &'static str)>,
    data_ui: Regex,
}

fn patterns() -> &'static Patterns {
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| Patterns {
        // Component patterns match case-insensitively (re.IGNORECASE);
        // the data-ui check below is case-SENSITIVE, like the original.
        components: vec![
            (Regex::new(r"(?i)<button[^>]*>").unwrap(), "button"),
            (Regex::new(r"(?i)<input[^>]*>").unwrap(), "input"),
            (Regex::new(r"(?i)<select[^>]*>").unwrap(), "select"),
            (Regex::new(r"(?i)<form[^>]*>").unwrap(), "form"),
            (Regex::new(r"(?i)<a[^>]*href=").unwrap(), "link"),
            (Regex::new(r"(?i)<textarea[^>]*>").unwrap(), "textarea"),
        ],
        data_ui: Regex::new(r#"data-ui=["'][\w-]+"#).unwrap(),
    })
}

impl super::Detector for DataUiCoverageDetector {
    fn name(&self) -> &'static str {
        "data-ui-coverage"
    }

    fn description(&self) -> &'static str {
        "Checks for missing data-ui attributes in UI components"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        let patterns = patterns();
        let mut findings = Vec::new();

        for entry in rglob_all(repo_path) {
            if !entry.is_file || !EXTENSIONS.contains(&py_suffix(&entry.path).as_str()) {
                continue;
            }
            if should_skip_file(&entry.path) {
                continue;
            }
            // Python truthiness: empty content is falsy → skipped.
            let Some(content) = read_file_safe(&entry.path) else {
                continue;
            };
            if content.is_empty() {
                continue;
            }

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                for (pattern, element_type) in &patterns.components {
                    for m in pattern.find_iter(line) {
                        let component_html = m.as_str();
                        if !patterns.data_ui.is_match(component_html) {
                            findings.push(Finding {
                                detector: "data-ui-coverage".to_string(),
                                severity: Severity::Warning,
                                message: format!(
                                    "Missing data-ui attribute on {element_type}"
                                ),
                                file_path: rel.clone(),
                                line_number: Some(line_num as i64),
                                evidence: Some(py_slice(py_strip(line), 100)),
                                suggestion: Some(format!(
                                    "Add data-ui attribute for test automation, e.g., data-ui=\"{element_type}-name\""
                                )),
                            });
                        }
                    }
                }
            }
        }

        findings
    }
}

#[cfg(test)]
mod tests {
    use super::super::Detector;
    use super::*;
    use crate::testsupport::{build_good_scorecard_repo, build_problematic_scorecard_repo, tmpdir};

    // Translated from tests/scorecard/test_detectors.py::TestDataUICoverageDetector.

    #[test]
    fn detects_missing_selectors() {
        let tmp = tmpdir("dataui-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = DataUiCoverageDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("data-ui"))
        );
    }

    #[test]
    fn no_findings_in_good_repo() {
        let tmp = tmpdir("dataui-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(DataUiCoverageDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_finding_shape_on_problematic_repo() {
        let tmp = tmpdir("dataui-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = DataUiCoverageDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.message, "Missing data-ui attribute on button");
        assert_eq!(f.file_path, "src/components/BadButton.tsx");
        assert_eq!(f.line_number, Some(2));
        assert_eq!(
            f.evidence.as_deref(),
            Some("return <button>Create your account right now</button>;")
        );
        assert_eq!(
            f.suggestion.as_deref(),
            Some("Add data-ui attribute for test automation, e.g., data-ui=\"button-name\"")
        );
    }
}
