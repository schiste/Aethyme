//! Detector for absolute paths that should be relative (port of
//! `src/scorecard/detectors/relative_links.py`).

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::{py_slice, py_strip};
use crate::walk::{py_suffix, read_file_safe, rglob_all, should_skip_file};

pub struct RelativeLinksDetector;

const EXTENSIONS: [&str; 9] = [
    ".md", ".py", ".ts", ".tsx", ".jsx", ".js", ".json", ".yaml", ".yml",
];

fn patterns() -> &'static Vec<(Regex, &'static str)> {
    static CELL: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            (Regex::new(r"/home/[\w/.-]+").unwrap(), "Linux home path"),
            (Regex::new(r"/Users/[\w/.-]+").unwrap(), "macOS home path"),
            (Regex::new(r"C:\\[\w\\.-]+").unwrap(), "Windows path"),
            (Regex::new(r"/var/[\w/.-]+").unwrap(), "System path"),
            (Regex::new(r"/tmp/[\w/.-]+").unwrap(), "Temp path"),
        ]
    })
}

impl super::Detector for RelativeLinksDetector {
    fn name(&self) -> &'static str {
        "relative-links"
    }

    fn description(&self) -> &'static str {
        "Checks for absolute file paths that should use relative links"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
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

            let rel = entry
                .path
                .strip_prefix(repo_path)
                .unwrap_or(&entry.path)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                for (pattern, path_type) in patterns() {
                    for m in pattern.find_iter(line) {
                        let absolute_path = m.as_str();
                        // Python skipped matches referenced after a `#` or
                        // `//` anywhere earlier in the line (dynamic
                        // `#.*<escaped>` / `//.*<escaped>` searches).
                        let escaped = regex::escape(absolute_path);
                        let hash_re = Regex::new(&format!("#.*{escaped}")).unwrap();
                        let slash_re = Regex::new(&format!("//.*{escaped}")).unwrap();
                        if hash_re.is_match(line) || slash_re.is_match(line) {
                            continue;
                        }
                        findings.push(Finding {
                            detector: "relative-links".to_string(),
                            severity: Severity::Warning,
                            message: format!("Absolute {path_type} should use relative path"),
                            file_path: rel.clone(),
                            line_number: Some(line_num as i64),
                            evidence: Some(py_slice(py_strip(line), 150)),
                            suggestion: Some(
                                "Use relative paths for better portability across environments"
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

    // Translated from tests/scorecard/test_detectors.py::TestRelativeLinksDetector.

    #[test]
    fn detects_absolute_paths() {
        let tmp = tmpdir("rellinks-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RelativeLinksDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("absolute"))
        );
    }

    #[test]
    fn relative_links_ok() {
        let tmp = tmpdir("rellinks-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(RelativeLinksDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_findings_and_comment_skip() {
        let tmp = tmpdir("rellinks-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RelativeLinksDetector.detect(&repo);
        // README.md line 3 (/Users/...) and line 4 (C:\...).
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].message,
            "Absolute macOS home path should use relative path"
        );
        assert_eq!(findings[0].line_number, Some(3));
        assert_eq!(
            findings[1].message,
            "Absolute Windows path should use relative path"
        );
        assert_eq!(findings[1].line_number, Some(4));

        // Comment-referenced paths are skipped.
        let repo2 = tmp.join("c");
        std::fs::create_dir_all(&repo2).unwrap();
        std::fs::write(
            repo2.join("code.py"),
            "x = 1  # see /home/user/example\nurl = \"https://x.io/var/data\"\n",
        )
        .unwrap();
        assert!(RelativeLinksDetector.detect(&repo2).is_empty());
    }
}
