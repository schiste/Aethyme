//! Scoring engine (port of `src/scorecard/engine.py`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::detectors::{Detector, all_detectors};
use crate::model::{DetectorResult, ScorecardReport};
use crate::util::{now_timestamps, uuid4};
use crate::walk::{count_files_skip, py_suffix, rglob_all};

/// Extensions `_count_files` counts.
const COUNT_EXTENSIONS: [&str; 11] = [
    ".py", ".ts", ".tsx", ".jsx", ".js", ".md", ".json", ".yaml", ".yml", ".html", ".vue",
];

#[derive(Debug)]
pub struct ScorecardEngine {
    pub repo_path: PathBuf,
    pub repository_id: Option<String>,
    pub tenant_id: Option<String>,
}

impl ScorecardEngine {
    /// Errors when the repository path does not exist (Python raised
    /// `ValueError` with this message).
    pub fn new(
        repo_path: &Path,
        repository_id: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<Self, String> {
        if !repo_path.exists() {
            return Err(format!(
                "Repository path does not exist: {}",
                repo_path.display()
            ));
        }
        Ok(ScorecardEngine {
            repo_path: repo_path.to_path_buf(),
            repository_id,
            tenant_id,
        })
    }

    /// Run the scan. `detectors = None` runs the full registry;
    /// otherwise the registry is filtered by name — registry order is
    /// preserved and unknown names are silently ignored, exactly like
    /// the Python `_get_detectors`.
    pub fn scan(&self, detectors: Option<&[String]>) -> ScorecardReport {
        let scan_id = uuid4();
        let start = Instant::now();
        let (timestamp_iso, timestamp_display) = now_timestamps();

        let mut report = ScorecardReport::new(
            scan_id,
            self.repo_path.display().to_string(),
            self.repository_id.clone(),
            self.tenant_id.clone(),
            timestamp_iso,
            timestamp_display,
        );

        let to_run: Vec<Box<dyn Detector>> = match detectors {
            None => all_detectors(),
            Some(names) => all_detectors()
                .into_iter()
                .filter(|d| names.iter().any(|n| n == d.name()))
                .collect(),
        };

        for detector in to_run {
            let det_start = Instant::now();
            // Python wrapped `detector.detect()` in a try/except that
            // stored the exception message in `error`. The Rust
            // detectors are total (IO failures degrade to "unreadable
            // file", like `read_file_safe`), so `error` is always None.
            let findings = detector.detect(&self.repo_path);
            let execution_time_ms = det_start.elapsed().as_secs_f64() * 1000.0;
            let result = DetectorResult {
                detector_name: detector.name().to_string(),
                findings,
                execution_time_ms,
                error: None,
            };
            for finding in &result.findings {
                report.add_finding(finding.clone());
            }
            report.detector_results.push(result);
        }

        report.calculate_score();
        report.total_scan_time_ms = start.elapsed().as_secs_f64() * 1000.0;
        report.files_scanned = self.count_files();
        report
    }

    /// Port of `_count_files`: every file under the repo whose Python
    /// `suffix` is in the counted set and whose path shares no
    /// component with the skip-dir list (note: NO hidden-component
    /// rule here — the engine's check differed from the detectors').
    fn count_files(&self) -> i64 {
        let mut count = 0i64;
        for entry in rglob_all(&self.repo_path) {
            if !entry.is_file {
                continue;
            }
            let suffix = py_suffix(&entry.path);
            if !COUNT_EXTENSIONS.contains(&suffix.as_str()) {
                continue;
            }
            if count_files_skip(&entry.path) {
                continue;
            }
            count += 1;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_missing_path() {
        let err = ScorecardEngine::new(Path::new("/nonexistent/path"), None, None).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn counts_files_like_python() {
        let tmp = std::env::temp_dir().join(format!("aq-count-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::create_dir_all(tmp.join("dist")).unwrap();
        fs::create_dir_all(tmp.join(".hidden")).unwrap();
        fs::write(tmp.join("a.py"), "").unwrap();
        fs::write(tmp.join("b.rs"), "").unwrap(); // not counted (.rs)
        fs::write(tmp.join("src/c.md"), "").unwrap();
        fs::write(tmp.join("dist/d.js"), "").unwrap(); // skip dir
        fs::write(tmp.join(".hidden/e.py"), "").unwrap(); // counted! no hidden rule
        let engine = ScorecardEngine::new(&tmp, None, None).unwrap();
        assert_eq!(engine.count_files(), 3);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_registry_scan_scores_100() {
        let tmp = std::env::temp_dir().join(format!("aq-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let engine = ScorecardEngine::new(&tmp, Some("rid".into()), Some("tid".into())).unwrap();
        let report = engine.scan(Some(&["no-such-detector".to_string()]));
        assert_eq!(report.score, 100);
        assert_eq!(report.total_findings, 0);
        assert!(report.detector_results.is_empty());
        assert_eq!(report.repository_id.as_deref(), Some("rid"));
        assert_eq!(report.tenant_id.as_deref(), Some("tid"));
        let _ = fs::remove_dir_all(&tmp);
    }
}
