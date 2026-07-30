//! Detector for schema/type drift (port of
//! `src/scorecard/detectors/schema_drift.py`).
//!
//! Serde-replacement design note (plan item 3): the Python detector
//! never introspected live pydantic classes — it regex-matched
//! `class X(BaseModel):` source text — so the "reflects over pydantic
//! constructs" surface is purely textual and ports as regex state
//! machinery, no serde schema modeling required. The
//! `_find_schema_files`/`_extract_schemas` pass fed only the stubbed
//! `_check_api_schema_consistency` (returns `[]`), so it is dropped
//! here with no observable difference; a real cross-language schema
//! comparison is V2 material.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::py_strip;
use crate::walk::{read_file_safe, rglob_all, should_skip_file};

/// fnmatch `*{ext}` semantics from `rglob('*.py')`: a NAME-suffix
/// match (so `.py` itself matches, unlike Python `Path.suffix`).
fn glob_name_matches(path: &Path, ext: &str) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().ends_with(ext))
        .unwrap_or(false)
}

pub struct SchemaDriftDetector;

struct Patterns {
    class_line: Regex,
    class_name: Regex,
    non_indented: Regex,
    field_line: Regex,
    validator: Regex,
    any_type: Regex,
}

fn patterns() -> &'static Patterns {
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| Patterns {
        class_line: Regex::new(r"^class\s+\w+\(BaseModel\):").unwrap(),
        class_name: Regex::new(r"class\s+(\w+)").unwrap(),
        non_indented: Regex::new(r"^\S").unwrap(),
        field_line: Regex::new(r"^\s+\w+:\s+").unwrap(),
        validator: Regex::new(r"@validator|@field_validator").unwrap(),
        any_type: Regex::new(r":\s*any\b").unwrap(),
    })
}

impl super::Detector for SchemaDriftDetector {
    fn name(&self) -> &'static str {
        "schema-drift"
    }

    fn description(&self) -> &'static str {
        "Checks for schema and type definition mismatches"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        let mut findings = Vec::new();
        findings.extend(self.check_pydantic_models(repo_path));
        findings.extend(self.check_typescript_interfaces(repo_path));
        findings
    }
}

impl SchemaDriftDetector {
    /// Port of `_check_pydantic_models` — the exact state machine,
    /// including its quirks: a model immediately followed by another
    /// `class ...(BaseModel):` line never emits (the new class line
    /// takes the first branch), and a model still open at EOF never
    /// emits (no flush after the loop).
    fn check_pydantic_models(&self, repo_path: &Path) -> Vec<Finding> {
        let patterns = patterns();
        let mut findings = Vec::new();

        for entry in rglob_all(repo_path) {
            // Python iterated rglob('*.py'): an fnmatch NAME filter
            // (no is_file gate). A directory named `x.py` would have
            // crashed the Python detector (IsADirectoryError escapes
            // read_file_safe's except tuple → engine error field); here
            // the read failure degrades to a skip — accepted divergence
            // for that pathological case.
            if !glob_name_matches(&entry.path, ".py") {
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

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();

            let mut in_model = false;
            let mut model_name = String::new();
            let mut model_line = 0usize;
            let mut has_fields = false;
            let mut has_validators = false;

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                if patterns.class_line.is_match(line) {
                    in_model = true;
                    let Some(caps) = patterns.class_name.captures(line) else {
                        in_model = false;
                        continue;
                    };
                    model_name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                    model_line = line_num;
                    has_fields = false;
                    has_validators = false;
                } else if in_model {
                    if patterns.non_indented.is_match(line) {
                        if has_fields && !has_validators {
                            findings.push(Finding {
                                detector: "schema-drift".to_string(),
                                severity: Severity::Info,
                                message: format!(
                                    "Pydantic model '{model_name}' has no validators"
                                ),
                                file_path: rel.clone(),
                                line_number: Some(model_line as i64),
                                evidence: None,
                                suggestion: Some(
                                    "Consider adding validators for data validation".to_string(),
                                ),
                            });
                        }
                        in_model = false;
                    }
                    // These still run in the same iteration even after
                    // in_model flips false (sequential ifs in Python).
                    if patterns.field_line.is_match(line) {
                        has_fields = true;
                    }
                    if patterns.validator.is_match(line) {
                        has_validators = true;
                    }
                }
            }
        }

        findings
    }

    /// Port of `_check_typescript_interfaces`, including the
    /// `content[:content.find(line)]` quirk: the "is there an
    /// interface above?" check slices at the FIRST occurrence of the
    /// line's text (empty lines slice at 0), not at the line's actual
    /// position.
    fn check_typescript_interfaces(&self, repo_path: &Path) -> Vec<Finding> {
        let patterns = patterns();
        let mut findings = Vec::new();

        for entry in rglob_all(repo_path) {
            if !glob_name_matches(&entry.path, ".ts") {
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

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                if patterns.any_type.is_match(line) {
                    let prefix_end = content.find(line).unwrap_or(0);
                    if content[..prefix_end].contains("interface") {
                        findings.push(Finding {
                            detector: "schema-drift".to_string(),
                            severity: Severity::Warning,
                            message: "Using 'any' type reduces type safety".to_string(),
                            file_path: rel.clone(),
                            line_number: Some(line_num as i64),
                            evidence: Some(py_strip(line).to_string()),
                            suggestion: Some(
                                "Replace 'any' with specific type or 'unknown'".to_string(),
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

    // Translated from tests/scorecard/test_detectors.py::TestSchemaDriftDetector.

    #[test]
    fn detects_any_types() {
        let tmp = tmpdir("schema-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = SchemaDriftDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("any"))
        );
    }

    #[test]
    fn proper_types() {
        let tmp = tmpdir("schema-good");
        let repo = build_good_scorecard_repo(&tmp);
        let findings = SchemaDriftDetector.detect(&repo);
        assert!(findings.iter().all(|f| f.severity != Severity::Blocker));
    }

    #[test]
    fn pydantic_state_machine_quirks() {
        let tmp = tmpdir("schema-quirks");
        let repo = tmp.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("models.py"),
            concat!(
                "from pydantic import BaseModel\n\n",
                // Fields, no validator, closed by a top-level line: EMITS.
                "class Alpha(BaseModel):\n    name: str\n\nEND = 1\n",
                // Fields + validator: silent.
                "class Beta(BaseModel):\n    value: int\n\n    @field_validator(\"value\")\n    def v(cls, v):\n        return v\n\nEND2 = 2\n",
                // Model directly followed by another model: the first
                // one never emits (quirk).
                "class Gamma(BaseModel):\n    x: int\n",
                "class Delta(BaseModel):\n    y: int\n",
                // Delta is open at EOF: never emits (quirk).
            ),
        )
        .unwrap();
        let findings = SchemaDriftDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.message, "Pydantic model 'Alpha' has no validators");
        assert_eq!(f.line_number, Some(3));
        assert_eq!(f.severity, Severity::Info);
    }

    #[test]
    fn ts_any_requires_interface_above_first_occurrence() {
        let tmp = tmpdir("schema-ts");
        let repo = tmp.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        // 'any' BEFORE the interface: content.find(line) slices before
        // 'interface' → silent.
        std::fs::write(
            repo.join("pre.ts"),
            "const x: any = 1;\ninterface Later {\n  a: string;\n}\n",
        )
        .unwrap();
        // 'any' after an interface: finding, evidence unstripped-length.
        std::fs::write(
            repo.join("post.ts"),
            "interface Thing {\n  data: any;\n}\n",
        )
        .unwrap();
        let findings = SchemaDriftDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file_path, "post.ts");
        assert_eq!(findings[0].line_number, Some(2));
        assert_eq!(findings[0].evidence.as_deref(), Some("data: any;"));
    }
}
