//! Detector for missing FOLDER.md documentation (port of
//! `src/scorecard/detectors/folder_docs.py`).

use std::path::Path;

use crate::model::{Finding, Severity};
use crate::walk::{rglob_all, should_skip_file};

pub struct FolderDocsDetector;

const IMPORTANT_DIRS: [&str; 15] = [
    "src",
    "components",
    "pages",
    "api",
    "utils",
    "lib",
    "services",
    "models",
    "controllers",
    "views",
    "routes",
    "middleware",
    "config",
    "tests",
    "docs",
];

impl super::Detector for FolderDocsDetector {
    fn name(&self) -> &'static str {
        "folder-docs"
    }

    fn description(&self) -> &'static str {
        "Checks for missing FOLDER.md documentation in directories"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        let mut findings = Vec::new();

        for entry in rglob_all(repo_path) {
            if !entry.is_dir {
                continue;
            }
            if should_skip_file(&entry.path) {
                continue;
            }

            let dir_name = entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let mut should_have_docs = IMPORTANT_DIRS.contains(&dir_name.as_str());

            if !should_have_docs {
                // Python: len(glob('*.py')) + len(glob('*.ts')) +
                // len(glob('*.tsx')) >= 3. fnmatch name-matches any
                // entry kind (a directory named x.py counts).
                let mut code_files = 0usize;
                if let Ok(children) = std::fs::read_dir(&entry.path) {
                    for child in children.flatten() {
                        let name = child.file_name().to_string_lossy().to_string();
                        if name.ends_with(".py") || name.ends_with(".ts") || name.ends_with(".tsx")
                        {
                            code_files += 1;
                        }
                    }
                }
                if code_files >= 3 {
                    should_have_docs = true;
                }
            }

            if should_have_docs {
                let folder_md = entry.path.join("FOLDER.md");
                let readme_md = entry.path.join("README.md");
                if !folder_md.exists() && !readme_md.exists() {
                    let rel = entry
                        .path
                        .strip_prefix(repo_path)
                        .unwrap_or(&entry.path)
                        .to_string_lossy()
                        .to_string();
                    findings.push(Finding {
                        detector: "folder-docs".to_string(),
                        severity: Severity::Warning,
                        message: format!("Missing FOLDER.md in {rel}"),
                        file_path: rel.clone(),
                        line_number: None,
                        evidence: None,
                        suggestion: Some(
                            "Create FOLDER.md to document the purpose and contents of this directory"
                                .to_string(),
                        ),
                    });
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

    // Translated from tests/scorecard/test_detectors.py::TestFolderDocsDetector.

    #[test]
    fn detects_missing_folder_docs() {
        let tmp = tmpdir("folderdocs-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = FolderDocsDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(findings.iter().any(|f| f.message.contains("FOLDER.md")));
    }

    #[test]
    fn good_repo_has_docs() {
        let tmp = tmpdir("folderdocs-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(FolderDocsDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_finding_set_on_problematic_repo() {
        let tmp = tmpdir("folderdocs-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = FolderDocsDetector.detect(&repo);
        let files: Vec<&str> = findings.iter().map(|f| f.file_path.as_str()).collect();
        // src, src/api, src/components (important names, no docs);
        // src/generated has < 3 code files and a non-important name.
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"src"));
        assert!(files.contains(&"src/api"));
        assert!(files.contains(&"src/components"));
        for f in &findings {
            assert_eq!(f.message, format!("Missing FOLDER.md in {}", f.file_path));
            assert_eq!(f.line_number, None);
            assert_eq!(f.evidence, None);
        }
    }

    #[test]
    fn three_code_files_trigger_requirement() {
        let tmp = tmpdir("folderdocs-three");
        let repo = tmp.join("r");
        for name in ["one.py", "two.ts", "three.tsx"] {
            let p = repo.join("misc").join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x\n").unwrap();
        }
        let findings = FolderDocsDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file_path, "misc");
    }
}
