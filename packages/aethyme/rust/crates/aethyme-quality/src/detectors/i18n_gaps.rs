//! Detector for hardcoded user-facing strings (port of
//! `src/scorecard/detectors/i18n_gaps.py`).

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::{py_slice, py_strip};
use crate::walk::{py_suffix, read_file_safe, rglob_all, should_skip_file};

pub struct I18nGapsDetector;

const EXTENSIONS: [&str; 3] = [".tsx", ".jsx", ".vue"];

struct Patterns {
    jsx_strings: Vec<Regex>,
    skips: Vec<Regex>,
    code_chars: Regex,
}

fn patterns() -> &'static Patterns {
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| Patterns {
        jsx_strings: vec![
            Regex::new(r">\s*([A-Z][a-zA-Z\s]{10,})\s*<").unwrap(),
            Regex::new(r#"placeholder=["']([^"']{10,})["']"#).unwrap(),
            Regex::new(r#"title=["']([^"']{10,})["']"#).unwrap(),
            Regex::new(r#"aria-label=["']([^"']{10,})["']"#).unwrap(),
        ],
        skips: vec![
            Regex::new(r#"t\(["']"#).unwrap(),
            Regex::new(r"i18n\.").unwrap(),
            Regex::new(r"\$t\(").unwrap(),
            Regex::new(r"formatMessage").unwrap(),
        ],
        code_chars: Regex::new(r"[{}()\[\]]").unwrap(),
    })
}

impl super::Detector for I18nGapsDetector {
    fn name(&self) -> &'static str {
        "i18n-gaps"
    }

    fn description(&self) -> &'static str {
        "Checks for hardcoded user-facing strings that should be internationalized"
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
            let Some(content) = read_file_safe(&entry.path) else {
                continue;
            };
            if content.is_empty() {
                continue;
            }

            let uses_i18n = patterns.skips.iter().any(|p| p.is_match(&content));

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                if patterns.skips.iter().any(|p| p.is_match(line)) {
                    continue;
                }
                for pattern in &patterns.jsx_strings {
                    for caps in pattern.captures_iter(line) {
                        let text = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                        // Skip if too short or looks like code.
                        if text.split_whitespace().count() < 3 {
                            continue;
                        }
                        if patterns.code_chars.is_match(text) {
                            continue;
                        }
                        findings.push(Finding {
                            detector: "i18n-gaps".to_string(),
                            severity: if uses_i18n {
                                Severity::Info
                            } else {
                                Severity::Warning
                            },
                            message: format!(
                                "Hardcoded user-facing text: '{}...'",
                                py_slice(text, 50)
                            ),
                            file_path: rel.clone(),
                            line_number: Some(line_num as i64),
                            evidence: Some(py_slice(py_strip(line), 150)),
                            suggestion: Some(
                                "Use i18n translation function, e.g., t('key') or formatMessage()"
                                    .to_string(),
                            ),
                        });
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

    // Translated from tests/scorecard/test_detectors.py::TestI18nGapsDetector.

    #[test]
    fn detects_hardcoded_strings() {
        let tmp = tmpdir("i18n-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = I18nGapsDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| {
            let m = f.message.to_lowercase();
            m.contains("hardcoded") || m.contains("text")
        }));
    }

    #[test]
    fn i18n_usage_ok() {
        let tmp = tmpdir("i18n-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(I18nGapsDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_finding_shape_and_severity_downgrade() {
        let tmp = tmpdir("i18n-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = I18nGapsDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(
            f.message,
            "Hardcoded user-facing text: 'Create your account right now...'"
        );
        assert_eq!(f.line_number, Some(2));

        // Files that use i18n anywhere downgrade findings to INFO.
        let repo2 = tmp.join("mixed");
        std::fs::create_dir_all(&repo2).unwrap();
        std::fs::write(
            repo2.join("Widget.jsx"),
            "const a = t('x');\nconst b = <span>Please review the following items</span>;\n",
        )
        .unwrap();
        let findings2 = I18nGapsDetector.detect(&repo2);
        assert_eq!(findings2.len(), 1);
        assert_eq!(findings2[0].severity, Severity::Info);
    }
}
