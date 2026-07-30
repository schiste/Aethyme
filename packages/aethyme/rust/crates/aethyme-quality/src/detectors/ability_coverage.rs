//! Detector for permission/ability coverage (port of
//! `src/scorecard/detectors/ability_coverage.py`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::util::{py_slice, py_strip};
use crate::walk::{any_entry_named, read_file_safe, rglob_all, should_skip_file};

pub struct AbilityCoverageDetector;

struct ProtectedRoute {
    file: PathBuf,
    line: usize,
    content: String,
}

struct Patterns {
    auth: Vec<Regex>,
    permission: Vec<Regex>,
}

fn patterns() -> &'static Patterns {
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| Patterns {
        auth: vec![
            Regex::new(r"@router\.(post|put|delete|patch)").unwrap(),
            Regex::new(r"Depends\(get_current_user\)").unwrap(),
            Regex::new(r"@require_auth").unwrap(),
            Regex::new(r"@login_required").unwrap(),
        ],
        permission: vec![
            Regex::new(r"can\(").unwrap(),
            Regex::new(r"check_permission").unwrap(),
            Regex::new(r"require_permission").unwrap(),
            Regex::new(r"@permission_required").unwrap(),
            Regex::new(r"has_permission").unwrap(),
            Regex::new(r"authorize\(").unwrap(),
        ],
    })
}

impl super::Detector for AbilityCoverageDetector {
    fn name(&self) -> &'static str {
        "ability-coverage"
    }

    fn description(&self) -> &'static str {
        "Checks for missing authorization and permission definitions"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        let routes = self.find_protected_routes(repo_path);
        let mut findings = self.check_permission_definitions(repo_path, &routes);
        findings.extend(self.check_authorization_checks(repo_path));
        findings
    }
}

impl AbilityCoverageDetector {
    fn find_protected_routes(&self, repo_path: &Path) -> Vec<ProtectedRoute> {
        let patterns = patterns();
        let mut routes = Vec::new();

        for entry in rglob_all(repo_path) {
            // rglob('*.py'): fnmatch NAME filter (see schema_drift's
            // note on the directory-named-x.py edge).
            let is_py = entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().ends_with(".py"))
                .unwrap_or(false);
            if !is_py {
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
            for (line_num, line) in content.split('\n').enumerate() {
                let line_num = line_num + 1;
                if patterns.auth.iter().any(|p| p.is_match(line)) {
                    routes.push(ProtectedRoute {
                        file: entry.path.clone(),
                        line: line_num,
                        content: py_strip(line).to_string(),
                    });
                }
            }
        }

        routes
    }

    fn check_permission_definitions(
        &self,
        repo_path: &Path,
        routes: &[ProtectedRoute],
    ) -> Vec<Finding> {
        let patterns = patterns();
        let mut findings = Vec::new();

        for route in routes {
            // Python re-read the file per route.
            let Some(content) = read_file_safe(&route.file) else {
                continue;
            };
            if content.is_empty() {
                continue;
            }
            let lines: Vec<&str> = content.split('\n').collect();
            let line_num = route.line; // 1-based

            // Window: 0-based indices [line_num-5, line_num+15) — 4
            // lines above through 14 below, decorator line included.
            let start = line_num.saturating_sub(5);
            let end = (line_num + 15).min(lines.len());
            let has_permission = (start..end).any(|i| {
                let line = lines.get(i).copied().unwrap_or("");
                patterns.permission.iter().any(|p| p.is_match(line))
            });

            if !has_permission {
                let rel = route
                    .file
                    .strip_prefix(repo_path)
                    .unwrap_or(&route.file)
                    .to_string_lossy()
                    .to_string();
                findings.push(Finding {
                    detector: "ability-coverage".to_string(),
                    severity: Severity::Warning,
                    message: "Protected route missing explicit permission check".to_string(),
                    file_path: rel,
                    line_number: Some(line_num as i64),
                    evidence: Some(py_slice(&route.content, 100)),
                    suggestion: Some(
                        "Add explicit permission check, e.g., check_permission('resource:action')"
                            .to_string(),
                    ),
                });
            }
        }

        findings
    }

    /// Port of `_check_authorization_checks`: pure existence globs
    /// (`**/abilities.py` etc.) with NO skip filter — a match under
    /// node_modules or .git counts, exactly like the Python glob.
    fn check_authorization_checks(&self, repo_path: &Path) -> Vec<Finding> {
        let any_ability_file = any_entry_named(repo_path, "abilities.py")
            || any_entry_named(repo_path, "permissions.py")
            || any_entry_named(repo_path, "authorization.py");
        if any_ability_file {
            return Vec::new();
        }
        vec![Finding {
            detector: "ability-coverage".to_string(),
            severity: Severity::Info,
            message: "No centralized permission/ability definitions found".to_string(),
            file_path: ".".to_string(),
            line_number: None,
            evidence: None,
            suggestion: Some(
                "Create abilities.py or permissions.py to centralize authorization logic"
                    .to_string(),
            ),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::super::Detector;
    use super::*;
    use crate::testsupport::{build_good_scorecard_repo, build_problematic_scorecard_repo, tmpdir};

    // Translated from tests/scorecard/test_detectors.py::TestAbilityCoverageDetector.

    #[test]
    fn detects_missing_permissions() {
        let tmp = tmpdir("ability-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = AbilityCoverageDetector.detect(&repo);
        assert!(!findings.is_empty());
    }

    #[test]
    fn good_repo_checks_pass() {
        let tmp = tmpdir("ability-good");
        let repo = build_good_scorecard_repo(&tmp);
        // permissions.py exists and the route window contains
        // check_permission → no findings at all.
        assert!(AbilityCoverageDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_shape_on_problematic_repo() {
        let tmp = tmpdir("ability-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = AbilityCoverageDetector.detect(&repo);
        // Two protected-route hits on the same file (Depends(get_current_user)
        // appears on the decorated def line, @router.post above it), plus
        // the no-central-definitions info finding.
        assert_eq!(findings.len(), 3);
        assert!(
            findings[..2]
                .iter()
                .all(|f| f.message == "Protected route missing explicit permission check"
                    && f.severity == Severity::Warning
                    && f.file_path == "src/api/routes.py")
        );
        let info = &findings[2];
        assert_eq!(info.severity, Severity::Info);
        assert_eq!(info.file_path, ".");
        assert_eq!(
            info.message,
            "No centralized permission/ability definitions found"
        );
    }

    #[test]
    fn ability_files_anywhere_suppress_info_finding() {
        let tmp = tmpdir("ability-glob");
        let repo = tmp.join("r");
        // Even under node_modules (no skip filter on the glob).
        let nested = repo.join("node_modules/pkg");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("permissions.py"), "x = 1\n").unwrap();
        assert!(AbilityCoverageDetector.detect(&repo).is_empty());
    }
}
