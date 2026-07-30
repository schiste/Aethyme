//! Detector for undocumented API routes (port of
//! `src/scorecard/detectors/route_coverage.py`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::model::{Finding, Severity};
use crate::walk::{py_suffix, read_file_safe, rglob_all, should_skip_file};

pub struct RouteCoverageDetector;

struct RouteDefinition {
    method: String,
    path: String,
    file: PathBuf,
    line: usize,
}

struct Patterns {
    fastapi: Regex,
    flask: Regex,
    express: Regex,
}

fn patterns() -> &'static Patterns {
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| Patterns {
        fastapi: Regex::new(r#"@(router|app)\.(get|post|put|delete|patch)\(["']([^"']+)["']"#)
            .unwrap(),
        flask: Regex::new(r#"@app\.route\(["']([^"']+)["'].*methods=\[([^\]]+)\]"#).unwrap(),
        express: Regex::new(r#"(router|app)\.(get|post|put|delete|patch)\(["']([^"']+)["']"#)
            .unwrap(),
    })
}

impl super::Detector for RouteCoverageDetector {
    fn name(&self) -> &'static str {
        "route-coverage"
    }

    fn description(&self) -> &'static str {
        "Checks for undocumented API routes and endpoints"
    }

    fn detect(&self, repo_path: &Path) -> Vec<Finding> {
        let routes = self.find_routes(repo_path);
        self.check_route_documentation(repo_path, &routes)
    }
}

impl RouteCoverageDetector {
    fn find_routes(&self, repo_path: &Path) -> Vec<RouteDefinition> {
        let patterns = patterns();
        let mut routes = Vec::new();

        for entry in rglob_all(repo_path) {
            if !entry.is_file {
                continue;
            }
            if should_skip_file(&entry.path) {
                continue;
            }
            let suffix = py_suffix(&entry.path);

            if suffix == ".py" {
                let Some(content) = read_file_safe(&entry.path) else {
                    continue;
                };
                if content.is_empty() {
                    continue;
                }
                for (line_num, line) in content.split('\n').enumerate() {
                    let line_num = line_num + 1;
                    // FastAPI routes (first, like the Python order).
                    if let Some(caps) = patterns.fastapi.captures(line) {
                        routes.push(RouteDefinition {
                            method: caps[2].to_uppercase(),
                            path: caps[3].to_string(),
                            file: entry.path.clone(),
                            line: line_num,
                        });
                    }
                    // Flask routes: one route per listed method.
                    if let Some(caps) = patterns.flask.captures(line) {
                        for method in caps[2].split(',') {
                            let method =
                                method.trim_matches(|c| c == ' ' || c == '"' || c == '\'');
                            routes.push(RouteDefinition {
                                method: method.to_uppercase(),
                                path: caps[1].to_string(),
                                file: entry.path.clone(),
                                line: line_num,
                            });
                        }
                    }
                }
            } else if suffix == ".ts" || suffix == ".js" {
                let Some(content) = read_file_safe(&entry.path) else {
                    continue;
                };
                if content.is_empty() {
                    continue;
                }
                for (line_num, line) in content.split('\n').enumerate() {
                    let line_num = line_num + 1;
                    if let Some(caps) = patterns.express.captures(line) {
                        routes.push(RouteDefinition {
                            method: caps[2].to_uppercase(),
                            path: caps[3].to_string(),
                            file: entry.path.clone(),
                            line: line_num,
                        });
                    }
                }
            }
        }

        routes
    }

    fn check_route_documentation(
        &self,
        repo_path: &Path,
        routes: &[RouteDefinition],
    ) -> Vec<Finding> {
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

            let has_marker =
                |s: &str| s.contains("\"\"\"") || s.contains("'''") || s.contains("/**");

            // Look at the 10 lines up to and INCLUDING the decorator
            // line (Python's range(max(0, line_num-10), line_num) over
            // 0-based indices).
            let start = line_num.saturating_sub(10);
            let mut has_docs = (start..line_num).any(|i| {
                let line = lines.get(i).copied().unwrap_or("");
                has_marker(line)
            });

            // Then the up-to-5 lines after the decorator, but only when
            // line_num < len(lines) - 1 (off-by-one kept as-is).
            if !has_docs && line_num + 1 < lines.len() {
                let end = (line_num + 5).min(lines.len());
                let next_content = lines[line_num..end].join("\n");
                if has_marker(&next_content) {
                    has_docs = true;
                }
            }

            if !has_docs {
                let rel = route
                    .file
                    .strip_prefix(repo_path)
                    .unwrap_or(&route.file)
                    .to_string_lossy()
                    .to_string();
                findings.push(Finding {
                    detector: "route-coverage".to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Undocumented API route: {} {}",
                        route.method, route.path
                    ),
                    file_path: rel,
                    line_number: Some(line_num as i64),
                    evidence: None,
                    suggestion: Some(
                        "Add docstring/JSDoc describing the endpoint, parameters, and responses"
                            .to_string(),
                    ),
                });
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

    // Translated from tests/scorecard/test_detectors.py::TestRouteCoverageDetector.

    #[test]
    fn detects_undocumented_routes() {
        let tmp = tmpdir("routes-bad");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RouteCoverageDetector.detect(&repo);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("undocumented"))
        );
    }

    #[test]
    fn documented_routes_are_silent() {
        let tmp = tmpdir("routes-good");
        let repo = build_good_scorecard_repo(&tmp);
        assert!(RouteCoverageDetector.detect(&repo).is_empty());
    }

    #[test]
    fn exact_shape_flask_methods_and_express() {
        let tmp = tmpdir("routes-shape");
        let repo = build_problematic_scorecard_repo(&tmp);
        let findings = RouteCoverageDetector.detect(&repo);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].message, "Undocumented API route: POST /users");
        assert_eq!(findings[0].file_path, "src/api/routes.py");
        assert_eq!(findings[0].line_number, Some(10));
        assert_eq!(findings[0].evidence, None);

        // Flask multi-method + express, all undocumented.
        let repo2 = tmp.join("r2");
        std::fs::create_dir_all(&repo2).unwrap();
        std::fs::write(
            repo2.join("app.py"),
            "@app.route(\"/legacy\", methods=[\"GET\", \"POST\"])\ndef legacy():\n    return 1\n\n\n\n",
        )
        .unwrap();
        std::fs::write(
            repo2.join("server.js"),
            "app.delete(\"/items/:id\", handler);\nmore();\nmore();\nmore();\n",
        )
        .unwrap();
        let mut messages: Vec<String> = RouteCoverageDetector
            .detect(&repo2)
            .into_iter()
            .map(|f| f.message)
            .collect();
        messages.sort();
        assert_eq!(
            messages,
            vec![
                "Undocumented API route: DELETE /items/:id".to_string(),
                "Undocumented API route: GET /legacy".to_string(),
                "Undocumented API route: POST /legacy".to_string(),
            ]
        );
    }
}
